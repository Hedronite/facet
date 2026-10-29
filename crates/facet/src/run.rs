//! `facet request run`: execute through the shared core and HTTP engine
//! exactly as `probe request run` does, then record the run in Lattice.
//! `facet replay` reuses [`execute`] and [`render`] with its own resolution.

use std::{
    borrow::Cow,
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

use facet_record::{
    Hydration, RecordPolicy, RecordRequest, Recording, Redactor, actor_from_env,
    child::ChildError,
    file_root::{self, Outside},
    overlay_secrets, record, record_policy, recording_disabled, request_preview_json,
    session_from_env,
};
use lattice::now_ms;
use probe_core::{
    HttpRequest, TransportLock, resolve_environment_with_overrides, resolve_request,
    resolve_request_strict,
};
use probe_http::{ExecutionOptions, HttpError, HttpResponse};
use probe_opencollection::LoadedWorkspace;
use serde_json::{Value, json};

use crate::{
    CommandOutput, FacetError, args,
    expect::{EXPECT_FAIL_TAG, Expectation},
    presentation::{response_human, response_json},
    workspace::{WorkspaceInput, load, overrides_from_parsed},
};

const VALUE_FLAGS: &[&str] = &[
    "--environment",
    "--output",
    "--var",
    "--tag",
    "--expect",
    "--inline-body-max",
    "--history-retention",
];
const SWITCH_FLAGS: &[&str] = &[
    "--strict-variables",
    "--no-record",
    "--dry-run",
    "--no-subprocess",
    "--allow-file-outside-root",
];

/// One executed request: when it started, how long it took, and the outcome.
pub(crate) struct Execution {
    pub(crate) started_at: i64,
    pub(crate) elapsed_ms: i64,
    pub(crate) result: Result<HttpResponse, HttpError>,
}

/// `request run`. Everything it prints, success or error, goes through one
/// [`Redactor`] built from the run's known secret values.
pub(crate) fn run(args: &[String], stdin: &mut impl Read) -> Result<CommandOutput, FacetError> {
    let mut redactor = Redactor::default();
    match run_unredacted(args, stdin, &mut redactor) {
        Ok(output) => Ok(output.redact(&redactor)),
        Err(error) => Err(error.redact(&redactor)),
    }
}

/// `request run` before redaction. Fills `redactor` once the run knows its
/// secret values.
fn run_unredacted(
    args: &[String],
    stdin: &mut impl Read,
    redactor: &mut Redactor,
) -> Result<CommandOutput, FacetError> {
    let parsed = args::parse(args, VALUE_FLAGS, SWITCH_FLAGS)?;
    if parsed.switch("--no-subprocess") {
        facet_record::child::disable();
    }
    let [path, selector] = parsed.positionals() else {
        return Err(FacetError::invalid_arguments(
            "request run requires <path> and <selector>",
        ));
    };
    let input = WorkspaceInput::from_argument(path);
    let environment = parsed.value("--environment")?.map(str::to_owned);
    let output = parsed.value("--output")?.map(PathBuf::from);
    let strict_variables = parsed.switch("--strict-variables");
    let variables = parse_vars(&parsed)?;
    let mut tags: Vec<String> = parsed
        .values("--tag")
        .into_iter()
        .map(str::to_owned)
        .collect();
    let should_record = !parsed.switch("--no-record") && !recording_disabled();
    let overrides = overrides_from_parsed(&parsed)?;
    let expect = parsed
        .value("--expect")?
        .map(Expectation::parse)
        .transpose()?;
    let dry_run = parsed.switch("--dry-run");
    let allow_outside = parsed.switch("--allow-file-outside-root");
    if dry_run && (expect.is_some() || output.is_some()) {
        return Err(FacetError::invalid_arguments(
            "--dry-run sends nothing; it cannot be combined with --expect or --output",
        ));
    }

    let loaded = load(&input, stdin)?;
    let base = input.base_directory();
    let raw = lookup_request(&loaded, selector)?;
    let policy = policy_for(&input, selector)?;
    let mut hydration = hydrate(
        base.as_deref(),
        environment.as_deref(),
        raw,
        &loaded,
        &variables,
    )?;
    let mut merged_variables = hydration.merged(&variables);
    add_auth_token(
        &loaded,
        raw,
        environment.as_deref(),
        &mut hydration,
        &mut merged_variables,
    )?;
    *redactor = Redactor::new(hydration.redact.iter().cloned());
    let request = resolve_selected(
        &loaded,
        raw,
        environment.as_deref(),
        &merged_variables,
        strict_variables,
    )?;
    let sendable = confine_files(&request, base.as_deref(), selector, allow_outside)?;
    if dry_run {
        let preview = dry_run_output(&request, &hydration, environment.is_some());
        return Ok(case_warnings(&hydration)
            .into_iter()
            .fold(preview, CommandOutput::warn));
    }
    let execution = send(
        &sendable,
        &hydration,
        input.base_directory(),
        output.as_deref(),
    )?;
    let status = execution
        .result
        .as_ref()
        .ok()
        .map(|response| response.status);
    let miss = expect.as_ref().and_then(|expect| expect.miss(status));
    if miss.is_some() {
        // Auto-tag, no schema: `history --tag expect:fail` answers "what
        // failed this session" without --sql.
        tags.push(EXPECT_FAIL_TAG.to_owned());
    }

    let recording = if should_record {
        let root = input.base_directory();
        let actor = actor_from_env();
        let session = session_from_env();
        record(&RecordRequest {
            root: root.as_deref(),
            overrides: &overrides,
            selector,
            environment: environment.as_deref(),
            request: &request,
            started_at: execution.started_at,
            elapsed_ms: execution.elapsed_ms,
            result: &execution.result,
            output: output.as_deref(),
            tags: &tags,
            actor: &actor,
            session: session.as_deref(),
            replayed_from: None,
            var_names: &var_names(&variables),
            redact: &hydration.redact,
            policy,
        })
    } else {
        Recording::Skipped("disabled")
    };

    let (lattice_json, lattice_human) = with_secrets(
        recording.json(),
        recording.human(),
        &hydration,
        environment.is_some(),
    );
    let rendered = render(
        &request,
        execution,
        output.as_deref(),
        lattice_json,
        lattice_human,
        case_warnings(&hydration)
            .into_iter()
            .chain(recording.warning())
            .collect(),
        redactor,
    )?;
    Ok(match miss {
        Some(error) => rendered.fail(error),
        None => rendered,
    })
}

/// `--dry-run`: the resolved request (hydration included), nothing sent,
/// nothing recorded. `lattice.requestHash` is what a real run would store,
/// so an agent can compare it against a recorded run before firing.
pub(crate) fn dry_run_output(
    request: &HttpRequest,
    hydration: &Hydration,
    environment_selected: bool,
) -> CommandOutput {
    let preview = request_preview_json(request, &hydration.redact);
    // Human view shows enabled query parameters on the URL line, as sent.
    let query: Vec<String> = preview["query"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|item| item["disabled"] != true)
                .map(|item| {
                    format!(
                        "{}={}",
                        item["name"].as_str().unwrap_or(""),
                        item["value"].as_str().unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let url = preview["url"].as_str().unwrap_or("<unset>");
    let url = if query.is_empty() {
        url.to_owned()
    } else {
        format!("{url}?{}", query.join("&"))
    };
    let mut human = format!(
        "{} {url}
Headers:
",
        preview["method"].as_str().unwrap_or("<unset>")
    );
    match preview["headers"].as_array() {
        Some(headers) if !headers.is_empty() => {
            for header in headers {
                human.push_str(&format!(
                    "  {}: {}\n",
                    header["name"].as_str().unwrap_or(""),
                    header["value"].as_str().unwrap_or("")
                ));
            }
        }
        _ => human.push_str("  (none)\n"),
    }
    human.push('\n');
    match (
        preview["body"]["content"].as_str(),
        preview["body"]["sizeBytes"].as_u64(),
    ) {
        (Some(content), _) => {
            human.push_str(content);
            if !content.ends_with('\n') {
                human.push('\n');
            }
        }
        (None, Some(size)) => human.push_str(&format!("Body omitted ({size} bytes)\n")),
        (None, None) => human.push_str("(no body)\n"),
    }
    let mut lattice_json = json!({
        "recorded": false,
        "reason": "dry_run",
        "requestHash": preview["requestHash"],
    });
    let (lattice_json, lattice_human) = with_secrets(
        std::mem::take(&mut lattice_json),
        "not recorded (dry_run)".to_owned(),
        hydration,
        environment_selected,
    );
    human.push_str(&format!("Lattice: {lattice_human}\n"));
    let mut json = json!({ "dryRun": true, "request": preview });
    json["request"]
        .as_object_mut()
        .expect("preview is an object")
        .remove("requestHash");
    json["lattice"] = lattice_json;
    CommandOutput::new(human, json)
}

/// Secret hydration (Goal 5): Lattice environment values for the names the
/// request references, placed before the user's `--var` so those win.
/// Only with `--environment`; otherwise an empty overlay.
pub(crate) fn hydrate(
    base: Option<&Path>,
    environment: Option<&str>,
    request: &HttpRequest,
    loaded: &LoadedWorkspace,
    variables: &[(String, String)],
) -> Result<Hydration, FacetError> {
    overlay_secrets(
        base,
        environment,
        request,
        loaded.workspace().environments(),
        variables,
    )
    .map_err(FacetError::hydrate)
}

/// The collection's `facet:` recording policy for `selector`, read from
/// the source before anything is sent. A stdin workspace has none.
pub(crate) fn policy_for(
    input: &WorkspaceInput,
    selector: &str,
) -> Result<RecordPolicy, FacetError> {
    let source = match input {
        WorkspaceInput::Path(path) => Some(path.as_path()),
        WorkspaceInput::Stdin => None,
    };
    record_policy(source, selector).map_err(|error| FacetError::policy(&error))
}

/// The `githubToken` the request's Authorization header references, from
/// `gh`, merged last and added to the values the run redacts. The token
/// goes only to `https://api.github.com`: any other rendered URL fails
/// `github_token_host_refused` before `gh` runs. A request that passes the
/// host check is a secret request and sends locked, before `gh` is asked.
/// With child processes off (`--no-subprocess`, `FACET_NO_SUBPROCESS`) it
/// then fails `subprocess_disabled`, before anything is sent.
pub(crate) fn add_auth_token(
    loaded: &LoadedWorkspace,
    request: &HttpRequest,
    environment: Option<&str>,
    hydration: &mut Hydration,
    merged_variables: &mut Vec<(String, String)>,
) -> Result<(), FacetError> {
    if !wants_auth_token(request, merged_variables) {
        return Ok(());
    }
    // Rendered with a stand-in that no allowed URL can hold where the
    // authority ends, so a `{{githubToken}}` in the host or port refuses.
    let mut stand_in = merged_variables.clone();
    stand_in.push(("githubToken".to_owned(), "<githubToken>".to_owned()));
    let rendered = resolve_selected(loaded, request, environment, &stand_in, false)?;
    let url = rendered.url.as_deref().unwrap_or_default();
    if !is_github_api(url) {
        return Err(FacetError::github_token_host_refused(url_host(url)));
    }
    if !hydration.lock.is_locked() {
        hydration.lock = TransportLock::SecretReferenced;
    }
    hydration.references_secret = true;
    if facet_record::child::disabled() {
        return Err(FacetError::subprocess(&ChildError::Disabled));
    }
    let token = facet_record::auth_token_for(request, merged_variables)
        .map_err(|error| FacetError::subprocess(&error))?;
    if let Some(token) = token {
        merged_variables.push(("githubToken".to_owned(), token.clone()));
        hydration.redact.push(token);
        hydration.hydrated.push("githubToken".to_owned());
        hydration.source = Some("gh");
    }
    Ok(())
}

/// Whether [`facet_record::auth_token_for`] would ask `gh`: the
/// Authorization header references `{{githubToken}}` and no non-empty
/// variable supplies it. Decided without running `gh`.
fn wants_auth_token(request: &HttpRequest, variables: &[(String, String)]) -> bool {
    request.headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("authorization")
            && header.value.contains("{{githubToken}}")
    }) && !variables
        .iter()
        .any(|(name, value)| name == "githubToken" && !value.is_empty())
}

/// `https://api.github.com`, ignoring ASCII case, with no userinfo, port
/// 443 or none, and nothing after the authority but a path, query or
/// fragment. Anything else, including a URL that does not parse, is not.
fn is_github_api(url: &str) -> bool {
    const ORIGIN: &str = "https://api.github.com";
    let Some(origin) = url.get(..ORIGIN.len()) else {
        return false;
    };
    if !origin.eq_ignore_ascii_case(ORIGIN) {
        return false;
    }
    let rest = &url[ORIGIN.len()..];
    let rest = rest.strip_prefix(":443").unwrap_or(rest);
    rest.is_empty() || rest.starts_with(['/', '?', '#'])
}

/// The host of `url` for an error: the authority without userinfo or
/// port, or `None` when there is none. Never the path or query.
fn url_host(url: &str) -> Option<&str> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host.rfind(':') {
        Some(colon) if !host[colon..].contains(']') => &host[..colon],
        _ => host,
    };
    (!host.is_empty()).then_some(host)
}

/// One warning per referenced name that is only a case away from a
/// declared secret (`{{Token}}` against `token`): that reference is not the
/// secret, so it is neither hydrated nor treated as one. Names only.
pub(crate) fn case_warnings(hydration: &Hydration) -> Vec<String> {
    hydration
        .secret_case_collisions
        .iter()
        .map(|(referenced, secret)| {
            format!(
                "{{{{{referenced}}}}} differs only by case from the declared secret \
                 {secret}; it is not treated as that secret"
            )
        })
        .collect()
}

/// Adds `lattice.secrets` (names only) whenever an environment was selected,
/// and the human note when something was hydrated.
pub(crate) fn with_secrets(
    mut lattice_json: Value,
    mut lattice_human: String,
    hydration: &Hydration,
    environment_selected: bool,
) -> (Value, String) {
    if environment_selected {
        lattice_json["secrets"] = hydration.json();
    }
    if let Some(note) = hydration.human() {
        lattice_human = format!("{lattice_human} · {note}");
    }
    (lattice_json, lattice_human)
}

/// Parses repeated `--var NAME=VALUE` flags in order.
pub(crate) fn parse_vars(parsed: &args::Parsed) -> Result<Vec<(String, String)>, FacetError> {
    parsed
        .values("--var")
        .into_iter()
        .map(|entry| {
            entry
                .split_once('=')
                .filter(|(name, _)| !name.is_empty())
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .ok_or_else(|| {
                    FacetError::invalid_arguments(format!(
                        "--var expects NAME=VALUE, got {entry:?}"
                    ))
                })
        })
        .collect()
}

/// The names of the `--var` overrides, in order; values never leave here.
pub(crate) fn var_names(variables: &[(String, String)]) -> Vec<String> {
    variables.iter().map(|(name, _)| name.clone()).collect()
}

/// The request as sent: every file body and multipart file part checked
/// against the collection root by [`file_root::confine`] and rewritten to its
/// canonical path. The caller keeps `request` for its hash, preview and
/// record. `allow_outside` is `--allow-file-outside-root`, which only
/// `request run` and `replay` take; the TUI and MCP never pass it.
pub(crate) fn confine_files(
    request: &HttpRequest,
    base_directory: Option<&Path>,
    selector: &str,
    allow_outside: bool,
) -> Result<HttpRequest, FacetError> {
    let outside = if allow_outside {
        Outside::Allow
    } else {
        Outside::Refuse
    };
    file_root::confine(request, base_directory, outside)
        .map_err(|error| FacetError::file_body_outside_root(selector, error))
}

/// Sends `request` for `run` and `replay`. A secret request (including one
/// carrying the `gh` token) takes the locked transport: redirects off, proxy
/// environment ignored, and no `gh api` fallback.
pub(crate) fn send(
    request: &HttpRequest,
    hydration: &Hydration,
    base_directory: Option<PathBuf>,
    output: Option<&Path>,
) -> Result<Execution, FacetError> {
    let lock = match hydration.lock {
        TransportLock::Open if hydration.references_secret => TransportLock::SecretReferenced,
        lock => lock,
    };
    execute_transport(
        request,
        base_directory,
        output,
        hydration.references_secret,
        lock,
        &hydration.redact,
    )
}

/// Executes one resolved request on a fresh single-thread runtime, with
/// Ctrl-C cancellation and optional streaming to `output`.
pub(crate) fn execute(
    request: &HttpRequest,
    base_directory: Option<PathBuf>,
    output: Option<&Path>,
) -> Result<Execution, FacetError> {
    execute_transport(
        request,
        base_directory,
        output,
        false,
        TransportLock::Open,
        &[],
    )
}

/// `lock` and `known` go to `api_fallback`, which stays off unless `lock` is
/// `Open` and keeps every `known` value off `gh`'s argv.
fn execute_transport(
    request: &HttpRequest,
    base_directory: Option<PathBuf>,
    output: Option<&Path>,
    lock_transport: bool,
    lock: TransportLock,
    known: &[String],
) -> Result<Execution, FacetError> {
    let options = ExecutionOptions {
        base_directory,
        ..ExecutionOptions::default()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| FacetError::runtime(&error))?;
    let engine = facet_record::http_engine_from_env();

    let started_at = now_ms();
    let clock = Instant::now();
    let mut result = runtime.block_on(async {
        let engine = engine?;
        if lock_transport {
            if let Some(output) = output {
                engine
                    .execute_cancellable_to_file_locked(
                        request,
                        &options,
                        output,
                        tokio::signal::ctrl_c(),
                    )
                    .await
            } else {
                engine
                    .execute_cancellable_locked(request, &options, tokio::signal::ctrl_c())
                    .await
            }
        } else if let Some(output) = output {
            engine
                .execute_cancellable_to_file(request, &options, output, tokio::signal::ctrl_c())
                .await
        } else {
            engine
                .execute_cancellable(request, &options, tokio::signal::ctrl_c())
                .await
        }
    });
    if result.is_err()
        && let Some(Ok(response)) = facet_record::api_fallback(request, lock, known)
    {
        result = Ok(response);
    }
    let elapsed_ms = i64::try_from(clock.elapsed().as_millis()).unwrap_or(i64::MAX);
    Ok(Execution {
        started_at,
        elapsed_ms,
        result,
    })
}

/// Renders the upstream response document plus Facet's `lattice` field
/// (`lattice_json` / `lattice_human`), or the upstream error envelope with
/// `error.details.lattice` on a transport failure. URLs and response
/// headers go through `redactor` here; the caller redacts the rest.
pub(crate) fn render(
    request: &HttpRequest,
    execution: Execution,
    output: Option<&Path>,
    lattice_json: Value,
    lattice_human: String,
    warnings: Vec<String>,
    redactor: &Redactor,
) -> Result<CommandOutput, FacetError> {
    match execution.result {
        Ok(response) => {
            let human = format!(
                "{}\nLattice: {lattice_human}\n",
                response_human(request, &response, output, redactor),
            );
            let mut json = response_json(request, &response, output, redactor);
            json["lattice"] = lattice_json;
            let mut command = CommandOutput::new(human, json);
            for warning in warnings {
                command = command.warn(warning);
            }
            Ok(command)
        }
        Err(error) => {
            Err(FacetError::http(&error).with_details(json!({ "lattice": lattice_json })))
        }
    }
}

/// The unresolved request behind a selector.
pub(crate) fn lookup_request<'a>(
    loaded: &'a LoadedWorkspace,
    selector: &str,
) -> Result<&'a HttpRequest, FacetError> {
    let key = loaded
        .request_key(selector)
        .ok_or_else(|| FacetError::request_not_found(selector))?;
    Ok(loaded
        .workspace()
        .request(key)
        .expect("repository request key must resolve"))
}

/// Mirrors `probe-cli`'s interpolation rules over an already looked-up
/// request. `variables` are applied last, so the caller decides precedence.
pub(crate) fn resolve_selected<'a>(
    loaded: &LoadedWorkspace,
    request: &'a HttpRequest,
    environment: Option<&str>,
    variables: &[(String, String)],
    strict_variables: bool,
) -> Result<Cow<'a, HttpRequest>, FacetError> {
    let workspace = loaded.workspace();
    if environment.is_some() || !variables.is_empty() || strict_variables {
        let environment =
            resolve_environment_with_overrides(workspace.environments(), environment, variables)
                .map_err(FacetError::configuration)?;
        if strict_variables {
            resolve_request_strict(request, &environment)
        } else {
            resolve_request(request, &environment)
        }
        .map(Cow::Owned)
        .map_err(FacetError::configuration)
    } else {
        Ok(Cow::Borrowed(request))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::Path, process::Command};

    use super::*;

    /// What the stub `gh auth token` prints: a canary, never a credential.
    const GH_TOKEN: &str = "fn2-unit-gh-canary";
    /// Set only in the child process [`gh_token_is_injected_only_for_github_api`]
    /// starts with the stub `gh` at `FACET_GH`.
    const CHILD: &str = "FACET_FN2_STUB_GH_MARKER";

    const COLLECTION: &str = r#"opencollection: 1.0.0
info:
  name: FN-2 unit
bundled: true
items:
  - info: { name: GitHub, type: http, seq: 1 }
    http:
      method: GET
      url: "{{base}}/user"
      headers:
        - name: Authorization
          value: "Bearer {{githubToken}}"
"#;

    #[test]
    fn github_api_url_is_https_api_github_com_only() {
        for url in [
            "https://api.github.com",
            "https://api.github.com/",
            "https://api.github.com/user",
            "https://API.GitHub.COM/user",
            "https://api.github.com:443/user",
            "https://api.github.com?x=1",
            "https://api.github.com#x",
        ] {
            assert!(is_github_api(url), "{url} should be allowed");
        }
        for url in [
            "",
            "api.github.com/user",
            "http://api.github.com/user",
            "https://api.github.com:8443/user",
            "https://api.github.com:4430/user",
            "https://api.github.com:/user",
            "https://api.github.com./user",
            "https://api.github.com.invalid/user",
            "https://api.github.community/user",
            "https://user@api.github.com/user",
            "https://user:pass@api.github.com/user",
            "https://api.github.com@127.0.0.1/user",
            "https://api.github.com\\@127.0.0.1/user",
            "https://api.github.com<githubToken>/user",
            "https://github.com/user",
            "http://127.0.0.1:9/user",
            " https://api.github.com/user",
        ] {
            assert!(!is_github_api(url), "{url} should be refused");
        }
    }

    #[test]
    fn refused_host_is_named_without_userinfo_port_or_path() {
        assert_eq!(url_host("http://127.0.0.1:9/x?q=1"), Some("127.0.0.1"));
        assert_eq!(url_host("https://u:p@evil.test/x"), Some("evil.test"));
        assert_eq!(url_host("https://[::1]:8080/x"), Some("[::1]"));
        assert_eq!(url_host("https://[::1]/x"), Some("[::1]"));
        assert_eq!(url_host("not a url"), None);
        assert_eq!(url_host("https:///x"), None);
    }

    #[test]
    fn a_supplied_github_token_never_asks_gh() {
        let loaded = load(&WorkspaceInput::Stdin, &mut COLLECTION.as_bytes()).unwrap();
        let request = lookup_request(&loaded, "items/0").unwrap();
        assert!(wants_auth_token(request, &[]));
        assert!(wants_auth_token(
            request,
            &[("githubToken".to_owned(), String::new())]
        ));
        assert!(!wants_auth_token(
            request,
            &[("githubToken".to_owned(), "user-value".to_owned())]
        ));
    }

    /// Runs [`stub_gh_child`] in a child process with a stub `gh` at
    /// `FACET_GH`: the test never changes its own environment and never
    /// touches the network.
    #[test]
    fn gh_token_is_injected_only_for_github_api() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("gh-ran");
        let gh = dir.path().join("gh");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\n: > '{}'\nprintf '%s\\n' '{GH_TOKEN}'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "run::tests::stub_gh_child", "--ignored"])
            .args(["--nocapture", "--test-threads=1"])
            .env("FACET_GH", &gh)
            .env_remove("FACET_NO_SUBPROCESS")
            .env(CHILD, &marker)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "stub gh child failed:\n{}",
            String::from_utf8_lossy(&child.stdout)
        );
        assert!(
            String::from_utf8_lossy(&child.stdout).contains("1 passed"),
            "the stub gh child test should run"
        );
    }

    #[test]
    #[ignore = "run by gh_token_is_injected_only_for_github_api"]
    fn stub_gh_child() {
        let Some(marker) = std::env::var_os(CHILD) else {
            return;
        };
        let marker = Path::new(&marker);
        let loaded = load(&WorkspaceInput::Stdin, &mut COLLECTION.as_bytes()).unwrap();
        let request = lookup_request(&loaded, "items/0").unwrap();
        let token = |merged: &[(String, String)]| {
            merged
                .iter()
                .any(|(name, value)| name == "githubToken" && value == GH_TOKEN)
        };

        // A loopback URL: refused before gh runs, nothing injected.
        let mut hydration = Hydration::default();
        let mut merged = vec![("base".to_owned(), "http://127.0.0.1:9".to_owned())];
        let error = add_auth_token(&loaded, request, None, &mut hydration, &mut merged)
            .expect_err("a loopback URL should refuse the gh token");
        assert_eq!(error.category, "github_token_host_refused");
        assert_eq!(error.details, Some(json!({ "host": "127.0.0.1" })));
        assert!(!marker.exists(), "gh ran for a refused host");
        assert!(
            !token(&merged),
            "the gh token was injected for a refused host"
        );
        assert!(hydration.redact.is_empty() && hydration.hydrated.is_empty());
        assert!(!hydration.lock.is_locked() && !hydration.references_secret);

        // https://api.github.com: gh runs, the token is merged last and
        // redacted, and the request becomes a locked secret request, which
        // `send` sends on the locked transport for run and replay alike.
        let mut hydration = Hydration::default();
        let mut merged = vec![("base".to_owned(), "https://API.github.com".to_owned())];
        add_auth_token(&loaded, request, None, &mut hydration, &mut merged)
            .expect("api.github.com should take the gh token");
        assert!(marker.exists(), "gh should run for api.github.com");
        assert!(
            token(&merged),
            "the gh token should be in the merged variables"
        );
        assert!(hydration.redact.iter().any(|value| value == GH_TOKEN));
        assert_eq!(hydration.hydrated, ["githubToken"]);
        assert_eq!(hydration.source, Some("gh"));
        assert_eq!(hydration.lock, TransportLock::SecretReferenced);
        assert!(hydration.lock.is_locked());
        assert!(hydration.references_secret);

        // An existing lock is kept, never lowered.
        let mut hydration = Hydration {
            lock: TransportLock::VarUnclassified,
            ..Hydration::default()
        };
        let mut merged = vec![("base".to_owned(), "https://api.github.com".to_owned())];
        add_auth_token(&loaded, request, None, &mut hydration, &mut merged).unwrap();
        assert_eq!(hydration.lock, TransportLock::VarUnclassified);
        assert!(hydration.references_secret);
    }
}

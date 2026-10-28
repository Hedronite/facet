use probe_core::{Body, HttpRequest, RequestBody, TransportLock};
use probe_http::{HttpError, HttpResponse, ResponseHeader};
use std::process::Stdio;
use std::time::Instant;

use crate::SENSITIVE_HEADERS;
use crate::child::{self, ChildError, Program};

/// Supplies githubToken only for a request whose YAML explicitly references it.
/// The token comes from gh's credential store and never enters YAML or Lattice.
pub fn auth_token_for(
    request: &HttpRequest,
    overrides: &[(String, String)],
) -> Result<Option<String>, ChildError> {
    let references_token = request.headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("authorization")
            && header.value.contains("{{githubToken}}")
    });
    if !references_token
        || overrides
            .iter()
            .any(|(name, value)| name == "githubToken" && !value.is_empty())
    {
        return Ok(None);
    }
    // Refused or switched off is `Err`; a failed or absent `gh` is
    // `Ok(None)`, exactly as when spawn failed on the base.
    let Some(output) = child::output(Program::Gh, ["auth", "token"])? else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let Ok(token) = String::from_utf8(output.stdout) else {
        return Ok(None);
    };
    let token = token.trim().to_owned();
    Ok((!token.is_empty()).then_some(token))
}

/// Runs a GitHub collection request through gh when native HTTP failed.
/// A successful exchange is returned as an ordinary HttpResponse, so the
/// existing recorder writes it exactly like a native response.
///
/// Never for a locked send: a `lock` other than
/// [`probe_core::TransportLock::Open`] means the native error stands. It
/// never puts a header named in `SENSITIVE_HEADERS` on argv, nor a header
/// whose value holds a value in `known`; a body carrying a known value
/// does not travel to a child's stdin at all. The disable switch, or a
/// refused `gh` path, keeps the fallback out entirely.
pub fn api_fallback(
    request: &HttpRequest,
    lock: TransportLock,
    known: &[String],
) -> Option<Result<HttpResponse, HttpError>> {
    if lock.is_locked() {
        return None;
    }
    let url = request.url.as_deref()?;
    let endpoint = github_api_path(url)?;
    let method = request.method.as_deref().unwrap_or("GET");
    let body = request_body(request);
    if body
        .as_deref()
        .is_some_and(|body| contains_a_known_value(body, known))
    {
        return None;
    }
    let gh = child::child(Program::Gh).ok()??;
    let mut command = gh.command();
    command
        .arg("api")
        .arg(if endpoint.is_empty() { "/" } else { endpoint })
        .arg("--method")
        .arg(method)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for header in &request.headers {
        if SENSITIVE_HEADERS
            .iter()
            .any(|name| header.name.eq_ignore_ascii_case(name))
            || contains_a_known_value(&header.value, known)
        {
            continue;
        }
        command
            .arg("--header")
            .arg(format!("{}: {}", header.name, header.value));
    }
    if body.is_some() {
        // The only stdin a child ever reads: an unlocked, known-value-free
        // body, because `gh api --input -` takes it off argv.
        command.arg("--input").arg("-").stdin(Stdio::piped());
    }
    let started = Instant::now();
    let mut running = match command.spawn() {
        Ok(running) => running,
        Err(_) => return None,
    };
    if let Some(body) = body
        && let Some(mut stdin) = running.stdin.take()
    {
        let _ = std::io::Write::write_all(&mut stdin, body.as_bytes());
    }
    let output = match running.wait_with_output() {
        Ok(output) => output,
        Err(error) => return Some(Err(HttpError::Transport(error.to_string()))),
    };
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Some(Err(HttpError::Transport(if message.is_empty() {
            "gh api failed".to_owned()
        } else {
            message
        })));
    }
    Some(Ok(HttpResponse {
        status: 200,
        reason: "OK".to_owned(),
        url: url.to_owned(),
        duration: started.elapsed(),
        size: output.stdout.len(),
        headers: vec![ResponseHeader {
            name: "content-type".to_owned(),
            value: "application/json".to_owned(),
        }],
        body: output.stdout,
        body_complete: true,
        body_file: None,
        body_retention_error: None,
    }))
}

/// The path part of a `https://api.github.com` URL, or `None` for anything
/// else: scheme and host match the origin exactly (ignoring ASCII case),
/// the port is 443 or absent, and nothing follows the authority but a
/// path, query or fragment. A longer host name (`api.github.com.invalid`),
/// a foreign port or userinfo keeps the fallback out, as `run.rs`'s
/// `github_token_host_refused` check does for the token.
fn github_api_path(url: &str) -> Option<&str> {
    const ORIGIN: &str = "https://api.github.com";
    let origin = url.get(..ORIGIN.len())?;
    if !origin.eq_ignore_ascii_case(ORIGIN) {
        return None;
    }
    let rest = &url[ORIGIN.len()..];
    let rest = rest.strip_prefix(":443").unwrap_or(rest);
    (rest.is_empty() || rest.starts_with(['/', '?', '#'])).then_some(rest)
}

/// Whether any non-empty value in `known` occurs in `text`. An empty value
/// can never match.
fn contains_a_known_value(text: &str, known: &[String]) -> bool {
    known
        .iter()
        .any(|known| !known.is_empty() && text.contains(known))
}

fn request_body(request: &HttpRequest) -> Option<String> {
    match request.body.as_ref()? {
        RequestBody::Single(Body::Raw(body)) => Some(body.data.clone()),
        _ => None,
    }
}

pub(crate) use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    thread::{self, JoinHandle},
};

pub(crate) use serde_json::{Value, json};

/// A throwaway workspace: the collection, its `.facet/`, and a private
/// machine store, all under one temporary directory.
pub(crate) struct Sandbox {
    pub(crate) dir: tempfile::TempDir,
}

impl Sandbox {
    pub(crate) fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("temporary directory"),
        }
    }

    pub(crate) fn root(&self) -> &Path {
        self.dir.path()
    }

    /// A bundled collection whose request references a declared secret
    /// (`token`, `secret: true`) in its URL query, a custom header, and its
    /// body, so the mock server sees the hydrated value, nothing but Lattice
    /// can supply it, and every stored text column is exercised by scrubbing.
    pub(crate) fn secret_workspace(&self, server_url: &str) -> PathBuf {
        let source = format!(
            "opencollection: 1.0.0\ninfo:\n  name: Secret fixture\nbundled: true\nconfig:\n  environments:\n    - name: local\n      variables:\n        - name: serverUrl\n          value: {server_url}\n        - secret: true\n          name: token\n          type: string\nitems:\n  - info:\n      name: Echo secret\n      type: http\n      seq: 1\n    http:\n      method: POST\n      url: \"{{{{serverUrl}}}}/echo?t={{{{token}}}}\"\n      headers:\n        - name: X-Token\n          value: \"{{{{token}}}}\"\n      body:\n        type: json\n        data: '{{\"token\":\"{{{{token}}}}\"}}'\n"
        );
        let path = self.root().join("workspace.yml");
        fs::write(&path, source).unwrap();
        path
    }

    /// Writes the phase-5 HTTP fixture with `__SERVER_URL__` replaced.
    pub(crate) fn workspace(&self, server_url: &str) -> PathBuf {
        let source = fs::read_to_string(fixture("phase5-http.yml")).unwrap();
        let path = self.root().join("workspace.yml");
        fs::write(&path, source.replace("__SERVER_URL__", server_url)).unwrap();
        path
    }

    /// A `facet` command whose machine store and config live in the sandbox.
    pub(crate) fn facet(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_facet"));
        command
            .env("FACET_DATA_DIR", self.root().join("machine"))
            .env("FACET_CONFIG_DIR", self.root().join("config"))
            .env_remove("FACET_ACTOR")
            .env_remove("FACET_SESSION")
            .env_remove("FACET_NO_RECORD")
            .env_remove("FACET_KUBECONFIG")
            .env_remove("FACET_KUBE_CONTEXT")
            .env_remove("FACET_HEDRON_DB")
            .env_remove("FACET_HEDRON_TOKEN")
            // `gh` and `git` come from FACET_GH / FACET_GIT or a fixed list,
            // never PATH. A missing FACET_GH is refused, so no test reaches a
            // real `gh`; a test that wants one points FACET_GH at a stub.
            .env("FACET_GH", self.root().join("no-gh"))
            .env_remove("FACET_GIT")
            .env_remove("FACET_NO_SUBPROCESS")
            // Encrypted secrets backend: deterministic, no OS keyring prompts.
            .env("FACET_SECRET_KEY", "test-master-key")
            // Session metadata picks these up when present; goldens must not.
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_TAB_ID")
            .env_remove("HERDR_PANE_ID")
            // Session commands count runs from the workspace store discovered
            // from the current directory.
            .current_dir(self.root());
        command
    }

    /// [`Self::facet`] with `FACET_SESSION` set, as a harness would run it.
    pub(crate) fn facet_in_session(&self, session: &str) -> Command {
        let mut command = self.facet();
        command.env("FACET_SESSION", session);
        command
    }

    /// Runs `facet … --json` inside a session and returns parsed stdout.
    pub(crate) fn run_json_in_session(&self, session: &str, arguments: &[&str]) -> Value {
        let output = self
            .facet_in_session(session)
            .args(arguments)
            .arg("--json")
            .output()
            .expect("facet should run");
        assert!(
            output.status.success(),
            "exit {:?}\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON")
    }

    pub(crate) fn run_json(&self, arguments: &[&str]) -> Value {
        let output = self
            .facet()
            .args(arguments)
            .arg("--json")
            .output()
            .expect("facet should run");
        assert!(
            output.status.success(),
            "exit {:?}\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON")
    }

    /// `facet env set --secret` with the value piped to `--value-stdin`, the
    /// way a user should store a secret: `--value --secret` warns on stderr.
    pub(crate) fn env_set_secret(
        &self,
        ws: &str,
        environment: &str,
        name: &str,
        value: &str,
    ) -> Value {
        let mut child = self
            .facet()
            .args([
                "env",
                "set",
                ws,
                "--environment",
                environment,
                "--name",
                name,
            ])
            .args(["--value-stdin", "--secret", "--json"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("facet should run");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(value.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success() && output.stderr.is_empty(),
            "env set --value-stdin: exit {:?}",
            output.status.code()
        );
        serde_json::from_slice(&output.stdout).expect("stdout should be JSON")
    }

    /// Runs `facet … --json` and returns the exit code plus parsed stdout (errors
    /// are JSON on stdout, not stderr).
    pub(crate) fn run_error_json(&self, arguments: &[&str]) -> (u8, Value) {
        let output = self
            .facet()
            .args(arguments)
            .arg("--json")
            .output()
            .expect("facet should run");
        let exit_code = output.status.code().unwrap_or(255) as u8;
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "stdout should be JSON (exit {exit_code}): {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (exit_code, value)
    }

    /// Turns the sandbox root into a git repository with one commit holding
    /// `workspace.yml`, returning HEAD's sha. Local only, no remote.
    pub(crate) fn git_init(&self) -> String {
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(self.root())
                .args(args)
                .output()
                .expect("git should run");
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=facet-test",
            "-c",
            "user.email=facet@test",
            "add",
            "workspace.yml",
        ]);
        git(&[
            "-c",
            "user.name=facet-test",
            "-c",
            "user.email=facet@test",
            "commit",
            "-q",
            "-m",
            "fixture",
        ]);
        git(&["rev-parse", "HEAD"])
    }

    /// Rewrites `duration_ms` for one run so `diff` output is deterministic.
    pub(crate) fn set_duration_ms(&self, run_id: &str, duration_ms: i64) {
        let db = std::path::absolute(self.root().join(".facet/lattice.db"))
            .expect("workspace store path");
        let conn = rusqlite::Connection::open(&db).expect("open workspace store");
        let updated = conn
            .execute(
                "UPDATE runs SET duration_ms = ?1 WHERE id = ?2",
                rusqlite::params![duration_ms, run_id],
            )
            .expect("set duration");
        assert_eq!(updated, 1, "set duration for one run");
    }

    /// Rewrites `started_at` for one run in the workspace store (test helper).
    pub(crate) fn backdate_run(&self, run_id: &str, started_at_ms: i64) {
        let db = std::path::absolute(self.root().join(".facet/lattice.db"))
            .expect("workspace store path");
        assert!(db.is_file(), "missing {}", db.display());
        let conn = rusqlite::Connection::open(&db).expect("open workspace store");
        let updated = conn
            .execute(
                "UPDATE runs SET started_at = ?1 WHERE id = ?2",
                rusqlite::params![started_at_ms, run_id],
            )
            .expect("backdate run");
        assert_eq!(updated, 1, "backdate one run in {}", db.display());
    }
}

pub(crate) fn fixture(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/opencollection")
        .join(path)
}

pub(crate) fn serve_once(body: Vec<u8>, content_type: &str) -> (String, JoinHandle<Vec<u8>>) {
    serve_once_at("127.0.0.1:0", body, content_type)
}

/// Serves one request again on the address a previous [`serve_once`] used,
/// so a replay resolves to the identical URL (and request hash).
pub(crate) fn serve_again(url: &str, body: Vec<u8>) -> JoinHandle<Vec<u8>> {
    let address = url.trim_start_matches("http://");
    serve_once_at(address, body, "application/json").1
}

fn serve_once_at(bind: &str, body: Vec<u8>, content_type: &str) -> (String, JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind(bind).expect("mock server should bind");
    let address = listener.local_addr().unwrap();
    let content_type = content_type.to_owned();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 8 * 1024];
        let header_end = loop {
            if let Some(position) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break position + 4;
            }
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
        };
        let head = String::from_utf8_lossy(&request[..header_end]).into_owned();
        let content_length = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .map(|(_, value)| value.trim().parse::<usize>().unwrap())
            .unwrap_or(0);
        while request.len() - header_end < content_length {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
        request[header_end..header_end + content_length].to_vec()
    });
    (format!("http://{address}"), handle)
}

/// Error envelopes carry volatile `message` text; category and exitCode are the contract.
pub(crate) fn normalize_error(value: Value) -> Value {
    let mut normalized = normalize(value);
    if let Value::Object(root) = &mut normalized
        && let Some(Value::Object(error)) = root.get_mut("error")
    {
        error.insert("message".to_owned(), json!("<message>"));
    }
    normalized
}

/// Replaces values that legitimately differ between runs (ids, times,
/// hashes, paths, ports) so JSON can be compared against a golden file.
pub(crate) fn normalize(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    let replaced = match key.as_str() {
                        "version" | "probeVersion" => json!("<version>"),
                        "id" | "runId" | "workspaceId" => json!("<ulid>"),
                        "sessionId" | "replayedFrom" if value.is_string() => json!("<ulid>"),
                        "startedAt" | "durationMs" | "updatedAt" | "pinnedAt" => json!("<int>"),
                        "endedAt" if value.is_number() => json!("<int>"),
                        "requestHash" | "recordedHash" | "currentHash" => json!("<sha256>"),
                        "hash" if value.is_string() => json!("<sha256>"),
                        "path" | "outputPath" | "cwd" | "workspacePath" | "dataDir"
                        | "configDir" | "dbPath"
                            if value.is_string() =>
                        {
                            json!("<path>")
                        }
                        "url" if value.is_string() => json!("<url>"),
                        // `diff` change rows and hash pairs: `{ "a": …, "b": … }`.
                        "a" | "b" if is_sha256(&value) => json!("<sha256>"),
                        "a" | "b"
                            if value
                                .as_str()
                                .is_some_and(|text| text.starts_with("http://")) =>
                        {
                            json!("<url>")
                        }
                        _ => normalize(value),
                    };
                    (key, replaced)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(normalize).collect()),
        other => other,
    }
}

fn is_sha256(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Pretty JSON with sorted keys regardless of serde_json's map feature.
pub(crate) fn canonical(value: &Value) -> String {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|key| (key.clone(), sorted(&map[key])))
                        .collect(),
                )
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string_pretty(&sorted(value)).unwrap()
}

/// Compares against `tests/golden/<name>`; set `UPDATE_GOLDEN=1` to rewrite.
pub(crate) fn assert_golden(name: &str, value: &Value) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    let actual = canonical(value);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        fs::write(&path, format!("{actual}\n")).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("missing golden {}: {error}", path.display()));
    assert_eq!(
        actual.trim_end(),
        expected.trim_end(),
        "golden mismatch for {name}; run with UPDATE_GOLDEN=1 to accept"
    );
}

//! Hermetic `facet apply --frozen` tests. HOME + FACET_CONFIG_DIR live in the
//! sandbox so nothing writes the operator `~/.facet/lattice.db`.

#[allow(dead_code, unused_imports)]
mod common;

use std::fs;
use std::io::Write;
use std::path::Path;

use common::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const WORLD: &str = r#"
let lib = import "lib.ncl" in
{
  replicas = 1,
  cluster = [ lib.pod { name = "web", n = replicas } ],
  intent = [
    {
      name = "test-docs-eod",
      importance = 0.5,
      spec = { kind = "docs_eod", date = "2026-09-12", required_briefs = [] },
    },
  ],
  calls = {
    opencollection = "1.0.0",
    environments = [ { name = "dev", variables = [ { name = "TOKEN", secret_ref = "kr:me" } ] } ],
    plaintext_token | not_exported = "hunter2",
  },
}
"#;

const LIB: &str = r#"
{
  pod = fun args => {
    apiVersion = "v1",
    kind = "Pod",
    metadata.name = args.name,
    metadata.annotations.replicas = std.to_string args.n,
  },
}
"#;

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

fn write_world(root: &Path) {
    fs::write(root.join("lib.ncl"), LIB).unwrap();
    fs::write(root.join("world.ncl"), WORLD).unwrap();
}

fn snapshot(root: &Path) -> Value {
    let mut sources = serde_json::Map::new();
    for name in ["lib.ncl", "world.ncl"] {
        sources.insert(
            name.to_owned(),
            json!(fs::read_to_string(root.join(name)).unwrap()),
        );
    }
    json!({"entry": "world.ncl", "sources": sources})
}

fn bundle(root: &Path) -> Value {
    let snap = snapshot(root);
    let entry = snap["sources"]["world.ncl"].as_str().unwrap();
    let module_hash = sha256_hex(entry.as_bytes());
    json!({
        "schemaVersion": 1,
        "path": "world.ncl",
        "moduleHash": module_hash,
        "exportHash": "c59535830c90b27220168775e936398073c61a8e073df4af8f9ca52b212ccff7",
        "contractSet": "k8s-1.34-h3s-0.9.1",
        "varNames": [],
        "sourceSnapshot": snap,
        "cluster": [{"apiVersion":"v1","kind":"Pod","metadata":{"name":"web"}}],
        "intent": [{
            "name": "test-docs-eod",
            "importance": 0.5,
            "spec": {"kind":"docs_eod","date":"2026-09-12","required_briefs":[]}
        }],
        "calls": {"opencollection":"1.0.0"}
    })
}

fn init_store(sandbox: &Sandbox) {
    let out = sandbox
        .facet()
        .args(["history", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "init lattice: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_frozen(sandbox: &Sandbox, extra: &[&str], body: &Value) -> std::process::Output {
    init_store(sandbox);
    let path = sandbox.root().join("bundle.json");
    fs::write(&path, serde_json::to_vec(body).unwrap()).unwrap();
    let mut cmd = sandbox.facet();
    cmd.env("HOME", sandbox.root());
    cmd.args(["apply", "--frozen", path.to_str().unwrap()]);
    cmd.args(extra);
    cmd.output().unwrap()
}

#[test]
fn docs_eod_and_frozen_apply_have_no_hedron_ncl_and_pinned_sha() {
    let src = include_str!("../src/docs_eod.rs");
    assert_eq!(
        sha256_hex(src.as_bytes()),
        "e013d039590950065a0006aaf8868e3eee97af154e59aad1ab8c67cda5b9b33d"
    );
    let needle = ["hedron", "ncl"].join("_");
    let fa = include_str!("../src/frozen_apply.rs");
    let prod = fa.split("#[cfg(test)]").next().unwrap();
    assert!(!prod.contains(&format!("use {needle}")));
    assert!(!prod.contains(&format!("{needle}::")));
    assert!(!src.contains(&format!("use {needle}")));
    assert!(!src.contains(&format!("{needle}::")));
}

#[test]
fn apply_frozen_json_and_human_and_quiet() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let body = bundle(sandbox.root());
    let json_out = run_frozen(&sandbox, &["--json"], &body);
    assert_eq!(
        json_out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&json_out.stderr)
    );
    let v: Value = serde_json::from_slice(&json_out.stdout).unwrap();
    assert_eq!(v["schemaVersion"], 1);
    assert_eq!(v["path"], "world.ncl");
    assert_eq!(v["contractSet"], "k8s-1.34-h3s-0.9.1");
    assert_eq!(v["action"]["cluster"]["skipped"], true);
    assert_eq!(v["action"]["intent"]["skipped"], true);
    assert!(json_out.stderr.is_empty());

    let human = run_frozen(&sandbox, &[], &body);
    assert_eq!(human.status.code(), Some(0));
    let line = String::from_utf8_lossy(&human.stdout);
    assert!(line.starts_with("ok world.ncl (moduleHash="));
    assert!(line.contains("cluster.posted=0"));
    assert!(line.contains("intent.put=0"));
    assert!(line.ends_with("\n"));

    let quiet = run_frozen(&sandbox, &["-q"], &body);
    assert_eq!(quiet.status.code(), Some(0));
    assert!(quiet.stdout.is_empty());
}

#[test]
fn apply_frozen_stdin() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    init_store(&sandbox);
    let body = bundle(sandbox.root());
    let mut child = sandbox.facet();
    child.env("HOME", sandbox.root());
    child.args(["apply", "--frozen", "-", "--json"]);
    child.stdin(std::process::Stdio::piped());
    child.stdout(std::process::Stdio::piped());
    child.stderr(std::process::Stdio::piped());
    let mut proc = child.spawn().unwrap();
    proc.stdin
        .as_mut()
        .unwrap()
        .write_all(serde_json::to_vec(&body).unwrap().as_slice())
        .unwrap();
    let out = proc.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schemaVersion"], 1);
}

#[test]
fn apply_frozen_session_and_actor_and_lattice_row() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let body = bundle(sandbox.root());
    init_store(&sandbox);
    let path = sandbox.root().join("bundle.json");
    fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .env("FACET_SESSION", "01ARZ3NDEKTSV4RRFFQ69G5FAV")
        .env("FACET_ACTOR", "agent:qa")
        .args(["apply", "--frozen", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hist = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .args(["history", "--json", "--limit", "1"])
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&hist.stdout).unwrap();
    let row = &v["runs"][0];
    assert_eq!(row["method"], "NCL");
    assert_eq!(row["requestPath"], "ncl:apply");
    assert_eq!(row["actor"], "agent:qa");
    assert_eq!(row["sessionId"], "01ARZ3NDEKTSV4RRFFQ69G5FAV");
    let tags = row["tags"].as_array().unwrap();
    assert!(
        tags.iter()
            .any(|t| t.as_str().unwrap().starts_with("ncl:module:"))
    );
    assert!(
        tags.iter()
            .any(|t| t.as_str().unwrap().starts_with("ncl:export:"))
    );
    assert!(
        tags.iter()
            .any(|t| t.as_str().unwrap() == "ncl:contracts:k8s-1.34-h3s-0.9.1")
    );
}

#[test]
fn apply_frozen_e5_empty_hedron_db() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let body = bundle(sandbox.root());
    init_store(&sandbox);
    let path = sandbox.root().join("bundle.json");
    fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .env("FACET_HEDRON_DB", "")
        .args(["apply", "--frozen", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["category"], "invalid_arguments");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("FACET_HEDRON_DB is empty")
    );
}

#[test]
fn apply_frozen_e7_bad_spec_without_db_skips_shape() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let mut body = bundle(sandbox.root());
    body["intent"][0]["spec"]["date"] = json!(1);
    let out = run_frozen(&sandbox, &["--json"], &body);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["action"]["intent"]["skipped"], true);
}

#[test]
fn apply_frozen_e4_bad_date_with_db() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let db = sandbox.root().join("hedron.sqlite");
    let mut body = bundle(sandbox.root());
    body["intent"][0]["spec"]["date"] = json!(1);
    init_store(&sandbox);
    let path = sandbox.root().join("bundle.json");
    fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .env("FACET_HEDRON_DB", &db)
        .args(["apply", "--frozen", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["category"], "invalid_arguments");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("intent spec shape:")
    );
}

#[test]
fn apply_frozen_e6_unknown_kind_with_db() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let db = sandbox.root().join("hedron.sqlite");
    let mut body = bundle(sandbox.root());
    body["intent"][0]["spec"]["kind"] = json!("nope");
    init_store(&sandbox);
    let path = sandbox.root().join("bundle.json");
    fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .env("FACET_HEDRON_DB", &db)
        .args(["apply", "--frozen", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown desired state kind")
    );
}

#[test]
fn apply_frozen_schema_errors() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let mut body = bundle(sandbox.root());
    body["nope"] = json!(1);
    let out = run_frozen(&sandbox, &["--json"], &body);
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["category"], "invalid_arguments");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("frozen bundle:")
    );

    let mut body = bundle(sandbox.root());
    body["moduleHash"] = json!("deadbeef");
    let out = run_frozen(&sandbox, &["--json"], &body);
    assert_eq!(out.status.code(), Some(2));

    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .args(["apply", "--frozen", "missing.json", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error"]["category"], "invalid_workspace");

    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .args(["apply", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));

    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .args([
            "apply", "--frozen", "a.json", "--frozen", "b.json", "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn apply_frozen_never_ncl_invalid() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let mut body = bundle(sandbox.root());
    body["varNames"] = json!(["a=b"]);
    let out = run_frozen(&sandbox, &["--json"], &body);
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_ne!(v["error"]["category"], "ncl_invalid");
    assert_ne!(v["error"]["exitCode"], 5);
}

fn history_apply_rows(sandbox: &Sandbox) -> Vec<Value> {
    let hist = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .args(["history", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        hist.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&hist.stderr)
    );
    let v: Value = serde_json::from_slice(&hist.stdout).unwrap();
    v["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["requestPath"] == "ncl:apply")
        .cloned()
        .collect()
}

fn blob_bytes(sandbox: &Sandbox, hash: &str) -> Vec<u8> {
    fs::read(sandbox.root().join(".facet/blobs").join(hash)).unwrap_or_else(|error| {
        panic!("blob {hash}: {error}");
    })
}

#[test]
fn apply_frozen_intent_put() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let db = sandbox.root().join("hedron.db");
    hedron_core::Store::open(&db).expect("create empty hedrondb schema");
    let body = bundle(sandbox.root());
    init_store(&sandbox);
    let path = sandbox.root().join("bundle.json");
    fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .env("FACET_HEDRON_DB", &db)
        .args(["apply", "--frozen", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["action"]["intent"]["skipped"], false);
    assert_eq!(v["action"]["intent"]["put"], 1);
    assert_eq!(v["action"]["intent"]["names"], json!(["test-docs-eod"]));
}

#[test]
fn apply_frozen_lattice_blob_parity_with_ncl_apply() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    init_store(&sandbox);

    let frozen = bundle(sandbox.root());
    let path = sandbox.root().join("bundle.json");
    fs::write(&path, serde_json::to_vec(&frozen).unwrap()).unwrap();
    let frozen_out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .args(["apply", "--frozen", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(
        frozen_out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&frozen_out.stderr)
    );

    let rows = history_apply_rows(&sandbox);
    assert_eq!(rows.len(), 1, "one ncl:apply row");
    let row = &rows[0];
    let golden: Value =
        serde_json::from_str(include_str!("fixtures/frozen_apply_lattice_apply_row.json")).unwrap();

    for key in [
        "method",
        "requestPath",
        "requestHash",
        "url",
        "actor",
        "tags",
        "varNames",
    ] {
        assert_eq!(row[key], golden[key], "{key}");
    }
    assert_eq!(
        row["request"]["body"]["hash"].as_str().unwrap(),
        golden["requestBodyHash"].as_str().unwrap()
    );
    assert_eq!(
        row["request"]["body"]["sizeBytes"].as_u64().unwrap(),
        golden["requestBodySizeBytes"].as_u64().unwrap()
    );
    assert_eq!(
        row["response"]["body"]["hash"].as_str().unwrap(),
        golden["responseBodyHash"].as_str().unwrap()
    );
    assert_eq!(
        row["response"]["body"]["sizeBytes"].as_u64().unwrap(),
        golden["responseBodySizeBytes"].as_u64().unwrap()
    );
    let req_hash = row["request"]["body"]["hash"].as_str().unwrap();
    let res_hash = row["response"]["body"]["hash"].as_str().unwrap();
    assert!(!blob_bytes(&sandbox, req_hash).is_empty());
    assert!(!blob_bytes(&sandbox, res_hash).is_empty());
}

#[test]
fn apply_frozen_more_schema_and_cli_errors() {
    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let mut body = bundle(sandbox.root());
    body["schemaVersion"] = json!("1");
    let out = run_frozen(&sandbox, &["--json"], &body);
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("frozen bundle:")
    );

    let mut body = bundle(sandbox.root());
    body["sourceSnapshot"]["entry"] = json!("lib.ncl");
    let out = run_frozen(&sandbox, &["--json"], &body);
    assert_eq!(out.status.code(), Some(2));

    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .args(["apply", "--frozen", "bundle.json", "extra", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));

    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .args(["apply", "--frozen", "bundle.json", "--dry-run", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn apply_frozen_cluster_post_against_mock_https() {
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    let sandbox = Sandbox::new();
    write_world(sandbox.root());
    let tls = sandbox.root().join("tls");
    fs::create_dir_all(&tls).unwrap();
    let ca_key = tls.join("ca.key");
    let ca_pem = tls.join("ca.pem");
    let server_key = tls.join("server.key");
    let server_csr = tls.join("server.csr");
    let server_pem = tls.join("server.pem");
    let client_key = tls.join("client.key");
    let client_csr = tls.join("client.csr");
    let client_pem = tls.join("client.pem");
    let ext = tls.join("san.cnf");
    fs::write(&ext, "subjectAltName=IP:127.0.0.1\n").unwrap();
    let run_ssl = |args: &[&str]| {
        let out = Command::new("openssl")
            .args(args)
            .output()
            .expect("openssl");
        assert!(
            out.status.success(),
            "openssl {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run_ssl(&[
        "req",
        "-x509",
        "-newkey",
        "rsa:2048",
        "-keyout",
        ca_key.to_str().unwrap(),
        "-out",
        ca_pem.to_str().unwrap(),
        "-days",
        "1",
        "-nodes",
        "-subj",
        "/CN=test-cluster-ca",
        "-addext",
        "basicConstraints=critical,CA:TRUE",
        "-addext",
        "keyUsage=critical,keyCertSign,cRLSign",
    ]);
    run_ssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-keyout",
        server_key.to_str().unwrap(),
        "-out",
        server_csr.to_str().unwrap(),
        "-nodes",
        "-subj",
        "/CN=test-server",
    ]);
    run_ssl(&[
        "x509",
        "-req",
        "-in",
        server_csr.to_str().unwrap(),
        "-CA",
        ca_pem.to_str().unwrap(),
        "-CAkey",
        ca_key.to_str().unwrap(),
        "-CAcreateserial",
        "-out",
        server_pem.to_str().unwrap(),
        "-days",
        "1",
        "-extfile",
        ext.to_str().unwrap(),
    ]);
    run_ssl(&[
        "req",
        "-newkey",
        "rsa:2048",
        "-keyout",
        client_key.to_str().unwrap(),
        "-out",
        client_csr.to_str().unwrap(),
        "-nodes",
        "-subj",
        "/CN=facet-test",
    ]);
    let client_ext = tls.join("client.cnf");
    fs::write(
        &client_ext,
        "[ext]\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=clientAuth\n",
    )
    .unwrap();
    run_ssl(&[
        "x509",
        "-req",
        "-in",
        client_csr.to_str().unwrap(),
        "-CA",
        ca_pem.to_str().unwrap(),
        "-CAkey",
        ca_key.to_str().unwrap(),
        "-CAcreateserial",
        "-out",
        client_pem.to_str().unwrap(),
        "-days",
        "1",
        "-extfile",
        client_ext.to_str().unwrap(),
        "-extensions",
        "ext",
    ]);
    let ready = tls.join("ready");
    let log = tls.join("server.log");
    let server_py = sandbox.root().join("k8s_mock.py");
    fs::write(
        &server_py,
        r#"import http.server, ssl, sys, pathlib
port = int(sys.argv[1])
server_cert, server_key, ca, ready = sys.argv[2:6]
class H(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(n)
        body = b'{"ok":true}'
        self.send_response(201)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, fmt, *args):
        sys.stderr.write((fmt % args) + "\n")
httpd = http.server.HTTPServer(("127.0.0.1", port), H)
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.minimum_version = ssl.TLSVersion.TLSv1_2
ctx.load_cert_chain(server_cert, server_key)
ctx.load_verify_locations(ca)
ctx.verify_mode = ssl.CERT_OPTIONAL
httpd.socket = ctx.wrap_socket(httpd.socket, server_side=True)
pathlib.Path(ready).write_text("ok")
httpd.handle_request()
"#,
    )
    .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let log_file = fs::File::create(&log).unwrap();
    let mut child = Command::new("python3")
        .args([
            server_py.to_str().unwrap(),
            &port.to_string(),
            server_pem.to_str().unwrap(),
            server_key.to_str().unwrap(),
            ca_pem.to_str().unwrap(),
            ready.to_str().unwrap(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone().unwrap()))
        .stderr(Stdio::from(log_file))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "https mock never became ready: {}",
            fs::read_to_string(&log).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(20));
    }
    let b64 = |path: &std::path::Path| {
        let out = Command::new("base64")
            .args(["-i", path.to_str().unwrap()])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().replace('\n', "")
    };
    let kube = sandbox.root().join("kubeconfig");
    fs::write(
        &kube,
        format!(
            "apiVersion: v1\nkind: Config\ncurrent-context: t\n\
contexts:\n- name: t\n  context: {{cluster: c, user: u}}\n\
clusters:\n- name: c\n  cluster:\n    server: https://127.0.0.1:{port}\n    certificate-authority-data: {}\n\
users:\n- name: u\n  user:\n    client-certificate-data: {}\n    client-key-data: {}\n",
            b64(&ca_pem),
            b64(&client_pem),
            b64(&client_key)
        ),
    )
    .unwrap();
    let mut body = bundle(sandbox.root());
    body["cluster"] = json!([{
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {"name": "web", "namespace": "default"}
    }]);
    init_store(&sandbox);
    let path = sandbox.root().join("bundle.json");
    fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    let out = sandbox
        .facet()
        .env("HOME", sandbox.root())
        .env("FACET_KUBECONFIG", &kube)
        .args(["apply", "--frozen", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout={} stderr={} server={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        fs::read_to_string(&log).unwrap_or_default()
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["action"]["cluster"]["skipped"], false);
    assert_eq!(v["action"]["cluster"]["posted"], 1);
    assert_eq!(v["action"]["cluster"]["results"][0]["kind"], "Pod");
    assert_eq!(v["action"]["cluster"]["results"][0]["name"], "web");
    assert_eq!(v["action"]["cluster"]["results"][0]["status"], 201);
}

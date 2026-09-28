//! `facet mcp`: a Model Context Protocol server over stdio.
//!
//! A typed adapter over the same functions the CLI uses. Every tool maps its
//! arguments onto the command function (`run::run`, `history::history`,
//! `session::session`, …) and returns that function's JSON document as the
//! tool result: the existing `schemaVersion: 1` envelopes, byte for byte the
//! same as `facet <command> --json`. Errors are the same
//! `error.{category, exitCode, message, details}` object. There are no
//! MCP-only fields and there is never a `std::process::Command`.
//!
//! `request_list` / `request_get` are Probe's commands; Facet delegates them
//! to `probe_cli::run` in-process, exactly as the CLI does, and returns the
//! upstream document unchanged.
//!
//! Transport: JSON-RPC 2.0, one message per line, stdin → stdout. Logging
//! goes to stderr only. No HTTP transport in v1.

use std::io::{self, BufRead, Write};
use std::path::Path;

use lattice::{LatticeConfig, MachineStore, WorkspaceStore};
use serde_json::{Map, Value, json};

use crate::{
    CommandOutput, FacetError, diff, history, replay, run, session, version, versioned_json,
    workspace::WorkspaceInput,
};

/// Protocol version answered when the client's is unknown.
const PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Serves MCP over the process's stdin/stdout until EOF. Exit code 0 on a
/// clean EOF, 1 on an I/O failure.
#[must_use]
pub fn run_mcp(args: &[String]) -> u8 {
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!(
            "facet mcp: Model Context Protocol server over stdio (JSON-RPC 2.0, one message per line).\n\
             Tools: {}.\nSee docs/FACET.md § mcp.",
            TOOLS.join(", ")
        );
        return 0;
    }
    if !args.is_empty() {
        eprintln!("error[invalid_arguments]: mcp takes no arguments");
        return 2;
    }
    let stdin = io::stdin().lock();
    let stdout = io::stdout().lock();
    match serve(stdin, stdout) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("error[mcp_io]: {error}");
            1
        }
    }
}

/// Tool names, in the order `tools/list` reports them.
pub const TOOLS: &[&str] = &[
    "session_start",
    "session_end",
    "request_list",
    "request_get",
    "request_run",
    "history_list",
    "history_get",
    "blob_get",
    "run_diff",
    "run_replay",
    "ncl_check",
    "ncl_export",
    "ncl_apply",
    "sql_query",
];

/// Reads JSON-RPC messages from `input` and writes responses to `output`.
pub(crate) fn serve<R: BufRead, W: Write>(mut input: R, mut output: W) -> io::Result<()> {
    let mut frame = Vec::with_capacity(8 * 1024);
    while let Some(oversized) = read_frame(&mut input, &mut frame)? {
        if oversized {
            write_message(
                &mut output,
                &rpc_error(
                    Value::Null,
                    -32700,
                    "parse error: MCP frame exceeds 8 MiB".to_owned(),
                ),
            )?;
            continue;
        }
        if frame.iter().all(|byte| byte.is_ascii_whitespace()) {
            continue;
        }
        let line = match std::str::from_utf8(&frame) {
            Ok(line) => line,
            Err(error) => {
                write_message(
                    &mut output,
                    &rpc_error(Value::Null, -32700, format!("parse error: {error}")),
                )?;
                continue;
            }
        };
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                write_message(
                    &mut output,
                    &rpc_error(Value::Null, -32700, format!("parse error: {error}")),
                )?;
                continue;
            }
        };
        if let Some(response) = handle(&message) {
            write_message(&mut output, &response)?;
        }
    }
    Ok(())
}

fn read_frame<R: BufRead>(input: &mut R, frame: &mut Vec<u8>) -> io::Result<Option<bool>> {
    frame.clear();
    let mut oversized = false;
    loop {
        let (consumed, ended) = {
            let buffer = input.fill_buf()?;
            if buffer.is_empty() {
                return if frame.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(oversized))
                };
            }
            let newline = buffer.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(buffer.len(), |index| index + 1);
            let payload_len = newline.map_or(consumed, |index| index);
            let room = MAX_FRAME_BYTES.saturating_sub(frame.len());
            let copied = payload_len.min(room);
            frame.extend_from_slice(&buffer[..copied]);
            if copied < payload_len {
                oversized = true;
            }
            (consumed, newline.is_some())
        };
        input.consume(consumed);
        if ended {
            return Ok(Some(oversized));
        }
    }
}

fn write_message<W: Write>(output: &mut W, message: &Value) -> io::Result<()> {
    let mut text = serde_json::to_string(message).expect("JSON value serialization cannot fail");
    text.push('\n');
    output.write_all(text.as_bytes())?;
    output.flush()
}

/// Handles one message. `None` for notifications (no `id`).
pub(crate) fn handle(message: &Value) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    if method.starts_with("notifications/") {
        return None;
    }
    let Some(id) = id else {
        // A request without an id is a notification we do not know; ignore.
        return None;
    };
    Some(match method {
        "initialize" => rpc_result(id, initialize_result(&params)),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, json!({ "tools": tool_descriptions() })),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = params
                .get("arguments")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if !TOOLS.contains(&name) {
                return Some(rpc_error(id, -32602, format!("unknown tool: {name}")));
            }
            let (document, is_error) = call_tool(name, &arguments);
            rpc_result(
                id,
                json!({
                    "content": [{ "type": "text", "text": pretty(&document) }],
                    "structuredContent": document,
                    "isError": is_error,
                }),
            )
        }
        other => rpc_error(id, -32601, format!("method not found: {other}")),
    })
}

fn initialize_result(params: &Value) -> Value {
    let protocol = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": protocol,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "facet", "version": version() },
        "instructions": concat!(
            "Facet: run, recall, replay HTTP requests with a Lattice history. ",
            "Every tool result is the same schemaVersion 1 JSON as `facet <command> --json`; ",
            "errors carry error.{category, exitCode, message, details}. ",
            "Bodies are explicit pulls (blob_get). Never pass secret values as arguments; ",
            "store them with `facet env set` and let hydration supply them. ",
            "TypeSafe/Jev: hydrate typesafeApiKey via facet env set --secret (TYPESAFE_API_KEY), never Lattice; jevShadow empty/ask/escalate/hold ≠ approve."
        ),
    })
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON value serialization cannot fail")
}

/// Runs one tool. Returns the versioned document and whether it is an error
/// (a `FacetError`, or a success document that carries `error`, such as an
/// `--expect` miss).
fn call_tool(name: &str, arguments: &Map<String, Value>) -> (Value, bool) {
    match dispatch(name, arguments) {
        Ok(output) => {
            let (json, _warnings, _exit_code) = output.into_parts();
            let document = versioned_json(json);
            let is_error = document.get("error").is_some();
            (document, is_error)
        }
        Err(error) => (error.envelope(), true),
    }
}

fn dispatch(name: &str, arguments: &Map<String, Value>) -> Result<CommandOutput, FacetError> {
    let args = Args(arguments);
    match name {
        "session_start" => {
            let mut argv = vec!["start".to_owned()];
            push_value(&mut argv, "--actor", args.string("actor")?);
            if let Some(meta) = args.object("meta")? {
                argv.push("--meta".to_owned());
                argv.push(Value::Object(meta).to_string());
            }
            session::session(&argv)
        }
        "session_end" => {
            let mut argv = vec!["end".to_owned()];
            if let Some(id) = args.string("id")? {
                argv.push(id);
            }
            session::session(&argv)
        }
        "request_list" => {
            let path = args.required("path")?;
            delegate(&["request", "list", &path])
        }
        "request_get" => {
            let path = args.required("path")?;
            let selector = args.required("selector")?;
            let mut argv = vec!["request", "get", path.as_str(), selector.as_str()];
            let environment = args.string("environment")?;
            if let Some(environment) = &environment {
                argv.push("--environment");
                argv.push(environment);
            }
            if args.bool("strictVariables")? {
                argv.push("--strict-variables");
            }
            delegate(&argv)
        }
        "request_run" => {
            let path = args.required("path")?;
            let selector = args.required("selector")?;
            let environment = args.string("environment")?;
            let vars = args.vars("var")?;
            reject_lattice_backed_vars(&path, environment.as_deref(), &vars)?;
            let mut argv = vec![path, selector];
            push_value(&mut argv, "--environment", environment);
            push_vars(&mut argv, &vars);
            push_list(&mut argv, "--tag", args.strings("tag")?);
            push_value(&mut argv, "--expect", args.expect()?);
            if args.bool("dryRun")? {
                argv.push("--dry-run".to_owned());
            }
            if args.bool("strictVariables")? {
                argv.push("--strict-variables".to_owned());
            }
            if args.bool("noRecord")? {
                argv.push("--no-record".to_owned());
            }
            run::run(&argv, &mut io::empty())
        }
        "history_list" => {
            let mut argv = Vec::new();
            if let Some(path) = args.string("path")? {
                argv.push(path);
            }
            push_value(
                &mut argv,
                "--limit",
                args.integer("limit")?.map(|n| n.to_string()),
            );
            push_value(&mut argv, "--request", args.string("request")?);
            push_value(
                &mut argv,
                "--status",
                args.integer("status")?.map(|n| n.to_string()),
            );
            push_value(&mut argv, "--actor", args.string("actor")?);
            push_value(
                &mut argv,
                "--since",
                args.integer("since")?.map(|n| n.to_string()),
            );
            push_value(&mut argv, "--session", args.string("session")?);
            push_value(&mut argv, "--environment", args.string("environment")?);
            push_list(&mut argv, "--tag", args.strings("tag")?);
            push_value(&mut argv, "--hash", args.string("hash")?);
            if args.bool("bodies")? {
                argv.push("--bodies".to_owned());
            }
            history::history(&argv)
        }
        "history_get" => {
            let mut argv = Vec::new();
            if let Some(path) = args.string("path")? {
                argv.push(path);
            }
            argv.push("--id".to_owned());
            argv.push(args.required("id")?);
            if args.bool("bodies")? {
                argv.push("--bodies".to_owned());
            }
            history::history(&argv)
        }
        "blob_get" => {
            let mut argv = vec![args.required("hash")?];
            if let Some(path) = args.string("path")? {
                argv.push(path);
            }
            history::blob(&argv)
        }
        "run_diff" => {
            let mut argv = vec![args.required("a")?, args.required("b")?];
            if let Some(path) = args.string("path")? {
                argv.push(path);
            }
            if args.bool("bodies")? {
                argv.push("--bodies".to_owned());
            }
            diff::diff(&argv)
        }
        "run_replay" => {
            let id = args.required("id")?;
            let path = args.string("path")?.unwrap_or_else(|| ".".to_owned());
            let environment = args.string("environment")?;
            let vars = args.vars("var")?;
            reject_lattice_backed_vars(&path, environment.as_deref(), &vars)?;
            let mut argv = vec![id, path];
            push_value(&mut argv, "--environment", environment);
            push_vars(&mut argv, &vars);
            push_list(&mut argv, "--tag", args.strings("tag")?);
            push_value(&mut argv, "--expect", args.expect()?);
            if args.bool("frozen")? {
                argv.push("--frozen".to_owned());
            }
            replay::replay(&argv, &mut io::empty())
        }
        "sql_query" => {
            let mut argv = Vec::new();
            if let Some(path) = args.string("path")? {
                argv.push(path);
            }
            argv.push("--sql".to_owned());
            argv.push(args.required("sql")?);
            history::history(&argv)
        }
        "ncl_check" => {
            let path = args.required("path")?;
            let overrides = ncl_overrides(&args)?;
            let import_paths = ncl_import_paths(&args)?;
            let result = crate::ncl::check(Path::new(&path), &overrides, &import_paths)?;
            Ok(CommandOutput::new(Vec::new(), result.to_json()))
        }
        "ncl_export" => {
            let path = args.required("path")?;
            let overrides = ncl_overrides(&args)?;
            let import_paths = ncl_import_paths(&args)?;
            let result = crate::ncl::export(Path::new(&path), &overrides, &import_paths)?;
            Ok(CommandOutput::new(Vec::new(), result.to_json()))
        }
        "ncl_apply" => {
            let path = args.required("path")?;
            let overrides = ncl_overrides(&args)?;
            let import_paths = ncl_import_paths(&args)?;
            let result = crate::ncl::apply(Path::new(&path), &overrides, &import_paths)?;
            Ok(CommandOutput::new(Vec::new(), result.to_json()))
        }
        other => Err(FacetError::invalid_arguments(format!(
            "unknown tool: {other}"
        ))),
    }
}

/// Probe's own commands, in-process, the upstream document unchanged.
fn delegate(argv: &[&str]) -> Result<CommandOutput, FacetError> {
    let mut args: Vec<String> = argv.iter().map(|item| (*item).to_owned()).collect();
    args.push("--json".to_owned());
    let output = probe_cli::run(args);
    let document: Value = serde_json::from_str(&output.stdout).map_err(|error| {
        FacetError::invalid_arguments(format!("upstream did not return JSON: {error}"))
    })?;
    // Upstream's error envelope (non-zero exit) passes through unchanged;
    // `call_tool` flags it by its `error` key. `versioned_json` re-inserts
    // the schemaVersion upstream already set.
    Ok(CommandOutput::new(Vec::new(), document))
}

/// MCP must not take raw secrets as tool arguments when a Lattice env key
/// exists: the value would sit in the client's transcript. Hydration
/// supplies stored values; `facet env set` changes them.
fn ncl_import_paths(args: &Args) -> Result<Vec<std::path::PathBuf>, FacetError> {
    crate::args::resolve_ncl_import_paths(args.string("import_path")?.as_deref())
}

fn ncl_overrides(args: &Args) -> Result<Vec<String>, FacetError> {
    let pairs = args.vars("var")?;
    let overrides: Vec<String> = pairs
        .iter()
        .map(|(path, value)| format!("{path}={value}"))
        .collect();
    crate::args::reject_secret_overrides(&overrides)?;
    Ok(overrides)
}
fn reject_lattice_backed_vars(
    path: &str,
    environment: Option<&str>,
    vars: &[(String, String)],
) -> Result<(), FacetError> {
    if vars.is_empty() {
        return Ok(());
    }
    let Some(base) = WorkspaceInput::from_argument(path).base_directory() else {
        return Ok(());
    };
    let Some(root) = WorkspaceStore::discover(&base) else {
        return Ok(());
    };
    let Ok(Some(store)) = LatticeConfig::load(&root)
        .map_err(lattice::LatticeError::from)
        .and_then(|config| WorkspaceStore::open_existing(&root, config))
    else {
        return Ok(());
    };
    let Ok(machine) = MachineStore::open(store.config()) else {
        return Ok(());
    };
    let Ok(rows) = machine.environments(store.workspace_id()) else {
        return Ok(());
    };
    for (name, _) in vars {
        let stored = rows
            .iter()
            .any(|row| row.key == *name && environment.is_none_or(|env| row.name == env));
        if stored {
            return Err(FacetError::invalid_arguments(format!(
                "`{name}` is stored in the Facet machine store; MCP tools must not receive its \
                 value as an argument. Let hydration supply it, or change it with `facet env set`."
            )));
        }
    }
    Ok(())
}

fn push_value(argv: &mut Vec<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value {
        argv.push(flag.to_owned());
        argv.push(value);
    }
}

fn push_list(argv: &mut Vec<String>, flag: &str, values: Vec<String>) {
    for value in values {
        argv.push(flag.to_owned());
        argv.push(value);
    }
}

fn push_vars(argv: &mut Vec<String>, vars: &[(String, String)]) {
    for (name, value) in vars {
        argv.push("--var".to_owned());
        argv.push(format!("{name}={value}"));
    }
}

/// Typed access to a tool's `arguments` object.
struct Args<'a>(&'a Map<String, Value>);

impl Args<'_> {
    fn required(&self, key: &str) -> Result<String, FacetError> {
        self.string(key)?
            .ok_or_else(|| FacetError::invalid_arguments(format!("{key} is required")))
    }

    fn string(&self, key: &str) -> Result<Option<String>, FacetError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(other) => Err(FacetError::invalid_arguments(format!(
                "{key} must be a string, got {other}"
            ))),
        }
    }

    fn integer(&self, key: &str) -> Result<Option<i64>, FacetError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(number)) if number.is_i64() => Ok(number.as_i64()),
            Some(other) => Err(FacetError::invalid_arguments(format!(
                "{key} must be an integer, got {other}"
            ))),
        }
    }

    fn bool(&self, key: &str) -> Result<bool, FacetError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(flag)) => Ok(*flag),
            Some(other) => Err(FacetError::invalid_arguments(format!(
                "{key} must be a boolean, got {other}"
            ))),
        }
    }

    fn strings(&self, key: &str) -> Result<Vec<String>, FacetError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str().map(str::to_owned).ok_or_else(|| {
                        FacetError::invalid_arguments(format!(
                            "{key} must be an array of strings, got {item}"
                        ))
                    })
                })
                .collect(),
            Some(other) => Err(FacetError::invalid_arguments(format!(
                "{key} must be an array of strings, got {other}"
            ))),
        }
    }

    fn object(&self, key: &str) -> Result<Option<Map<String, Value>>, FacetError> {
        match self.0.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(map)) => Ok(Some(map.clone())),
            Some(other) => Err(FacetError::invalid_arguments(format!(
                "{key} must be an object, got {other}"
            ))),
        }
    }

    /// `{ "name": "value" }` → `--var name=value` pairs, in key order.
    fn vars(&self, key: &str) -> Result<Vec<(String, String)>, FacetError> {
        let Some(map) = self.object(key)? else {
            return Ok(Vec::new());
        };
        map.into_iter()
            .map(|(name, value)| match value {
                Value::String(text) => Ok((name, text)),
                Value::Number(number) => Ok((name, number.to_string())),
                Value::Bool(flag) => Ok((name, flag.to_string())),
                other => Err(FacetError::invalid_arguments(format!(
                    "{key}.{name} must be a string, got {other}"
                ))),
            })
            .collect()
    }

    /// `expect`: a string spec (`"2xx"`, `"200,201"`) or an array of codes
    /// and class strings, joined into the CLI spec.
    fn expect(&self) -> Result<Option<String>, FacetError> {
        match self.0.get("expect") {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(Value::Array(items)) => {
                let tokens = items
                    .iter()
                    .map(|item| match item {
                        Value::Number(number) => Ok(number.to_string()),
                        Value::String(text) => Ok(text.clone()),
                        other => Err(FacetError::invalid_arguments(format!(
                            "expect entries must be codes or classes, got {other}"
                        ))),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Some(tokens.join(",")))
            }
            Some(other) => Err(FacetError::invalid_arguments(format!(
                "expect must be a string or an array, got {other}"
            ))),
        }
    }
}

/// `tools/list` entries: name, description, JSON Schema for the arguments.
pub(crate) fn tool_descriptions() -> Vec<Value> {
    let string = |description: &str| json!({ "type": "string", "description": description });
    let integer = |description: &str| json!({ "type": "integer", "description": description });
    let boolean = |description: &str| json!({ "type": "boolean", "description": description });
    let strings = |description: &str| json!({ "type": "array", "items": { "type": "string" }, "description": description });
    let path_optional = string(
        "File or directory inside the workspace (default: current directory); Facet walks up to .facet/lattice.db",
    );
    let path_required =
        string("Collection path: an OpenCollection YAML file or an unbundled workspace directory");
    let expect = json!({
        "description": "Assert the status after a real run: \"2xx\", \"200,201\", or an array like [200, \"3xx\"]. Miss → exit 1 expect_failed with the full document.",
        "oneOf": [ { "type": "string" }, { "type": "array", "items": { "oneOf": [ { "type": "integer" }, { "type": "string" } ] } } ]
    });
    let ncl_var = json!({
        "type": "object",
        "additionalProperties": { "type": "string" },
        "description": "Nickel field overrides (path.to.field → expression). Never a secret value or secret field path."
    });
    let var = json!({
        "type": "object",
        "additionalProperties": { "type": "string" },
        "description": "Variable overrides (--var). Never a secret: names stored in the Facet machine store are refused; hydration supplies them."
    });
    let schema = |properties: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    };
    let tool = |name: &str, description: &str, input_schema: Value| json!({ "name": name, "description": description, "inputSchema": input_schema });
    vec![
        tool(
            "session_start",
            "Start a session in the machine store and return it ({ session: { id, actor, startedAt, endedAt, meta, runs? } }). Export the id as FACET_SESSION for the runs that follow.",
            schema(
                json!({
                    "actor": string("Agent name (default: FACET_ACTOR, else human)"),
                    "meta": json!({ "type": "object", "description": "Pointers only (harness ids, cwd); merged over Herdr ids. Never transcripts or secrets." }),
                }),
                &[],
            ),
        ),
        tool(
            "session_end",
            "End a session (default: FACET_SESSION). Idempotent: alreadyEnded says whether it was already closed.",
            schema(json!({ "id": string("Session ULID, or `current`") }), &[]),
        ),
        tool(
            "request_list",
            "Probe's request list for a collection: { requests: [ { selector, name, method, url, … } ] }. Use selectors, never names.",
            schema(json!({ "path": path_required.clone() }), &["path"]),
        ),
        tool(
            "request_get",
            "Probe's request get: one request as stored, or resolved with an environment.",
            schema(
                json!({
                    "path": path_required.clone(),
                    "selector": string("Request selector from request_list"),
                    "environment": string("Environment name to resolve with"),
                    "strictVariables": boolean("Fail on any unresolved variable"),
                }),
                &["path", "selector"],
            ),
        ),
        tool(
            "request_run",
            "Execute a request through Facet and record it in Lattice. Result is Probe's request-run document plus lattice { runId, requestHash, secrets, … }. With dryRun the request is resolved and previewed, nothing sent or recorded. TypeSafe/Jev: hydrate typesafeApiKey via facet env set --secret (TYPESAFE_API_KEY), never Lattice; jevShadow empty/ask/escalate/hold ≠ approve.",
            schema(
                json!({
                    "path": path_required.clone(),
                    "selector": string("Request selector"),
                    "environment": string("Environment name; enables secret hydration from the machine store"),
                    "var": var.clone(),
                    "tag": strings("Tags to store with the run"),
                    "expect": expect.clone(),
                    "dryRun": boolean("Preview only: resolve, redact, do not send"),
                    "strictVariables": boolean("Fail on any unresolved variable"),
                    "noRecord": boolean("Execute without writing to Lattice"),
                }),
                &["path", "selector"],
            ),
        ),
        tool(
            "history_list",
            "Recorded runs, newest first, metadata only (bodies are explicit pulls). Filters AND together.",
            schema(
                json!({
                    "path": path_optional.clone(),
                    "limit": integer("Maximum rows (default 50)"),
                    "request": string("Only runs of this selector"),
                    "status": integer("Only runs with this HTTP status"),
                    "actor": string("Only runs by this actor"),
                    "since": integer("Only runs started at or after this Unix millisecond time"),
                    "session": string("Session ULID, or `current` for FACET_SESSION"),
                    "environment": string("Only runs resolved with this environment"),
                    "tag": strings("Every listed tag must be present"),
                    "hash": string("Request hash, request body hash, or response body hash; rows say matchedHash"),
                    "bodies": boolean("Include inline bodies"),
                }),
                &[],
            ),
        ),
        tool(
            "history_get",
            "One recorded run by id: { workspace, run }. run_not_found (exit 4) when unknown.",
            schema(
                json!({
                    "id": string("Run ULID"),
                    "path": path_optional.clone(),
                    "bodies": boolean("Include inline bodies"),
                }),
                &["id"],
            ),
        ),
        tool(
            "blob_get",
            "One stored body by SHA-256: { blob: { hash, sizeBytes, contentType, body } }. Bodies may contain secrets the redaction list cannot see.",
            schema(
                json!({
                    "hash": string("SHA-256 hex from a history row"),
                    "path": path_optional.clone(),
                }),
                &["hash"],
            ),
        ),
        tool(
            "run_diff",
            "Hash-first comparison of two recorded runs: changes by field, request.hash, both bodies. durationMs, actor, and tags never flip equal.",
            schema(
                json!({
                    "a": string("Run ULID"),
                    "b": string("Run ULID"),
                    "path": path_optional.clone(),
                    "bodies": boolean("Unified diff for differing inline UTF-8 response bodies"),
                }),
                &["a", "b"],
            ),
        ),
        tool(
            "run_replay",
            "Re-send a recorded run from the current YAML at the recorded environment; the new run carries replayedFrom. frozen refuses when the request hash changed (exit 1 replay_changed).",
            schema(
                json!({
                    "id": string("Run ULID to replay"),
                    "path": string("Collection path (default: current directory)"),
                    "environment": string("Override the recorded environment"),
                    "var": var,
                    "tag": strings("Extra tags (the recorded ones carry over)"),
                    "expect": expect,
                    "frozen": boolean("Refuse when the resolved request differs from the recorded hash"),
                }),
                &["id"],
            ),
        ),
        tool(
            "ncl_check",
            "Parse, typecheck and evaluate a Nickel world module in-process. Returns { path, moduleHash, contractSet }. Nothing persisted.",
            schema(
                json!({
                    "path": string("Path to a .ncl module"),
                    "var": ncl_var.clone(),
                    "import_path": string("Materialized pack root (--import-path / FACET_NCL_IMPORT_PATH)"),
                }),
                &["path"],
            ),
        ),
        tool(
            "ncl_export",
            "eval_full_for_export on a Nickel world module. Returns { path, moduleHash, exportHash, contractSet, cluster, intent, calls }. Fields marked | not_exported are absent.",
            schema(
                json!({
                    "path": string("Path to a .ncl module"),
                    "var": ncl_var.clone(),
                    "import_path": string("Materialized pack root (--import-path / FACET_NCL_IMPORT_PATH)"),
                }),
                &["path"],
            ),
        ),
        tool(
            "ncl_apply",
            "Export a Nickel world module, POST frozen cluster JSON when FACET_KUBECONFIG is set, put intent rows through Hedron Store when FACET_HEDRON_DB is set, and record one ncl:apply Lattice row. Returns export fields plus action summary.",
            schema(
                json!({
                    "path": string("Path to a .ncl module"),
                    "var": ncl_var.clone(),
                    "import_path": string("Materialized pack root (--import-path / FACET_NCL_IMPORT_PATH)"),
                }),
                &["path"],
            ),
        ),
        tool(
            "sql_query",
            "Read-only SQL over the workspace store (runs, blobs). Writes fail with sql_read_only. The machine store is never attached.",
            schema(
                json!({
                    "sql": string("One SELECT statement"),
                    "path": path_optional,
                }),
                &["sql"],
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{MAX_FRAME_BYTES, TOOLS, handle, serve, tool_descriptions};

    #[test]
    fn lists_every_tool_with_a_schema() {
        let tools = tool_descriptions();
        assert_eq!(tools.len(), TOOLS.len());
        for (tool, name) in tools.iter().zip(TOOLS) {
            assert_eq!(tool["name"], *name);
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        }
    }

    #[test]
    fn initialize_ping_and_unknown_method() {
        let init = handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" } }))
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(init["result"]["serverInfo"]["name"], "facet");
        assert!(
            handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).is_none()
        );
        let pong = handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" })).unwrap();
        assert_eq!(pong["result"], json!({}));
        let missing = handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "nope" })).unwrap();
        assert_eq!(missing["error"]["code"], -32601);
        let unknown_tool = handle(&json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "gc_apply", "arguments": {} } }))
        .unwrap();
        assert_eq!(unknown_tool["error"]["code"], -32602);
    }

    #[test]
    fn serve_reports_parse_errors_and_keeps_going() {
        let ping = b"{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"ping\"}\n";
        for mut malformed in [
            b"not json\n".to_vec(),
            {
                let mut frame = vec![b'{'; MAX_FRAME_BYTES + 1];
                frame.push(b'\n');
                frame
            },
            vec![0xff, b'\n'],
        ] {
            malformed.extend_from_slice(ping);
            let mut output = Vec::new();
            serve(&malformed[..], &mut output).unwrap();
            let lines: Vec<serde_json::Value> = String::from_utf8(output)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[0]["error"]["code"], -32700);
            assert_eq!(lines[1]["id"], 9);
        }
    }

    #[test]
    fn tool_errors_are_facet_envelopes() {
        let response = handle(&json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call",
            "params": { "name": "history_get", "arguments": { "id": 42 } } }))
        .unwrap();
        let result = &response["result"];
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["schemaVersion"], 1);
        assert_eq!(
            result["structuredContent"]["error"]["category"],
            "invalid_arguments"
        );
        assert_eq!(result["structuredContent"]["error"]["exitCode"], 2);
    }
}

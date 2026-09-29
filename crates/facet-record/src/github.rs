use std::process::{Command, Stdio};
use std::time::Instant;

use probe_core::{Body, HttpRequest, RequestBody};
use probe_http::{HttpError, HttpResponse, ResponseHeader};

/// Supplies githubToken only for a request whose YAML explicitly references it.
/// The token comes from gh's credential store and never enters YAML or Lattice.
pub fn auth_token_for(request: &HttpRequest, overrides: &[(String, String)]) -> Option<String> {
    let references_token = request.headers.iter().any(|header| {
        header.name.eq_ignore_ascii_case("authorization")
            && header.value.contains("{{githubToken}}")
    });
    if !references_token
        || overrides
            .iter()
            .any(|(name, value)| name == "githubToken" && !value.is_empty())
    {
        return None;
    }
    let output = Command::new("gh")
        .args(["auth", "token"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

/// Runs a GitHub collection request through gh when native HTTP failed.
/// A successful exchange is returned as an ordinary HttpResponse, so the
/// existing recorder writes it exactly like a native response.
pub fn api_fallback(request: &HttpRequest) -> Option<Result<HttpResponse, HttpError>> {
    let url = request.url.as_deref()?;
    let endpoint = url.strip_prefix("https://api.github.com")?;
    let method = request.method.as_deref().unwrap_or("GET");
    let mut command = Command::new("gh");
    command
        .arg("api")
        .arg(if endpoint.is_empty() { "/" } else { endpoint })
        .arg("--method")
        .arg(method)
        .stdin(Stdio::piped())
        .stderr(Stdio::piped());
    for header in &request.headers {
        if !header.name.eq_ignore_ascii_case("authorization") {
            command
                .arg("--header")
                .arg(format!("{}: {}", header.name, header.value));
        }
    }
    let body = request_body(request);
    if body.is_some() {
        command.arg("--input").arg("-");
    }
    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return None,
    };
    if let Some(body) = body
        && let Some(mut stdin) = child.stdin.take()
    {
        let _ = std::io::Write::write_all(&mut stdin, &body);
    }
    let output = match child.wait_with_output() {
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

fn request_body(request: &HttpRequest) -> Option<Vec<u8>> {
    match request.body.as_ref()? {
        RequestBody::Single(Body::Raw(body)) => Some(body.data.as_bytes().to_vec()),
        _ => None,
    }
}

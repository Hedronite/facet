//! Response rendering. `response_human` and `response_json` keep
//! `probe-cli`'s presentation shape so `facet request run` keeps the
//! upstream contract; Facet appends its `lattice` field afterwards. Both
//! show the request and final URL through [`Redactor::url`] and each
//! response header through [`Redactor::header`]; the caller runs the
//! known-values pass over the whole document.

use std::path::Path;

use facet_record::Redactor;
use probe_core::HttpRequest;
use probe_http::{HttpResponse, MAX_IN_MEMORY_RESPONSE_BYTES};
use serde_json::{Value, json};

pub(crate) fn response_human(
    request: &HttpRequest,
    response: &HttpResponse,
    output: Option<&Path>,
    redactor: &Redactor,
) -> String {
    let method = request.method.as_deref().unwrap_or("<unset>");
    let url = request
        .url
        .as_deref()
        .map_or_else(|| "<unset>".to_owned(), |url| redactor.url(url));
    let mut rendered = format!(
        "{method} {url}\n\n{} {}\n{} ms\n{}\nFinal URL: {}\nHeaders:\n",
        response.status,
        response.reason,
        response.duration.as_millis(),
        human_size(response.size),
        redactor.url(response.url.as_str()),
    );
    if response.headers.is_empty() {
        rendered.push_str("  (none)\n");
    } else {
        for header in &response.headers {
            rendered.push_str(&format!(
                "  {}: {}\n",
                header.name,
                redactor.header(&header.name, &header.value)
            ));
        }
    }
    rendered.push('\n');
    if let Some(output) = output {
        rendered.push_str(&format!("Response body written to {}\n", output.display()));
    } else if !response.body_complete {
        rendered.push_str(&format!(
            "Response body omitted because it exceeds {} bytes; use --output <file>.\n",
            MAX_IN_MEMORY_RESPONSE_BYTES
        ));
    } else if let Ok(body) = std::str::from_utf8(&response.body) {
        rendered.push_str(body);
        if !body.ends_with('\n') {
            rendered.push('\n');
        }
    } else {
        rendered.push_str("Binary response body omitted; use --output <file>.\n");
    }
    rendered
}

pub(crate) fn response_json(
    request: &HttpRequest,
    response: &HttpResponse,
    output: Option<&Path>,
    redactor: &Redactor,
) -> Value {
    let output_path = output.map(|path| path.to_string_lossy().into_owned());
    let (content, encoding, omitted, omission_reason) = if output.is_some() {
        (None, None, false, None)
    } else if !response.body_complete {
        (None, None, true, Some("too_large"))
    } else if let Ok(body) = std::str::from_utf8(&response.body) {
        (Some(body), Some("utf8"), false, None)
    } else {
        (None, None, true, Some("binary"))
    };
    json!({
        "request": {
            "method": request.method,
            "url": request.url.as_deref().map(|url| redactor.url(url)),
        },
        "response": {
            "body": {
                "content": content,
                "encoding": encoding,
                "omissionReason": omission_reason,
                "omitted": omitted,
                "outputPath": output_path,
            },
            "durationMs": response.duration.as_millis(),
            "headers": response.headers.iter().map(|header| json!({
                "name": header.name,
                "value": redactor.header(&header.name, &header.value),
            })).collect::<Vec<_>>(),
            "reason": response.reason,
            "sizeBytes": response.size,
            "status": response.status,
            "url": redactor.url(response.url.as_str()),
        }
    })
}

/// The upstream `body` object shape for bytes Facet already holds.
pub(crate) fn stored_body_json(bytes: Option<&[u8]>, omission: Option<&str>) -> Value {
    let (content, encoding, omitted, omission_reason) = match (bytes, omission) {
        (_, Some(reason)) => (None, None, true, Some(reason.to_owned())),
        (None, None) => (None, None, true, Some("not_retained".to_owned())),
        (Some(bytes), None) if bytes.len() > MAX_IN_MEMORY_RESPONSE_BYTES => {
            (None, None, true, Some("too_large".to_owned()))
        }
        (Some(bytes), None) => match std::str::from_utf8(bytes) {
            Ok(text) => (Some(text.to_owned()), Some("utf8"), false, None),
            Err(_) => (None, None, true, Some("binary".to_owned())),
        },
    };
    json!({
        "content": content,
        "encoding": encoding,
        "omissionReason": omission_reason,
        "omitted": omitted,
    })
}

pub(crate) fn human_size(size: usize) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.0)
    } else {
        format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
    }
}

/// Formats Unix milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ` without a
/// calendar dependency (Howard Hinnant's civil-from-days).
pub(crate) fn format_utc(unix_ms: i64) -> String {
    let seconds = unix_ms.div_euclid(1000);
    let millis = unix_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        day_seconds / 3600,
        (day_seconds % 3600) / 60,
        day_seconds % 60,
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month: u32 = if mp < 10 {
        (mp + 3) as u32
    } else {
        (mp - 9) as u32
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::{format_utc, human_size, stored_body_json};

    #[test]
    fn formats_unix_milliseconds_as_utc() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_utc(1_757_160_000_123), "2025-09-06T12:00:00.123Z");
        assert_eq!(format_utc(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn sizes_match_upstream() {
        assert_eq!(human_size(12), "12 B");
        assert_eq!(human_size(2048), "2.0 KB");
    }

    #[test]
    fn stored_body_reports_omissions() {
        assert_eq!(stored_body_json(Some(b"hi"), None)["content"], "hi");
        assert_eq!(
            stored_body_json(Some(&[0xFF]), None)["omissionReason"],
            "binary"
        );
        assert_eq!(
            stored_body_json(None, None)["omissionReason"],
            "not_retained"
        );
        assert_eq!(
            stored_body_json(None, Some("blob"))["omissionReason"],
            "blob"
        );
    }
}

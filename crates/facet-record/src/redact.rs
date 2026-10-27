//! One redactor for every surface that shows a run: stdout, stderr, MCP and
//! the Lattice store. It is built from the secret values a run knows (the
//! hydrated secrets, `--var` secrets, and any token the run sends) and
//! replaces each with `<redacted>`.
//!
//! - Values shorter than [`MIN_REDACT_LEN`] bytes are ignored: they would
//!   mangle unrelated text.
//! - Each value is matched exactly and in its URL-encoded forms
//!   (percent-encoded with `%20` or `+` for a space, upper- or lower-case
//!   hex).
//! - Headers named in `SENSITIVE_HEADERS` are masked whatever their value.
//! - Known values are held in [`Zeroizing`] and wiped on drop. `Debug` shows
//!   only how many there are.
//!
//! There is no streaming API: callers pass a whole rendered document.

use std::{borrow::Cow, fmt};

use zeroize::Zeroizing;

use crate::{MIN_REDACT_LEN, REDACTED, SENSITIVE_HEADERS};

/// Replaces known secret values, and the values of sensitive headers, with
/// `<redacted>`.
#[derive(Clone, Default)]
pub struct Redactor {
    /// Each known value as given, for matching decoded query values.
    values: Vec<Zeroizing<String>>,
    /// Every value and its encoded forms, longest first, without repeats.
    patterns: Vec<Zeroizing<String>>,
}

impl fmt::Debug for Redactor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Redactor")
            .field("known", &self.values.len())
            .finish_non_exhaustive()
    }
}

impl Redactor {
    /// A redactor for `values`. Values shorter than [`MIN_REDACT_LEN`]
    /// bytes are dropped.
    #[must_use]
    pub fn new<I>(values: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<Zeroizing<String>>,
    {
        let mut redactor = Self::default();
        for value in values {
            redactor.add_known(value);
        }
        redactor
    }

    /// Adds `value` to the known values, with its URL-encoded forms. A value
    /// shorter than [`MIN_REDACT_LEN`] bytes, or one already known, is
    /// ignored.
    pub fn add_known(&mut self, value: impl Into<Zeroizing<String>>) {
        let value = value.into();
        if value.len() < MIN_REDACT_LEN || self.values.contains(&value) {
            return;
        }
        for pattern in encoded_forms(&value) {
            if !self.patterns.contains(&pattern) {
                self.patterns.push(pattern);
            }
        }
        self.patterns
            .sort_by_key(|pattern| std::cmp::Reverse(pattern.len()));
        self.values.push(value);
    }

    /// Whether no value is known. Sensitive headers are still masked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// `text` with every known value replaced. Borrowed when nothing matched.
    #[must_use]
    pub fn text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        for pattern in &self.patterns {
            if out.contains(pattern.as_str()) {
                out = Cow::Owned(out.replace(pattern.as_str(), REDACTED));
            }
        }
        out
    }

    /// `bytes` with every known value replaced. Borrowed when nothing
    /// matched.
    #[must_use]
    pub fn bytes<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        let mut out = Cow::Borrowed(bytes);
        for pattern in &self.patterns {
            if let Some(replaced) = replace_bytes(&out, pattern.as_bytes(), REDACTED.as_bytes()) {
                out = Cow::Owned(replaced);
            }
        }
        out
    }

    /// `url` with any userinfo replaced by `<redacted>`, any query value
    /// that decodes to a known value replaced, and then every known value
    /// replaced wherever it appears.
    #[must_use]
    pub fn url(&self, url: &str) -> String {
        let url = strip_userinfo(url);
        let (before_fragment, fragment) = match url.split_once('#') {
            Some((head, fragment)) => (head, Some(fragment)),
            None => (url.as_ref(), None),
        };
        let mut out = match before_fragment.split_once('?') {
            Some((base, query)) => {
                let query = query
                    .split('&')
                    .map(|pair| match pair.split_once('=') {
                        Some((name, value)) if self.decodes_to_known(value) => {
                            format!("{name}={REDACTED}")
                        }
                        _ => pair.to_owned(),
                    })
                    .collect::<Vec<_>>()
                    .join("&");
                format!("{base}?{query}")
            }
            None => before_fragment.to_owned(),
        };
        if let Some(fragment) = fragment {
            out.push('#');
            out.push_str(fragment);
        }
        self.text(&out).into_owned()
    }

    /// The value to show for the header `name`: `<redacted>` for a sensitive
    /// header, else `value` with known values replaced.
    #[must_use]
    pub fn header<'a>(&self, name: &str, value: &'a str) -> Cow<'a, str> {
        if is_sensitive_header(name) {
            Cow::Borrowed(REDACTED)
        } else {
            self.text(value)
        }
    }

    /// [`Self::header`] over a whole header list, names kept, order kept.
    #[must_use]
    pub fn headers<'a, I>(&self, headers: I) -> Vec<(&'a str, Cow<'a, str>)>
    where
        I: IntoIterator<Item = (&'a str, &'a str)>,
    {
        headers
            .into_iter()
            .map(|(name, value)| (name, self.header(name, value)))
            .collect()
    }

    fn decodes_to_known(&self, raw: &str) -> bool {
        let decoded: Zeroizing<String> = Zeroizing::new(
            url::form_urlencoded::parse(format!("v={raw}").as_bytes())
                .next()
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default(),
        );
        self.values.iter().any(|value| **value == *decoded)
    }
}

/// Whether the header `name` is never shown, whatever its value.
#[must_use]
pub fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS
        .iter()
        .any(|sensitive| name.eq_ignore_ascii_case(sensitive))
}

/// `value`, then its percent-encoded and form-encoded forms in upper- and
/// lower-case hex, without repeats.
fn encoded_forms(value: &str) -> Vec<Zeroizing<String>> {
    let mut forms = vec![Zeroizing::new(value.to_owned())];
    for space in ["%20", "+"] {
        let upper = Zeroizing::new(percent_encode(value, space));
        let lower = Zeroizing::new(lower_hex(&upper));
        for form in [upper, lower] {
            if !forms.contains(&form) {
                forms.push(form);
            }
        }
    }
    let serialized: Zeroizing<String> =
        Zeroizing::new(url::form_urlencoded::byte_serialize(value.as_bytes()).collect());
    if !forms.contains(&serialized) {
        forms.push(serialized);
    }
    forms
}

/// Percent-encodes every byte outside RFC 3986 unreserved, with `space` for
/// a space.
fn percent_encode(value: &str, space: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push_str(space),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// `encoded` with the two hex digits after each `%` lower-cased.
fn lower_hex(encoded: &str) -> String {
    let mut out = String::with_capacity(encoded.len());
    let mut hex_left = 0;
    for character in encoded.chars() {
        if hex_left > 0 {
            out.push(character.to_ascii_lowercase());
            hex_left -= 1;
        } else {
            if character == '%' {
                hex_left = 2;
            }
            out.push(character);
        }
    }
    out
}

/// `url` with `user[:password]@` in its authority replaced by `<redacted>@`.
fn strip_userinfo(url: &str) -> Cow<'_, str> {
    let Some(scheme_end) = url.find("://") else {
        return Cow::Borrowed(url);
    };
    let authority_start = scheme_end + 3;
    let authority_end = url[authority_start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |offset| authority_start + offset);
    let authority = &url[authority_start..authority_end];
    match authority.rfind('@') {
        Some(at) => Cow::Owned(format!(
            "{}{REDACTED}@{}{}",
            &url[..authority_start],
            &authority[at + 1..],
            &url[authority_end..]
        )),
        None => Cow::Borrowed(url),
    }
}

/// `haystack` with every `needle` replaced by `with`, or `None` when there
/// is none.
fn replace_bytes(haystack: &[u8], needle: &[u8], with: &[u8]) -> Option<Vec<u8>> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut index = 0;
    let mut found = false;
    while index + needle.len() <= haystack.len() {
        if &haystack[index..index + needle.len()] == needle {
            out.extend_from_slice(&haystack[start..index]);
            out.extend_from_slice(with);
            index += needle.len();
            start = index;
            found = true;
        } else {
            index += 1;
        }
    }
    found.then(|| {
        out.extend_from_slice(&haystack[start..]);
        out
    })
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use zeroize::Zeroizing;

    use super::{Redactor, is_sensitive_header};

    const SECRET: &str = "redact-canary value/1";

    fn redactor() -> Redactor {
        Redactor::new([SECRET.to_owned(), "ab".to_owned()])
    }

    #[test]
    fn text_replaces_exact_and_url_encoded_values() {
        let redactor = redactor();
        for text in [
            "x redact-canary value/1 y",
            "x redact-canary%20value%2F1 y",
            "x redact-canary+value%2F1 y",
            "x redact-canary%20value%2f1 y",
            "x redact-canary+value%2f1 y",
        ] {
            assert_eq!(redactor.text(text), "x <redacted> y", "a form leaked");
        }
    }

    #[test]
    fn text_borrows_when_nothing_matches_and_ignores_short_values() {
        let redactor = redactor();
        assert!(matches!(redactor.text("plain ab text"), Cow::Borrowed(_)));
        assert!(matches!(redactor.bytes(b"plain ab"), Cow::Borrowed(_)));
        assert!(Redactor::new(["abc".to_owned()]).is_empty());
    }

    #[test]
    fn bytes_replaces_known_values_in_binary() {
        let redactor = redactor();
        let mut input = vec![0_u8, 159];
        input.extend_from_slice(SECRET.as_bytes());
        input.push(0);
        let mut expected = vec![0_u8, 159];
        expected.extend_from_slice(b"<redacted>");
        expected.push(0);
        assert_eq!(redactor.bytes(&input).as_ref(), expected.as_slice());
    }

    #[test]
    fn longer_values_are_replaced_before_their_prefixes() {
        let redactor = Redactor::new(["canary".to_owned(), "canary-longer".to_owned()]);
        assert_eq!(redactor.text("canary-longer"), "<redacted>");
    }

    #[test]
    fn url_strips_userinfo_and_known_query_values() {
        let redactor = redactor();
        assert_eq!(
            redactor.url("https://user:pw@api.example.com/v1?x=1"),
            "https://<redacted>@api.example.com/v1?x=1"
        );
        assert_eq!(
            redactor.url("http://h/p?t=redact-canary+value%2F1&keep=me#frag"),
            "http://h/p?t=<redacted>&keep=me#frag"
        );
        assert_eq!(
            redactor.url("http://h/redact-canary%20value%2F1?q=1"),
            "http://h/<redacted>?q=1"
        );
        assert_eq!(redactor.url("not a url"), "not a url");
    }

    #[test]
    fn headers_mask_sensitive_names_and_scrub_the_rest() {
        let redactor = redactor();
        let masked = redactor.headers([
            ("Set-Cookie", "sid=anything"),
            ("authorization", "Bearer anything"),
            ("X-Echo", "redact-canary value/1"),
            ("Accept", "application/json"),
        ]);
        let values: Vec<&str> = masked.iter().map(|(_, value)| value.as_ref()).collect();
        assert_eq!(
            values,
            ["<redacted>", "<redacted>", "<redacted>", "application/json"]
        );
        assert!(is_sensitive_header("PROXY-AUTHORIZATION"));
        assert!(!is_sensitive_header("x-request-id"));
    }

    #[test]
    fn add_known_takes_zeroizing_values_and_debug_hides_them() {
        let mut redactor = Redactor::default();
        redactor.add_known(Zeroizing::new(SECRET.to_owned()));
        redactor.add_known(SECRET.to_owned());
        assert!(!redactor.is_empty());
        assert_eq!(redactor.text(SECRET), "<redacted>");
        let debug = format!("{redactor:?}");
        assert!(!debug.contains("canary"), "Debug shows a value");
        assert!(debug.contains("known: 1"));
    }
}

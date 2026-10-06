//! The "View Source" model: the raw message as display text, with a class
//! per line so frontends can color it without parsing mail (§3).

use std::sync::LazyLock;

use regex::Regex;

use crate::types::SourceLine;

static BOUNDARY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bboundary\s*=\s*(?:"([^"]+)"|([^\s;"]+))"#).expect("valid regex"));

/// Base64 data: one long token per line.
fn is_encoded(line: &str) -> bool {
    line.len() >= 40 && line.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Split `raw` into display lines and classify each. The text has the same
/// line count as the raw message (`\n`-separated, `\r` dropped); bytes that
/// aren't UTF-8 show as U+FFFD. Saving must use the raw bytes, not this.
pub fn classify(raw: &[u8]) -> (String, Vec<SourceLine>) {
    let mut text = String::with_capacity(raw.len());
    let mut kinds = Vec::new();
    let mut boundaries: Vec<String> = Vec::new();
    let mut in_headers = true;
    // The header field being read, to find boundary= in folded lines.
    let mut field = String::new();

    /// Done with a header field: remember multipart boundaries.
    fn flush_field(field: &mut String, boundaries: &mut Vec<String>) {
        if field.get(..13).is_some_and(|p| p.eq_ignore_ascii_case("content-type:")) {
            for c in BOUNDARY.captures_iter(field) {
                // Bounded: every line is checked against every boundary.
                if let Some(b) = c.get(1).or(c.get(2))
                    && boundaries.len() < 64
                    && !boundaries.iter().any(|x| x == b.as_str())
                {
                    boundaries.push(b.as_str().to_owned());
                }
            }
        }
        field.clear();
    }

    for (i, raw_line) in raw.split(|&b| b == b'\n').enumerate() {
        let raw_line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let line = String::from_utf8_lossy(raw_line);
        // A paragraph separator would start a new line in Qt's text
        // document and shift every class after it.
        let line = line.replace(['\u{2028}', '\u{2029}'], "\u{fffd}");
        if i > 0 {
            text.push('\n');
        }
        text.push_str(&line);

        let boundary = line.strip_prefix("--").and_then(|rest| {
            let rest = rest.trim_end();
            boundaries.iter().find(|b| rest == b.as_str() || rest.strip_suffix("--") == Some(b.as_str()))
        });
        let kind = if let Some(b) = boundary {
            // A closing boundary (`--b--`) is followed by an epilogue, an
            // opening one by the part's headers.
            in_headers = line.trim_end() == format!("--{b}");
            flush_field(&mut field, &mut boundaries);
            SourceLine::Boundary
        } else if in_headers {
            if line.is_empty() {
                flush_field(&mut field, &mut boundaries);
                in_headers = false;
                SourceLine::Body
            } else if line.starts_with([' ', '\t']) {
                field.push_str(&line);
                SourceLine::HeaderContinuation
            } else if line.contains(':') {
                flush_field(&mut field, &mut boundaries);
                field.push_str(&line);
                SourceLine::HeaderField
            } else {
                SourceLine::Body
            }
        } else if line.starts_with("-----BEGIN PGP") || line.starts_with("-----END PGP") {
            SourceLine::Armor
        } else if line.starts_with('>') {
            SourceLine::Quote
        } else if is_encoded(&line) {
            SourceLine::Encoded
        } else {
            SourceLine::Body
        };
        kinds.push(kind);
    }
    (text, kinds)
}

/// A file name for saving the message: its subject, made safe, plus `.eml`.
pub fn file_name(subject: &str) -> String {
    let safe: String =
        subject
            .chars()
            .map(|c| {
                if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                    '_'
                } else {
                    c
                }
            })
            .take(100)
            .collect();
    let safe = safe.trim().trim_start_matches('.').trim();
    if safe.is_empty() { "message.eml".to_owned() } else { format!("{safe}.eml") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use SourceLine::*;

    #[test]
    fn classifies_lines() {
        let raw = b"From: a@b.c\r\n\
            Content-Type: multipart/mixed;\r\n\
            \tboundary=\"xyz\"\r\n\
            \r\n\
            preamble\r\n\
            --xyz\r\n\
            Content-Type: text/plain\r\n\
            \r\n\
            > quoted\r\n\
            hello: not a header\r\n\
            --xyz\r\n\
            Content-Transfer-Encoding: base64\r\n\
            \r\n\
            SGVsbG8gd29ybGQgdGhpcyBpcyBhIHRlc3Qgb2YgYmFzZTY0IGxpbmVz\r\n\
            --xyz--\r\n\
            epilogue\r\n";
        let (text, kinds) = classify(raw);
        assert_eq!(text.lines().count() + 1, kinds.len(), "{text:?}");
        assert!(!text.contains('\r'));
        assert_eq!(
            kinds,
            [
                HeaderField,
                HeaderField,
                HeaderContinuation,
                Body,
                Body,
                Boundary,
                HeaderField,
                Body,
                Quote,
                Body,
                Boundary,
                HeaderField,
                Body,
                Encoded,
                Boundary,
                Body,
                Body,
            ]
        );
    }

    #[test]
    fn non_utf8_and_separators_keep_line_count() {
        let raw = b"Subject: x\n\nab\xff\xfe\ncd\xe2\x80\xa9ef\nlast";
        let (text, kinds) = classify(raw);
        assert_eq!(text, "Subject: x\n\nab\u{fffd}\u{fffd}\ncd\u{fffd}ef\nlast");
        assert_eq!(kinds.len(), 5);
    }

    #[test]
    fn file_names() {
        assert_eq!(file_name("Re: Invoice 3/2026?"), "Re_ Invoice 3_2026_.eml");
        assert_eq!(file_name("  ..hidden "), "hidden.eml");
        assert_eq!(file_name(""), "message.eml");
        assert_eq!(file_name("tab\there"), "tab_here.eml");
    }
}

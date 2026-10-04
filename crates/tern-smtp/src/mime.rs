//! A small MIME writer for outgoing mail.
//!
//! Hand-written rather than a builder crate because PGP/MIME (RFC 3156) needs
//! the exact bytes of the signed part, and full control over its encoding:
//! every part we generate is 7-bit clean (quoted-printable or base64) with
//! CRLF line endings, so signatures survive transport unchanged.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use std::hash::{BuildHasher, Hasher};

use tern_core::model::Address;

pub const CRLF: &str = "\r\n";

/// A random token for boundaries and Message-IDs.
pub fn random_token() -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    h.write_u32(std::process::id());
    h.write_u64(COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let a = h.finish();
    h.write_u64(a);
    format!("{:016x}{:016x}", a, h.finish())
}

/// RFC 2047 encoded-word(s) for header text when needed; ASCII passes as is.
pub fn encode_header_text(s: &str) -> String {
    if s.is_ascii() && !s.contains("=?") && !s.contains(['\r', '\n']) {
        return s.to_owned();
    }
    // Split on char boundaries so each encoded word stays under 75 chars
    // (45 bytes → 60 base64 chars + 12 overhead).
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in s.chars().filter(|c| *c != '\r' && *c != '\n') {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(format!("=?UTF-8?B?{}?=", B64.encode(&chunk)));
            chunk.clear();
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(format!("=?UTF-8?B?{}?=", B64.encode(&chunk)));
    }
    words.join(&format!("{CRLF} "))
}

/// `Name <addr>` with the name quoted or encoded as needed.
pub fn format_address(a: &Address) -> String {
    match a.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        None => a.email.clone(),
        Some(n) if !n.is_ascii() => format!("{} <{}>", encode_header_text(n), a.email),
        Some(n) if n.chars().all(|c| c.is_ascii_alphanumeric() || " !#$%&'*+-/=?^_`{|}~".contains(c)) => {
            format!("{n} <{}>", a.email)
        }
        Some(n) => format!("\"{}\" <{}>", n.replace('\\', "\\\\").replace('"', "\\\""), a.email),
    }
}

pub fn format_address_list(list: &[Address]) -> String {
    list.iter().map(format_address).collect::<Vec<_>>().join(&format!(",{CRLF} "))
}

/// Text → format=flowed (RFC 3676): long lines are soft-wrapped at spaces
/// with a trailing space, trailing spaces of hard lines are removed, and lines
/// starting with space, `>` (unquoted) or `From ` are space-stuffed.
pub fn flow(text: &str) -> String {
    const WIDTH: usize = 72;
    let mut out = Vec::new();
    for line in text.replace("\r\n", "\n").split('\n') {
        // The signature separator is the one hard line ending in a space.
        if line == "-- " {
            out.push(line.to_owned());
            continue;
        }
        let line = line.trim_end_matches(' ');
        // Quote prefix stays on every wrapped piece.
        let quote_len = line.chars().take_while(|&c| c == '>').count();
        let (quote, body) = line.split_at(quote_len);
        let body = if quote_len > 0 { body.strip_prefix(' ').unwrap_or(body) } else { body };
        if quote_len > 0 && body.is_empty() {
            // A blank quoted line: no trailing space, or it would become a
            // soft line break and join the quoted paragraphs.
            out.push(quote.to_owned());
            continue;
        }
        let prefix = if quote_len > 0 { format!("{quote} ") } else { String::new() };
        let mut rest = body;
        loop {
            if rest.chars().count() + prefix.len() <= WIDTH {
                out.push(stuff(&format!("{prefix}{rest}"), quote_len));
                break;
            }
            // Break after the last space within the width, else the first.
            let limit =
                rest.char_indices().nth(WIDTH.saturating_sub(prefix.len())).map(|(i, _)| i).unwrap_or(rest.len());
            let cut = rest[..limit].rfind(' ').or_else(|| rest[limit..].find(' ').map(|i| i + limit));
            match cut {
                Some(i) => {
                    out.push(stuff(&format!("{prefix}{} ", &rest[..i]), quote_len));
                    rest = &rest[i + 1..];
                }
                None => {
                    out.push(stuff(&format!("{prefix}{rest}"), quote_len));
                    break;
                }
            }
        }
    }
    out.join(CRLF)
}

fn stuff(line: &str, quote_len: usize) -> String {
    if quote_len == 0 && (line.starts_with(' ') || line.starts_with('>') || line.starts_with("From ")) {
        format!(" {line}")
    } else {
        line.to_owned()
    }
}

/// Quoted-printable (RFC 2045) over CRLF-separated text lines. Trailing
/// spaces (format=flowed soft breaks) are encoded so no transport strips them.
pub fn quoted_printable(text: &str) -> String {
    let mut out = String::new();
    for (n, line) in text.split(CRLF).enumerate() {
        if n > 0 {
            out.push_str(CRLF);
        }
        let mut col = 0;
        let bytes = line.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            let enc = match b {
                b'=' => format!("={b:02X}"),
                b' ' | b'\t' if last => format!("={b:02X}"),
                // Keep "From " and leading dots safe for broken transports.
                b'.' if col == 0 => "=2E".to_owned(),
                b'F' if col == 0 && line.starts_with("From ") => "=46".to_owned(),
                33..=126 | b' ' | b'\t' => (b as char).to_string(),
                _ => format!("={b:02X}"),
            };
            if col + enc.len() > 75 {
                out.push('=');
                out.push_str(CRLF);
                col = 0;
            }
            col += enc.len();
            out.push_str(&enc);
        }
    }
    out
}

fn base64_lines(data: &[u8]) -> String {
    let enc = B64.encode(data);
    enc.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).expect("base64 is ascii")).collect::<Vec<_>>().join(CRLF)
}

/// RFC 2231 parameter value (`name*=UTF-8''...`) when not plain ASCII.
fn param(name: &str, value: &str) -> String {
    let plain = value.chars().all(|c| c.is_ascii_graphic() || c == ' ') && !value.contains(['"', '\\']);
    if plain {
        format!("{name}=\"{value}\"")
    } else {
        let enc: String = value
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                _ => format!("%{b:02X}"),
            })
            .collect();
        format!("{name}*=UTF-8''{enc}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// A MIME entity: header lines and body, rendered with CRLF.
fn entity(headers: &[String], body: &str) -> Vec<u8> {
    let mut s = headers.join(CRLF);
    s.push_str(CRLF);
    s.push_str(CRLF);
    s.push_str(body);
    s.into_bytes()
}

fn text_entity(text: &str) -> Vec<u8> {
    entity(
        &[
            "Content-Type: text/plain; charset=utf-8; format=flowed".into(),
            "Content-Transfer-Encoding: quoted-printable".into(),
        ],
        &quoted_printable(&flow(text)),
    )
}

fn multipart(subtype_and_params: &str, parts: &[Vec<u8>]) -> Vec<u8> {
    let boundary = format!("=-{}", random_token());
    let mut out =
        format!("Content-Type: multipart/{subtype_and_params};{CRLF} boundary=\"{boundary}\"{CRLF}{CRLF}").into_bytes();
    for p in parts {
        out.extend_from_slice(format!("--{boundary}{CRLF}").as_bytes());
        out.extend_from_slice(p);
        out.extend_from_slice(CRLF.as_bytes());
    }
    out.extend_from_slice(format!("--{boundary}--{CRLF}").as_bytes());
    out
}

/// The content entity of a message: the text alone, or multipart/mixed with
/// attachments.
pub fn body_entity(text: &str, attachments: &[Attachment]) -> Vec<u8> {
    with_attachments(text_entity(text), attachments)
}

/// Like [`body_entity`], with an HTML version next to the text
/// (multipart/alternative, text first as RFC 2046 requires).
pub fn alternative_body_entity(text: &str, html: &str, attachments: &[Attachment]) -> Vec<u8> {
    let html = entity(
        &["Content-Type: text/html; charset=utf-8".into(), "Content-Transfer-Encoding: quoted-printable".into()],
        &quoted_printable(&html.replace("\r\n", "\n").replace('\n', CRLF)),
    );
    with_attachments(multipart("alternative", &[text_entity(text), html]), attachments)
}

fn with_attachments(content: Vec<u8>, attachments: &[Attachment]) -> Vec<u8> {
    if attachments.is_empty() {
        return content;
    }
    let mut parts = vec![content];
    for a in attachments {
        parts.push(entity(
            &[
                format!("Content-Type: {}; {}", a.content_type, param("name", &a.filename)),
                format!("Content-Disposition: attachment; {}", param("filename", &a.filename)),
                "Content-Transfer-Encoding: base64".into(),
            ],
            &base64_lines(&a.data),
        ));
    }
    multipart("mixed", &parts)
}

/// RFC 3156 §5: multipart/signed around `signed` (exact bytes) with an
/// armored detached signature.
pub fn signed_entity(signed: &[u8], signature: &[u8]) -> Vec<u8> {
    let sig = entity(
        &[
            "Content-Type: application/pgp-signature; name=\"signature.asc\"".into(),
            "Content-Description: OpenPGP digital signature".into(),
            "Content-Disposition: attachment; filename=\"signature.asc\"".into(),
        ],
        &String::from_utf8_lossy(signature).replace("\r\n", "\n").replace('\n', CRLF),
    );
    multipart("signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"", &[signed.to_vec(), sig])
}

/// RFC 3156 §4: multipart/encrypted with an armored OpenPGP message.
pub fn encrypted_entity(armored: &[u8]) -> Vec<u8> {
    let control = entity(
        &[
            "Content-Type: application/pgp-encrypted".into(),
            "Content-Description: PGP/MIME version identification".into(),
        ],
        "Version: 1",
    );
    let data = entity(
        &[
            "Content-Type: application/octet-stream; name=\"encrypted.asc\"".into(),
            "Content-Description: OpenPGP encrypted message".into(),
            "Content-Disposition: inline; filename=\"encrypted.asc\"".into(),
        ],
        &String::from_utf8_lossy(armored).replace("\r\n", "\n").replace('\n', CRLF),
    );
    multipart("encrypted; protocol=\"application/pgp-encrypted\"", &[control, data])
}

/// Message-level header fields.
#[derive(Debug, Clone, Default)]
pub struct Headers {
    pub from: Address,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    /// Without angle brackets.
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// RFC 2822 date string.
    pub date: String,
}

/// A complete RFC 5322 message: top-level headers followed by the content
/// entity (whose own Content-* headers become the message's). Bcc is never
/// written.
pub fn message(h: &Headers, content: &[u8]) -> Vec<u8> {
    let mut lines = vec![format!("Date: {}", h.date), format!("From: {}", format_address(&h.from))];
    if !h.to.is_empty() {
        lines.push(format!("To: {}", format_address_list(&h.to)));
    }
    if !h.cc.is_empty() {
        lines.push(format!("Cc: {}", format_address_list(&h.cc)));
    }
    lines.push(format!("Subject: {}", encode_header_text(&h.subject)));
    lines.push(format!("Message-ID: <{}>", h.message_id));
    if let Some(irt) = &h.in_reply_to {
        lines.push(format!("In-Reply-To: <{irt}>"));
    }
    if !h.references.is_empty() {
        let refs: Vec<String> = h.references.iter().map(|r| format!("<{r}>")).collect();
        lines.push(format!("References: {}", refs.join(&format!("{CRLF} "))));
    }
    lines.push("MIME-Version: 1.0".into());
    lines.push(format!("User-Agent: Tern/{}", env!("CARGO_PKG_VERSION")));
    let mut out = lines.join(CRLF).into_bytes();
    out.extend_from_slice(CRLF.as_bytes());
    out.extend_from_slice(content);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_encoding() {
        assert_eq!(encode_header_text("Hello"), "Hello");
        let e = encode_header_text("Grüße aus Köln");
        assert!(e.starts_with("=?UTF-8?B?"));
        let a = Address { name: Some("Doe, John".into()), email: "j@x.org".into() };
        assert_eq!(format_address(&a), "\"Doe, John\" <j@x.org>");
        let a = Address { name: Some("Sébastien".into()), email: "s@x.org".into() };
        assert!(format_address(&a).starts_with("=?UTF-8?B?"));
    }

    #[test]
    fn flowed() {
        let long = "word ".repeat(30);
        let f = flow(&format!("{long}\nFrom here\n> quoted\n>\n> more\n-- \nsig"));
        let lines: Vec<&str> = f.split(CRLF).collect();
        assert!(lines.iter().all(|l| l.len() <= 78), "{lines:?}");
        assert!(lines[0].ends_with(' '));
        assert!(lines.contains(&" From here"));
        assert!(lines.contains(&"> quoted"));
        assert!(lines.contains(&">"));
        assert!(lines.contains(&"-- "));
    }

    #[test]
    fn qp() {
        assert_eq!(quoted_printable("a=b"), "a=3Db");
        assert_eq!(quoted_printable("soft "), "soft=20");
        assert_eq!(quoted_printable("é"), "=C3=A9");
        let long = quoted_printable(&"x".repeat(200));
        assert!(long.split(CRLF).all(|l| l.len() <= 76));
    }

    #[test]
    fn alternative_parses() {
        let h = Headers {
            from: Address { name: None, email: "a@example.org".into() },
            to: vec![Address { name: None, email: "b@example.org".into() }],
            subject: "s".into(),
            message_id: "m@example.org".into(),
            date: "Sat, 4 Oct 2026 10:00:00 +0200".into(),
            ..Default::default()
        };
        let att = Attachment { filename: "a.txt".into(), content_type: "text/plain".into(), data: b"x".to_vec() };
        let html = format!("<p>Grüße <b>{}</b></p>", "long ".repeat(40));
        let raw = message(&h, &alternative_body_entity("**Grüße**", &html, &[att]));
        assert!(raw.is_ascii());
        let m = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert!(m.body_text(0).unwrap().contains("**Grüße**"));
        assert!(m.body_html(0).unwrap().contains("<b>long"));
        assert_eq!(m.attachment_count(), 1);
    }

    #[test]
    fn full_message_parses() {
        let h = Headers {
            from: Address { name: Some("Ann".into()), email: "ann@example.org".into() },
            to: vec![Address { name: None, email: "bob@example.org".into() }],
            subject: "Grüße".into(),
            message_id: "abc@example.org".into(),
            in_reply_to: Some("p@x".into()),
            references: vec!["r@x".into(), "p@x".into()],
            date: "Sat, 4 Oct 2026 10:00:00 +0200".into(),
            ..Default::default()
        };
        let att =
            Attachment {
                filename: "räksmörgås.txt".into(), content_type: "text/plain".into(), data: b"data".to_vec()
            };
        let raw = message(&h, &body_entity("Hej då = bye\nline two", &[att]));
        assert!(raw.is_ascii());
        let m = mail_parser::MessageParser::default().parse(&raw).unwrap();
        assert_eq!(m.subject(), Some("Grüße"));
        assert!(m.body_text(0).unwrap().contains("Hej då = bye"));
        let a = m.attachment(0).unwrap();
        use mail_parser::MimeHeaders;
        assert_eq!(a.attachment_name(), Some("räksmörgås.txt"));
        assert_eq!(a.contents(), b"data");
        assert_eq!(m.in_reply_to().as_text(), Some("p@x"));
    }
}

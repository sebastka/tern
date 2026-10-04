//! The shared rendering policy (ARCHITECTURE.md §4, §10): what a frontend
//! shows for a message. Frontends display the documents produced here through
//! the `tern-msg:` scheme and only *enforce* the policy (no JS, remote content
//! blocked unless allowed, links opened externally).
//!
//! Decrypted content only lives in memory (§11).

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use mail_parser::{Message, MessageParser, MimeHeaders, PartType};
use regex::Regex;
use tern_pgp::{Gpg, Signature, Trust};

use crate::types::{AttachmentInfo, SignatureState};

/// Everything produced from one message, cached by the app for the scheme
/// handler, attachment saving and reply quoting.
#[derive(Debug, Clone, Default)]
pub struct Rendered {
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub date: i64,
    pub from_address: String,
    /// Sanitized HTML document, if the message has an HTML part.
    pub html_doc: Option<String>,
    /// Plain text rendered as an HTML document.
    pub text_doc: String,
    pub text: String,
    pub has_remote_content: bool,
    /// Remote content was allowed when rendering (it's in the CSP).
    pub remote_allowed: bool,
    pub attachments: Vec<(AttachmentInfo, Vec<u8>)>,
    /// Content-ID (without brackets) → (content type, bytes).
    pub cids: HashMap<String, (String, Vec<u8>)>,
    pub encrypted: bool,
    pub decryption_failed: bool,
    pub signature: SignatureState,
    pub signature_text: String,
    // Reply support.
    pub message_id: Option<String>,
    pub references: Vec<String>,
    pub reply_to: String,
    pub to_list: Vec<String>,
    pub cc_list: Vec<String>,
}

pub struct RenderOptions<'a> {
    pub gpg: &'a Gpg,
    pub allow_remote: bool,
    /// `tern-msg:/<account>/<id>` — prefix for cid: rewriting.
    pub url_base: String,
}

fn ct_is(part: &mail_parser::MessagePart<'_>, ty: &str, sub: &str) -> bool {
    part.content_type()
        .is_some_and(|c| c.ctype().eq_ignore_ascii_case(ty) && c.subtype().is_some_and(|s| s.eq_ignore_ascii_case(sub)))
}

fn ct_param<'a>(part: &'a mail_parser::MessagePart<'_>, name: &str) -> Option<&'a str> {
    part.content_type()?.attribute(name)
}

/// Split the raw body of a multipart entity into its raw parts (RFC 2046):
/// each part excludes the CRLF that precedes the next delimiter.
pub fn split_multipart(body: &[u8], boundary: &str) -> Vec<Vec<u8>> {
    let delim = format!("--{boundary}");
    let d = delim.as_bytes();
    let mut starts = Vec::new(); // (line start, line end incl. CRLF)
    let mut pos = 0;
    while pos < body.len() {
        let line_end = body[pos..].iter().position(|&b| b == b'\n').map(|i| pos + i + 1).unwrap_or(body.len());
        let line = &body[pos..line_end];
        if line.starts_with(d) {
            let rest = &line[d.len()..];
            let rest = rest.strip_suffix(b"\n").unwrap_or(rest);
            let rest = rest.strip_suffix(b"\r").unwrap_or(rest);
            let closing = rest.starts_with(b"--");
            if rest.iter().all(|b| *b == b' ' || *b == b'\t') || closing {
                starts.push((pos, line_end, closing));
                if closing {
                    break;
                }
            }
        }
        pos = line_end;
    }
    let mut parts = Vec::new();
    for w in starts.windows(2) {
        let (_, content_start, closing) = w[0];
        if closing {
            break;
        }
        let mut end = w[1].0;
        // Strip the line break belonging to the next delimiter.
        if end > content_start && body[end - 1] == b'\n' {
            end -= 1;
            if end > content_start && body[end - 1] == b'\r' {
                end -= 1;
            }
        }
        parts.push(body[content_start..end.max(content_start)].to_vec());
    }
    parts
}

/// Bare LF → CRLF (signatures are over canonical text).
fn canonical_crlf(data: &[u8]) -> Cow<'_, [u8]> {
    if !data.iter().enumerate().any(|(i, &b)| b == b'\n' && (i == 0 || data[i - 1] != b'\r')) {
        return Cow::Borrowed(data);
    }
    let mut out = Vec::with_capacity(data.len() + data.len() / 40);
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    Cow::Owned(out)
}

fn signature_state(sig: &Signature) -> (SignatureState, String) {
    match sig {
        Signature::Good { user_id, trust, fingerprint } => match trust {
            Trust::Full | Trust::Ultimate => (SignatureState::Good, format!("Good signature from {user_id}")),
            _ => (
                SignatureState::Warning,
                format!("Good signature from {user_id}, but the key {fingerprint} is not certified as trusted"),
            ),
        },
        Signature::Problem { user_id, reason, .. } => {
            (SignatureState::Warning, format!("Signature from {user_id}: {reason}"))
        }
        Signature::Bad { user_id, .. } => (SignatureState::Bad, format!("BAD signature claiming to be from {user_id}")),
        Signature::UnknownKey { key_id } => (SignatureState::Unknown, format!("Signed with unknown key {key_id}")),
        Signature::Error(e) => (SignatureState::Unknown, format!("Cannot check signature: {e}")),
    }
}

fn addr_list(a: Option<&mail_parser::Address<'_>>) -> (String, Vec<String>) {
    let Some(a) = a else { return (String::new(), Vec::new()) };
    let items: Vec<(String, String)> = a
        .iter()
        .filter_map(|x| {
            let email = x.address()?.to_owned();
            let name = x.name().map(str::to_owned).unwrap_or_default();
            Some((name, email))
        })
        .collect();
    let display = items
        .iter()
        .map(|(n, e)| if n.is_empty() { e.clone() } else { format!("{n} <{e}>") })
        .collect::<Vec<_>>()
        .join(", ");
    let full = items
        .iter()
        .map(|(n, e)| if n.is_empty() { e.clone() } else { format!("\"{}\" <{e}>", n.replace('"', "")) })
        .collect();
    (display, full)
}

/// Render a raw message. Never fails: problems are shown in the document.
pub async fn render(raw: &[u8], opts: &RenderOptions<'_>) -> Rendered {
    let parser = MessageParser::default();
    let Some(outer) = parser.parse(raw) else {
        let text = String::from_utf8_lossy(raw).into_owned();
        return Rendered { text_doc: text_document(&text), text, ..Default::default() };
    };

    let mut r = Rendered {
        subject: outer.subject().unwrap_or("").to_owned(),
        date: outer.date().map(|d| d.to_timestamp()).unwrap_or(0),
        remote_allowed: opts.allow_remote,
        message_id: outer.message_id().map(str::to_owned),
        ..Default::default()
    };
    (r.from, _) = addr_list(outer.from());
    r.from_address = outer.from().and_then(|a| a.first()).and_then(|a| a.address()).unwrap_or("").to_owned();
    (r.to, r.to_list) = addr_list(outer.to());
    (r.cc, r.cc_list) = addr_list(outer.cc());
    // Quoted form: display names may contain commas.
    r.reply_to = addr_list(outer.reply_to().or(outer.from())).1.join(", ");
    r.references = match outer.references() {
        mail_parser::HeaderValue::Text(t) => vec![t.to_string()],
        mail_parser::HeaderValue::TextList(l) => l.iter().map(|s| s.to_string()).collect(),
        _ => Vec::new(),
    };

    // Unwrap PGP/MIME layers (encrypted, signed, signed-inside-encrypted).
    let mut content: Vec<u8> = raw.to_vec();
    for _ in 0..4 {
        let Some(msg) = parser.parse(&content) else { break };
        let root = &msg.parts[0];
        let children = match &root.body {
            PartType::Multipart(c) => c.clone(),
            _ => break,
        };
        if ct_is(root, "multipart", "encrypted") {
            r.encrypted = true;
            let data = children.get(1).and_then(|&i| msg.parts.get(i as usize)).map(|p| p.contents().to_vec());
            match data {
                Some(ct) => match opts.gpg.decrypt(&ct).await {
                    Ok(d) => {
                        if let Some(sig) = &d.signature {
                            (r.signature, r.signature_text) = signature_state(sig);
                        }
                        content = d.plaintext;
                        continue;
                    }
                    Err(e) => {
                        r.decryption_failed = true;
                        r.text = format!("This message is encrypted and could not be decrypted.\n\n{e}");
                        r.text_doc = text_document(&r.text);
                        return r;
                    }
                },
                None => break,
            }
        } else if ct_is(root, "multipart", "signed")
            && ct_param(root, "protocol").is_some_and(|p| p.eq_ignore_ascii_case("application/pgp-signature"))
        {
            let Some(boundary) = ct_param(root, "boundary") else { break };
            let body = &content[root.offset_body as usize..(root.offset_end as usize).min(content.len())];
            let raw_parts = split_multipart(body, boundary);
            let sig = children.get(1).and_then(|&i| msg.parts.get(i as usize)).map(|p| p.contents().to_vec());
            match (raw_parts.first(), sig) {
                (Some(signed), Some(sig)) => {
                    let signed = canonical_crlf(signed).into_owned();
                    match opts.gpg.verify_detached(&signed, &sig).await {
                        Ok(s) => (r.signature, r.signature_text) = signature_state(&s),
                        Err(e) => (r.signature, r.signature_text) = (SignatureState::Unknown, e.to_string()),
                    }
                    content = signed;
                    continue;
                }
                _ => break,
            }
        }
        break;
    }

    let Some(body) = parser.parse(&content) else {
        r.text = String::from_utf8_lossy(&content).into_owned();
        r.text_doc = text_document(&r.text);
        return r;
    };

    let mut text = body_text(&body);
    // Legacy inline PGP (read only). The PGP data lives in the text part, so
    // an HTML alternative is not covered by it: show only the text then.
    let inline_pgp =
        armored_block(&text, "PGP MESSAGE").is_some() || armored_block(&text, "PGP SIGNED MESSAGE").is_some();
    if let Some(armored) = armored_block(&text, "PGP MESSAGE") {
        r.encrypted = true;
        match opts.gpg.decrypt(armored.as_bytes()).await {
            Ok(d) => {
                if let Some(sig) = &d.signature {
                    (r.signature, r.signature_text) = signature_state(sig);
                }
                text = String::from_utf8_lossy(&d.plaintext).into_owned();
            }
            Err(e) => {
                r.decryption_failed = true;
                text = format!("This message is encrypted and could not be decrypted.\n\n{e}");
            }
        }
    } else if let Some(signed) = armored_block(&text, "PGP SIGNED MESSAGE")
        && let Ok((plain, sig)) = opts.gpg.verify_inline(signed.as_bytes()).await
    {
        (r.signature, r.signature_text) = signature_state(&sig);
        text = String::from_utf8_lossy(&plain).into_owned();
    }

    // Attachments and inline parts. Parts the HTML shows through `cid:` are
    // part of the body, not attachments.
    let html_source: String = body
        .html_body
        .iter()
        .filter_map(|&i| body.parts.get(i as usize))
        .filter_map(|p| match &p.body {
            PartType::Html(h) => Some(h.as_ref()),
            _ => None,
        })
        .collect();
    let shown_inline = |p: &mail_parser::MessagePart<'_>| {
        p.content_id().is_some_and(|cid| html_source.contains(&format!("cid:{}", cid.trim_matches(['<', '>']))))
    };
    for (n, a) in body.attachments().filter(|a| !shown_inline(a)).enumerate() {
        let filename = a.attachment_name().map(str::to_owned).unwrap_or_else(|| format!("attachment-{}", n + 1));
        let content_type = a
            .content_type()
            .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")).to_ascii_lowercase())
            .unwrap_or_else(|| "application/octet-stream".into());
        let data = match &a.body {
            PartType::Message(m) => m.raw_message.to_vec(),
            _ => a.contents().to_vec(),
        };
        r.attachments.push((AttachmentInfo { index: n as u32, filename, content_type, size: data.len() as u64 }, data));
    }
    for p in &body.parts {
        if let Some(cid) = p.content_id() {
            let ct = p
                .content_type()
                .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")).to_ascii_lowercase())
                .unwrap_or_default();
            r.cids.insert(cid.trim_matches(['<', '>']).to_owned(), (ct, p.contents().to_vec()));
        }
    }

    let html = body
        .html_body
        .iter()
        .filter_map(|&i| body.parts.get(i as usize))
        .find_map(|p| match &p.body {
            PartType::Html(h) => Some(h.to_string()),
            _ => None,
        })
        .filter(|_| !r.decryption_failed && !inline_pgp);
    if let Some(html) = html {
        let clean = sanitize(&html, &opts.url_base);
        r.has_remote_content = REMOTE.is_match(&clean);
        r.html_doc = Some(html_document(&clean, opts.allow_remote));
    }
    r.text = text;
    r.text_doc = text_document(&r.text);
    r
}

/// The text body; HTML-only messages are converted to text. format=flowed is
/// decoded.
fn body_text(m: &Message<'_>) -> String {
    let flowed = m.text_body.iter().filter_map(|&i| m.parts.get(i as usize)).find_map(|p| match &p.body {
        PartType::Text(t) => {
            let ct = p.content_type();
            let is_flowed = ct.and_then(|c| c.attribute("format")).is_some_and(|f| f.eq_ignore_ascii_case("flowed"));
            let delsp = ct.and_then(|c| c.attribute("delsp")).is_some_and(|f| f.eq_ignore_ascii_case("yes"));
            Some(if is_flowed { unflow(t, delsp) } else { t.to_string() })
        }
        _ => None,
    });
    if let Some(t) = flowed {
        return t;
    }
    if let Some(html) = m.body_html(0) {
        return html2text::from_read(html.as_bytes(), 80).unwrap_or_default();
    }
    m.body_text(0).map(|t| t.into_owned()).unwrap_or_default()
}

/// Decode format=flowed (RFC 3676) into logical lines.
pub fn unflow(text: &str, delsp: bool) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut current: Option<(usize, String)> = None; // (quote depth, text)
    for raw in text.replace("\r\n", "\n").split('\n') {
        let depth = raw.chars().take_while(|&c| c == '>').count();
        let mut line = &raw[depth..];
        if line.starts_with(' ') {
            line = &line[1..]; // space-stuffing
        }
        let soft = line.ends_with(' ') && line != "-- ";
        let content = if soft && delsp { &line[..line.len() - 1] } else { line };
        match &mut current {
            Some((d, buf)) if *d == depth => buf.push_str(content),
            _ => {
                if let Some((d, buf)) = current.take() {
                    out.push(quoted(d, &buf));
                }
                current = Some((depth, content.to_owned()));
            }
        }
        if !soft {
            let (d, buf) = current.take().expect("set above");
            out.push(quoted(d, &buf));
        }
    }
    if let Some((d, buf)) = current {
        out.push(quoted(d, &buf));
    }
    out.join("\n")
}

fn quoted(depth: usize, text: &str) -> String {
    if depth == 0 { text.to_owned() } else { format!("{} {text}", ">".repeat(depth)) }
}

fn armored_block<'a>(text: &'a str, kind: &str) -> Option<&'a str> {
    let begin = format!("-----BEGIN {kind}-----");
    let start = text.find(&begin)?;
    let end_marker =
        if kind == "PGP SIGNED MESSAGE" { "-----END PGP SIGNATURE-----" } else { "-----END PGP MESSAGE-----" };
    let end = text[start..].find(end_marker)? + start + end_marker.len();
    Some(&text[start..end])
}

static REMOTE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:src|background|srcset|poster)\s*=\s*"\s*https?:|url\(\s*['"]?\s*https?:|@import"#)
        .expect("valid regex")
});

/// Sanitize HTML with ammonia. Inline styles and `<style>` are kept (no JS can
/// run and remote loads are blocked separately); `cid:` references are
/// rewritten to the message's `tern-msg:` URL.
pub fn sanitize(html: &str, url_base: &str) -> String {
    let base = url_base.to_owned();
    let mut b = ammonia::Builder::default();
    b.add_tags(["style", "center", "font"])
        .rm_clean_content_tags(["style"])
        .add_generic_attributes([
            "style",
            "class",
            "align",
            "valign",
            "width",
            "height",
            "bgcolor",
            "color",
            "border",
            "cellpadding",
            "cellspacing",
            "dir",
            "background",
        ])
        .add_tag_attributes("font", ["face", "size"])
        .url_schemes(["http", "https", "mailto", "cid", "data"].into_iter().collect())
        .link_rel(Some("noopener noreferrer"))
        .strip_comments(true)
        .attribute_filter(move |_el, attr, value| {
            if matches!(attr, "src" | "background")
                && let Some(cid) = value.trim().strip_prefix("cid:")
            {
                return Some(format!("{base}/cid/{}", percent(cid)).into());
            }
            Some(value.into())
        });
    b.clean(html).to_string()
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn html_document(body: &str, allow_remote: bool) -> String {
    let remote = if allow_remote { " http: https:" } else { "" };
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; \
style-src 'unsafe-inline' tern-msg: data:{remote}; img-src tern-msg: data:{remote}; \
font-src tern-msg: data:{remote}\">\
<style>html{{color-scheme:light;background:#fff;color:#000}}body{{margin:8px;font-family:sans-serif;overflow-wrap:anywhere}}img{{max-width:100%;height:auto}}</style>\
</head><body>{body}</body></html>"
    )
}

static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(?:https?://|mailto:)[^\s<>"']+[^\s<>"'.,;:!?)\]]"#).expect("valid regex"));

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Plain text as an HTML document: escaped, links clickable, quote levels
/// styled. Follows the light/dark preference.
pub fn text_document(text: &str) -> String {
    let mut body = String::new();
    for line in text.lines() {
        let depth = line.chars().take_while(|c| *c == '>' || *c == ' ').filter(|c| *c == '>').count().min(3);
        let mut out = String::new();
        let mut last = 0;
        for m in LINK.find_iter(line) {
            out.push_str(&escape(&line[last..m.start()]));
            let url = escape(m.as_str());
            out.push_str(&format!("<a href=\"{url}\">{url}</a>"));
            last = m.end();
        }
        out.push_str(&escape(&line[last..]));
        if depth > 0 {
            body.push_str(&format!("<span class=\"q{depth}\">{out}</span>\n"));
        } else {
            body.push_str(&out);
            body.push('\n');
        }
    }
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\">\
<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\">\
<style>:root{{color-scheme:light dark}}body{{margin:8px}}\
pre{{white-space:pre-wrap;overflow-wrap:anywhere;font-family:monospace;font-size:10pt;margin:0}}\
.q1{{color:#2a6fb0}}.q2{{color:#3c8c3c}}.q3{{color:#9c6b1d}}</style>\
</head><body><pre>{body}</pre></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_split_is_exact() {
        let body = b"preamble\r\n--b\r\nContent-Type: text/plain\r\n\r\nhello\r\n--b\r\nsig\r\n--b--\r\n";
        let parts = split_multipart(body, "b");
        assert_eq!(parts, vec![b"Content-Type: text/plain\r\n\r\nhello".to_vec(), b"sig".to_vec()]);
    }

    #[test]
    fn sanitizing() {
        let html = r#"<p onclick="x()">Hi<script>alert(1)</script><img src="cid:logo@x"><img src="https://t.example/p.gif"><style>p{color:red}</style><a href="javascript:x">j</a></p>"#;
        let clean = sanitize(html, "tern-msg:/acc/5");
        assert!(!clean.contains("script") && !clean.contains("onclick") && !clean.contains("javascript"));
        assert!(clean.contains(r#"src="tern-msg:/acc/5/cid/logo@x""#), "{clean}");
        assert!(clean.contains("p{color:red}"));
        assert!(REMOTE.is_match(&clean));
        assert!(!REMOTE.is_match(&sanitize("<p style=\"color:red\">x</p>", "b")));
    }

    #[test]
    fn flowed_decoding() {
        assert_eq!(unflow("Hello \r\nworld\r\n> quo \r\n> ted\r\n From x", false), "Hello world\n> quo ted\nFrom x");
        assert_eq!(unflow("ab \r\ncd", true), "abcd");
        assert_eq!(unflow("-- \r\nsig", false), "-- \nsig");
    }

    #[test]
    fn text_doc_links_and_escaping() {
        let d = text_document("see https://example.org/x?a=1&b=2.\n> <b>quote</b>");
        assert!(d.contains(r#"<a href="https://example.org/x?a=1&amp;b=2">"#), "{d}");
        assert!(d.contains("&lt;b&gt;"));
        assert!(d.contains("class=\"q1\""));
    }

    #[tokio::test]
    async fn inline_cid_parts_are_not_attachments() {
        let gpg = Gpg::new("gpg");
        let opts = RenderOptions { gpg: &gpg, allow_remote: false, url_base: "tern-msg:/a/1".into() };
        let raw = b"From: a@x.org\r\nSubject: s\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=m\r\n\r\n--m\r\n\
Content-Type: multipart/related; boundary=r\r\n\r\n--r\r\nContent-Type: text/html\r\n\r\n\
<img src=\"cid:logo@x\">\r\n--r\r\nContent-Type: image/png\r\nContent-ID: <logo@x>\r\n\
Content-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n--r--\r\n--m\r\n\
Content-Type: application/pdf; name=doc.pdf\r\nContent-Disposition: attachment; filename=doc.pdf\r\n\r\n\
PDF\r\n--m--\r\n";
        let r = render(raw, &opts).await;
        assert_eq!(r.attachments.len(), 1);
        assert_eq!(r.attachments[0].0.filename, "doc.pdf");
        assert_eq!(r.cids["logo@x"].1, b"hello");
    }

    #[tokio::test]
    async fn renders_plain_and_html() {
        let gpg = Gpg::new("gpg");
        let opts = RenderOptions { gpg: &gpg, allow_remote: false, url_base: "tern-msg:/a/1".into() };
        let raw = b"From: Ann <ann@example.org>\r\nTo: b@example.org\r\nSubject: Hi\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/alternative; boundary=x\r\n\r\n--x\r\nContent-Type: text/plain\r\n\r\nplain body\r\n\
--x\r\nContent-Type: text/html\r\n\r\n<p>html <img src=\"http://track.example/a.png\"></p>\r\n--x--\r\n";
        let r = render(raw, &opts).await;
        assert_eq!(r.subject, "Hi");
        assert_eq!(r.from, "Ann <ann@example.org>");
        assert!(r.text.contains("plain body"));
        assert!(r.html_doc.as_ref().unwrap().contains("html"));
        assert!(r.has_remote_content);
        assert!(!r.html_doc.unwrap().contains("img-src tern-msg: data: http:"));
    }
}

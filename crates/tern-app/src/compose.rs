//! Composing: address parsing, reply/forward templates and building the
//! outgoing RFC 5322 message, signed and/or encrypted with PGP/MIME.

use mail_parser::MessageParser;
use tern_core::headers::base_subject;
use tern_core::model::Address;
use tern_pgp::Gpg;
use tern_smtp::mime::{self, Attachment, Headers};

use crate::format::{self, Signature, signature_block};
use crate::render::Rendered;
use crate::types::{BodyFormat, Draft, MessageKey, ReplyMode};

/// Parse a free-form address list ("Ann <a@x>, b@y, \"Doe, J\" <j@z>").
pub fn parse_addresses(input: &str) -> Result<Vec<Address>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let header = format!("To: {}\r\n\r\n", input.replace(['\r', '\n'], " "));
    let msg = MessageParser::default().parse_headers(header.as_bytes()).ok_or("cannot parse addresses")?;
    let list = tern_core::headers::addresses_of(msg.to());
    if list.is_empty() || list.iter().any(|a| !valid_email(&a.email)) {
        return Err(format!("invalid address in {input:?}"));
    }
    Ok(list)
}

fn valid_email(e: &str) -> bool {
    matches!(e.rsplit_once('@'), Some((l, d)) if !l.is_empty() && (d.contains('.') || d == "localhost"))
        && !e.contains(char::is_whitespace)
}

fn quote_text(text: &str) -> String {
    text.lines()
        .map(|l| if l.starts_with('>') { format!(">{l}") } else { format!("> {l}") })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.with_timezone(&chrono::Local).format("%a, %d %b %Y %H:%M").to_string())
        .unwrap_or_default()
}

/// Body of a new message: room to write, then the signature.
pub fn new_body(format: BodyFormat, sig: Option<&Signature>) -> String {
    assemble(format, sig, None)
}

/// `lead` (space to write) + signature + `tail` (quote or forwarded
/// message, given as plain text and converted to the editor's format).
fn assemble(format: BodyFormat, sig: Option<&Signature>, tail: Option<&str>) -> String {
    let sig = sig.map(|s| signature_block(s, format));
    let tail = tail.map(|t| format::convert(t, BodyFormat::Plain, format));
    match format {
        BodyFormat::Plain | BodyFormat::Markdown => {
            let mut body = String::from("\n\n");
            if let Some(s) = sig {
                body.push_str(&s);
                body.push_str("\n\n");
            }
            if let Some(t) = tail {
                body.push_str(&t);
                body.push('\n');
            }
            body
        }
        BodyFormat::Html => {
            let mut body = String::from("<p><br></p>\n");
            if let Some(s) = sig {
                body.push_str(&s);
                body.push_str("\n<p><br></p>\n");
            }
            if let Some(t) = tail {
                body.push_str(&t);
            }
            body
        }
    }
}

/// A reply/forward template for `original`, in the editor `format`.
pub fn reply_template(
    key: &MessageKey,
    original: &Rendered,
    mode: ReplyMode,
    own_address: &str,
    format: BodyFormat,
    sig: Option<&Signature>,
) -> Draft {
    let base = base_subject(&original.subject);
    let own = own_address.to_ascii_lowercase();
    let not_me = |a: &&String| !a.to_ascii_lowercase().contains(&format!("<{own}>")) && a.to_ascii_lowercase() != own;
    let mut d = Draft { account: key.account.clone(), format, ..Default::default() };
    match mode {
        ReplyMode::Reply | ReplyMode::ReplyAll => {
            d.subject = format!("Re: {base}");
            d.to = original.reply_to.clone();
            if mode == ReplyMode::ReplyAll {
                let mut to: Vec<&String> = original.to_list.iter().filter(not_me).collect();
                let reply_to = original.reply_to.to_ascii_lowercase();
                to.retain(|a| !reply_to.contains(&a.to_ascii_lowercase()));
                if !to.is_empty() {
                    d.to = std::iter::once(&d.to).chain(to).cloned().collect::<Vec<_>>().join(", ");
                }
                d.cc = original.cc_list.iter().filter(not_me).cloned().collect::<Vec<_>>().join(", ");
            }
            let tail =
                format!("On {}, {} wrote:\n{}", format_date(original.date), original.from, quote_text(&original.text));
            d.body = assemble(format, sig, Some(&tail));
            if let Some(mid) = &original.message_id {
                d.in_reply_to = mid.clone();
                let mut refs = original.references.clone();
                refs.push(mid.clone());
                d.references = refs.join(" ");
            }
            d.reply_to_message = Some(key.clone());
        }
        ReplyMode::Forward => {
            d.subject = format!("Fwd: {base}");
            let tail = format!(
                "---------- Forwarded message ----------\nFrom: {}\nDate: {}\nSubject: {}\nTo: {}\n\n{}",
                original.from,
                format_date(original.date),
                original.subject,
                original.to,
                original.text
            );
            d.body = assemble(format, sig, Some(&tail));
            d.forward_message = Some(key.clone());
        }
    }
    d
}

/// Guess a MIME type from a file name.
fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "txt" | "text" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "json" => "application/json",
        "xml" => "application/xml",
        "odt" => "application/vnd.oasis.opendocument.text",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "eml" => "message/rfc822",
        "ics" => "text/calendar",
        "asc" => "application/pgp-keys",
        _ => "application/octet-stream",
    }
}

/// Identity and PGP settings of the sending account.
pub struct Sender<'a> {
    pub from: Address,
    pub pgp_key: Option<&'a str>,
}

/// A message ready for the outbox.
pub struct Built {
    pub raw: Vec<u8>,
    pub from: String,
    /// To + Cc + Bcc.
    pub recipients: Vec<String>,
    pub subject: String,
}

pub async fn build(draft: &Draft, sender: &Sender<'_>, gpg: &Gpg, forwarded: Option<Vec<u8>>) -> Result<Built, String> {
    let to = parse_addresses(&draft.to)?;
    let cc = parse_addresses(&draft.cc)?;
    let bcc = parse_addresses(&draft.bcc)?;
    let visible: Vec<String> = to.iter().chain(&cc).map(|a| a.email.clone()).collect();
    let hidden: Vec<String> = bcc.iter().map(|a| a.email.clone()).collect();
    let recipients: Vec<String> = visible.iter().chain(&hidden).cloned().collect();
    if recipients.is_empty() {
        return Err("no recipients".into());
    }

    let mut attachments = Vec::new();
    for path in &draft.attachments {
        let data = std::fs::read(path).map_err(|e| format!("cannot read attachment {path}: {e}"))?;
        let filename =
            std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        attachments.push(Attachment { content_type: content_type_for(&filename).into(), filename, data });
    }
    if let Some(raw) = forwarded {
        attachments.push(Attachment {
            filename: format!("{}.eml", sanitize_filename(&draft.subject)),
            content_type: "message/rfc822".into(),
            data: raw,
        });
    }
    let content = match draft.format {
        BodyFormat::Plain => mime::body_entity(&draft.body, &attachments),
        // The Markdown source is the text version: it's meant to be readable.
        BodyFormat::Markdown => mime::alternative_body_entity(
            &draft.body,
            &format::outgoing_html_document(&format::markdown_to_html(&draft.body)),
            &attachments,
        ),
        BodyFormat::Html => {
            let fragment = format::clean_html(&draft.body);
            mime::alternative_body_entity(
                &format::html_to_text(&fragment),
                &format::outgoing_html_document(&fragment),
                &attachments,
            )
        }
    };

    let content = if draft.encrypt {
        let keys = gpg.resolve_recipients(&visible).await.map_err(|e| e.to_string())?;
        let hidden_keys = gpg.resolve_recipients(&hidden).await.map_err(|e| e.to_string())?;
        let sign_key = if draft.sign { Some(sender.pgp_key.ok_or("no PGP key configured for signing")?) } else { None };
        let armored =
            gpg.encrypt(&content, &keys, &hidden_keys, sign_key, sender.pgp_key).await.map_err(|e| e.to_string())?;
        mime::encrypted_entity(&armored)
    } else if draft.sign {
        let key = sender.pgp_key.ok_or("no PGP key configured for signing")?;
        let sig = gpg.sign_detached(&content, key).await.map_err(|e| e.to_string())?;
        mime::signed_entity(&content, &sig)
    } else {
        content
    };

    let domain = sender.from.email.rsplit_once('@').map(|(_, d)| d).unwrap_or("localhost");
    let headers = Headers {
        from: sender.from.clone(),
        to,
        cc,
        subject: draft.subject.clone(),
        message_id: format!("{}@{domain}", mime::random_token()),
        in_reply_to: Some(clean_msgid(&draft.in_reply_to)).filter(|s| !s.is_empty()),
        references: draft.references.split_whitespace().map(clean_msgid).filter(|s| !s.is_empty()).collect(),
        date: chrono::Local::now().to_rfc2822(),
    };
    Ok(Built {
        raw: mime::message(&headers, &content),
        from: sender.from.email.clone(),
        recipients,
        subject: draft.subject.clone(),
    })
}

/// A Message-ID without brackets, whitespace or line breaks (these end up
/// in headers).
fn clean_msgid(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && !c.is_control() && !matches!(c, '<' | '>')).collect()
}

fn sanitize_filename(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
    let s = s.trim();
    if s.is_empty() { "message".into() } else { s.chars().take(60).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        let a = parse_addresses("Ann <ann@example.org>, \"Doe, J\" <j@example.com>, bob@example.net").unwrap();
        assert_eq!(a.len(), 3);
        assert_eq!(a[1].name.as_deref(), Some("Doe, J"));
        assert!(parse_addresses("not an address").is_err());
        assert!(parse_addresses("  ").unwrap().is_empty());
        assert!(parse_addresses("@localhost").is_err());
        assert_eq!(clean_msgid("<a@b>\r\nBcc: x"), "a@bBcc:x");
    }

    #[test]
    fn reply_all_excludes_self() {
        let r = Rendered {
            subject: "Re: Plans".into(),
            from: "Ann <ann@x.org>".into(),
            reply_to: "Ann <ann@x.org>".into(),
            to_list: vec!["\"Me\" <me@x.org>".into(), "\"Bob\" <bob@x.org>".into()],
            cc_list: vec!["carl@x.org".into(), "me@x.org".into()],
            text: "hello\n> older".into(),
            message_id: Some("m1@x".into()),
            references: vec!["m0@x".into()],
            ..Default::default()
        };
        let key = MessageKey { account: "a".into(), id: 1 };
        let d = reply_template(&key, &r, ReplyMode::ReplyAll, "me@x.org", BodyFormat::Plain, None);
        assert_eq!(d.subject, "Re: Plans");
        assert_eq!(d.to, "Ann <ann@x.org>, \"Bob\" <bob@x.org>");
        assert_eq!(d.cc, "carl@x.org");
        assert!(d.body.contains("> hello\n>> older"));
        assert_eq!(d.references, "m0@x m1@x");
        assert_eq!(d.in_reply_to, "m1@x");
    }

    #[test]
    fn reply_in_each_format_with_signature() {
        let r = Rendered {
            subject: "Plans".into(),
            from: "Ann <ann@x.org>".into(),
            reply_to: "Ann <ann@x.org>".into(),
            text: "line *one*\nline two".into(),
            ..Default::default()
        };
        let key = MessageKey { account: "a".into(), id: 1 };
        let sig = Signature { format: BodyFormat::Plain, content: "Seb".into() };
        let p = reply_template(&key, &r, ReplyMode::Reply, "me@x.org", BodyFormat::Plain, Some(&sig));
        assert!(p.body.starts_with("\n\n-- \nSeb\n\nOn "), "{:?}", p.body);
        assert!(p.body.contains("> line *one*\n> line two"));
        let m = reply_template(&key, &r, ReplyMode::Reply, "me@x.org", BodyFormat::Markdown, Some(&sig));
        assert!(m.body.contains("> line \\*one\\*\\\n> line two"), "{:?}", m.body);
        let h = reply_template(&key, &r, ReplyMode::Reply, "me@x.org", BodyFormat::Html, Some(&sig));
        assert!(h.body.contains("tern-signature") && h.body.contains("<blockquote>line *one*<br>"), "{}", h.body);
        assert_eq!(h.format, BodyFormat::Html);
    }

    #[tokio::test]
    async fn builds_markdown_and_html_messages() {
        let gpg = Gpg::new("gpg");
        let sender = Sender { from: Address { name: None, email: "me@x.org".into() }, pgp_key: None };
        let d = Draft {
            to: "a@x.org".into(),
            subject: "Hi".into(),
            body: "**bold** text\n\n-- \nSeb".into(),
            format: BodyFormat::Markdown,
            ..Default::default()
        };
        let b = build(&d, &sender, &gpg, None).await.unwrap();
        let m = mail_parser::MessageParser::default().parse(&b.raw).unwrap();
        assert!(m.body_text(0).unwrap().contains("**bold** text"));
        assert!(m.body_html(0).unwrap().contains("<strong>bold</strong>"));
        let d = Draft { body: "<p>Hello <b>there</b><script>x()</script></p>".into(), format: BodyFormat::Html, ..d };
        let b = build(&d, &sender, &gpg, None).await.unwrap();
        let m = mail_parser::MessageParser::default().parse(&b.raw).unwrap();
        let html = m.body_html(0).unwrap();
        assert!(html.contains("<b>there</b>") && !html.contains("script"), "{html}");
        assert!(m.body_text(0).unwrap().contains("Hello"));
    }

    #[tokio::test]
    async fn builds_plain_message() {
        let gpg = Gpg::new("gpg");
        let sender = Sender { from: Address { name: Some("Me".into()), email: "me@x.org".into() }, pgp_key: None };
        let d = Draft {
            to: "a@x.org".into(),
            bcc: "secret@x.org".into(),
            subject: "Hi".into(),
            body: "Body".into(),
            ..Default::default()
        };
        let b = build(&d, &sender, &gpg, None).await.unwrap();
        assert_eq!(b.recipients, vec!["a@x.org", "secret@x.org"]);
        let s = String::from_utf8(b.raw).unwrap();
        assert!(s.contains("Message-ID: <") && s.contains("@x.org>"));
        assert!(!s.contains("secret"));
    }
}

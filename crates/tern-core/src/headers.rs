//! Extract the indexed [`Envelope`] from raw RFC 5322 header bytes.

use mail_parser::{HeaderValue, MessageParser, MimeHeaders};

use crate::model::{Address, Envelope};

/// Parse the header block of a message (also accepts a full message).
/// `fallback_date` (INTERNALDATE) is used when `Date:` is missing or bogus.
pub fn parse_envelope(raw: &[u8], fallback_date: Option<i64>) -> Envelope {
    let Some(msg) = MessageParser::default().parse_headers(raw) else {
        return Envelope { date: fallback_date.unwrap_or(0), ..Default::default() };
    };
    let date = msg.date().map(|d| d.to_timestamp()).filter(|&t| t > 0).or(fallback_date).unwrap_or(0);
    let (has_attachments, encrypted) = match msg.content_type() {
        Some(ct) => {
            let t = ct.ctype().to_ascii_lowercase();
            let s = ct.subtype().unwrap_or("").to_ascii_lowercase();
            (
                t == "multipart" && matches!(s.as_str(), "mixed" | "encrypted" | "signed"),
                t == "multipart" && s == "encrypted",
            )
        }
        None => (false, false),
    };
    Envelope {
        date,
        from: addresses_of(msg.from()),
        to: addresses_of(msg.to()),
        cc: addresses_of(msg.cc()),
        subject: msg.subject().unwrap_or("").trim().to_owned(),
        message_id: msg.message_id().map(clean_id),
        in_reply_to: text_list(msg.in_reply_to()).into_iter().next(),
        references: text_list(msg.references()),
        has_attachments,
        encrypted,
    }
}

fn clean_id(s: &str) -> String {
    s.trim().trim_start_matches('<').trim_end_matches('>').to_owned()
}

fn text_list(v: &HeaderValue<'_>) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => vec![clean_id(t)],
        HeaderValue::TextList(l) => l.iter().map(|t| clean_id(t)).collect(),
        _ => Vec::new(),
    }
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect()
}

pub fn addresses_of(a: Option<&mail_parser::Address<'_>>) -> Vec<Address> {
    let Some(a) = a else { return Vec::new() };
    a.iter()
        .filter_map(|addr| {
            Some(Address {
                name: addr.name().map(|n| n.trim().to_owned()).filter(|n| !n.is_empty()),
                email: addr.address()?.trim().to_owned(),
            })
        })
        .collect()
}

/// Subject without reply/forward prefixes, for display and thread grouping.
pub fn base_subject(subject: &str) -> &str {
    let mut s = subject.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let stripped = ["re:", "fwd:", "fw:", "aw:", "sv:", "vs:", "tr:", "wg:"]
            .iter()
            .find(|p| lower.starts_with(*p))
            .map(|p| s[p.len()..].trim_start());
        match stripped {
            Some(rest) => s = rest,
            None => return s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HDR: &[u8] = b"From: =?utf-8?q?S=C3=A9bastien?= <seb@example.org>\r\n\
To: a@example.org, \"B B\" <b@example.org>\r\n\
Subject: Re: Hello\r\n\
Date: Sat, 04 Oct 2026 10:00:00 +0200\r\n\
Message-ID: <abc@x>\r\n\
In-Reply-To: <parent@x>\r\n\
References: <root@x> <parent@x>\r\n\
Content-Type: multipart/mixed; boundary=xx\r\n\r\n";

    #[test]
    fn parses_envelope() {
        let e = parse_envelope(HDR, None);
        assert_eq!(e.from[0].name.as_deref(), Some("Sébastien"));
        assert_eq!(e.from[0].email, "seb@example.org");
        assert_eq!(e.to.len(), 2);
        assert_eq!(e.subject, "Re: Hello");
        assert_eq!(e.message_id.as_deref(), Some("abc@x"));
        assert_eq!(e.in_reply_to.as_deref(), Some("parent@x"));
        assert_eq!(e.references, vec!["root@x", "parent@x"]);
        assert_eq!(e.date, 1_791_100_800);
        assert!(e.has_attachments);
    }

    #[test]
    fn fallback_date_and_garbage() {
        let e = parse_envelope(b"Subject: x\r\n\r\n", Some(42));
        assert_eq!(e.date, 42);
        let e = parse_envelope(b"\xff\xfe garbage", Some(7));
        assert_eq!(e.date, 7);
    }

    #[test]
    fn strips_prefixes() {
        assert_eq!(base_subject("Re: AW: Fwd:  Hello"), "Hello");
        assert_eq!(base_subject("Recipe"), "Recipe");
    }
}

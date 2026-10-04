//! Message body formats for composing: plain text, Markdown and HTML, the
//! conversions between them (when the user switches editor mode, and for
//! signatures), and the outgoing HTML document.
//!
//! What goes on the wire:
//! * plain: text/plain (format=flowed)
//! * Markdown: multipart/alternative, the Markdown source as text/plain and
//!   the rendered HTML
//! * HTML: multipart/alternative, a text rendering and the editor's HTML,
//!   cleaned with ammonia

use std::path::Path;
use std::sync::LazyLock;

use pulldown_cmark::{Options, Parser};
use regex::Regex;

use crate::types::BodyFormat;

impl From<tern_config::ComposeFormat> for BodyFormat {
    fn from(f: tern_config::ComposeFormat) -> Self {
        match f {
            tern_config::ComposeFormat::Plain => Self::Plain,
            tern_config::ComposeFormat::Markdown => Self::Markdown,
            tern_config::ComposeFormat::Html => Self::Html,
        }
    }
}

/// The signature separator line (RFC 3676 §4.3).
pub const SIG_SEPARATOR: &str = "-- ";

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

static LINK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(?:https?://|mailto:)[^\s<>"']+[^\s<>"'.,;:!?)\]]"#).expect("valid regex"));

fn linkify_escaped(line: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for m in LINK.find_iter(line) {
        out.push_str(&escape(&line[last..m.start()]));
        let url = escape(m.as_str());
        out.push_str(&format!("<a href=\"{url}\">{url}</a>"));
        last = m.end();
    }
    out.push_str(&escape(&line[last..]));
    out
}

/// Split a plain-text line into quote depth and content (`> > x` → 2, `x`).
fn quote_depth(line: &str) -> (usize, &str) {
    let mut depth = 0;
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start_matches(' ');
        match trimmed.strip_prefix('>') {
            Some(r) if trimmed.len() < rest.len() + 1 => {
                depth += 1;
                rest = r;
            }
            _ => break,
        }
    }
    (depth, rest.strip_prefix(' ').unwrap_or(rest))
}

/// Plain text → HTML fragment: escaped, links clickable, quote levels as
/// nested blockquotes, line breaks kept.
pub fn text_to_html(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    let mut first_in_block = true;
    for line in text.replace("\r\n", "\n").split('\n') {
        let (d, content) = if line == SIG_SEPARATOR { (0, line) } else { quote_depth(line) };
        if d != depth {
            while depth < d {
                out.push_str("<blockquote>");
                depth += 1;
            }
            while depth > d {
                out.push_str("</blockquote>");
                depth -= 1;
            }
            first_in_block = true;
        }
        if !first_in_block {
            out.push_str("<br>\n");
        }
        out.push_str(&linkify_escaped(content));
        first_in_block = false;
    }
    while depth > 0 {
        out.push_str("</blockquote>");
        depth -= 1;
    }
    out
}

/// Plain text → Markdown that renders the same: Markdown syntax characters
/// are escaped, line breaks become hard breaks, quotes stay quotes.
pub fn text_to_markdown(text: &str) -> String {
    let lines: Vec<(usize, String)> = text
        .replace("\r\n", "\n")
        .split('\n')
        .map(|line| {
            if line == SIG_SEPARATOR {
                return (0, line.to_owned());
            }
            let (d, content) = quote_depth(line);
            let mut esc = String::with_capacity(content.len());
            for (i, c) in content.chars().enumerate() {
                if matches!(c, '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~')
                    || (i == 0 && matches!(c, '-' | '+' | '='))
                {
                    esc.push('\\');
                }
                esc.push(c);
            }
            // "1. text" would become a list.
            if let Some(dot) = esc.find(". ")
                && esc[..dot].chars().all(|c| c.is_ascii_digit())
                && dot > 0
            {
                esc.insert(dot, '\\');
            }
            (d, esc)
        })
        .collect();
    let mut out = Vec::with_capacity(lines.len());
    for (i, (d, content)) in lines.iter().enumerate() {
        let prefix = "> ".repeat(*d);
        // A hard break (trailing backslash) when the next line continues the
        // same paragraph.
        let continues = lines.get(i + 1).is_some_and(|(nd, nc)| nd == d && !nc.is_empty())
            && !content.is_empty()
            && content != SIG_SEPARATOR;
        out.push(format!("{prefix}{content}{}", if continues { "\\" } else { "" }));
    }
    out.join("\n")
}

/// Markdown → sanitized HTML fragment.
///
/// Two departures from strict CommonMark, both because this is email: a line
/// break is a line break (as in GitHub comments; CommonMark would join the
/// lines), and a `-- ` line stays a signature separator (it would otherwise
/// turn the previous line into a heading).
pub fn markdown_to_html(md: &str) -> String {
    let prepared: String = md
        .replace("\r\n", "\n")
        .split('\n')
        .map(|l| if l == SIG_SEPARATOR { "\n\\-- \\".to_owned() } else { l.to_owned() })
        .collect::<Vec<_>>()
        .join("\n");
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    use pulldown_cmark::{Event, Tag, TagEnd};
    // Also: bare URLs become links (outside links and code), as in GitHub.
    let mut in_link = 0usize;
    let events = Parser::new_ext(&prepared, opts).map(|e| match e {
        Event::SoftBreak => Event::HardBreak,
        Event::Start(Tag::Link { .. }) => {
            in_link += 1;
            e
        }
        Event::End(TagEnd::Link) => {
            in_link = in_link.saturating_sub(1);
            e
        }
        Event::Text(t) if in_link == 0 && LINK.is_match(&t) => Event::InlineHtml(linkify_escaped(&t).into()),
        e => e,
    });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    clean_html(&html)
}

/// HTML → plain text.
pub fn html_to_text(html: &str) -> String {
    html2text::from_read(html.as_bytes(), 78).unwrap_or_default().trim_end().to_owned()
}

/// HTML → Markdown.
pub fn html_to_markdown(html: &str) -> String {
    htmd::convert(&clean_html(html)).unwrap_or_else(|_| html_to_text(html))
}

/// Convert a body between editor formats.
pub fn convert(content: &str, from: BodyFormat, to: BodyFormat) -> String {
    use BodyFormat::*;
    match (from, to) {
        (a, b) if a == b => content.to_owned(),
        (Plain, Markdown) => text_to_markdown(content),
        (Plain, Html) => text_to_html(content),
        // Markdown is meant to be readable as is.
        (Markdown, Plain) => content.to_owned(),
        (Markdown, Html) => markdown_to_html(content),
        (Html, Plain) => html_to_text(content),
        (Html, Markdown) => html_to_markdown(content),
        _ => content.to_owned(),
    }
}

/// Clean HTML for sending (editor output, rendered Markdown): no scripts, no
/// `<style>` blocks, no remote resources other than links; inline styles
/// are kept.
pub fn clean_html(html: &str) -> String {
    let mut b = ammonia::Builder::default();
    b.add_tags(["font", "center", "u", "s", "strike"])
        .add_generic_attributes(["style", "align", "dir"])
        .add_tag_attributes("font", ["color", "face", "size"])
        .add_tag_attributes("input", ["type", "checked", "disabled"])
        .add_tags(["input"])
        .url_schemes(["http", "https", "mailto", "data"].into_iter().collect())
        .link_rel(None)
        .strip_comments(true);
    b.clean(html).to_string()
}

/// The text/html part: a complete document around a cleaned fragment.
pub fn outgoing_html_document(fragment: &str) -> String {
    format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n<style>\n\
body{{font-family:sans-serif;font-size:11pt;line-height:1.4}}\n\
blockquote{{margin:0 0 0 .8ex;border-left:2px solid #999;padding-left:1ex;color:#444}}\n\
pre,code{{font-family:monospace}}\n\
.tern-signature{{color:#666}}\n\
</style></head>\n<body>\n{fragment}\n</body></html>\n"
    )
}

/// HTML document for the composer's Markdown preview.
pub fn markdown_preview(md: &str) -> String {
    outgoing_html_document(&markdown_to_html(md))
}

/// A signature file, read fresh each time (edits apply immediately).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub format: BodyFormat,
    pub content: String,
}

pub fn load_signature(path: &Path) -> Result<Signature, String> {
    let format = tern_config::signature_format(path)
        .map(BodyFormat::from)
        .ok_or_else(|| format!("{}: not a .txt, .md or .html file", path.display()))?;
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if meta.len() > tern_config::MAX_SIGNATURE_BYTES {
        return Err(format!("{}: larger than 64 KiB", path.display()));
    }
    let content = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    // Writers often end files with a newline, or start the signature with
    // their own separator.
    let content = content.trim_end().strip_prefix("-- \n").unwrap_or(content.trim_end()).to_owned();
    Ok(Signature { format, content })
}

/// The signature with its separator, in the editor's format.
pub fn signature_block(sig: &Signature, target: BodyFormat) -> String {
    let body = convert(&sig.content, sig.format, target);
    match target {
        BodyFormat::Plain | BodyFormat::Markdown => format!("{SIG_SEPARATOR}\n{body}"),
        BodyFormat::Html => format!("<div class=\"tern-signature\">-- <br>\n{body}</div>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_to_html_quotes_and_links() {
        let h = text_to_html("Hi <you>\nsee https://example.org/a?b=1&c\n> quoted\n> > deeper\nback");
        assert_eq!(
            h,
            "Hi &lt;you&gt;<br>\nsee <a href=\"https://example.org/a?b=1&amp;c\">https://example.org/a?b=1&amp;c</a>\
<blockquote>quoted<blockquote>deeper</blockquote></blockquote>back"
        );
    }

    #[test]
    fn plain_to_markdown_renders_like_the_text() {
        let text = "1. not a list\n*not bold* and a_b\n> quote line one\n> quote line two\n\n-- \nSeb";
        let md = text_to_markdown(text);
        let html = markdown_to_html(&md);
        assert!(!html.contains("<ol>") && !html.contains("<em>"), "{html}");
        assert!(html.contains("*not bold*"), "{html}");
        assert!(html.contains("quote line one<br>"), "{html}");
        assert!(html.contains("<blockquote>"), "{html}");
        assert!(!html.contains("<h2>"), "{html}");
        assert!(html.contains("-- <br>"), "{html}");
    }

    #[test]
    fn markdown_rendering_is_sanitized() {
        let html =
            markdown_to_html("# Title\n\n**bold** <script>alert(1)</script> <img src=x onerror=y>\n\n- [x] done");
        assert!(html.contains("<h1>Title</h1>") && html.contains("<strong>bold</strong>"));
        assert!(!html.contains("script") && !html.contains("onerror"), "{html}");
        assert!(html.contains("checkbox"), "{html}");
    }

    #[test]
    fn bare_urls_are_linked() {
        let html = markdown_to_html("see https://example.org/a?b=1 and [x](https://x.org) and `https://code.org`");
        assert!(html.contains(r#"<a href="https://example.org/a?b=1">"#), "{html}");
        assert_eq!(html.matches("<a ").count(), 2, "{html}");
        assert!(html.contains("<code>https://code.org</code>"), "{html}");
    }

    #[test]
    fn newlines_are_kept() {
        let html = markdown_to_html("**Seb** · Tern\n<https://example.org>");
        assert!(html.contains("Tern<br>"), "{html}");
    }

    #[test]
    fn signature_separator_never_makes_a_heading() {
        let html = markdown_to_html("Thanks\n-- \nSeb");
        assert!(!html.contains("<h2>"), "{html}");
        assert!(html.contains("Thanks") && html.contains("Seb"));
    }

    #[test]
    fn html_round_trips() {
        let md = html_to_markdown("<p>Hello <strong>world</strong></p><ul><li>one</li></ul>");
        assert!(md.contains("**world**") && md.contains("one"), "{md}");
        let text = html_to_text("<p>Hello <b>there</b></p><blockquote>quoted</blockquote>");
        assert!(text.contains("Hello") && text.contains("there") && text.contains("quoted"), "{text}");
    }

    #[test]
    fn signatures_in_each_format() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("sig.md");
        std::fs::write(&p, "-- \n**Seb** — <https://karlsen.fr>\n").unwrap();
        let sig = load_signature(&p).unwrap();
        assert_eq!(sig.content, "**Seb** — <https://karlsen.fr>");
        assert_eq!(signature_block(&sig, BodyFormat::Markdown), "-- \n**Seb** — <https://karlsen.fr>");
        let html = signature_block(&sig, BodyFormat::Html);
        assert!(html.starts_with("<div class=\"tern-signature\">-- <br>") && html.contains("<strong>Seb</strong>"));
        let txt = Signature { format: BodyFormat::Plain, content: "Seb\n*phone*".into() };
        assert_eq!(signature_block(&txt, BodyFormat::Markdown), "-- \nSeb\\\n\\*phone\\*");
        assert!(load_signature(&t.path().join("x.doc")).is_err());
    }
}

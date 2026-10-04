//! Domain types shared by the store, the sync engine and the backends.

use serde::{Deserialize, Serialize};

/// Local folder id (rowid in the account's `store.sqlite`).
pub type FolderId = i64;
/// Local message id (rowid in the account's `store.sqlite`).
pub type MessageId = i64;

/// SPECIAL-USE roles (RFC 6154), plus INBOX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FolderRole {
    Inbox,
    Drafts,
    Sent,
    Trash,
    Junk,
    Archive,
    All,
    Flagged,
}

impl FolderRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Drafts => "drafts",
            Self::Sent => "sent",
            Self::Trash => "trash",
            Self::Junk => "junk",
            Self::Archive => "archive",
            Self::All => "all",
            Self::Flagged => "flagged",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "inbox" => Self::Inbox,
            "drafts" => Self::Drafts,
            "sent" => Self::Sent,
            "trash" => Self::Trash,
            "junk" => Self::Junk,
            "archive" => Self::Archive,
            "all" => Self::All,
            "flagged" => Self::Flagged,
            _ => return None,
        })
    }

    /// Map an IMAP SPECIAL-USE attribute (`\Sent`, ...).
    pub fn from_special_use(attr: &str) -> Option<Self> {
        Some(match attr.to_ascii_lowercase().as_str() {
            "\\drafts" => Self::Drafts,
            "\\sent" => Self::Sent,
            "\\trash" => Self::Trash,
            "\\junk" => Self::Junk,
            "\\archive" => Self::Archive,
            "\\all" => Self::All,
            "\\flagged" => Self::Flagged,
            _ => return None,
        })
    }

    /// Guess from common folder names, for servers without SPECIAL-USE.
    pub fn guess_from_name(name: &str) -> Option<Self> {
        let leaf = name.rsplit(['/', '.']).next().unwrap_or(name).to_ascii_lowercase();
        Some(match leaf.as_str() {
            "inbox" if name.eq_ignore_ascii_case("inbox") => Self::Inbox,
            "drafts" | "draft" => Self::Drafts,
            "sent" | "sent items" | "sent messages" | "sent mail" => Self::Sent,
            "trash" | "deleted" | "deleted items" | "deleted messages" | "bin" => Self::Trash,
            "junk" | "spam" | "junk e-mail" => Self::Junk,
            "archive" | "archives" => Self::Archive,
            _ => return None,
        })
    }
}

/// System flags as a bit set; custom keywords are kept separately.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Flags(pub u32);

impl Flags {
    pub const SEEN: Self = Self(1);
    pub const ANSWERED: Self = Self(1 << 1);
    pub const FLAGGED: Self = Self(1 << 2);
    pub const DELETED: Self = Self(1 << 3);
    pub const DRAFT: Self = Self(1 << 4);
    pub const FORWARDED: Self = Self(1 << 5); // `$Forwarded` keyword

    pub const ALL: [(Self, &'static str); 6] = [
        (Self::SEEN, "\\Seen"),
        (Self::ANSWERED, "\\Answered"),
        (Self::FLAGGED, "\\Flagged"),
        (Self::DELETED, "\\Deleted"),
        (Self::DRAFT, "\\Draft"),
        (Self::FORWARDED, "$Forwarded"),
    ];

    pub const fn empty() -> Self {
        Self(0)
    }
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// The IMAP names of the flags in the set.
    pub fn names(self) -> Vec<&'static str> {
        Self::ALL.iter().filter(|(f, _)| self.contains(*f)).map(|(_, n)| *n).collect()
    }

    /// Parse IMAP flags; returns the system flags and remaining keywords.
    /// `\Recent` is session-specific and dropped.
    pub fn from_imap<'a>(names: impl IntoIterator<Item = &'a str>) -> (Self, Vec<String>) {
        let mut f = Self::empty();
        let mut keywords = Vec::new();
        for n in names {
            match Self::ALL.iter().find(|(_, name)| name.eq_ignore_ascii_case(n)) {
                Some((flag, _)) => f = f.union(*flag),
                None if n.eq_ignore_ascii_case("\\Recent") || n == "\\*" => {}
                None => keywords.push(n.to_owned()),
            }
        }
        (f, keywords)
    }
}

impl std::ops::BitOr for Flags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

/// A mailbox address with optional display name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub name: Option<String>,
    pub email: String,
}

impl Address {
    /// "Name <email>" or just the email.
    pub fn display(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => format!("{n} <{}>", self.email),
            _ => self.email.clone(),
        }
    }

    /// The name if present, otherwise the email.
    pub fn short(&self) -> &str {
        self.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&self.email)
    }
}

/// Parsed header fields kept in the message index for fast lists, threading
/// and search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    /// Unix seconds; `Date:` header, falling back to INTERNALDATE.
    pub date: i64,
    pub from: Vec<Address>,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub subject: String,
    /// Without angle brackets.
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    /// Content-Type says multipart/mixed, or encrypted/signed.
    pub has_attachments: bool,
    pub encrypted: bool,
}

/// A folder as reported by the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFolder {
    /// Full server-side name (decoded).
    pub name: String,
    pub delimiter: Option<String>,
    pub role: Option<FolderRole>,
    pub selectable: bool,
}

/// Status after selecting a folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderStatus {
    pub uidvalidity: u32,
    pub uidnext: Option<u32>,
    pub exists: u32,
    /// Present when the server supports CONDSTORE.
    pub highestmodseq: Option<u64>,
}

/// Header-phase fetch result for one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHeader {
    pub uid: u32,
    pub flags: Flags,
    pub keywords: Vec<String>,
    pub size: u32,
    pub modseq: Option<u64>,
    /// INTERNALDATE as unix seconds.
    pub internal_date: Option<i64>,
    /// Raw RFC 5322 header block.
    pub header: Vec<u8>,
}

/// Flag state of one message on the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFlags {
    pub uid: u32,
    pub flags: Flags,
    pub keywords: Vec<String>,
    pub modseq: Option<u64>,
}

/// What a backend supports; the sync engine adapts to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackendCaps {
    pub condstore: bool,
    pub qresync: bool,
    pub idle: bool,
    pub move_: bool,
    pub uidplus: bool,
}

/// A row of the local message index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSummary {
    pub id: MessageId,
    pub folder_id: FolderId,
    pub uid: Option<u32>,
    pub blob: Option<String>,
    pub flags: Flags,
    pub size: u32,
    pub envelope: Envelope,
}

/// A row of the local folder table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    pub name: String,
    pub delimiter: Option<String>,
    pub role: Option<FolderRole>,
    pub selectable: bool,
    pub uidvalidity: Option<u32>,
    pub uidnext: Option<u32>,
    pub highestmodseq: Option<u64>,
    pub total: u32,
    pub unread: u32,
}

impl Folder {
    /// Last path component, for display in a tree.
    pub fn leaf_name(&self) -> &str {
        match &self.delimiter {
            Some(d) if !d.is_empty() => self.name.rsplit(d.as_str()).next().unwrap_or(&self.name),
            _ => &self.name,
        }
    }

    /// Parent's full name, if any.
    pub fn parent_name(&self) -> Option<&str> {
        let d = self.delimiter.as_deref().filter(|d| !d.is_empty())?;
        self.name.rsplit_once(d).map(|(p, _)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_roundtrip() {
        let (f, kw) = Flags::from_imap(["\\Seen", "\\Recent", "$Forwarded", "$label1"]);
        assert!(f.contains(Flags::SEEN));
        assert!(f.contains(Flags::FORWARDED));
        assert!(!f.contains(Flags::FLAGGED));
        assert_eq!(kw, vec!["$label1"]);
        assert_eq!(f.names(), vec!["\\Seen", "$Forwarded"]);
    }

    #[test]
    fn folder_names() {
        let f = Folder {
            id: 1,
            name: "INBOX/Lists/rust".into(),
            delimiter: Some("/".into()),
            role: None,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highestmodseq: None,
            total: 0,
            unread: 0,
        };
        assert_eq!(f.leaf_name(), "rust");
        assert_eq!(f.parent_name(), Some("INBOX/Lists"));
        assert_eq!(FolderRole::guess_from_name("INBOX.Sent"), Some(FolderRole::Sent));
        assert_eq!(FolderRole::guess_from_name("inbox"), Some(FolderRole::Inbox));
    }
}

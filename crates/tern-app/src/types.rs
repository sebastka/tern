//! Plain data crossing the frontend boundary (ARCHITECTURE.md §4): owned
//! strings, integers, vectors and simple enums only, so `cxx` (and later
//! gtk-rs) can map them directly.

/// Identifies a message: account id + local message id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageKey {
    pub account: String,
    pub id: i64,
}

/// Identifies a folder: account id + local folder id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderKey {
    pub account: String,
    pub folder: i64,
}

/// What the startup code needs to decide whether to show the profile picker.
#[derive(Debug, Clone, Default)]
pub struct StartupInfo {
    pub profiles: Vec<String>,
    /// Set when the picker can be skipped (single profile, `--profile`, or
    /// `ask_on_startup = false` with a valid `default_profile`).
    pub auto_profile: Option<String>,
    /// Problems in `tern.toml`.
    pub issues: Vec<String>,
}

/// One node of the folder tree, in display order (pre-order). Account roots
/// have `folder == 0` and `depth == 0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderNode {
    pub account: String,
    pub folder: i64,
    pub depth: u32,
    /// Index of the parent node in the same list, or -1 for account roots.
    pub parent: i32,
    /// Display name: account name for roots, leaf name for folders.
    pub name: String,
    /// Full server-side name (empty for account roots).
    pub path: String,
    /// `inbox`, `sent`, ... or empty.
    pub role: String,
    pub selectable: bool,
    pub unread: u32,
    pub total: u32,
}

/// A message list column (and sort key), from `[ui.message_list]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListColumn {
    Flag,
    Subject,
    From,
    To,
    /// From, or To in Sent and Drafts folders.
    Correspondent,
    Date,
    Attachment,
    Size,
}

/// Quick filters of the message list; all off shows everything. With any
/// on, the list is flat (like search results).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListFilter {
    pub unread: bool,
    pub flagged: bool,
    pub attachments: bool,
}

impl ListFilter {
    pub fn any(&self) -> bool {
        self.unread || self.flagged || self.attachments
    }
}

/// A date section of the message list (when sorted by date).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DateGroup {
    Today,
    Yesterday,
    /// Earlier this week (weeks start on Monday).
    ThisWeek,
    LastWeek,
    /// A calendar month: `year` and `month` of [`ListGroup`].
    Month,
}

/// A section header in the message list, before row `start`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListGroup {
    /// Index of the first row (in `list_rows` numbering) of the section.
    pub start: u32,
    pub kind: DateGroup,
    /// For `Month`; 0 otherwise.
    pub year: i32,
    pub month: u32,
}

/// Message list columns and order, as configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListLayout {
    /// Left to right.
    pub columns: Vec<ListColumn>,
    /// May be a column that isn't shown.
    pub sort_by: ListColumn,
    pub descending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow {
    pub key: MessageKey,
    /// Thread depth (0 in flat mode).
    pub depth: u32,
    /// Messages in the thread, on thread roots in threaded mode; else 0.
    pub thread_size: u32,
    /// Unix seconds.
    pub date: i64,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub unread: bool,
    pub flagged: bool,
    pub answered: bool,
    /// `$Forwarded` keyword (set by Tern and most other clients).
    pub forwarded: bool,
    pub has_attachments: bool,
    pub encrypted: bool,
    pub size: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SignatureState {
    #[default]
    None,
    Good,
    /// Valid, but the key isn't trusted or something expired/was revoked.
    Warning,
    Bad,
    /// Missing public key or error.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentInfo {
    pub index: u32,
    pub filename: String,
    pub content_type: String,
    pub size: u64,
}

/// A sender or recipient, for display (name may be empty).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Person {
    pub name: String,
    pub email: String,
}

/// One header field, for the extended header view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderField {
    pub name: String,
    pub value: String,
}

/// What a line of the message source is, for coloring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLine {
    Body,
    /// `Name: value` (message or MIME part header).
    HeaderField,
    /// A folded header line (starts with a space or tab).
    HeaderContinuation,
    /// A multipart boundary line.
    Boundary,
    /// Base64 data.
    Encoded,
    /// `>` quoted text.
    Quote,
    /// `-----BEGIN/END PGP ...` lines.
    Armor,
}

/// A message's source as stored (exactly as received).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageSource {
    /// Suggested file name for saving (`<subject>.eml`).
    pub file_name: String,
    /// The bytes to save.
    pub raw: Vec<u8>,
    /// For display: `raw` as text, one line per raw line.
    pub text: String,
    /// One entry per line of `text`.
    pub lines: Vec<SourceLine>,
}

/// The rendered message, as decided by the shared rendering policy (§10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageView {
    pub key: MessageKey,
    pub subject: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub date: i64,
    /// `tern-msg:` URL of the document to show by default (HTML or the
    /// plain-text rendering, depending on content and preference).
    pub url: String,
    /// `tern-msg:` URL of the plain-text rendering (always available).
    pub text_url: String,
    /// Plain text body (for quoting, copying).
    pub text: String,
    pub has_html: bool,
    pub has_remote_content: bool,
    /// Remote content may be loaded (per message or sender allow-list).
    pub remote_allowed: bool,
    pub attachments: Vec<AttachmentInfo>,
    pub encrypted: bool,
    /// Decryption failed: the body is the error explanation.
    pub decryption_failed: bool,
    pub signature: SignatureState,
    pub signature_text: String,
    /// All header fields in order (empty while the body is missing).
    pub headers: Vec<HeaderField>,
    /// Structured sender and recipients, for the header pane.
    pub sender: Person,
    pub to_people: Vec<Person>,
    pub cc_people: Vec<Person>,
    /// Sender picture (PNG/JPEG/GIF), empty if none; see `[avatars]`. A
    /// lookup started for this message arrives as `Event::AvatarReady`.
    pub avatar: Vec<u8>,
    /// Body not downloaded yet (header-only sync so far).
    pub body_missing: bool,
}

/// Editor mode / body format of an outgoing message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum BodyFormat {
    #[default]
    Plain,
    Markdown,
    /// `body` is HTML (from a rich-text editor).
    Html,
}

/// An outgoing message as entered in the composer. Address fields are
/// free-form comma-separated lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Draft {
    pub account: String,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// In the format given by `format`.
    pub body: String,
    pub format: BodyFormat,
    pub in_reply_to: String,
    /// Space-separated Message-IDs without brackets.
    pub references: String,
    /// Local file paths.
    pub attachments: Vec<String>,
    pub sign: bool,
    pub encrypt: bool,
    /// A reply to or forward of encrypted mail: it quotes decrypted text, so
    /// it must stay encrypted (the composer doesn't turn `encrypt` off).
    pub encryption_required: bool,
    /// When replying: mark this message as answered after sending.
    pub reply_to_message: Option<MessageKey>,
    /// When forwarding: attach this message as `message/rfc822`.
    pub forward_message: Option<MessageKey>,
}

/// Which recipients of a draft have an encryption key in the local keyring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipientKeys {
    /// The address fields could be parsed (not while half-typed).
    pub valid: bool,
    /// Distinct recipient addresses (To, Cc and Bcc).
    pub recipients: u32,
    /// Recipients without a usable key.
    pub missing: Vec<String>,
}

impl RecipientKeys {
    /// Every recipient has a key, and there is at least one recipient.
    pub fn all_have_keys(&self) -> bool {
        self.valid && self.recipients > 0 && self.missing.is_empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyMode {
    Reply,
    ReplyAll,
    Forward,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountInfo {
    pub id: String,
    pub name: String,
    pub email: String,
    pub sign_by_default: bool,
    pub encrypt_when_possible: bool,
    pub has_pgp_key: bool,
    /// An archive folder pattern is configured.
    pub can_archive: bool,
    pub compose_format: BodyFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    Connecting,
    Syncing,
    Online,
    Offline,
    /// Needs user action (e.g. authentication failed).
    Error,
}

/// Events delivered to the frontend through the registered callback, on a
/// runtime thread. Frontends must hop to their UI thread before acting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Folders added/removed/renamed or counts changed: re-read the tree.
    FolderTreeChanged,
    /// The current message list changed: re-read `list_rows`.
    ListChanged {
        count: u32,
    },
    /// Result of `open_message`.
    MessageLoaded(MessageView),
    /// Result of `prepare_reply`.
    ComposeReady(Draft),
    AccountStatus {
        account: String,
        state: AccountState,
        text: String,
    },
    /// Download progress of one folder.
    Progress {
        account: String,
        folder: String,
        done: u32,
        total: u32,
    },
    /// The configuration changed; `issues` is empty when it is valid.
    ConfigChanged {
        issues: Vec<String>,
    },
    /// Result of `send`: the message was built and queued in the outbox
    /// (`error` empty), or could not be built (bad address, missing key...).
    DraftQueued {
        request: u64,
        error: String,
    },
    /// An outgoing message was sent (`ok`) or failed.
    SendResult {
        ok: bool,
        text: String,
    },
    /// Non-fatal error to show in the status bar.
    Error {
        text: String,
    },
    /// Another `tern` process asked this one to show itself.
    RaiseWindow {
        activation_token: String,
    },
    /// A sender picture was found (PNG/JPEG/GIF); show it if `email` is the
    /// sender of the message on screen.
    AvatarReady {
        email: String,
        image: Vec<u8>,
    },
    /// Play a sound from the freedesktop sound theme (an event id such as
    /// `message-new-email`).
    PlaySound {
        sound: String,
    },
    /// The user clicked a new-mail notification: raise the window (with the
    /// XDG activation token, if any) and show this message in its folder.
    ShowMessage {
        key: MessageKey,
        folder: FolderKey,
        activation_token: String,
    },
}

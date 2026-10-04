//! Configuration file schema (ARCHITECTURE.md §5).
//!
//! All structs use `deny_unknown_fields` so typos are reported instead of
//! silently ignored: the config is the only way to change settings, so it must
//! be strict.

use serde::Deserialize;

/// `tern.toml`
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct GlobalConfig {
    pub default_profile: Option<String>,
    pub ask_on_startup: AskOnStartup,
    pub ui: UiConfig,
    pub gpg: GpgConfig,
    pub compose: ComposeConfig,
    pub memory: MemoryConfig,
}

/// `[memory]` in tern.toml.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct MemoryConfig {
    /// Upper bound for rendered messages kept in memory (bodies, inline
    /// images and attachments), in MiB. The message on screen is always kept.
    pub message_cache_mb: u32,
    /// Keep a spare Chromium renderer process ready, so the next message
    /// view starts faster (about 30 MiB). Applies after a restart.
    pub spare_renderer: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { message_cache_mb: 64, spare_renderer: true }
    }
}

/// Wrapper so the default is `true` without a custom serde function per field.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct AskOnStartup(pub bool);

impl Default for AskOnStartup {
    fn default() -> Self {
        Self(true)
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct UiConfig {
    /// Show the plain-text alternative instead of HTML when both exist.
    pub prefer_plain_text: bool,
    /// Show message lists threaded (JWZ) instead of flat.
    pub threaded: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self { prefer_plain_text: false, threaded: true }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct GpgConfig {
    /// The gpg binary, looked up in `$PATH` unless absolute.
    pub program: String,
    /// Allow `gpg --locate-keys` (WKD) when a recipient key is missing.
    pub wkd_lookup: bool,
}

impl Default for GpgConfig {
    fn default() -> Self {
        Self { program: "gpg".into(), wkd_lookup: false }
    }
}

/// `profiles/<name>/profile.toml`
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct ProfileConfig {
    /// Identity defaults, used by accounts that don't set their own.
    pub display_name: Option<String>,
    pub pgp: Option<PgpConfig>,
    pub store: StoreConfig,
    pub remote_content: RemoteContentConfig,
    /// Default archive folder pattern for all accounts (see `AccountConfig`).
    pub archive: Option<String>,
    pub compose: ComposeConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct StoreConfig {
    /// zstd-compress message blobs (§8).
    pub compress: bool,
    /// zstd level, 1..=19.
    pub compression_level: i32,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self { compress: true, compression_level: 3 }
    }
}

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct RemoteContentConfig {
    /// Senders (exact addresses, or `@domain`) whose remote content is always
    /// loaded.
    pub allow_senders: Vec<String>,
}

impl RemoteContentConfig {
    pub fn allows(&self, sender: &str) -> bool {
        let sender = sender.trim().to_ascii_lowercase();
        self.allow_senders.iter().any(|a| {
            let a = a.trim().to_ascii_lowercase();
            if a.starts_with('@') { sender.ends_with(&a) } else { sender == a }
        })
    }
}

/// `profiles/<name>/accounts/<id>.toml`
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub name: String,
    pub email: String,
    pub display_name: Option<String>,
    pub imap: ImapConfig,
    pub smtp: SmtpConfig,
    pub pgp: Option<PgpConfig>,
    #[serde(default)]
    pub sync: SyncConfig,
    /// Folder for the Archive action, e.g. `"Archive/{year}"`. `{year}` and
    /// `{month}` come from the message date; `/` separates levels. Without
    /// it (here or in profile.toml), archiving is disabled.
    pub archive: Option<String>,
    #[serde(default)]
    pub compose: ComposeConfig,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ImapConfig {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub tls: TlsMode,
    pub username: String,
    pub password: PasswordSource,
}

impl ImapConfig {
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.tls {
            TlsMode::Implicit => 993,
            TlsMode::Starttls | TlsMode::InsecurePlaintext => 143,
        })
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SmtpConfig {
    pub host: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub tls: TlsMode,
    pub username: String,
    pub password: PasswordSource,
    /// Append sent mail to the Sent folder. Set to `false` for servers that
    /// already store submitted mail there (e.g. Gmail).
    #[serde(default = "yes")]
    pub save_to_sent: bool,
}

impl SmtpConfig {
    pub fn effective_port(&self) -> u16 {
        self.port.unwrap_or(match self.tls {
            TlsMode::Implicit => 465,
            TlsMode::Starttls | TlsMode::InsecurePlaintext => 587,
        })
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TlsMode {
    #[default]
    Implicit,
    Starttls,
    /// No TLS at all. Only for local test servers; rejected unless the host is
    /// a loopback address.
    InsecurePlaintext,
}

/// Format of the message editor.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ComposeFormat {
    #[default]
    Plain,
    Markdown,
    Html,
}

/// `[compose]` in tern.toml, profile.toml or an account file; the most
/// specific setting wins.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct ComposeConfig {
    /// Editor mode for new messages and replies.
    pub format: Option<ComposeFormat>,
    /// Signature file: `.txt`, `.md` or `.html`, relative to the config
    /// directory (`~/.config/tern/`) unless absolute. Not in tern.toml.
    pub signature: Option<String>,
}

/// Where a password comes from. Plaintext passwords are not representable.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PasswordSource {
    /// Shell command; the first line of stdout is the password.
    pub command: Option<String>,
    /// Secret Service lookup; see [`crate::secrets`] for the syntax.
    pub keyring: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PgpConfig {
    /// Key ID or fingerprint (`0x` prefix optional).
    pub key: String,
    #[serde(default)]
    pub sign_by_default: bool,
    #[serde(default)]
    pub encrypt_when_possible: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct SyncConfig {
    /// Folders watched with IDLE in addition to INBOX.
    pub idle_folders: Vec<String>,
    /// Polling interval for all other folders, in seconds.
    pub poll_interval_secs: u64,
    /// Folders never synced (IMAP names).
    pub exclude_folders: Vec<String>,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self { idle_folders: Vec::new(), poll_interval_secs: 300, exclude_folders: Vec::new() }
    }
}

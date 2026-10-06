//! Loading and validating the config tree.

use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use crate::model::*;

/// One problem in one config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub file: PathBuf,
    pub message: String,
}

impl std::fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.file.display(), self.message)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{} configuration problem(s): {}", .0.len(), .0.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("; "))]
pub struct ConfigErrors(pub Vec<ConfigIssue>);

/// An account as loaded from `accounts/<id>.toml`.
#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    /// File stem; stable identifier used for storage paths.
    pub id: String,
    pub config: AccountConfig,
}

impl Account {
    /// Display name for the From header: account, then profile default.
    pub fn display_name<'a>(&'a self, profile: &'a ProfileConfig) -> Option<&'a str> {
        self.config.display_name.as_deref().or(profile.display_name.as_deref())
    }

    /// PGP settings: account, then profile default.
    pub fn pgp<'a>(&'a self, profile: &'a ProfileConfig) -> Option<&'a PgpConfig> {
        self.config.pgp.as_ref().or(profile.pgp.as_ref())
    }

    /// Archive folder pattern: account, then profile default.
    pub fn archive<'a>(&'a self, profile: &'a ProfileConfig) -> Option<&'a str> {
        self.config.archive.as_deref().or(profile.archive.as_deref())
    }

    /// Sent folder (`/`-separated levels): account, then profile, then
    /// tern.toml. `None` means the server's `\Sent` folder.
    pub fn sent_folder<'a>(&'a self, profile: &'a ProfileConfig, global: &'a GlobalConfig) -> Option<&'a str> {
        self.config.sent_folder.as_deref().or(profile.sent_folder.as_deref()).or(global.sent_folder.as_deref())
    }

    /// New-mail notification settings, per key: account, then profile, then
    /// tern.toml, then the defaults (on, with sound, INBOX).
    pub fn notifications(&self, profile: &ProfileConfig, global: &GlobalConfig) -> Notifications {
        let levels = [&self.config.notifications, &profile.notifications, &global.notifications];
        Notifications {
            enabled: levels.iter().find_map(|n| n.enabled).unwrap_or(true),
            sound: levels.iter().find_map(|n| n.sound).unwrap_or(true),
            folders: levels.iter().find_map(|n| n.folders.clone()).unwrap_or_else(|| vec!["INBOX".into()]),
            exclude_folders: levels.iter().find_map(|n| n.exclude_folders.clone()).unwrap_or_default(),
        }
    }

    /// Editor mode: account, then profile, then tern.toml, then plain.
    pub fn compose_format(&self, profile: &ProfileConfig, global: &GlobalConfig) -> ComposeFormat {
        self.config.compose.format.or(profile.compose.format).or(global.compose.format).unwrap_or_default()
    }

    /// Signature file (resolved path): account, then profile.
    pub fn signature(&self, profile: &ProfileConfig, config_dir: &Path) -> Option<PathBuf> {
        let s = self.config.compose.signature.as_deref().or(profile.compose.signature.as_deref())?;
        Some(resolve_path(config_dir, s))
    }
}

/// Effective new-mail notification settings of an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notifications {
    pub enabled: bool,
    pub sound: bool,
    /// Folder patterns (see [`folder_matches`]).
    pub folders: Vec<String>,
    /// Folder patterns that win over `folders`.
    pub exclude_folders: Vec<String>,
}

impl Notifications {
    /// Does new mail in `folder` (server name; levels separated by
    /// `delimiter`) notify?
    pub fn watches(&self, folder: &str, delimiter: &str) -> bool {
        let any = |patterns: &[String]| patterns.iter().any(|p| folder_matches(p, folder, delimiter));
        any(&self.folders) && !any(&self.exclude_folders)
    }
}

/// Match a folder pattern from the config against a server folder name:
/// `*` is every folder, `A/*` is `A` and everything below it, anything else
/// is one folder. `/` in the pattern is the server's `delimiter`, and
/// `INBOX` matches in any case, as in IMAP.
pub fn folder_matches(pattern: &str, folder: &str, delimiter: &str) -> bool {
    let same = |a: &str, b: &str| a == b || (a.eq_ignore_ascii_case("INBOX") && b.eq_ignore_ascii_case("INBOX"));
    if pattern == "*" {
        return true;
    }
    if let Some(parent) = pattern.strip_suffix("/*") {
        let parent = parent.replace('/', delimiter);
        return same(&parent, folder)
            || folder.strip_prefix(&parent).is_some_and(|rest| rest.starts_with(delimiter))
            // INBOX children, e.g. "INBOX.Lists" for "inbox/*".
            || (parent.eq_ignore_ascii_case("INBOX")
                && folder.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("INBOX"))
                && folder[5..].starts_with(delimiter));
    }
    same(&pattern.replace('/', delimiter), folder)
}

/// Resolve a path from the config: `~/` is the home directory, relative paths
/// are relative to the config directory.
pub fn resolve_path(config_dir: &Path, s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    config_dir.join(s)
}

/// Kind of a signature file, by extension.
pub fn signature_format(path: &Path) -> Option<ComposeFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "txt" => Some(ComposeFormat::Plain),
        "md" | "markdown" => Some(ComposeFormat::Markdown),
        "html" | "htm" => Some(ComposeFormat::Html),
        _ => None,
    }
}

/// Maximum signature size; anything larger is surely a mistake.
pub const MAX_SIGNATURE_BYTES: u64 = 64 * 1024;

/// A fully loaded and validated profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub name: String,
    pub config: ProfileConfig,
    /// In display order: `account_order` first, then the rest by id.
    pub accounts: Vec<Account>,
}

impl Profile {
    pub fn account(&self, id: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == id)
    }
}

/// Valid profile and account ids: they become directory names.
pub fn is_valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn read_toml<T: DeserializeOwned>(file: &Path) -> Result<T, ConfigIssue> {
    let text = fs::read_to_string(file)
        .map_err(|e| ConfigIssue { file: file.into(), message: format!("cannot read: {e}") })?;
    toml::from_str(&text)
        .map_err(|e| ConfigIssue { file: file.into(), message: e.to_string().trim_end().replace('\n', " ") })
}

/// Load `tern.toml`. A missing file means all defaults.
pub fn load_global(config_dir: &Path) -> Result<GlobalConfig, ConfigErrors> {
    let file = config_dir.join("tern.toml");
    if !file.exists() {
        return Ok(GlobalConfig::default());
    }
    let cfg: GlobalConfig = read_toml(&file).map_err(|e| ConfigErrors(vec![e]))?;
    let mut issues = Vec::new();
    if let Some(p) = &cfg.default_profile
        && !is_valid_id(p)
    {
        issues.push(ConfigIssue { file: file.clone(), message: format!("invalid default_profile {p:?}") });
    }
    if cfg.gpg.program.trim().is_empty() {
        issues.push(ConfigIssue { file: file.clone(), message: "gpg.program must not be empty".into() });
    }
    if !(1..=4096).contains(&cfg.memory.message_cache_mb) {
        issues.push(ConfigIssue {
            file: file.clone(),
            message: format!("memory.message_cache_mb must be 1..=4096, got {}", cfg.memory.message_cache_mb),
        });
    }
    if cfg.compose.signature.is_some() {
        issues.push(ConfigIssue {
            file: file.clone(),
            message: "compose.signature belongs in profile.toml or an account file, not in tern.toml".into(),
        });
    }
    let mut push = |m: String| issues.push(ConfigIssue { file: file.clone(), message: m });
    validate_folder_name("sent_folder", cfg.sent_folder.as_deref(), &mut push);
    let list = &cfg.ui.message_list;
    if list.columns.is_empty() {
        push("ui.message_list.columns must not be empty".into());
    }
    if let Some(dup) = first_duplicate(&list.columns) {
        push(format!("ui.message_list.columns lists {dup:?} twice"));
    }
    validate_notifications(&cfg.notifications, &mut push);
    if issues.is_empty() { Ok(cfg) } else { Err(ConfigErrors(issues)) }
}

fn first_duplicate<T: PartialEq + std::fmt::Debug>(items: &[T]) -> Option<&T> {
    items.iter().enumerate().find(|(i, x)| items[..*i].contains(x)).map(|(_, x)| x)
}

fn validate_notifications(n: &NotificationsConfig, push: &mut impl FnMut(String)) {
    for (key, patterns) in [("folders", &n.folders), ("exclude_folders", &n.exclude_folders)] {
        for p in patterns.iter().flatten() {
            let base = if p == "*" { "" } else { p.strip_suffix("/*").unwrap_or(p) };
            if p != "*" && base.split('/').any(|l| l.trim().is_empty()) {
                push(format!("notifications.{key}: {p:?} has an empty folder level"));
            } else if base.contains('*') {
                push(format!("notifications.{key}: {p:?}: `*` only works alone or as a last `/*`"));
            }
        }
    }
}

/// A fixed folder name: `/`-separated, non-empty levels.
fn validate_folder_name(key: &str, name: Option<&str>, push: &mut impl FnMut(String)) {
    let Some(n) = name else { return };
    if n.split('/').any(|l| l.trim().is_empty()) {
        push(format!("{key} {n:?}: folder levels must not be empty"));
    }
}

/// Names of all profile directories, sorted.
pub fn list_profiles(config_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(config_dir.join("profiles"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_valid_id(n))
        .collect();
    names.sort();
    names
}

/// Load and validate one profile with all its accounts. Every problem found is
/// reported, not just the first one.
pub fn load_profile(config_dir: &Path, name: &str) -> Result<Profile, ConfigErrors> {
    let dir = config_dir.join("profiles").join(name);
    let mut issues = Vec::new();
    if !is_valid_id(name) || !dir.is_dir() {
        return Err(ConfigErrors(vec![ConfigIssue { file: dir, message: format!("no profile named {name:?}") }]));
    }

    let profile_file = dir.join("profile.toml");
    let config = if profile_file.exists() {
        match read_toml::<ProfileConfig>(&profile_file) {
            Ok(c) => {
                validate_profile(&c, &profile_file, config_dir, &mut issues);
                c
            }
            Err(e) => {
                issues.push(e);
                ProfileConfig::default()
            }
        }
    } else {
        ProfileConfig::default()
    };

    let mut files: Vec<PathBuf> = fs::read_dir(dir.join("accounts"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();

    let mut accounts = Vec::new();
    for file in files {
        let id = file.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
        if !is_valid_id(&id) {
            issues.push(ConfigIssue {
                file,
                message: "account file name may only contain letters, digits, '-', '_' and '.'".into(),
            });
            continue;
        }
        match read_toml::<AccountConfig>(&file) {
            Ok(c) => {
                validate_account(&c, &file, config_dir, &mut issues);
                accounts.push(Account { id, config: c });
            }
            Err(e) => issues.push(e),
        }
    }

    // Accounts come sorted by id; move the listed ones to the front.
    for id in &config.account_order {
        // A file that exists but has errors is already reported.
        let has_file = issues.iter().any(|i| i.file.file_stem() == Some(std::ffi::OsStr::new(id)));
        if !accounts.iter().any(|a| &a.id == id) && !has_file {
            issues.push(ConfigIssue {
                file: profile_file.clone(),
                message: format!("account_order: no account file accounts/{id}.toml"),
            });
        }
    }
    let rank = |a: &Account| config.account_order.iter().position(|id| *id == a.id).unwrap_or(usize::MAX);
    accounts.sort_by_key(rank);

    if issues.is_empty() { Ok(Profile { name: name.to_owned(), config, accounts }) } else { Err(ConfigErrors(issues)) }
}

fn validate_profile(c: &ProfileConfig, file: &Path, config_dir: &Path, issues: &mut Vec<ConfigIssue>) {
    let mut push = |m: String| issues.push(ConfigIssue { file: file.into(), message: m });
    if !(1..=19).contains(&c.store.compression_level) {
        push(format!("store.compression_level must be 1..=19, got {}", c.store.compression_level));
    }
    if let Some(p) = &c.pgp {
        validate_pgp(p, &mut push);
    }
    validate_archive(c.archive.as_deref(), &mut push);
    validate_folder_name("sent_folder", c.sent_folder.as_deref(), &mut push);
    validate_signature(c.compose.signature.as_deref(), config_dir, &mut push);
    if let Some(dup) = first_duplicate(&c.account_order) {
        push(format!("account_order lists {dup:?} twice"));
    }
    validate_notifications(&c.notifications, &mut push);
}

/// `{year}`/`{month}` placeholders, `/`-separated non-empty levels.
fn validate_archive(pattern: Option<&str>, push: &mut impl FnMut(String)) {
    let Some(p) = pattern else { return };
    let stripped = p.replace("{year}", "").replace("{month}", "");
    if p.trim().is_empty() || p.split('/').any(|l| l.trim().is_empty()) {
        push(format!("archive {p:?}: folder levels must not be empty"));
    } else if stripped.contains(['{', '}']) {
        push(format!("archive {p:?}: only {{year}} and {{month}} are supported as placeholders"));
    }
}

fn validate_signature(sig: Option<&str>, config_dir: &Path, push: &mut impl FnMut(String)) {
    let Some(s) = sig else { return };
    let path = resolve_path(config_dir, s);
    if signature_format(&path).is_none() {
        push(format!("compose.signature {s:?} must be a .txt, .md or .html file"));
        return;
    }
    match fs::metadata(&path) {
        Ok(m) if m.len() > MAX_SIGNATURE_BYTES => {
            push(format!("compose.signature {}: larger than 64 KiB", path.display()))
        }
        Ok(_) => {}
        Err(e) => push(format!("compose.signature {}: {e}", path.display())),
    }
}

/// `pgp.key`: a long key id (16 hex digits) or a fingerprint (40), `0x`
/// optional. Short ids (8 digits) are refused: they are easy to collide.
fn validate_pgp(p: &PgpConfig, push: &mut impl FnMut(String)) {
    let key = p.key.strip_prefix("0x").or_else(|| p.key.strip_prefix("0X")).unwrap_or(&p.key);
    let hex = key.chars().all(|c| c.is_ascii_hexdigit());
    if key.contains(char::is_whitespace) {
        push(format!("pgp.key {:?}: write the fingerprint without spaces", p.key));
    } else if !hex {
        push(format!("pgp.key {:?} is not a hex key id or fingerprint", p.key));
    } else if key.len() == 8 {
        push(format!(
            "pgp.key {:?} is a short key id, which is easy to forge; use the 16-digit long id or the \
             40-digit fingerprint (gpg --list-keys --keyid-format long)",
            p.key
        ));
    } else if key.len() != 16 && key.len() != 40 {
        push(format!(
            "pgp.key {:?} has {} hex digits; expected a 16-digit long key id or a 40-digit fingerprint",
            p.key,
            key.len()
        ));
    }
}

fn validate_account(c: &AccountConfig, file: &Path, config_dir: &Path, issues: &mut Vec<ConfigIssue>) {
    let mut push = |m: String| issues.push(ConfigIssue { file: file.into(), message: m });
    validate_archive(c.archive.as_deref(), &mut push);
    validate_folder_name("sent_folder", c.sent_folder.as_deref(), &mut push);
    validate_signature(c.compose.signature.as_deref(), config_dir, &mut push);
    validate_notifications(&c.notifications, &mut push);
    if c.name.trim().is_empty() {
        push("name must not be empty".into());
    }
    if !looks_like_address(&c.email) {
        push(format!("email {:?} is not a valid address", c.email));
    }
    for (proto, host, tls, user, pw) in [
        ("imap", &c.imap.host, c.imap.tls, &c.imap.username, &c.imap.password),
        ("smtp", &c.smtp.host, c.smtp.tls, &c.smtp.username, &c.smtp.password),
    ] {
        if host.trim().is_empty() {
            push(format!("{proto}.host must not be empty"));
        }
        if user.is_empty() {
            push(format!("{proto}.username must not be empty"));
        }
        if tls == TlsMode::InsecurePlaintext && !is_loopback(host) {
            push(format!("{proto}.tls = \"insecure-plaintext\" is only allowed for localhost"));
        }
        match (&pw.command, &pw.keyring) {
            (Some(_), Some(_)) => push(format!("{proto}.password: set either `command` or `keyring`, not both")),
            (None, None) => push(format!("{proto}.password: set `command` or `keyring`")),
            (Some(c), None) if c.trim().is_empty() => push(format!("{proto}.password.command must not be empty")),
            (None, Some(k)) if k.trim().is_empty() => push(format!("{proto}.password.keyring must not be empty")),
            _ => {}
        }
    }
    if let Some(p) = &c.pgp {
        validate_pgp(p, &mut push);
    }
    if c.sync.poll_interval_secs < 30 {
        push("sync.poll_interval_secs must be at least 30".into());
    }
}

fn looks_like_address(s: &str) -> bool {
    match s.rsplit_once('@') {
        Some((local, domain)) => !local.is_empty() && !domain.is_empty() && !s.contains(char::is_whitespace),
        None => false,
    }
}

fn is_loopback(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().map(|ip| ip.is_loopback()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    const ACCOUNT: &str = r#"
name = "Posteo"
email = "me@posteo.de"
display_name = "Sebastian"

[imap]
host = "posteo.de"
port = 993
tls = "implicit"
username = "me@posteo.de"
password.command = "pass show mail/posteo"

[smtp]
host = "posteo.de"
port = 465
tls = "implicit"
username = "me@posteo.de"
password.keyring = "service=mail account=posteo"

[pgp]
key = "0xDEADBEEFCAFEBABE"
sign_by_default = false
encrypt_when_possible = true
"#;

    #[test]
    fn loads_architecture_example() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "tern.toml", "default_profile = \"personal\"\nask_on_startup = false\n");
        write(t.path(), "profiles/personal/accounts/posteo.toml", ACCOUNT);
        let g = load_global(t.path()).unwrap();
        assert_eq!(g.default_profile.as_deref(), Some("personal"));
        assert!(!g.ask_on_startup.0);
        assert_eq!(list_profiles(t.path()), vec!["personal"]);
        let p = load_profile(t.path(), "personal").unwrap();
        assert_eq!(p.accounts.len(), 1);
        assert_eq!(p.accounts[0].id, "posteo");
        assert_eq!(p.accounts[0].config.imap.effective_port(), 993);
        assert!(p.config.store.compress);
    }

    #[test]
    fn rejects_plaintext_password() {
        let t = tempfile::tempdir().unwrap();
        let bad = ACCOUNT.replace("password.command = \"pass show mail/posteo\"", "password = \"hunter2\"");
        write(t.path(), "profiles/p/accounts/a.toml", &bad);
        let e = load_profile(t.path(), "p").unwrap_err();
        assert_eq!(e.0.len(), 1);
    }

    #[test]
    fn reports_all_issues() {
        let t = tempfile::tempdir().unwrap();
        let bad = ACCOUNT
            .replace("me@posteo.de\"\ndisplay", "nope\"\ndisplay")
            .replace("host = \"posteo.de\"\nport = 993", "host = \"posteo.de\"\nport = 993\ntypo = 1");
        write(t.path(), "profiles/p/accounts/a.toml", &bad);
        write(t.path(), "profiles/p/accounts/b.toml", &ACCOUNT.replace("0xDEADBEEFCAFEBABE", "xyz"));
        write(t.path(), "profiles/p/profile.toml", "[store]\ncompression_level = 40\n");
        let e = load_profile(t.path(), "p").unwrap_err();
        // a.toml: unknown field (parse error); b.toml: bad key; profile: level.
        assert_eq!(e.0.len(), 3, "{e}");
    }

    #[test]
    fn plaintext_tls_only_on_loopback() {
        let t = tempfile::tempdir().unwrap();
        let a = ACCOUNT.replacen("tls = \"implicit\"", "tls = \"insecure-plaintext\"", 1);
        write(t.path(), "profiles/p/accounts/a.toml", &a);
        assert!(load_profile(t.path(), "p").is_err());
        let a = a.replacen("host = \"posteo.de\"", "host = \"127.0.0.1\"", 1);
        write(t.path(), "profiles/p/accounts/a.toml", &a);
        assert!(load_profile(t.path(), "p").is_ok());
    }

    #[test]
    fn compose_and_archive_settings() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "tern.toml", "[compose]\nformat = \"markdown\"\n");
        write(t.path(), "signatures/me.md", "**Seb**");
        write(
            t.path(),
            "profiles/p/profile.toml",
            "archive = \"Archive/{year}\"\n[compose]\nsignature = \"signatures/me.md\"\n",
        );
        write(t.path(), "profiles/p/accounts/a.toml", ACCOUNT);
        let b = ACCOUNT.replace("[imap]", "archive = \"Old/{year}/{month}\"\n[compose]\nformat = \"html\"\n\n[imap]");
        write(t.path(), "profiles/p/accounts/b.toml", &b);
        let g = load_global(t.path()).unwrap();
        let p = load_profile(t.path(), "p").unwrap();
        let (a, b) = (&p.accounts[0], &p.accounts[1]);
        assert_eq!(a.compose_format(&p.config, &g), ComposeFormat::Markdown);
        assert_eq!(b.compose_format(&p.config, &g), ComposeFormat::Html);
        assert_eq!(a.archive(&p.config), Some("Archive/{year}"));
        assert_eq!(b.archive(&p.config), Some("Old/{year}/{month}"));
        assert_eq!(a.signature(&p.config, t.path()), Some(t.path().join("signatures/me.md")));

        // Bad pattern, missing signature, wrong extension.
        write(
            t.path(),
            "profiles/p/profile.toml",
            "archive = \"Archive//{day}\"\n[compose]\nsignature = \"nope.md\"\n",
        );
        let b = ACCOUNT.replace("[imap]", "[compose]\nsignature = \"sig.doc\"\n\n[imap]");
        write(t.path(), "profiles/p/accounts/b.toml", &b);
        let e = load_profile(t.path(), "p").unwrap_err();
        assert_eq!(e.0.len(), 3, "{e}");
        write(t.path(), "tern.toml", "[compose]\nsignature = \"x.txt\"\n");
        assert!(load_global(t.path()).is_err());
    }

    #[test]
    fn memory_settings() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(load_global(t.path()).unwrap().memory, MemoryConfig { message_cache_mb: 64, spare_renderer: true });
        write(t.path(), "tern.toml", "[memory]\nmessage_cache_mb = 16\nspare_renderer = false\n");
        let g = load_global(t.path()).unwrap();
        assert_eq!(g.memory, MemoryConfig { message_cache_mb: 16, spare_renderer: false });
        write(t.path(), "tern.toml", "[memory]\nmessage_cache_mb = 0\n");
        assert!(load_global(t.path()).is_err());
    }

    #[test]
    fn account_order() {
        let t = tempfile::tempdir().unwrap();
        for id in ["a", "b", "c", "d"] {
            write(t.path(), &format!("profiles/p/accounts/{id}.toml"), ACCOUNT);
        }
        let ids = |p: &Profile| p.accounts.iter().map(|a| a.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&load_profile(t.path(), "p").unwrap()), ["a", "b", "c", "d"]);
        write(t.path(), "profiles/p/profile.toml", "account_order = [\"c\", \"a\"]\n");
        assert_eq!(ids(&load_profile(t.path(), "p").unwrap()), ["c", "a", "b", "d"]);
        write(t.path(), "profiles/p/profile.toml", "account_order = [\"c\", \"nope\", \"c\"]\n");
        let e = load_profile(t.path(), "p").unwrap_err();
        assert_eq!(e.0.len(), 2, "{e}");
    }

    #[test]
    fn sent_folder() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "profiles/p/accounts/a.toml", ACCOUNT);
        write(t.path(), "profiles/p/accounts/b.toml", &ACCOUNT.replace("[imap]", "sent_folder = \"Mine\"\n\n[imap]"));
        let p = load_profile(t.path(), "p").unwrap();
        let g = load_global(t.path()).unwrap();
        assert_eq!(p.accounts[0].sent_folder(&p.config, &g), None);
        assert_eq!(p.accounts[1].sent_folder(&p.config, &g), Some("Mine"));

        write(t.path(), "tern.toml", "sent_folder = \"Global\"\n");
        let g = load_global(t.path()).unwrap();
        assert_eq!(p.accounts[0].sent_folder(&p.config, &g), Some("Global"));
        write(t.path(), "profiles/p/profile.toml", "sent_folder = \"INBOX/Sent\"\n");
        let p = load_profile(t.path(), "p").unwrap();
        assert_eq!(p.accounts[0].sent_folder(&p.config, &g), Some("INBOX/Sent"));
        assert_eq!(p.accounts[1].sent_folder(&p.config, &g), Some("Mine"));

        write(t.path(), "profiles/p/profile.toml", "sent_folder = \"INBOX//Sent\"\n");
        assert!(load_profile(t.path(), "p").is_err());
        write(t.path(), "tern.toml", "sent_folder = \"\"\n");
        assert!(load_global(t.path()).is_err());
    }

    #[test]
    fn message_list_settings() {
        let t = tempfile::tempdir().unwrap();
        let g = load_global(t.path()).unwrap();
        assert_eq!(g.ui.message_list.sort_by, ListField::Date);
        assert_eq!(g.ui.message_list.sort_order, SortOrder::Desc);
        assert_eq!(g.ui.message_list.columns.len(), 5);
        write(
            t.path(),
            "tern.toml",
            "[ui.message_list]\ncolumns = [\"date\", \"from\", \"subject\"]\nsort_by = \"subject\"\nsort_order = \"asc\"\n",
        );
        let g = load_global(t.path()).unwrap();
        assert_eq!(g.ui.message_list.columns, [ListField::Date, ListField::From, ListField::Subject]);
        assert_eq!(g.ui.message_list.sort_by, ListField::Subject);
        assert_eq!(g.ui.message_list.sort_order, SortOrder::Asc);
        for bad in ["columns = []", "columns = [\"date\", \"date\"]", "columns = [\"color\"]", "sort_order = \"up\""] {
            write(t.path(), "tern.toml", &format!("[ui.message_list]\n{bad}\n"));
            assert!(load_global(t.path()).is_err(), "{bad}");
        }
    }

    #[test]
    fn notification_settings() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "profiles/p/accounts/a.toml", ACCOUNT);
        let b = ACCOUNT.replace("[imap]", "[notifications]\nfolders = [\"INBOX\", \"Lists/rust\"]\n\n[imap]");
        write(t.path(), "profiles/p/accounts/b.toml", &b);
        let g = load_global(t.path()).unwrap();
        let p = load_profile(t.path(), "p").unwrap();
        let a = p.accounts[0].notifications(&p.config, &g);
        assert_eq!(
            a,
            Notifications { enabled: true, sound: true, folders: vec!["INBOX".into()], exclude_folders: vec![] }
        );
        assert!(a.watches("inbox", "/") && !a.watches("Lists/rust", "/"));

        write(t.path(), "tern.toml", "[notifications]\nsound = false\n");
        write(t.path(), "profiles/p/profile.toml", "[notifications]\nenabled = false\nsound = true\n");
        let g = load_global(t.path()).unwrap();
        let p = load_profile(t.path(), "p").unwrap();
        // Per key: the profile's `sound` wins over tern.toml.
        let a = p.accounts[0].notifications(&p.config, &g);
        assert!(!a.enabled && a.sound);
        let b = p.accounts[1].notifications(&p.config, &g);
        // The account's "Lists/rust" is matched with the server's delimiter.
        assert!(b.watches("Lists.rust", ".") && b.watches("INBOX", "."));

        // Opt-out: everything in tern.toml, exclusions per account.
        write(t.path(), "tern.toml", "[notifications]\nfolders = [\"*\"]\nexclude_folders = [\"Junk\"]\n");
        write(t.path(), "profiles/p/profile.toml", "");
        let c = ACCOUNT.replace("[imap]", "[notifications]\nexclude_folders = [\"Trash\", \"Lists/*\"]\n\n[imap]");
        write(t.path(), "profiles/p/accounts/b.toml", &c);
        let g = load_global(t.path()).unwrap();
        let p = load_profile(t.path(), "p").unwrap();
        let (a, b) = (p.accounts[0].notifications(&p.config, &g), p.accounts[1].notifications(&p.config, &g));
        assert!(a.watches("Lists/rust", "/") && !a.watches("Junk", "/"));
        // The account's exclusions replace tern.toml's (per key, not merged).
        assert!(b.watches("Junk", "/") && !b.watches("Trash", "/") && !b.watches("Lists/rust", "/"));
        assert!(b.watches("INBOX", "/") && b.watches("Listsx", "/"));

        for bad in ["folders = [\" \"]", "exclude_folders = [\"a//b\"]", "folders = [\"Li*\"]", "volume = 3"] {
            write(t.path(), "tern.toml", &format!("[notifications]\n{bad}\n"));
            assert!(load_global(t.path()).is_err(), "{bad}");
        }
    }

    #[test]
    fn folder_patterns() {
        assert!(folder_matches("*", "anything", "/"));
        assert!(folder_matches("inbox", "INBOX", "."));
        assert!(folder_matches("Lists/rust", "Lists.rust", "."));
        assert!(!folder_matches("Lists/rust", "Lists/rust-dev", "/"));
        assert!(folder_matches("Lists/*", "Lists", "/"));
        assert!(folder_matches("Lists/*", "Lists/a/b", "/"));
        assert!(!folder_matches("Lists/*", "Listsx", "/"));
        assert!(folder_matches("inbox/*", "INBOX.Archive", "."));
        assert!(!folder_matches("Archive/*", "INBOX.Archive", "."));
    }

    #[test]
    fn pgp_key_formats() {
        let check = |key: &str| {
            let mut issues = Vec::new();
            validate_pgp(
                &PgpConfig { key: key.into(), sign_by_default: false, encrypt_when_possible: false },
                &mut |m| issues.push(m),
            );
            issues
        };
        assert!(check("0xC74C02E66D0CBECF").is_empty());
        assert!(check("c74c02e66d0cbecf").is_empty());
        assert!(check("0B25B26C537B40B5B208F3A6C74C02E66D0CBECF").is_empty());
        assert!(check("0x6D0CBECF")[0].contains("short key id"));
        assert!(check("0B25 B26C 537B 40B5 B208  F3A6 C74C 02E6 6D0C BECF")[0].contains("without spaces"));
        assert!(check("0xC74C02E66D0CBEC")[0].contains("15 hex digits"));
        assert!(check("sebastian@karlsen.fr")[0].contains("not a hex"));
    }

    #[test]
    fn remote_allow_list() {
        let r = RemoteContentConfig { allow_senders: vec!["@lwn.net".into(), "a@b.c".into()] };
        assert!(r.allows("news@LWN.net"));
        assert!(r.allows("a@b.c"));
        assert!(!r.allows("x@b.c"));
    }
}

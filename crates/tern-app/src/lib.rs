//! UI-agnostic application facade (ARCHITECTURE.md §4).
//!
//! Frontends talk only to [`App`]: commands are plain method calls, results
//! and changes arrive as [`Event`]s through the callback given to
//! [`App::new`], on a runtime thread. Read models (folder tree, message list
//! windows) are synchronous reads of the local store: the UI never waits for
//! the network.

mod account;
mod avatar;
mod cache;
pub mod compose;
mod dbus;
pub mod format;
mod groups;
mod hub;
mod logging;
mod notify;
pub mod render;
mod sort;
mod source;
mod tree;
pub mod types;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, Weak};
use std::time::Duration;

use tern_config::{AvatarLookup, ConfigWatcher, Dirs, GlobalConfig, Profile};
use tern_core::lock::{LockError, ProfileLock};
use tern_core::thread::ThreadRow;
use tern_core::{Flags, ops};
use tern_pgp::Gpg;
use tracing::{info, warn};

use crate::account::{AccountRt, Request};
use crate::hub::Hub;
use crate::render::{RenderOptions, Rendered};
pub use crate::types::*;

pub const URL_SCHEME: &str = "tern-msg";

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error("profile {0:?} is already open in another Tern window")]
    AlreadyRunning(String),
    #[error("no profile named {0:?}")]
    NoSuchProfile(String),
    #[error("{0}")]
    Other(String),
}

struct ProfileState {
    name: String,
    config: Option<Profile>,
    accounts: Vec<Arc<AccountRt>>,
    /// Problems in the profile's files.
    issues: Vec<String>,
    /// Problems in tern.toml.
    global_issues: Vec<String>,
    /// Problems with the configured PGP keys in the gpg keyring.
    pgp_issues: Vec<String>,
    /// Sender pictures, cached in this profile's cache directory.
    avatars: Arc<avatar::Avatars>,
    /// Desktop notifications; `None` without a notification server.
    notifier: Option<Arc<notify::Notifier>>,
    _lock: ProfileLock,
    _watcher: Option<ConfigWatcher>,
    _dbus: Option<zbus::Connection>,
}

impl ProfileState {
    fn all_issues(&self) -> Vec<String> {
        self.global_issues.iter().chain(&self.issues).chain(&self.pgp_issues).cloned().collect()
    }
}

fn issue_strings(e: &tern_config::ConfigErrors) -> Vec<String> {
    e.0.iter().map(|i| i.to_string()).collect()
}

impl Drop for ProfileState {
    fn drop(&mut self) {
        for a in &self.accounts {
            a.stop();
        }
    }
}

#[derive(Default)]
struct ListState {
    folder: Option<FolderKey>,
    threaded: bool,
    query: String,
    filter: ListFilter,
    /// With a filter on: rows already shown stay until the list is reopened
    /// (opening an unread message must not make it vanish).
    sticky: HashSet<i64>,
    rows: Vec<ThreadRow>,
    groups: Vec<ListGroup>,
}

struct Inner {
    dirs: Dirs,
    hub: Arc<Hub>,
    rt: tokio::runtime::Handle,
    global: RwLock<GlobalConfig>,
    profile: Mutex<Option<ProfileState>>,
    list: Mutex<ListState>,
    /// Rendered messages, bounded by `[memory] message_cache_mb`. Decrypted
    /// content stays here (memory only).
    cache: Mutex<cache::RenderCache>,
    current: Mutex<Option<MessageKey>>,
    remote_allowed: Mutex<HashSet<MessageKey>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Settings a frontend needs before it starts its toolkit (they can't change
/// at runtime).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupSettings {
    /// `[memory] spare_renderer`
    pub spare_renderer: bool,
}

/// Read [`StartupSettings`] from tern.toml (defaults if missing or invalid;
/// the App reports config problems later).
pub fn startup_settings() -> StartupSettings {
    let g = tern_config::load_global(&Dirs::from_env().config).unwrap_or_default();
    StartupSettings { spare_renderer: g.memory.spare_renderer }
}

pub struct App {
    inner: Arc<Inner>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl App {
    /// Create the app with XDG directories from the environment. `sink`
    /// receives events on runtime threads.
    pub fn new(sink: impl Fn(Event) + Send + Sync + 'static) -> Self {
        Self::with_dirs(Dirs::from_env(), sink)
    }

    pub fn with_dirs(dirs: Dirs, sink: impl Fn(Event) + Send + Sync + 'static) -> Self {
        logging::init(&dirs.logs());
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("tern-rt")
            .enable_all()
            .build()
            .expect("tokio runtime");
        let global = tern_config::load_global(&dirs.config).unwrap_or_default();
        let inner = Arc::new(Inner {
            dirs,
            hub: Arc::new(Hub::new(Arc::new(sink))),
            rt: runtime.handle().clone(),
            list: Mutex::new(ListState { threaded: global.ui.threaded, ..Default::default() }),
            global: RwLock::new(global),
            profile: Mutex::new(None),
            cache: Mutex::new(cache::RenderCache::default()),
            current: Mutex::new(None),
            remote_allowed: Mutex::new(HashSet::new()),
        });
        runtime.spawn(ticker(Arc::downgrade(&inner)));
        info!("Tern {} started", env!("CARGO_PKG_VERSION"));
        Self { inner, runtime: Some(runtime) }
    }

    // ------------------------------------------------------------- profiles

    /// Profiles and whether the picker can be skipped. `requested` is the
    /// `--profile` argument.
    pub fn startup_info(&self, requested: Option<&str>) -> StartupInfo {
        let dirs = &self.inner.dirs;
        let profiles = tern_config::list_profiles(&dirs.config);
        let (global, issues) = match tern_config::load_global(&dirs.config) {
            Ok(g) => (g, Vec::new()),
            Err(e) => (GlobalConfig::default(), e.0.iter().map(|i| i.to_string()).collect()),
        };
        let auto_profile = match requested {
            Some(p) => Some(p.to_owned()),
            None if profiles.len() == 1 => profiles.first().cloned(),
            None if !global.ask_on_startup.0 => global.default_profile.clone().filter(|p| profiles.contains(p)),
            None => None,
        };
        StartupInfo { profiles, auto_profile, issues }
    }

    /// The profile used last (for preselection in the picker).
    pub fn last_profile(&self) -> Option<String> {
        std::fs::read_to_string(self.inner.dirs.state.join("last_profile")).ok().map(|s| s.trim().to_owned())
    }

    /// Lock and open a profile, start syncing its accounts.
    pub fn open_profile(&self, name: &str) -> Result<(), OpenError> {
        let inner = &self.inner;
        if !tern_config::list_profiles(&inner.dirs.config).iter().any(|p| p == name) {
            return Err(OpenError::NoSuchProfile(name.to_owned()));
        }
        let profile_lock = ProfileLock::acquire(&inner.dirs.lock_file(name)).map_err(|e| match e {
            LockError::Held => OpenError::AlreadyRunning(name.to_owned()),
            e => OpenError::Other(e.to_string()),
        })?;
        let _ = std::fs::create_dir_all(&inner.dirs.state);
        let _ = std::fs::write(inner.dirs.state.join("last_profile"), name);

        let (config, issues) = match tern_config::load_profile(&inner.dirs.config, name) {
            Ok(p) => (Some(p), Vec::new()),
            Err(e) => (None, issue_strings(&e)),
        };
        let global_issues =
            tern_config::load_global(&inner.dirs.config).err().map(|e| issue_strings(&e)).unwrap_or_default();
        let accounts = config.as_ref().map(|p| inner.start_accounts(p, &[])).unwrap_or_default();

        let weak = Arc::downgrade(inner);
        let watcher = ConfigWatcher::spawn(&inner.dirs.config, move || {
            if let Some(inner) = weak.upgrade() {
                inner.reload();
                inner.check_keys();
            }
        })
        .map_err(|e| warn!("config watcher: {e}"))
        .ok();

        let sink = inner.hub.sink.clone();
        let dbus = inner
            .rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(3), dbus::serve(name, sink)).await })
            .map_err(|_| "timeout".to_owned())
            .and_then(|r| r.map_err(|e| e.to_string()))
            .map_err(|e| warn!("D-Bus single-instance service unavailable: {e}"))
            .ok();
        let notifier = inner
            .rt
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(3), notify::Notifier::connect(inner.hub.sink.clone())).await
            })
            .map_err(|_| "timeout".to_owned())
            .and_then(|r| r.map_err(|e| e.to_string()))
            .map_err(|e| warn!("desktop notifications unavailable: {e}"))
            .ok();

        let state = ProfileState {
            name: name.to_owned(),
            config,
            accounts,
            issues,
            global_issues,
            pgp_issues: Vec::new(),
            avatars: Arc::new(avatar::Avatars::new(inner.dirs.profile_cache(name).join("avatars"))),
            notifier,
            _lock: profile_lock,
            _watcher: watcher,
            _dbus: dbus,
        };
        let issues = state.all_issues();
        *lock(&inner.profile) = Some(state);
        info!(profile = name, "profile opened");
        for i in &issues {
            warn!("configuration problem: {i}");
        }
        inner.hub.emit(Event::ConfigChanged { issues });
        inner.hub.emit(Event::FolderTreeChanged);
        inner.check_keys();
        Ok(())
    }

    /// Ask the process that has `profile` open to show its window.
    pub fn raise_existing(&self, profile: &str, activation_token: &str) -> bool {
        self.inner
            .rt
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(3), dbus::raise_existing(profile, activation_token)).await
            })
            .map(|r| r.is_ok())
            .unwrap_or(false)
    }

    pub fn profile_name(&self) -> Option<String> {
        lock(&self.inner.profile).as_ref().map(|p| p.name.clone())
    }

    pub fn config_issues(&self) -> Vec<String> {
        lock(&self.inner.profile).as_ref().map(ProfileState::all_issues).unwrap_or_default()
    }

    pub fn accounts(&self) -> Vec<AccountInfo> {
        let p = lock(&self.inner.profile);
        let Some(p) = p.as_ref() else { return Vec::new() };
        let Some(profile) = &p.config else { return Vec::new() };
        profile
            .accounts
            .iter()
            .map(|a| {
                let pgp = a.pgp(&profile.config);
                AccountInfo {
                    id: a.id.clone(),
                    name: a.config.name.clone(),
                    email: a.config.email.clone(),
                    sign_by_default: pgp.is_some_and(|p| p.sign_by_default),
                    encrypt_when_possible: pgp.is_some_and(|p| p.encrypt_when_possible),
                    has_pgp_key: pgp.is_some(),
                    can_archive: a.archive(&profile.config).is_some(),
                    compose_format: a.compose_format(&profile.config, &self.inner.global_config()).into(),
                }
            })
            .collect()
    }

    /// UI preferences from `tern.toml`.
    pub fn prefer_plain_text(&self) -> bool {
        self.inner.global.read().unwrap_or_else(|p| p.into_inner()).ui.prefer_plain_text
    }

    pub fn threaded_by_default(&self) -> bool {
        self.inner.global.read().unwrap_or_else(|p| p.into_inner()).ui.threaded
    }

    /// Message list columns and sort order (`[ui.message_list]`). Re-read
    /// on `Event::ConfigChanged`.
    pub fn list_layout(&self) -> ListLayout {
        let g = self.inner.global.read().unwrap_or_else(|p| p.into_inner());
        let l = &g.ui.message_list;
        ListLayout {
            columns: l.columns.iter().map(|&c| c.into()).collect(),
            sort_by: l.sort_by.into(),
            descending: l.sort_order == tern_config::SortOrder::Desc,
        }
    }

    // ------------------------------------------------------------ read models

    /// Accounts in the configured order (`account_order`).
    pub fn folder_tree(&self) -> Vec<FolderNode> {
        let mut out = Vec::new();
        for acc in self.inner.accounts() {
            let folders = acc.store.folders().unwrap_or_default();
            let sent = acc.sent_folder_name().ok().flatten();
            tree::append_account(&mut out, &acc.id, &acc.config.config.name, &folders, sent.as_deref());
        }
        out
    }

    /// Make `folder` the current message list. An empty `query` lists all
    /// messages, otherwise search results (flat). Rows are in the configured
    /// order. Returns the row count.
    pub fn open_list(&self, folder: FolderKey, threaded: bool, query: &str, filter: ListFilter) -> u32 {
        let mut l = lock(&self.inner.list);
        l.folder = Some(folder);
        l.threaded = threaded;
        l.query = query.trim().to_owned();
        l.filter = filter;
        l.sticky.clear();
        self.inner.recompute_list(&mut l);
        l.rows.len() as u32
    }

    /// Date sections of the current list (empty unless sorted by date and
    /// `group_by_date` is on). Re-read after `open_list` and `ListChanged`.
    pub fn list_groups(&self) -> Vec<ListGroup> {
        lock(&self.inner.list).groups.clone()
    }

    pub fn close_list(&self) {
        *lock(&self.inner.list) = ListState::default();
    }

    pub fn list_count(&self) -> u32 {
        lock(&self.inner.list).rows.len() as u32
    }

    /// A window of the current list.
    pub fn list_rows(&self, offset: u32, count: u32) -> Vec<MessageRow> {
        let (account, window) = {
            let l = lock(&self.inner.list);
            let Some(folder) = &l.folder else { return Vec::new() };
            let start = (offset as usize).min(l.rows.len());
            let end = (start + count as usize).min(l.rows.len());
            (folder.account.clone(), l.rows[start..end].to_vec())
        };
        let Some(acc) = self.inner.account(&account) else { return Vec::new() };
        window
            .iter()
            .filter_map(|r| {
                let m = acc.store.message(r.id).ok().flatten()?;
                let e = &m.envelope;
                Some(MessageRow {
                    key: MessageKey { account: account.clone(), id: m.id },
                    depth: r.depth,
                    thread_size: r.thread_size,
                    date: e.date,
                    from: e.from.first().map(|a| a.short().to_owned()).unwrap_or_default(),
                    to: e.to.first().map(|a| a.short().to_owned()).unwrap_or_default(),
                    subject: e.subject.clone(),
                    unread: !m.flags.contains(Flags::SEEN),
                    flagged: m.flags.contains(Flags::FLAGGED),
                    answered: m.flags.contains(Flags::ANSWERED),
                    forwarded: m.flags.contains(Flags::FORWARDED),
                    has_attachments: e.has_attachments,
                    encrypted: e.encrypted,
                    size: m.size,
                })
            })
            .collect()
    }

    /// Position of a message in the current list (to keep the selection
    /// across refreshes).
    pub fn list_index_of(&self, key: &MessageKey) -> Option<u32> {
        let l = lock(&self.inner.list);
        if l.folder.as_ref().is_none_or(|f| f.account != key.account) {
            return None;
        }
        l.rows.iter().position(|r| r.id == key.id).map(|i| i as u32)
    }

    // ------------------------------------------------------------- messages

    /// Render a message; the result arrives as `Event::MessageLoaded`.
    pub fn open_message(&self, key: MessageKey, allow_remote: bool) {
        *lock(&self.inner.current) = Some(key.clone());
        if allow_remote {
            lock(&self.inner.remote_allowed).insert(key.clone());
        }
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move { inner.load_message(key).await });
    }

    /// Serve a `tern-msg:` URL: (MIME type, bytes).
    pub fn resource(&self, url: &str) -> Option<(String, Vec<u8>)> {
        let path = url.strip_prefix(URL_SCHEME)?.strip_prefix(':')?.trim_start_matches('/');
        let path = path.split(['?', '#']).next().unwrap_or(path);
        let mut it = path.splitn(4, '/');
        let account = it.next()?;
        let id: i64 = it.next()?.parse().ok()?;
        let kind = it.next()?;
        let rest = it.next();
        let r = self.inner.cached(&MessageKey { account: account.to_owned(), id })?;
        let html = "text/html; charset=utf-8".to_owned();
        match (kind, rest) {
            ("html", _) => r.html_doc.clone().map(|d| (html, d.into_bytes())),
            ("text", _) => Some((html, r.text_doc.clone().into_bytes())),
            ("cid", Some(cid)) => r.cids.get(&render::percent_decode(cid)).map(|(t, d)| (t.clone(), d.clone())),
            _ => None,
        }
    }

    /// An attachment of a loaded message: (file name, bytes).
    pub fn attachment(&self, key: &MessageKey, index: u32) -> Option<(String, Vec<u8>)> {
        let r = self.inner.cached(key)?;
        r.attachments.iter().find(|(a, _)| a.index == index).map(|(a, d)| (a.filename.clone(), d.clone()))
    }

    /// The message exactly as stored (as received from the server), with a
    /// display version for "View Source". `None` if it isn't downloaded yet
    /// (the download is then requested).
    pub fn message_source(&self, key: &MessageKey) -> Option<MessageSource> {
        let acc = self.inner.account(&key.account)?;
        let m = acc.store.message(key.id).ok()??;
        let Some(blob) = m.blob.as_deref() else {
            acc.request(Request::Body(key.id));
            return None;
        };
        let raw = acc.blobs.get(blob).ok()?;
        let (text, lines) = source::classify(&raw);
        Some(MessageSource { file_name: source::file_name(&m.envelope.subject), raw, text, lines })
    }

    pub fn mark_read(&self, keys: &[MessageKey], read: bool) {
        let (add, remove) = if read { (Flags::SEEN, Flags::empty()) } else { (Flags::empty(), Flags::SEEN) };
        self.inner.change(keys, |acc, ids| ops::set_flags(&acc.store, ids, add, remove));
    }

    pub fn mark_flagged(&self, keys: &[MessageKey], flagged: bool) {
        let (add, remove) = if flagged { (Flags::FLAGGED, Flags::empty()) } else { (Flags::empty(), Flags::FLAGGED) };
        self.inner.change(keys, |acc, ids| ops::set_flags(&acc.store, ids, add, remove));
    }

    /// Move messages to a folder of the same account.
    pub fn move_messages(&self, keys: &[MessageKey], target: &FolderKey) {
        if keys.iter().any(|k| k.account != target.account) {
            self.inner.hub.error("Moving messages between accounts is not supported".into());
            return;
        }
        self.inner.change(keys, |acc, ids| ops::move_messages(&acc.store, ids, target.folder));
    }

    /// Move to Trash, or delete permanently from Trash.
    pub fn delete_messages(&self, keys: &[MessageKey]) {
        self.inner.change(keys, |acc, ids| ops::delete(&acc.store, ids));
    }

    /// Archive: move to the account's Archive folder, if it has one.
    /// Archive: move each message to the account's `archive` folder pattern
    /// (e.g. `Archive/{year}` with the message's year), creating folders as
    /// needed. Accounts without a pattern are skipped.
    pub fn archive_messages(&self, keys: &[MessageKey]) {
        for (account, group) in group_by_account(keys) {
            let Some(acc) = self.inner.account(&account) else { continue };
            let pattern = {
                let p = lock(&self.inner.profile);
                p.as_ref()
                    .and_then(|p| p.config.as_ref())
                    .and_then(|prof| prof.account(&account).and_then(|a| a.archive(&prof.config)).map(str::to_owned))
            };
            let Some(pattern) = pattern else {
                self.inner.hub.error(format!("{}: no archive folder configured", acc.config.config.name));
                continue;
            };
            if let Err(e) = self.inner.archive(&acc, &group, &pattern) {
                self.inner.hub.error(format!("Archiving failed: {e}"));
            }
        }
    }

    /// Sync all accounts now.
    pub fn sync_now(&self) {
        for a in self.inner.accounts() {
            a.request(Request::Sync);
        }
    }

    // -------------------------------------------------------------- compose

    /// Prepare a reply/forward; the draft arrives as `Event::ComposeReady`.
    pub fn prepare_reply(&self, key: MessageKey, mode: ReplyMode) {
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move {
            let rendered = match inner.cached(&key) {
                Some(r) => Some(r),
                None => inner.render(&key).await,
            };
            let Some(r) = rendered else {
                inner.hub.error("The message body is not downloaded yet".into());
                return;
            };
            let Some(acc) = inner.account(&key.account) else { return };
            let (fmt, sig) = inner.compose_settings(&key.account);
            let mut draft = compose::reply_template(&key, &r, mode, &acc.config.config.email, fmt, sig.as_ref());
            if inner.apply_pgp_defaults(&mut draft, r.encrypted) && !draft.encrypt {
                draft.encrypt = inner.recipient_keys(&draft.to, &draft.cc, &draft.bcc).await.all_have_keys();
            }
            inner.hub.emit(Event::ComposeReady(draft));
        });
    }

    /// An empty draft for an account: its editor format, signature and PGP
    /// defaults. It has no recipients yet, so it isn't encrypted; the
    /// composer follows `encrypt_when_possible` with [`Self::recipient_keys`]
    /// as recipients are entered.
    pub fn new_draft(&self, account: &str) -> Draft {
        let (format, sig) = self.inner.compose_settings(account);
        let mut d = Draft {
            account: account.to_owned(),
            body: compose::new_body(format, sig.as_ref()),
            format,
            ..Default::default()
        };
        self.inner.apply_pgp_defaults(&mut d, false);
        d
    }

    /// Which recipients in the address fields (free-form, as typed) have an
    /// encryption key in the local keyring. Quick (one local gpg call), for
    /// the composer to follow `encrypt_when_possible` while the user types.
    pub fn recipient_keys(&self, to: &str, cc: &str, bcc: &str) -> RecipientKeys {
        self.inner.rt.block_on(self.inner.recipient_keys(to, cc, bcc))
    }

    /// Convert a draft body when the user switches editor mode.
    pub fn convert_body(&self, body: &str, from: BodyFormat, to: BodyFormat) -> String {
        format::convert(body, from, to)
    }

    /// HTML document previewing a Markdown body as it will be sent.
    pub fn markdown_preview(&self, markdown: &str) -> String {
        format::markdown_preview(markdown)
    }

    /// Build the message (sign/encrypt), put it in the outbox and send it in
    /// the background. Returns a request id: `Event::DraftQueued` reports
    /// whether the message could be built and queued, `Event::SendResult`
    /// later reports the SMTP outcome.
    pub fn send(&self, draft: Draft) -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let request = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let inner = self.inner.clone();
        self.inner.rt.spawn(async move {
            let error = inner.send(draft).await.err().unwrap_or_default();
            inner.hub.emit(Event::DraftQueued { request, error });
        });
        request
    }

    /// Stop all background work and release the profile.
    pub fn close_profile(&self) {
        let state = lock(&self.inner.profile).take();
        drop(state);
        *lock(&self.inner.list) = ListState::default();
        lock(&self.inner.cache).clear();
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.close_profile();
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

/// Does the folder hold outgoing mail (Sent, Drafts or the configured Sent
/// folder)? Same rule as the `sent`/`drafts` roles in the folder tree.
fn person(a: &tern_core::Address) -> Person {
    Person { name: a.name.clone().unwrap_or_default(), email: a.email.clone() }
}

fn is_outgoing(acc: &AccountRt, folder: i64) -> bool {
    let Ok(Some(f)) = acc.store.folder(folder) else { return false };
    matches!(f.role, Some(tern_core::FolderRole::Sent | tern_core::FolderRole::Drafts))
        || acc.sent_folder_name().ok().flatten().as_deref() == Some(f.name.as_str())
}

fn group_by_account(keys: &[MessageKey]) -> BTreeMap<String, Vec<MessageKey>> {
    let mut map: BTreeMap<String, Vec<MessageKey>> = BTreeMap::new();
    for k in keys {
        map.entry(k.account.clone()).or_default().push(k.clone());
    }
    map
}

impl Inner {
    fn accounts(&self) -> Vec<Arc<AccountRt>> {
        lock(&self.profile).as_ref().map(|p| p.accounts.clone()).unwrap_or_default()
    }

    fn account(&self, id: &str) -> Option<Arc<AccountRt>> {
        self.accounts().into_iter().find(|a| a.id == id)
    }

    fn gpg(&self) -> Gpg {
        let g = self.global.read().unwrap_or_else(|p| p.into_inner());
        Gpg::new(g.gpg.program.clone()).with_wkd(g.gpg.wkd_lookup)
    }

    /// Start runtimes for `profile`'s accounts, reusing `keep` entries whose
    /// configuration is unchanged.
    fn start_accounts(&self, profile: &Profile, keep: &[Arc<AccountRt>]) -> Vec<Arc<AccountRt>> {
        let data = self.dirs.profile_data(&profile.name);
        let accounts: Vec<Arc<AccountRt>> = profile
            .accounts
            .iter()
            .filter_map(|a| {
                if let Some(existing) = keep.iter().find(|k| k.id == a.id && k.config == *a) {
                    return Some(existing.clone());
                }
                match account::start(&data, a.clone(), &profile.config, self.hub.clone(), &self.rt) {
                    Ok(rt) => Some(rt),
                    Err(e) => {
                        self.hub.error(format!("{}: cannot open local store: {e}", a.config.name));
                        None
                    }
                }
            })
            .collect();
        self.apply_inherited_settings(profile, &accounts);
        accounts
    }

    /// Check every account's `pgp.key` against the gpg keyring (exists,
    /// valid, belongs to the account's address, secret keys reachable) and
    /// report problems with the configuration problems. Runs in the
    /// background: gpg may be slow, and the profile must open regardless.
    fn check_keys(self: &Arc<Self>) {
        let checks: Vec<(String, String, String, bool)> = {
            let p = lock(&self.profile);
            let Some(prof) = p.as_ref().and_then(|s| s.config.as_ref()) else { return };
            let dir = self.dirs.config.join("profiles").join(&prof.name);
            prof.accounts
                .iter()
                .filter_map(|a| {
                    let pgp = a.pgp(&prof.config)?;
                    // The file the key is configured in.
                    let file = if a.config.pgp.is_some() {
                        dir.join("accounts").join(format!("{}.toml", a.id))
                    } else {
                        dir.join("profile.toml")
                    };
                    let label = format!("{}: pgp.key {} ({})", file.display(), pgp.key, a.config.email);
                    Some((label, pgp.key.clone(), a.config.email.clone(), pgp.sign_by_default))
                })
                .collect()
        };
        let inner = self.clone();
        self.rt.spawn(async move {
            let gpg = inner.gpg();
            let mut found = Vec::new();
            for (label, key, email, sign) in checks {
                match gpg.check_own_key(&key, &email, sign).await {
                    Ok(problems) => found.extend(problems.into_iter().map(|p| format!("{label}: {p}"))),
                    Err(e) => {
                        // gpg can't run at all: one message is enough.
                        found.push(format!("gpg.program: {e}"));
                        break;
                    }
                }
            }
            for f in &found {
                warn!("PGP key problem: {f}");
            }
            let issues = {
                let mut p = lock(&inner.profile);
                let Some(state) = p.as_mut() else { return };
                if state.pgp_issues == found {
                    return;
                }
                state.pgp_issues = found;
                state.all_issues()
            };
            inner.hub.emit(Event::ConfigChanged { issues });
        });
    }

    /// Settings an account inherits from profile.toml or tern.toml, which
    /// can change without the account being restarted.
    fn apply_inherited_settings(&self, profile: &Profile, accounts: &[Arc<AccountRt>]) {
        let global = self.global_config();
        for acc in accounts {
            let sent = profile.account(&acc.id).and_then(|a| a.sent_folder(&profile.config, &global));
            *lock(&acc.sent_folder) = sent.map(str::to_owned);
        }
    }

    /// Hot reload: keep the last valid config on errors (§5).
    fn reload(&self) {
        let (global_changed, global_issues) = match tern_config::load_global(&self.dirs.config) {
            Ok(g) => {
                let mut current = self.global.write().unwrap_or_else(|p| p.into_inner());
                let changed = *current != g;
                *current = g;
                (changed, Vec::new())
            }
            Err(e) => {
                warn!("tern.toml has errors, keeping the previous version");
                let issues = issue_strings(&e);
                for i in &issues {
                    warn!("configuration problem: {i}");
                }
                (false, issues)
            }
        };
        let mut guard = lock(&self.profile);
        let Some(state) = guard.as_mut() else { return };
        let mut changed = global_changed || state.global_issues != global_issues;
        state.global_issues = global_issues;
        match tern_config::load_profile(&self.dirs.config, &state.name) {
            Ok(profile) if state.config.as_ref() != Some(&profile) || !state.issues.is_empty() => {
                // Uses the new tern.toml too.
                let new = self.start_accounts(&profile, &state.accounts);
                for old in &state.accounts {
                    if !new.iter().any(|n| Arc::ptr_eq(n, old)) {
                        old.stop();
                    }
                }
                state.accounts = new;
                state.config = Some(profile);
                state.issues.clear();
                info!("configuration reloaded");
                changed = true;
            }
            Ok(_) => {}
            Err(e) => {
                state.issues = issue_strings(&e);
                warn!("configuration has errors, keeping the previous one");
                for i in &state.issues {
                    warn!("configuration problem: {i}");
                }
                changed = true;
            }
        }
        if !changed {
            return;
        }
        if global_changed && let Some(profile) = &state.config {
            self.apply_inherited_settings(profile, &state.accounts);
        }
        let issues = state.all_issues();
        drop(guard);
        self.hub.emit(Event::ConfigChanged { issues });
        self.hub.emit(Event::FolderTreeChanged);
    }

    fn recompute_list(&self, l: &mut ListState) {
        let Some(folder) = &l.folder else {
            l.rows.clear();
            l.groups.clear();
            return;
        };
        let Some(acc) = self.account(&folder.account) else {
            l.rows.clear();
            l.groups.clear();
            return;
        };
        let (field, order, group_by_date) = {
            let g = self.global.read().unwrap_or_else(|p| p.into_inner());
            let m = &g.ui.message_list;
            (m.sort_by, m.sort_order, m.group_by_date)
        };
        let filtered = l.filter.any();
        // Sort fields (incl. flags) are needed to re-sort or to filter.
        let inputs = (!sort::is_default(field, order) || filtered)
            .then(|| acc.store.sort_inputs(folder.folder).unwrap_or_default());
        let outgoing = is_outgoing(&acc, folder.folder);
        // Store and threading give newest first; other orders re-sort.
        let custom = inputs.as_ref().filter(|_| !sort::is_default(field, order));
        let flat = |ids: Vec<i64>| -> Vec<ThreadRow> {
            let ids = match custom {
                Some(inputs) => sort::sort_flat(ids, inputs, field, order, outgoing),
                None => ids,
            };
            ids.into_iter().map(|id| ThreadRow { id, depth: 0, thread_size: 0 }).collect()
        };
        let threaded = l.threaded && l.query.is_empty() && !filtered;
        l.rows = if filtered {
            let ids = if l.query.is_empty() {
                acc.store.ids_by_date(folder.folder).unwrap_or_default()
            } else {
                acc.store.search(Some(folder.folder), &l.query, 5000).unwrap_or_default()
            };
            let by_id: HashMap<i64, &tern_core::store::SortInput> =
                inputs.iter().flatten().map(|m| (m.id, m)).collect();
            let f = l.filter;
            let matches = |m: &tern_core::store::SortInput| {
                (!f.unread || !m.flags.contains(Flags::SEEN))
                    && (!f.flagged || m.flags.contains(Flags::FLAGGED))
                    && (!f.attachments || m.has_attachments)
            };
            let sticky = &l.sticky;
            flat(
                ids.into_iter().filter(|id| sticky.contains(id) || by_id.get(id).is_some_and(|m| matches(m))).collect(),
            )
        } else if !l.query.is_empty() {
            flat(acc.store.search(Some(folder.folder), &l.query, 5000).unwrap_or_default())
        } else if threaded {
            let rows = tern_core::thread::thread(&acc.store.thread_inputs(folder.folder).unwrap_or_default());
            match custom {
                Some(inputs) => sort::sort_threads(rows, inputs, field, order, outgoing),
                None => rows,
            }
        } else {
            flat(acc.store.ids_by_date(folder.folder).unwrap_or_default())
        };
        if filtered {
            l.sticky = l.rows.iter().map(|r| r.id).collect();
        }
        l.groups = if group_by_date && field == tern_config::ListField::Date {
            let dates = acc.store.dates(folder.folder).unwrap_or_default();
            groups::date_groups(&l.rows, &dates, threaded, chrono::Local::now().date_naive())
        } else {
            Vec::new()
        };
    }

    /// Apply a local change grouped by account, then refresh and queue the
    /// server replay.
    fn change(&self, keys: &[MessageKey], f: impl Fn(&AccountRt, &[i64]) -> tern_core::Result<()>) {
        for (account, group) in group_by_account(keys) {
            let Some(acc) = self.account(&account) else { continue };
            let ids: Vec<i64> = group.iter().map(|k| k.id).collect();
            let folders: HashSet<i64> =
                acc.store.messages(&ids).unwrap_or_default().iter().map(|m| m.folder_id).collect();
            match f(&acc, &ids) {
                Ok(()) => {
                    for folder in folders {
                        self.hub.folder_dirty(&account, folder);
                    }
                    // Moves change the target folder too.
                    for m in acc.store.messages(&ids).unwrap_or_default() {
                        self.hub.folder_dirty(&account, m.folder_id);
                    }
                    acc.request(Request::Replay);
                }
                Err(e) => self.hub.error(e.to_string()),
            }
        }
    }

    fn cached(&self, key: &MessageKey) -> Option<Arc<Rendered>> {
        lock(&self.cache).get(key)
    }

    /// Render from the local store (None if the body isn't downloaded).
    async fn render(&self, key: &MessageKey) -> Option<Arc<Rendered>> {
        let acc = self.account(&key.account)?;
        let m = acc.store.message(key.id).ok()??;
        let raw = acc.blobs.get(m.blob.as_deref()?).ok()?;
        let sender = m.envelope.from.first().map(|a| a.email.clone()).unwrap_or_default();
        let allow_remote = lock(&self.remote_allowed).contains(key)
            || lock(&self.profile)
                .as_ref()
                .and_then(|p| p.config.as_ref())
                .is_some_and(|p| p.config.remote_content.allows(&sender));
        let gpg = self.gpg();
        let opts =
            RenderOptions { gpg: &gpg, allow_remote, url_base: format!("{URL_SCHEME}:/{}/{}", key.account, key.id) };
        let r = Arc::new(render::render(&raw, &opts).await);
        let limit = self.global_config().memory.message_cache_mb as usize * 1024 * 1024;
        let current = lock(&self.current).clone();
        let mut cache = lock(&self.cache);
        cache.insert(key.clone(), r.clone(), limit, current.as_ref());
        tracing::debug!(entries = cache.len(), bytes = cache.bytes(), "render cache");
        Some(r)
    }

    async fn load_message(self: &Arc<Self>, key: MessageKey) {
        let Some(acc) = self.account(&key.account) else { return };
        let Some(m) = acc.store.message(key.id).ok().flatten() else {
            self.hub.error("Message not found".into());
            return;
        };
        let base = format!("{URL_SCHEME}:/{}/{}", key.account, key.id);
        if m.blob.is_none() {
            acc.request(Request::Body(key.id));
            let e = &m.envelope;
            let list = |l: &[tern_core::Address]| l.iter().map(|a| a.display()).collect::<Vec<_>>().join(", ");
            self.hub.emit(Event::MessageLoaded(MessageView {
                key,
                subject: e.subject.clone(),
                from: list(&e.from),
                to: list(&e.to),
                cc: list(&e.cc),
                date: e.date,
                url: String::new(),
                text_url: String::new(),
                text: String::new(),
                has_html: false,
                has_remote_content: false,
                remote_allowed: false,
                attachments: Vec::new(),
                encrypted: e.encrypted,
                decryption_failed: false,
                signature: SignatureState::None,
                signature_text: String::new(),
                headers: Vec::new(),
                sender: e.from.first().map(person).unwrap_or_default(),
                to_people: e.to.iter().map(person).collect(),
                cc_people: e.cc.iter().map(person).collect(),
                // Not rendered yet, so remote content isn't known to be allowed:
                // only a cached picture.
                avatar: self.sender_avatar(e.from.first().map(|a| a.email.as_str()).unwrap_or_default(), false),
                body_missing: true,
            }));
            return;
        }
        let Some(r) = self.render(&key).await else { return };
        // The user may have moved on while gpg was running.
        if lock(&self.current).as_ref() != Some(&key) {
            return;
        }
        let has_html = r.html_doc.is_some();
        self.hub.emit(Event::MessageLoaded(MessageView {
            key,
            subject: r.subject.clone(),
            from: r.from.clone(),
            to: r.to.clone(),
            cc: r.cc.clone(),
            date: r.date,
            // The frontend applies `prefer_plain_text` (switching to `text_url`),
            // so HTML stays one click away.
            url: if has_html { format!("{base}/html") } else { format!("{base}/text") },
            text_url: format!("{base}/text"),
            text: r.text.clone(),
            has_html,
            has_remote_content: r.has_remote_content,
            remote_allowed: r.remote_allowed,
            attachments: r.attachments.iter().map(|(a, _)| a.clone()).collect(),
            encrypted: r.encrypted,
            decryption_failed: r.decryption_failed,
            signature: r.signature,
            signature_text: r.signature_text.clone(),
            headers: r
                .headers
                .iter()
                .map(|(name, value)| HeaderField { name: name.clone(), value: value.clone() })
                .collect(),
            sender: r.sender.clone(),
            to_people: r.to_people.clone(),
            cc_people: r.cc_people.clone(),
            avatar: self.sender_avatar(&r.sender.email, r.remote_allowed),
            body_missing: false,
        }));
    }

    /// Notify about arrivals, per account: unread messages in the folders
    /// its `[notifications]` settings watch. One notification per account,
    /// and at most one sound per batch.
    async fn notify_new_mail(&self, arrivals: BTreeMap<(String, i64), Vec<i64>>) {
        let global = self.global_config();
        let (settings, names, notifier) = {
            let p = lock(&self.profile);
            let Some(state) = p.as_ref() else { return };
            let Some(prof) = &state.config else { return };
            let settings: HashMap<String, tern_config::Notifications> =
                prof.accounts.iter().map(|a| (a.id.clone(), a.notifications(&prof.config, &global))).collect();
            // Name the account only when there is more than one.
            let names: HashMap<String, String> = if prof.accounts.len() > 1 {
                prof.accounts.iter().map(|a| (a.id.clone(), a.config.name.clone())).collect()
            } else {
                HashMap::new()
            };
            (settings, names, state.notifier.clone())
        };

        let mut per_account: BTreeMap<String, Vec<(tern_core::MessageSummary, i64)>> = BTreeMap::new();
        for ((account, folder), ids) in arrivals {
            let Some(n) = settings.get(&account) else { continue };
            if !n.enabled && !n.sound {
                continue;
            }
            let Some(acc) = self.account(&account) else { continue };
            let Ok(Some(f)) = acc.store.folder(folder) else { continue };
            if !n.watches(&f.name, f.delimiter.as_deref().unwrap_or("/")) {
                continue;
            }
            let unread = acc
                .store
                .messages(&ids)
                .unwrap_or_default()
                .into_iter()
                .filter(|m| !m.flags.contains(Flags::SEEN) && !m.flags.contains(Flags::DELETED));
            per_account.entry(account).or_default().extend(unread.map(|m| (m, folder)));
        }

        let mut sound = false;
        for (account, mut msgs) in per_account {
            if msgs.is_empty() {
                continue;
            }
            let n = &settings[&account];
            sound |= n.sound;
            let Some(notifier) = notifier.as_ref().filter(|_| n.enabled) else { continue };
            msgs.sort_by_key(|(m, _)| std::cmp::Reverse(m.envelope.date));
            let arrivals: Vec<notify::Arrival> = msgs
                .iter()
                .map(|(m, _)| notify::Arrival {
                    sender: m.envelope.from.first().map(|a| a.short().to_owned()).unwrap_or_default(),
                    subject: m.envelope.subject.clone(),
                })
                .collect();
            let (summary, lines) = notify::compose(&arrivals, names.get(&account).map(String::as_str));
            // Clicking shows the newest message.
            let (newest, folder) = &msgs[0];
            let target = (
                MessageKey { account: account.clone(), id: newest.id },
                FolderKey { account: account.clone(), folder: *folder },
            );
            notifier.show(&summary, &lines, target).await;
        }
        if sound {
            self.hub.emit(Event::PlaySound { sound: notify::NEW_MAIL_SOUND.to_owned() });
        }
    }

    /// The cached picture of `email` (empty if none or `[avatars]` is off),
    /// and a background lookup when the policy allows it and the cache is
    /// stale: `all`, or `trusted` and this message may load remote content.
    /// A found picture arrives as `Event::AvatarReady`.
    fn sender_avatar(self: &Arc<Self>, email: &str, remote_allowed: bool) -> Vec<u8> {
        let cfg = self.global_config().avatars;
        if cfg.lookup == AvatarLookup::Off || email.is_empty() {
            return Vec::new();
        }
        let Some(avatars) = lock(&self.profile).as_ref().map(|p| p.avatars.clone()) else { return Vec::new() };
        let cached = avatars.cached(email);
        let may_look_up = cfg.lookup == AvatarLookup::All || (cfg.lookup == AvatarLookup::Trusted && remote_allowed);
        if cached.stale && may_look_up && avatars.begin(email) {
            let inner = self.clone();
            let email = email.to_owned();
            // Blocking HTTP: off the async workers.
            self.rt.spawn_blocking(move || {
                if let avatar::Lookup::Found(image) = avatars.lookup(&email, &cfg.sources) {
                    inner.hub.emit(Event::AvatarReady { email, image });
                }
            });
        }
        cached.image.unwrap_or_default()
    }

    fn global_config(&self) -> GlobalConfig {
        self.global.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Editor format and signature for an account. A signature that can't be
    /// read is reported and left out.
    fn compose_settings(&self, account: &str) -> (BodyFormat, Option<format::Signature>) {
        let global = self.global_config();
        let (fmt, sig_path) = {
            let p = lock(&self.profile);
            let Some(prof) = p.as_ref().and_then(|p| p.config.as_ref()) else {
                return (BodyFormat::Plain, None);
            };
            let Some(a) = prof.account(account) else { return (BodyFormat::Plain, None) };
            (a.compose_format(&prof.config, &global), a.signature(&prof.config, &self.dirs.config))
        };
        let sig = sig_path.and_then(|path| match format::load_signature(&path) {
            Ok(s) => Some(s),
            Err(e) => {
                self.hub.error(format!("Signature not added: {e}"));
                None
            }
        });
        (fmt.into(), sig)
    }

    /// Move messages into folders expanded from an archive pattern.
    fn archive(&self, acc: &AccountRt, keys: &[MessageKey], pattern: &str) -> tern_core::Result<()> {
        use chrono::Datelike;
        let folders = acc.store.folders()?;
        let delimiter = folders.iter().find_map(|f| f.delimiter.clone()).unwrap_or_else(|| "/".into());
        let ids: Vec<i64> = keys.iter().map(|k| k.id).collect();
        let mut targets: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        for m in acc.store.messages(&ids)? {
            let date =
                chrono::DateTime::from_timestamp(m.envelope.date, 0).unwrap_or_default().with_timezone(&chrono::Local);
            let name = pattern
                .replace("{year}", &format!("{:04}", date.year()))
                .replace("{month}", &format!("{:02}", date.month()))
                .replace('/', &delimiter);
            targets.entry(name).or_default().push(m.id);
        }
        let sources: HashSet<i64> = acc.store.messages(&ids)?.iter().map(|m| m.folder_id).collect();
        for (name, ids) in targets {
            let folder = ops::ensure_folder(&acc.store, &name, Some(&delimiter))?;
            ops::move_messages(&acc.store, &ids, folder)?;
            self.hub.folder_dirty(&acc.id, folder);
        }
        for f in sources {
            self.hub.folder_dirty(&acc.id, f);
        }
        acc.request(Request::Replay);
        Ok(())
    }

    /// Sign and encrypt defaults that don't depend on the recipients.
    /// Returns whether the account wants encryption when every recipient
    /// has a key (`encrypt_when_possible`): the caller decides that once
    /// the recipients are known (see [`Self::recipient_keys`]).
    fn apply_pgp_defaults(&self, d: &mut Draft, replying_to_encrypted: bool) -> bool {
        // Replies to encrypted mail stay encrypted, configured or not: the
        // quoted text was decrypted with the user's keyring.
        d.encryption_required = replying_to_encrypted;
        d.encrypt = replying_to_encrypted;
        let p = lock(&self.profile);
        let Some(profile) = p.as_ref().and_then(|p| p.config.as_ref()) else { return false };
        let Some(acc) = profile.account(&d.account) else { return false };
        let Some(pgp) = acc.pgp(&profile.config) else { return false };
        d.sign = pgp.sign_by_default;
        pgp.encrypt_when_possible
    }

    /// Which recipients of the address fields have a key in the local
    /// keyring (no network lookup).
    async fn recipient_keys(&self, to: &str, cc: &str, bcc: &str) -> RecipientKeys {
        let mut emails: Vec<String> = Vec::new();
        for field in [to, cc, bcc] {
            match compose::parse_addresses(field) {
                Ok(list) => emails.extend(list.into_iter().map(|a| a.email.to_ascii_lowercase())),
                Err(_) => return RecipientKeys::default(),
            }
        }
        emails.sort();
        emails.dedup();
        match self.gpg().without_local_key(&emails).await {
            Ok(missing) => RecipientKeys { valid: true, recipients: emails.len() as u32, missing },
            Err(e) => {
                self.hub.error(format!("Cannot check recipient keys: {e}"));
                RecipientKeys::default()
            }
        }
    }

    async fn send(&self, draft: Draft) -> Result<(), String> {
        let (acc, from, key) = {
            let p = lock(&self.profile);
            let profile = p.as_ref().and_then(|p| p.config.as_ref()).ok_or("no profile open")?;
            let a = profile.account(&draft.account).ok_or("unknown account")?;
            let from = tern_core::Address {
                name: a.display_name(&profile.config).map(str::to_owned),
                email: a.config.email.clone(),
            };
            let key = a.pgp(&profile.config).map(|p| p.key.clone());
            (a.id.clone(), from, key)
        };
        // Not inside the block above: `account()` takes the profile lock too.
        let acc = self.account(&acc).ok_or("account not running")?;
        let forwarded = match &draft.forward_message {
            Some(k) => {
                let m = acc.store.message(k.id).map_err(|e| e.to_string())?.ok_or("forwarded message not found")?;
                let blob = m.blob.ok_or("the forwarded message is not downloaded yet")?;
                Some(acc.blobs.get(&blob).map_err(|e| e.to_string())?)
            }
            None => None,
        };
        let gpg = self.gpg();
        let sender = compose::Sender { from, pgp_key: key.as_deref() };
        let built = compose::build(&draft, &sender, &gpg, forwarded).await?;
        let meta = tern_smtp::OutboxMeta {
            from: built.from,
            recipients: built.recipients,
            subject: built.subject,
            queued_at: chrono::Utc::now().timestamp(),
            attempts: 0,
            last_error: None,
            failed: false,
            save_to_sent: acc.config.config.smtp.save_to_sent,
            reply_to_message: draft.reply_to_message.as_ref().filter(|k| k.account == acc.id).map(|k| k.id),
            forwarded_message: draft.forward_message.as_ref().filter(|k| k.account == acc.id).map(|k| k.id),
        };
        acc.outbox.enqueue(&built.raw, &meta).map_err(|e| format!("cannot write to the outbox: {e}"))?;
        acc.request(Request::Outbox);
        Ok(())
    }
}

/// Coalesce hub notifications into frontend events.
async fn ticker(weak: Weak<Inner>) {
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    loop {
        interval.tick().await;
        let Some(inner) = weak.upgrade() else { return };
        let dirty = inner.hub.take();
        if dirty.tree {
            inner.hub.emit(Event::FolderTreeChanged);
        }
        let changed_count = {
            let mut l = lock(&inner.list);
            let hit = l.folder.as_ref().is_some_and(|f| dirty.folders.contains(&(f.account.clone(), f.folder)));
            if hit {
                inner.recompute_list(&mut l);
                Some(l.rows.len() as u32)
            } else {
                None
            }
        };
        if let Some(count) = changed_count {
            inner.hub.emit(Event::ListChanged { count });
        }
        for ((account, folder), (done, total)) in dirty.progress {
            inner.hub.emit(Event::Progress { account, folder, done, total });
        }
        if !dirty.new_mail.is_empty() {
            let inner = inner.clone();
            tokio::spawn(async move { inner.notify_new_mail(dirty.new_mail).await });
        }
        let current = lock(&inner.current).clone();
        for key in dirty.bodies {
            if current.as_ref() == Some(&key) {
                // Not awaited: gpg may wait for a passphrase, and the ticker
                // must keep delivering list updates meanwhile.
                let inner = inner.clone();
                tokio::spawn(async move { inner.load_message(key).await });
            }
        }
    }
}

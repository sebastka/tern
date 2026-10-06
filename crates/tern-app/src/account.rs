//! Per-account runtime: local store, outbox and the background tasks that
//! sync with the server (ARCHITECTURE.md §9).
//!
//! * One **worker** task owns the main IMAP connection and serializes all
//!   server work: replaying queued ops, syncing folders, downloading bodies,
//!   sending the outbox. It runs on requests and on the poll interval.
//! * One **IDLE** task per watched folder (INBOX plus `sync.idle_folders`)
//!   holds its own connection and only wakes the worker.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tern_config::{Account, ProfileConfig, TlsMode};
use tern_core::backend::{BackendError, MailBackend, WaitOutcome};
use tern_core::blobs::BlobStore;
use tern_core::store::Store;
use tern_core::sync::{SyncContext, SyncEvent, SyncPhase};
use tern_core::{Flags, FolderRole, MessageId, ops};
use tern_imap::{ImapBackend, ImapSettings, Security};
use tern_smtp::{Outbox, SendError};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::hub::Hub;
use crate::types::{AccountState, MessageKey};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Request {
    /// Full sync: replay, folder list, all folders, bodies, outbox.
    Sync,
    /// Replay queued offline ops soon.
    Replay,
    /// Sync one folder (by server name), e.g. after IDLE reported changes.
    Folder(String),
    /// Download one message body now (the user opened it).
    Body(MessageId),
    /// Send queued outgoing mail.
    Outbox,
}

pub struct AccountRt {
    pub id: String,
    pub config: Account,
    pub store: Arc<Store>,
    pub blobs: Arc<BlobStore>,
    pub outbox: Outbox,
    /// Configured Sent folder (`sent_folder`, `/`-separated), resolved over
    /// account, profile and tern.toml. Kept current on config reloads.
    pub sent_folder: Mutex<Option<String>>,
    tx: mpsc::UnboundedSender<Request>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl AccountRt {
    pub fn request(&self, r: Request) {
        let _ = self.tx.send(r);
    }

    /// Server name of the configured Sent folder (levels joined with the
    /// server's delimiter), if one is configured.
    pub fn sent_folder_name(&self) -> tern_core::Result<Option<String>> {
        let Some(name) = self.sent_folder.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return Ok(None);
        };
        Ok(Some(name.replace('/', &self.delimiter()?)))
    }

    /// The server's hierarchy delimiter (`/` until folders are known).
    pub fn delimiter(&self) -> tern_core::Result<String> {
        Ok(self.store.folders()?.iter().find_map(|f| f.delimiter.clone()).unwrap_or_else(|| "/".into()))
    }

    /// Stop all background tasks (connections are dropped).
    pub fn stop(&self) {
        for t in self.tasks.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
            t.abort();
        }
    }
}

fn security(t: TlsMode) -> (Security, tern_smtp::Security) {
    match t {
        TlsMode::Implicit => (Security::Implicit, tern_smtp::Security::Implicit),
        TlsMode::Starttls => (Security::StartTls, tern_smtp::Security::StartTls),
        TlsMode::InsecurePlaintext => (Security::Plaintext, tern_smtp::Security::Plaintext),
    }
}

/// Open the account's storage and start its tasks.
pub fn start(
    data_dir: &Path,
    account: Account,
    profile: &ProfileConfig,
    hub: Arc<Hub>,
    rt: &tokio::runtime::Handle,
) -> tern_core::Result<Arc<AccountRt>> {
    let dir = data_dir.join(&account.id);
    let store = Arc::new(Store::open(&dir.join("store.sqlite"))?);
    let compression = profile.store.compress.then_some(profile.store.compression_level);
    let blobs = Arc::new(BlobStore::open(dir.join("blobs"), compression)?);
    let outbox = Outbox::open(dir.join("outbox"))?;
    let (tx, rx) = mpsc::unbounded_channel();
    let acc = Arc::new(AccountRt {
        id: account.id.clone(),
        config: account,
        store,
        blobs,
        outbox,
        sent_folder: Mutex::new(None),
        tx: tx.clone(),
        tasks: Mutex::new(Vec::new()),
    });
    let worker = Worker {
        acc: acc.clone(),
        hub,
        rx,
        imap_password: None,
        smtp_password: None,
        idle_started: false,
        rt: rt.clone(),
    };
    let handle = rt.spawn(worker.run());
    acc.tasks.lock().expect("fresh mutex").push(handle);
    // Blob GC after things settled; unreferenced blobs older than a week.
    let gc_acc = acc.clone();
    let gc = rt.spawn(async move {
        tokio::time::sleep(Duration::from_secs(120)).await;
        let acc = gc_acc;
        let r = tokio::task::spawn_blocking(move || -> tern_core::Result<usize> {
            let refs = acc.store.referenced_blobs()?;
            Ok(acc.blobs.gc(&refs, Duration::from_secs(7 * 24 * 3600))?)
        })
        .await;
        if let Ok(Ok(n)) = r
            && n > 0
        {
            info!(n, "garbage-collected blobs");
        }
    });
    acc.tasks.lock().expect("fresh mutex").push(gc);
    Ok(acc)
}

struct Worker {
    acc: Arc<AccountRt>,
    hub: Arc<Hub>,
    rx: mpsc::UnboundedReceiver<Request>,
    imap_password: Option<String>,
    smtp_password: Option<String>,
    idle_started: bool,
    rt: tokio::runtime::Handle,
}

/// Why a run failed, deciding the retry policy.
enum Failure {
    /// Network trouble: retry with backoff.
    Offline(String),
    /// Needs the user (password command failed, authentication refused):
    /// only retried on an explicit sync or after a long pause.
    NeedsUser(String),
    Other(String),
}

impl From<tern_core::Error> for Failure {
    fn from(e: tern_core::Error) -> Self {
        match e {
            tern_core::Error::Backend(BackendError::Connection(m)) => Failure::Offline(m),
            tern_core::Error::Backend(BackendError::Auth(m)) => {
                Failure::NeedsUser(format!("authentication failed: {m}"))
            }
            e => Failure::Other(e.to_string()),
        }
    }
}

impl From<BackendError> for Failure {
    fn from(e: BackendError) -> Self {
        tern_core::Error::Backend(e).into()
    }
}

const MIN_BACKOFF: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(600);

impl Worker {
    fn status(&self, state: AccountState, text: impl Into<String>) {
        self.hub.account_status(&self.acc.id, state, text.into());
    }

    async fn run(mut self) {
        let poll = Duration::from_secs(self.acc.config.config.sync.poll_interval_secs);
        let mut next_poll = Instant::now();
        let mut backoff = MIN_BACKOFF;
        let mut backend: Option<ImapBackend> = None;
        // Outgoing mail queued while the app was closed.
        let _ = self.acc.tx.send(Request::Outbox);
        loop {
            let mut reqs = BTreeSet::new();
            tokio::select! {
                r = self.rx.recv() => match r {
                    Some(r) => { reqs.insert(r); }
                    None => return,
                },
                _ = tokio::time::sleep_until(next_poll) => { reqs.insert(Request::Sync); }
            }
            while let Ok(r) = self.rx.try_recv() {
                reqs.insert(r);
            }
            let full = reqs.contains(&Request::Sync);
            match self.handle(&mut backend, reqs).await {
                Ok(()) => {
                    backoff = MIN_BACKOFF;
                    // Only a completed full sync proves we are up to date.
                    if full {
                        self.status(AccountState::Online, "");
                        next_poll = Instant::now() + poll;
                    }
                }
                Err(Failure::Offline(m)) => {
                    warn!(account = %self.acc.id, "offline: {m}");
                    self.status(AccountState::Offline, m);
                    next_poll = Instant::now() + backoff;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
                Err(Failure::NeedsUser(m)) => {
                    warn!(account = %self.acc.id, "{m}");
                    self.status(AccountState::Error, m);
                    self.imap_password = None;
                    self.smtp_password = None;
                    backend = None;
                    next_poll = Instant::now() + Duration::from_secs(3600);
                }
                Err(Failure::Other(m)) => {
                    warn!(account = %self.acc.id, "sync error: {m}");
                    self.status(AccountState::Error, m);
                    next_poll = Instant::now() + poll;
                }
            }
        }
    }

    async fn imap_settings(&mut self) -> Result<ImapSettings, Failure> {
        let c = &self.acc.config.config.imap;
        if self.imap_password.is_none() {
            let pw = tern_config::secrets::resolve(&c.password)
                .await
                .map_err(|e| Failure::NeedsUser(format!("IMAP password: {e}")))?;
            self.imap_password = Some(pw);
        }
        Ok(ImapSettings {
            host: c.host.clone(),
            port: c.effective_port(),
            security: security(c.tls).0,
            username: c.username.clone(),
            password: self.imap_password.clone().unwrap_or_default(),
        })
    }

    async fn smtp_settings(&mut self) -> Result<tern_smtp::SmtpSettings, Failure> {
        let c = &self.acc.config.config.smtp;
        if self.smtp_password.is_none() {
            // Same source as IMAP: don't ask twice.
            let pw = if c.password == self.acc.config.config.imap.password && self.imap_password.is_some() {
                self.imap_password.clone().unwrap_or_default()
            } else {
                tern_config::secrets::resolve(&c.password)
                    .await
                    .map_err(|e| Failure::NeedsUser(format!("SMTP password: {e}")))?
            };
            self.smtp_password = Some(pw);
        }
        Ok(tern_smtp::SmtpSettings {
            host: c.host.clone(),
            port: c.effective_port(),
            security: security(c.tls).1,
            username: c.username.clone(),
            password: self.smtp_password.clone().unwrap_or_default(),
        })
    }

    fn events(&self) -> impl Fn(SyncEvent) + Send + Sync + use<> {
        let hub = self.hub.clone();
        let acc = self.acc.clone();
        move |e| match e {
            SyncEvent::FolderListChanged => hub.tree_dirty(),
            SyncEvent::FolderChanged(f) => hub.folder_dirty(&acc.id, f),
            SyncEvent::NewMessages { folder, ids } => hub.new_mail(&acc.id, folder, ids),
            SyncEvent::Progress { folder, phase, done, total } => {
                let name = acc.store.folder(folder).ok().flatten().map(|f| f.name).unwrap_or_default();
                let label = match phase {
                    SyncPhase::Headers => format!("{name} (headers)"),
                    SyncPhase::Bodies => name,
                };
                hub.progress(&acc.id, label, done, total);
            }
        }
    }

    async fn handle(&mut self, backend: &mut Option<ImapBackend>, reqs: BTreeSet<Request>) -> Result<(), Failure> {
        let full = reqs.contains(&Request::Sync);
        // Sending doesn't need IMAP; do it first so mail leaves even when
        // IMAP is down. SMTP problems are reported but don't stop the sync.
        if full || reqs.contains(&Request::Outbox) {
            self.flush_outbox().await;
        }
        let only_outbox = reqs.iter().all(|r| *r == Request::Outbox);
        if only_outbox && self.acc.store.pending_ops()?.is_empty() {
            return Ok(());
        }
        if backend.is_none() {
            if full {
                self.status(AccountState::Connecting, "");
            }
            let settings = self.imap_settings().await?;
            *backend = Some(ImapBackend::new(settings));
        }
        let b = backend.as_mut().expect("set above");
        if full {
            self.status(AccountState::Syncing, "");
        }
        self.replay(b).await?;
        for r in &reqs {
            match r {
                Request::Body(id) => self.fetch_body(b, *id).await?,
                Request::Folder(name) if !full => self.sync_one(b, name).await?,
                _ => {}
            }
        }
        if full {
            self.full_sync(b).await?;
        }
        if !self.idle_started && b.caps().idle {
            self.idle_started = true;
            self.start_idle(b).await;
        }
        Ok(())
    }

    async fn replay(&mut self, b: &mut ImapBackend) -> Result<(), Failure> {
        let report = ops::replay(&self.acc.store, &self.acc.blobs, b).await?;
        for d in &report.dropped {
            self.hub.error(format!("{}: gave up on {d}", self.acc.config.config.name));
        }
        Ok(())
    }

    /// Handle requests that arrived during a long sync, so user actions
    /// don't wait for the initial download to finish.
    async fn interleave(&mut self, b: &mut ImapBackend) -> Result<(), Failure> {
        let mut pending = BTreeSet::new();
        while let Ok(r) = self.rx.try_recv() {
            pending.insert(r);
        }
        if pending.is_empty() {
            return Ok(());
        }
        self.replay(b).await?;
        for r in pending {
            match r {
                Request::Body(id) => self.fetch_body(b, id).await?,
                Request::Folder(name) => self.sync_one(b, &name).await?,
                Request::Outbox => self.flush_outbox().await,
                Request::Sync | Request::Replay => {}
            }
        }
        Ok(())
    }

    async fn full_sync(&mut self, b: &mut ImapBackend) -> Result<(), Failure> {
        let events = self.events();
        let exclude = self.acc.config.config.sync.exclude_folders.clone();
        let store = self.acc.store.clone();
        let blobs = self.acc.blobs.clone();
        let ctx = SyncContext { store: &store, blobs: &blobs, events: &events, exclude: &exclude };
        ctx.sync_folder_list(b).await?;
        let folders = ctx.sync_order()?;
        for f in &folders {
            ctx.sync_folder(b, f).await?;
            self.interleave(b).await?;
        }
        for f in &folders {
            ctx.fetch_bodies(b, f).await?;
            self.interleave(b).await?;
        }
        self.hub.progress_done(&self.acc.id);
        Ok(())
    }

    async fn sync_one(&mut self, b: &mut ImapBackend, name: &str) -> Result<(), Failure> {
        let Some(folder) = self.acc.store.folder_by_name(name)? else { return Ok(()) };
        let events = self.events();
        let ctx = SyncContext { store: &self.acc.store, blobs: &self.acc.blobs, events: &events, exclude: &[] };
        ctx.sync_folder(b, &folder).await?;
        ctx.fetch_bodies(b, &folder).await?;
        Ok(())
    }

    async fn fetch_body(&mut self, b: &mut ImapBackend, id: MessageId) -> Result<(), Failure> {
        let Some(m) = self.acc.store.message(id)? else { return Ok(()) };
        let (Some(uid), None) = (m.uid, &m.blob) else {
            self.hub.body_ready(MessageKey { account: self.acc.id.clone(), id });
            return Ok(());
        };
        let Some(folder) = self.acc.store.folder(m.folder_id)? else { return Ok(()) };
        b.select(&folder.name).await?;
        let bodies = b.fetch_bodies(&folder.name, &[uid]).await?;
        let mut stored = Vec::new();
        for (uid, raw) in &bodies {
            stored.push((*uid, self.acc.blobs.put(raw).map_err(tern_core::Error::from)?));
        }
        self.acc.blobs.sync().map_err(tern_core::Error::from)?;
        self.acc.store.set_blobs(folder.id, &stored)?;
        self.hub.body_ready(MessageKey { account: self.acc.id.clone(), id });
        Ok(())
    }

    /// Send queued mail. Errors are reported to the UI; transient ones are
    /// retried on the next sync.
    async fn flush_outbox(&mut self) {
        let items = match self.acc.outbox.list() {
            Ok(items) => items.into_iter().filter(|i| !i.meta.failed).collect::<Vec<_>>(),
            Err(e) => {
                self.hub.error(format!("outbox: {e}"));
                return;
            }
        };
        if items.is_empty() {
            return;
        }
        let settings = match self.smtp_settings().await {
            Ok(s) => s,
            Err(Failure::NeedsUser(m) | Failure::Offline(m) | Failure::Other(m)) => {
                self.hub.sent(false, format!("Mail not sent yet: {m}"));
                return;
            }
        };
        let sender = match tern_smtp::Sender::new(&settings) {
            Ok(s) => s,
            Err(e) => {
                self.hub.sent(false, format!("Mail not sent yet: {e}"));
                return;
            }
        };
        for mut item in items {
            let raw = match self.acc.outbox.message(&item.id) {
                Ok(r) => r,
                Err(e) => {
                    self.hub.error(format!("outbox: {e}"));
                    continue;
                }
            };
            match sender.send(&item.meta.from, &item.meta.recipients, &raw).await {
                Ok(()) => {
                    info!(account = %self.acc.id, id = %item.id, "sent");
                    // Remove first: whatever happens below must never cause
                    // the message to be sent twice.
                    if let Err(e) = self.acc.outbox.remove(&item.id) {
                        warn!("cannot remove sent message {} from the outbox: {e}", item.id);
                        item.meta.failed = true;
                        item.meta.last_error = Some(format!("sent, but could not be removed from the outbox: {e}"));
                        let _ = self.acc.outbox.update(&item);
                    }
                    self.hub.sent(true, format!("Sent: {}", item.meta.subject));
                    if let Err(e) = self.after_send(&item.meta, &raw) {
                        self.hub.error(format!("Sent, but could not file the copy: {e}"));
                    }
                    let _ = self.acc.tx.send(Request::Replay);
                }
                Err(SendError::Transient(e)) => {
                    item.meta.attempts += 1;
                    item.meta.last_error = Some(e.clone());
                    let _ = self.acc.outbox.update(&item);
                    self.hub.sent(false, format!("Not sent yet, will retry: {e}"));
                    return;
                }
                Err(SendError::Permanent(e)) => {
                    item.meta.attempts += 1;
                    item.meta.failed = true;
                    item.meta.last_error = Some(e.clone());
                    let _ = self.acc.outbox.update(&item);
                    self.hub.sent(false, format!("Sending \"{}\" failed: {e}", item.meta.subject));
                }
            }
        }
    }

    /// Best-effort bookkeeping after a successful send: copy to Sent and mark
    /// the replied-to message.
    fn after_send(&self, meta: &tern_smtp::OutboxMeta, raw: &[u8]) -> tern_core::Result<()> {
        if meta.save_to_sent {
            let sent = match self.acc.sent_folder_name()? {
                Some(name) => Some(ops::ensure_folder(&self.acc.store, &name, Some(&self.acc.delimiter()?))?),
                None => self.acc.store.folder_by_role(FolderRole::Sent)?.map(|f| f.id),
            };
            if let Some(sent) = sent {
                ops::append(&self.acc.store, &self.acc.blobs, sent, raw, Flags::SEEN)?;
                self.hub.folder_dirty(&self.acc.id, sent);
            }
        }
        if let Some(m) = meta.reply_to_message {
            ops::set_flags(&self.acc.store, &[m], Flags::ANSWERED, Flags::empty())?;
            if let Some(msg) = self.acc.store.message(m)? {
                self.hub.folder_dirty(&self.acc.id, msg.folder_id);
            }
        }
        Ok(())
    }

    async fn start_idle(&mut self, b: &ImapBackend) {
        let _ = b;
        let Ok(settings) = self.imap_settings().await else { return };
        let mut folders = vec!["INBOX".to_owned()];
        for f in &self.acc.config.config.sync.idle_folders {
            if !folders.contains(f) {
                folders.push(f.clone());
            }
        }
        for folder in folders {
            let settings = settings.clone();
            let tx = self.acc.tx.clone();
            let account = self.acc.id.clone();
            let handle = self.rt.spawn(async move {
                let mut b = ImapBackend::new(settings);
                loop {
                    match b.wait_for_changes(&folder, Duration::from_secs(25 * 60)).await {
                        Ok(WaitOutcome::Changed) => {
                            if tx.send(Request::Folder(folder.clone())).is_err() {
                                return;
                            }
                        }
                        Ok(WaitOutcome::Timeout) => {}
                        Err(BackendError::Auth(_)) => return,
                        Err(e) => {
                            info!(%account, %folder, "IDLE interrupted: {e}");
                            tokio::time::sleep(Duration::from_secs(60)).await;
                        }
                    }
                }
            });
            self.acc.tasks.lock().unwrap_or_else(|p| p.into_inner()).push(handle);
        }
    }
}

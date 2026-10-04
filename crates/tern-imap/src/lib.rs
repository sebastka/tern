//! IMAP4rev1 backend for Tern, on top of `async-imap`.
//!
//! The session connects lazily and reconnects after connection errors, so the
//! sync engine and the offline queue can simply retry. All mailbox names
//! crossing this API are UTF-8; modified UTF-7 is only used on the wire.

mod conn;
pub mod utf7;

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use async_imap::imap_proto::rfc4315::UidSetMember;
use async_imap::imap_proto::{self, Response, ResponseCode, Status};
use async_imap::types::{Fetch, Flag, NameAttribute};
use async_imap::{Client, Session};
use async_trait::async_trait;
use futures::TryStreamExt;
use tern_core::backend::{BackendError, BackendResult, MailBackend, WaitOutcome};
use tern_core::model::*;
use tokio::io::AsyncWriteExt;
use tracing::{debug, info};

use crate::conn::Conn;

pub use conn::tls_config;

/// Connection security.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Implicit,
    StartTls,
    /// Only for local test servers (enforced by config validation).
    Plaintext,
}

#[derive(Clone)]
pub struct ImapSettings {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    pub password: String,
}

impl fmt::Debug for ImapSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImapSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("security", &self.security)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

/// Timeout for one command round trip; a silent connection is treated as
/// dead after this.
const OP_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ImapBackend {
    settings: ImapSettings,
    session: Option<Session<Conn>>,
    caps: BackendCaps,
    /// EXISTS of the currently selected mailbox, to skip `1:*` fetches on
    /// empty folders.
    selected: Option<(String, u32)>,
}

type ImapErr = async_imap::error::Error;

fn map_err(e: ImapErr) -> BackendError {
    match e {
        ImapErr::Io(e) => BackendError::Connection(e.to_string()),
        ImapErr::ConnectionLost => BackendError::Connection("connection lost".into()),
        ImapErr::No(m) => BackendError::Refused(m),
        ImapErr::Bad(m) => BackendError::Protocol(format!("BAD {m}")),
        e => BackendError::Protocol(e.to_string()),
    }
}

async fn timed<T>(fut: impl Future<Output = Result<T, ImapErr>>) -> Result<T, ImapErr> {
    match tokio::time::timeout(OP_TIMEOUT, fut).await {
        Ok(r) => r,
        Err(_) => Err(ImapErr::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "server did not answer"))),
    }
}

/// Quote a string for an IMAP command (RFC 3501 quoted string).
fn quote(s: &str) -> Result<String, BackendError> {
    if s.contains(['\r', '\n', '\0']) {
        return Err(BackendError::Protocol("mailbox name contains a line break".into()));
    }
    Ok(format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// Compact UID set: `1:5,7,9:12`.
pub fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut parts = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        parts.push(if start == end { start.to_string() } else { format!("{start}:{end}") });
        i += 1;
    }
    parts.join(",")
}

fn expand(set: &[UidSetMember]) -> Vec<u32> {
    set.iter()
        .flat_map(|m| match m {
            UidSetMember::Uid(u) => *u..=*u,
            UidSetMember::UidRange(r) => r.clone(),
        })
        .collect()
}

fn flags_of(fetch: &Fetch) -> (Flags, Vec<String>) {
    let names: Vec<String> = fetch
        .flags()
        .map(|f| match f {
            Flag::Seen => "\\Seen".to_owned(),
            Flag::Answered => "\\Answered".to_owned(),
            Flag::Flagged => "\\Flagged".to_owned(),
            Flag::Deleted => "\\Deleted".to_owned(),
            Flag::Draft => "\\Draft".to_owned(),
            Flag::Recent => "\\Recent".to_owned(),
            Flag::MayCreate => "\\*".to_owned(),
            Flag::Custom(c) => c.into_owned(),
        })
        .collect();
    Flags::from_imap(names.iter().map(String::as_str))
}

fn flag_list(flags: Flags) -> String {
    format!("({})", flags.names().join(" "))
}

impl ImapBackend {
    /// Create a backend; no connection is made until first use.
    pub fn new(settings: ImapSettings) -> Self {
        Self { settings, session: None, caps: BackendCaps::default(), selected: None }
    }

    /// Connect and log in now (to check credentials).
    pub async fn connect(&mut self) -> BackendResult<()> {
        self.session().await.map(|_| ())
    }

    async fn open(&self) -> BackendResult<(Session<Conn>, BackendCaps)> {
        let s = &self.settings;
        let conn_err = |e: std::io::Error| BackendError::Connection(format!("{}:{}: {e}", s.host, s.port));
        let tcp = tokio::time::timeout(OP_TIMEOUT, conn::tcp(&s.host, s.port))
            .await
            .map_err(|_| BackendError::Connection(format!("{}:{}: connect timed out", s.host, s.port)))?
            .map_err(conn_err)?;
        let mut client = match s.security {
            Security::Implicit => Client::new(conn::tls_wrap(&s.host, tcp).await.map_err(conn_err)?),
            Security::Plaintext => Client::new(Conn::Plain(tcp)),
            Security::StartTls => {
                let mut plain = Client::new(Conn::Plain(tcp));
                read_greeting(&mut plain).await?;
                plain.run_command_and_check_ok("STARTTLS", None).await.map_err(map_err)?;
                let Conn::Plain(tcp) = plain.into_inner() else { unreachable!() };
                Client::new(conn::tls_wrap(&s.host, tcp).await.map_err(conn_err)?)
            }
        };
        if s.security != Security::StartTls {
            read_greeting(&mut client).await?;
        }
        let mut session = client.login(&s.username, &s.password).await.map_err(|(e, _)| match e {
            ImapErr::No(m) | ImapErr::Bad(m) => BackendError::Auth(m),
            e => map_err(e),
        })?;
        let c = timed(session.capabilities()).await.map_err(map_err)?;
        let caps = BackendCaps {
            condstore: c.has_str("CONDSTORE") || c.has_str("QRESYNC"),
            qresync: c.has_str("QRESYNC"),
            idle: c.has_str("IDLE"),
            move_: c.has_str("MOVE"),
            uidplus: c.has_str("UIDPLUS"),
        };
        info!(host = %s.host, ?caps, "IMAP session established");
        Ok((session, caps))
    }

    async fn session(&mut self) -> BackendResult<&mut Session<Conn>> {
        if self.session.is_none() {
            let (session, caps) = self.open().await?;
            self.caps = caps;
            self.selected = None;
            self.session = Some(session);
        }
        Ok(self.session.as_mut().expect("just set"))
    }

    /// Convert a command result; connection errors drop the session so the
    /// next call reconnects.
    fn done<T>(&mut self, r: Result<T, ImapErr>) -> BackendResult<T> {
        r.map_err(|e| {
            let e = map_err(e);
            if matches!(e, BackendError::Connection(_) | BackendError::Protocol(_)) {
                debug!(%e, "dropping IMAP session");
                self.session = None;
                self.selected = None;
            }
            e
        })
    }

    /// Select `folder` unless it already is; returns its EXISTS count.
    async fn ensure_selected(&mut self, folder: &str) -> BackendResult<u32> {
        match &self.selected {
            Some((f, n)) if f == folder => Ok(*n),
            _ => Ok(self.select(folder).await?.exists),
        }
    }
}

async fn read_greeting(client: &mut Client<Conn>) -> BackendResult<()> {
    match tokio::time::timeout(OP_TIMEOUT, client.read_response()).await {
        Ok(Ok(Some(_))) => Ok(()),
        Ok(Ok(None)) => Err(BackendError::Connection("server closed the connection".into())),
        Ok(Err(e)) => Err(BackendError::Connection(e.to_string())),
        Err(_) => Err(BackendError::Connection("no greeting from server".into())),
    }
}

/// Source → destination UIDs from COPYUID, or (empty, destination) from
/// APPENDUID.
type UidMapping = Option<(Vec<u32>, Vec<u32>)>;

/// Run a command and read every response up to its tagged completion.
async fn run_collect(s: &mut Session<Conn>, cmd: &str) -> Result<UidMapping, ImapErr> {
    let id = s.run_command(cmd).await?;
    collect_until_done(s, &id).await
}

/// Read responses until the tagged completion of `id`, picking up a COPYUID
/// or APPENDUID response code on the way (MOVE sends COPYUID untagged).
async fn collect_until_done(s: &mut Session<Conn>, id: &imap_proto::RequestId) -> Result<UidMapping, ImapErr> {
    let mut mapping = None;
    loop {
        let r = s.read_response().await?.ok_or(ImapErr::ConnectionLost)?;
        let (done, outcome) = match r.parsed() {
            Response::Done { tag, status, outcome } if tag == id => (Some(status), outcome),
            Response::Data { outcome, .. } => (None, outcome),
            _ => continue,
        };
        match &outcome.code {
            Some(ResponseCode::CopyUid(_, src, dst)) => mapping = Some((expand(src), expand(dst))),
            Some(ResponseCode::AppendUid(_, dst)) => mapping = Some((Vec::new(), expand(dst))),
            _ => {}
        }
        if let Some(status) = done {
            let info = outcome.information.as_deref().unwrap_or("").to_owned();
            return match status {
                Status::Ok => Ok(mapping),
                Status::No => Err(ImapErr::No(info)),
                _ => Err(ImapErr::Bad(info)),
            };
        }
    }
}

#[async_trait]
impl MailBackend for ImapBackend {
    fn caps(&self) -> BackendCaps {
        self.caps
    }

    async fn list_folders(&mut self) -> BackendResult<Vec<RemoteFolder>> {
        let s = self.session().await?;
        let r = timed(async {
            let names: Vec<_> = s.list(Some(""), Some("*")).await?.try_collect().await?;
            Ok(names)
        })
        .await;
        let names = self.done(r)?;
        let mut out: Vec<RemoteFolder> = names
            .iter()
            .filter_map(|n| {
                let mut role = None;
                let mut selectable = true;
                for a in n.attributes() {
                    match a {
                        NameAttribute::NoSelect => selectable = false,
                        NameAttribute::Extension(e) if e.eq_ignore_ascii_case("\\NonExistent") => return None,
                        NameAttribute::All => role = Some(FolderRole::All),
                        NameAttribute::Archive => role = Some(FolderRole::Archive),
                        NameAttribute::Drafts => role = Some(FolderRole::Drafts),
                        NameAttribute::Flagged => role = Some(FolderRole::Flagged),
                        NameAttribute::Junk => role = Some(FolderRole::Junk),
                        NameAttribute::Sent => role = Some(FolderRole::Sent),
                        NameAttribute::Trash => role = Some(FolderRole::Trash),
                        _ => {}
                    }
                }
                let wire = n.name();
                let inbox = wire.eq_ignore_ascii_case("INBOX");
                Some(RemoteFolder {
                    name: if inbox { "INBOX".to_owned() } else { utf7::decode(wire) },
                    delimiter: n.delimiter().map(str::to_owned),
                    role: if inbox { Some(FolderRole::Inbox) } else { role },
                    selectable,
                })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out.dedup_by(|a, b| a.name == b.name);
        Ok(out)
    }

    async fn select(&mut self, folder: &str) -> BackendResult<FolderStatus> {
        // Connect first: capabilities are only known afterwards.
        self.session().await?;
        let condstore = self.caps.condstore;
        let s = self.session().await?;
        // Unilateral responses from earlier commands are irrelevant now.
        while s.unsolicited_responses.try_recv().is_ok() {}
        let wire = utf7::encode(folder);
        let r = timed(async { if condstore { s.select_condstore(&wire).await } else { s.select(&wire).await } }).await;
        let mbox = self.done(r)?;
        let uidvalidity =
            mbox.uid_validity.ok_or_else(|| BackendError::Protocol(format!("no UIDVALIDITY for {folder}")))?;
        self.selected = Some((folder.to_owned(), mbox.exists));
        Ok(FolderStatus {
            uidvalidity,
            uidnext: mbox.uid_next,
            exists: mbox.exists,
            highestmodseq: if condstore { mbox.highest_modseq } else { None },
        })
    }

    async fn uids(&mut self, folder: &str) -> BackendResult<Vec<u32>> {
        if self.ensure_selected(folder).await? == 0 {
            return Ok(Vec::new());
        }
        let s = self.session().await?;
        let r = timed(s.uid_search("ALL")).await;
        let mut v: Vec<u32> = self.done(r)?.into_iter().collect();
        v.sort_unstable();
        Ok(v)
    }

    async fn fetch_headers(&mut self, folder: &str, uids: &[u32]) -> BackendResult<Vec<RemoteHeader>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_selected(folder).await?;
        let query = if self.caps.condstore {
            "(UID FLAGS RFC822.SIZE INTERNALDATE MODSEQ BODY.PEEK[HEADER])"
        } else {
            "(UID FLAGS RFC822.SIZE INTERNALDATE BODY.PEEK[HEADER])"
        };
        let set = uid_set(uids);
        let s = self.session().await?;
        let r = timed(async {
            let v: Vec<Fetch> = s.uid_fetch(&set, query).await?.try_collect().await?;
            Ok(v)
        })
        .await;
        let fetches = self.done(r)?;
        Ok(fetches
            .iter()
            .filter_map(|f| {
                let (flags, keywords) = flags_of(f);
                Some(RemoteHeader {
                    uid: f.uid?,
                    flags,
                    keywords,
                    size: f.size.unwrap_or(0),
                    modseq: f.modseq,
                    internal_date: f.internal_date().map(|d| d.timestamp()),
                    header: f.header().unwrap_or_default().to_vec(),
                })
            })
            .collect())
    }

    async fn fetch_flags(&mut self, folder: &str, changed_since: Option<u64>) -> BackendResult<Vec<RemoteFlags>> {
        if self.ensure_selected(folder).await? == 0 {
            return Ok(Vec::new());
        }
        let query = match changed_since {
            Some(m) => format!("(UID FLAGS MODSEQ) (CHANGEDSINCE {m})"),
            None if self.caps.condstore => "(UID FLAGS MODSEQ)".to_owned(),
            None => "(UID FLAGS)".to_owned(),
        };
        let s = self.session().await?;
        let r = timed(async {
            let v: Vec<Fetch> = s.uid_fetch("1:*", &query).await?.try_collect().await?;
            Ok(v)
        })
        .await;
        let fetches = self.done(r)?;
        Ok(fetches
            .iter()
            .filter_map(|f| {
                let (flags, keywords) = flags_of(f);
                Some(RemoteFlags { uid: f.uid?, flags, keywords, modseq: f.modseq })
            })
            .collect())
    }

    async fn fetch_bodies(&mut self, folder: &str, uids: &[u32]) -> BackendResult<Vec<(u32, Vec<u8>)>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_selected(folder).await?;
        let set = uid_set(uids);
        let s = self.session().await?;
        let r = timed(async {
            let v: Vec<Fetch> = s.uid_fetch(&set, "(UID BODY.PEEK[])").await?.try_collect().await?;
            Ok(v)
        })
        .await;
        let fetches = self.done(r)?;
        Ok(fetches.iter().filter_map(|f| Some((f.uid?, f.body()?.to_vec()))).collect())
    }

    async fn store_flags(&mut self, folder: &str, uids: &[u32], add: Flags, remove: Flags) -> BackendResult<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.ensure_selected(folder).await?;
        let set = uid_set(uids);
        let s = self.session().await?;
        let r = timed(async {
            if !add.is_empty() {
                let _: Vec<Fetch> =
                    s.uid_store(&set, format!("+FLAGS.SILENT {}", flag_list(add))).await?.try_collect().await?;
            }
            if !remove.is_empty() {
                let _: Vec<Fetch> =
                    s.uid_store(&set, format!("-FLAGS.SILENT {}", flag_list(remove))).await?.try_collect().await?;
            }
            Ok(())
        })
        .await;
        self.done(r)
    }

    async fn move_messages(&mut self, from: &str, uids: &[u32], to: &str) -> BackendResult<Vec<Option<u32>>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_selected(from).await?;
        let caps = self.caps;
        let set = uid_set(uids);
        let target = quote(&utf7::encode(to))?;
        let s = self.session().await?;
        let r = timed(async {
            if caps.move_ {
                run_collect(s, &format!("UID MOVE {set} {target}")).await
            } else {
                let mapping = run_collect(s, &format!("UID COPY {set} {target}")).await?;
                let _: Vec<Fetch> = s.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await?.try_collect().await?;
                if caps.uidplus {
                    let _: Vec<u32> = s.uid_expunge(&set).await?.try_collect().await?;
                } else {
                    // Without UIDPLUS a plain EXPUNGE could also remove other
                    // messages marked \Deleted by another client; leave the
                    // marked originals for the next expunge instead.
                    tracing::warn!(folder = from, "server lacks MOVE and UIDPLUS; originals stay marked \\Deleted");
                }
                Ok(mapping)
            }
        })
        .await;
        // Our own changes make the cached EXISTS stale.
        self.selected = None;
        let mapping = self.done(r)?;
        let map: HashMap<u32, u32> = match mapping {
            Some((src, dst)) if src.len() == dst.len() => src.into_iter().zip(dst).collect(),
            _ => HashMap::new(),
        };
        Ok(uids.iter().map(|u| map.get(u).copied()).collect())
    }

    async fn expunge(&mut self, folder: &str, uids: &[u32]) -> BackendResult<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.ensure_selected(folder).await?;
        let uidplus = self.caps.uidplus;
        let set = uid_set(uids);
        let s = self.session().await?;
        let r = timed(async {
            let _: Vec<Fetch> = s.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await?.try_collect().await?;
            if uidplus {
                let _: Vec<u32> = s.uid_expunge(&set).await?.try_collect().await?;
            } else {
                let _: Vec<u32> = s.expunge().await?.try_collect().await?;
            }
            Ok(())
        })
        .await;
        self.selected = None;
        self.done(r)
    }

    async fn create_folder(&mut self, name: &str) -> BackendResult<()> {
        let wire = quote(&utf7::encode(name))?;
        let s = self.session().await?;
        let r = timed(run_collect(s, &format!("CREATE {wire}"))).await;
        match self.done(r) {
            Ok(_) => {}
            // Already there (another client, or an earlier attempt that
            // succeeded but whose answer was lost): fine.
            Err(BackendError::Refused(m)) => {
                if !self.list_folders().await?.iter().any(|f| f.name == name) {
                    return Err(BackendError::Refused(m));
                }
            }
            Err(e) => return Err(e),
        }
        // So that other clients show it too; not essential.
        let s = self.session().await?;
        let r = timed(run_collect(s, &format!("SUBSCRIBE {wire}"))).await;
        if let Err(e) = self.done(r) {
            debug!(folder = name, "SUBSCRIBE failed: {e}");
        }
        Ok(())
    }

    async fn append(&mut self, folder: &str, flags: Flags, raw: &[u8]) -> BackendResult<Option<u32>> {
        let target = quote(&utf7::encode(folder))?;
        let s = self.session().await?;
        let r = timed(async {
            let id = s.run_command(format!("APPEND {target} {} {{{}}}", flag_list(flags), raw.len())).await?;
            match s.read_response().await?.ok_or(ImapErr::ConnectionLost)?.parsed() {
                Response::Continue { .. } => {}
                Response::Done { outcome, .. } => {
                    return Err(ImapErr::No(outcome.information.as_deref().unwrap_or("APPEND refused").to_owned()));
                }
                _ => return Err(ImapErr::Append),
            }
            let stream = s.get_mut();
            stream.write_all(raw).await?;
            stream.write_all(b"\r\n").await?;
            stream.flush().await?;
            collect_until_done(s, &id).await
        })
        .await;
        self.selected = None;
        let mapping = self.done(r)?;
        Ok(mapping.and_then(|(_, dst)| dst.first().copied()))
    }

    async fn wait_for_changes(&mut self, folder: &str, timeout: Duration) -> BackendResult<WaitOutcome> {
        self.select(folder).await?;
        if !self.caps.idle {
            tokio::time::sleep(timeout).await;
            return Ok(WaitOutcome::Timeout);
        }
        let session = self.session.take().expect("selected above");
        self.selected = None;
        let mut handle = session.idle();
        timed(handle.init()).await.map_err(map_err)?;
        let outcome = {
            let (fut, _stop) = handle.wait_with_timeout(timeout);
            match fut.await.map_err(map_err)? {
                async_imap::extensions::idle::IdleResponse::NewData(_) => WaitOutcome::Changed,
                _ => WaitOutcome::Timeout,
            }
        };
        let session = timed(handle.done()).await.map_err(map_err)?;
        self.session = Some(session);
        Ok(outcome)
    }

    async fn logout(&mut self) {
        if let Some(mut s) = self.session.take() {
            let _ = tokio::time::timeout(Duration::from_secs(5), s.logout()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_sets() {
        assert_eq!(uid_set(&[5, 1, 2, 3, 7, 9, 10, 11, 3]), "1:3,5,7,9:11");
        assert_eq!(uid_set(&[42]), "42");
    }

    #[test]
    fn quoting() {
        assert_eq!(quote("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
        assert!(quote("a\r\nb").is_err());
    }
}

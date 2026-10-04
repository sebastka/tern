//! Sync engine and offline replay against an in-memory fake server.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tern_core::backend::{BackendResult, WaitOutcome};
use tern_core::blobs::BlobStore;
use tern_core::store::Store;
use tern_core::sync::{SyncContext, SyncEvent};
use tern_core::*;

#[derive(Clone)]
struct Msg {
    raw: Vec<u8>,
    flags: Flags,
    modseq: u64,
}

#[derive(Default)]
struct FakeFolder {
    uidvalidity: u32,
    uidnext: u32,
    msgs: BTreeMap<u32, Msg>,
}

#[derive(Default)]
struct Server {
    folders: BTreeMap<String, FakeFolder>,
    modseq: u64,
    offline: bool,
}

impl Server {
    fn add(&mut self, folder: &str, subject: &str, mid: &str) -> u32 {
        self.modseq += 1;
        let f = self.folders.entry(folder.into()).or_insert_with(|| FakeFolder {
            uidvalidity: 1,
            uidnext: 1,
            ..Default::default()
        });
        let uid = f.uidnext;
        f.uidnext += 1;
        let raw = format!(
            "From: a@example.org\r\nSubject: {subject}\r\nMessage-ID: <{mid}>\r\nDate: Sat, 04 Oct 2026 10:00:{:02} +0000\r\n\r\nbody {subject}\r\n",
            uid % 60
        );
        f.msgs.insert(uid, Msg { raw: raw.into_bytes(), flags: Flags::empty(), modseq: self.modseq });
        uid
    }
}

struct Fake(Arc<Mutex<Server>>);

impl Fake {
    fn srv(&self) -> BackendResult<std::sync::MutexGuard<'_, Server>> {
        let s = self.0.lock().unwrap();
        if s.offline {
            return Err(BackendError::Connection("offline".into()));
        }
        Ok(s)
    }
}

fn header_of(raw: &[u8]) -> Vec<u8> {
    let end = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4).unwrap_or(raw.len());
    raw[..end].to_vec()
}

#[async_trait]
impl MailBackend for Fake {
    fn caps(&self) -> BackendCaps {
        BackendCaps { condstore: true, uidplus: true, move_: true, ..Default::default() }
    }
    async fn list_folders(&mut self) -> BackendResult<Vec<RemoteFolder>> {
        Ok(self
            .srv()?
            .folders
            .keys()
            .map(|n| RemoteFolder { name: n.clone(), delimiter: Some("/".into()), role: None, selectable: true })
            .collect())
    }
    async fn select(&mut self, folder: &str) -> BackendResult<FolderStatus> {
        let s = self.srv()?;
        let f = s.folders.get(folder).ok_or_else(|| BackendError::Refused("no such folder".into()))?;
        Ok(FolderStatus {
            uidvalidity: f.uidvalidity,
            uidnext: Some(f.uidnext),
            exists: f.msgs.len() as u32,
            highestmodseq: f.msgs.values().map(|m| m.modseq).max().or(Some(1)),
        })
    }
    async fn uids(&mut self, folder: &str) -> BackendResult<Vec<u32>> {
        Ok(self.srv()?.folders[folder].msgs.keys().copied().collect())
    }
    async fn fetch_headers(&mut self, folder: &str, uids: &[u32]) -> BackendResult<Vec<RemoteHeader>> {
        let s = self.srv()?;
        let f = &s.folders[folder];
        Ok(uids
            .iter()
            .filter_map(|u| f.msgs.get(u).map(|m| (u, m)))
            .map(|(u, m)| RemoteHeader {
                uid: *u,
                flags: m.flags,
                keywords: vec![],
                size: m.raw.len() as u32,
                modseq: Some(m.modseq),
                internal_date: None,
                header: header_of(&m.raw),
            })
            .collect())
    }
    async fn fetch_flags(&mut self, folder: &str, since: Option<u64>) -> BackendResult<Vec<RemoteFlags>> {
        let s = self.srv()?;
        Ok(s.folders[folder]
            .msgs
            .iter()
            .filter(|(_, m)| since.is_none_or(|s| m.modseq > s))
            .map(|(u, m)| RemoteFlags { uid: *u, flags: m.flags, keywords: vec![], modseq: Some(m.modseq) })
            .collect())
    }
    async fn fetch_bodies(&mut self, folder: &str, uids: &[u32]) -> BackendResult<Vec<(u32, Vec<u8>)>> {
        let s = self.srv()?;
        let f = &s.folders[folder];
        Ok(uids.iter().filter_map(|u| f.msgs.get(u).map(|m| (*u, m.raw.clone()))).collect())
    }
    async fn store_flags(&mut self, folder: &str, uids: &[u32], add: Flags, remove: Flags) -> BackendResult<()> {
        let mut s = self.srv()?;
        s.modseq += 1;
        let ms = s.modseq;
        for u in uids {
            if let Some(m) = s.folders.get_mut(folder).unwrap().msgs.get_mut(u) {
                m.flags = m.flags.union(add).difference(remove);
                m.modseq = ms;
            }
        }
        Ok(())
    }
    async fn move_messages(&mut self, from: &str, uids: &[u32], to: &str) -> BackendResult<Vec<Option<u32>>> {
        let mut s = self.srv()?;
        if !s.folders.contains_key(to) {
            return Err(BackendError::Refused("[TRYCREATE] no such mailbox".into()));
        }
        let mut out = Vec::new();
        for u in uids {
            let m = s.folders.get_mut(from).unwrap().msgs.remove(u);
            out.push(m.map(|m| {
                let t = s.folders.get_mut(to).unwrap();
                let nu = t.uidnext;
                t.uidnext += 1;
                t.msgs.insert(nu, m);
                nu
            }));
        }
        Ok(out)
    }
    async fn expunge(&mut self, folder: &str, uids: &[u32]) -> BackendResult<()> {
        let mut s = self.srv()?;
        for u in uids {
            s.folders.get_mut(folder).unwrap().msgs.remove(u);
        }
        Ok(())
    }
    async fn create_folder(&mut self, name: &str) -> BackendResult<()> {
        let mut s = self.srv()?;
        s.folders.entry(name.into()).or_insert_with(|| FakeFolder { uidvalidity: 1, uidnext: 1, ..Default::default() });
        Ok(())
    }
    async fn append(&mut self, folder: &str, flags: Flags, raw: &[u8]) -> BackendResult<Option<u32>> {
        let mut s = self.srv()?;
        s.modseq += 1;
        let ms = s.modseq;
        let f = s.folders.get_mut(folder).unwrap();
        let u = f.uidnext;
        f.uidnext += 1;
        f.msgs.insert(u, Msg { raw: raw.to_vec(), flags, modseq: ms });
        Ok(Some(u))
    }
    async fn wait_for_changes(&mut self, _: &str, _: Duration) -> BackendResult<WaitOutcome> {
        Ok(WaitOutcome::Timeout)
    }
    async fn logout(&mut self) {}
}

struct Env {
    _dir: tempfile::TempDir,
    store: Store,
    blobs: BlobStore,
    events: Arc<Mutex<Vec<SyncEvent>>>,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("store.sqlite")).unwrap();
        let blobs = BlobStore::open(dir.path().join("blobs"), Some(3)).unwrap();
        Self { _dir: dir, store, blobs, events: Default::default() }
    }

    async fn sync(&self, backend: &mut dyn MailBackend) -> Result<ops::ReplayReport> {
        let ev = self.events.clone();
        let sink = move |e: SyncEvent| ev.lock().unwrap().push(e);
        let ctx = SyncContext { store: &self.store, blobs: &self.blobs, events: &sink, exclude: &[] };
        ctx.sync_account(backend).await
    }

    fn folder(&self, name: &str) -> Folder {
        self.store.folder_by_name(name).unwrap().unwrap()
    }
}

fn server() -> Arc<Mutex<Server>> {
    let mut s = Server::default();
    s.add("INBOX", "first", "1@x");
    s.add("INBOX", "second", "2@x");
    s.add("Archive", "old", "3@x");
    s.folders.entry("Trash".into()).or_insert_with(|| FakeFolder { uidvalidity: 1, uidnext: 1, ..Default::default() });
    Arc::new(Mutex::new(s))
}

#[tokio::test]
async fn initial_sync_downloads_everything() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    let inbox = env.folder("INBOX");
    assert_eq!(inbox.role, Some(FolderRole::Inbox));
    assert_eq!(inbox.total, 2);
    assert_eq!(env.folder("Trash").role, Some(FolderRole::Trash));
    let ids = env.store.ids_by_date(inbox.id).unwrap();
    let m = env.store.message(ids[0]).unwrap().unwrap();
    assert_eq!(m.envelope.subject, "second");
    let raw = env.blobs.get(m.blob.as_deref().unwrap()).unwrap();
    assert!(raw.ends_with(b"body second\r\n"));
    assert!(env.events.lock().unwrap().contains(&SyncEvent::FolderListChanged));
}

#[tokio::test]
async fn incremental_new_expunged_and_flags() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    {
        let mut s = srv.lock().unwrap();
        s.add("INBOX", "third", "4@x");
        s.folders.get_mut("INBOX").unwrap().msgs.remove(&1);
        s.modseq += 1;
        let ms = s.modseq;
        let m = s.folders.get_mut("INBOX").unwrap().msgs.get_mut(&2).unwrap();
        m.flags = Flags::SEEN;
        m.modseq = ms;
    }
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    let inbox = env.folder("INBOX");
    assert_eq!(env.store.uids(inbox.id).unwrap(), vec![2, 3]);
    assert_eq!(inbox.unread, 1);
}

#[tokio::test]
async fn uidvalidity_change_resets_folder() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    {
        let mut s = srv.lock().unwrap();
        let f = s.folders.get_mut("INBOX").unwrap();
        f.uidvalidity = 2;
        let msgs = std::mem::take(&mut f.msgs);
        f.msgs = msgs.into_values().enumerate().map(|(i, m)| (100 + i as u32, m)).collect();
        f.uidnext = 200;
    }
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    assert_eq!(env.store.uids(env.folder("INBOX").id).unwrap(), vec![100, 101]);
}

#[tokio::test]
async fn offline_changes_are_replayed() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    let inbox = env.folder("INBOX");
    let archive = env.folder("Archive");
    let ids = env.store.ids_by_date(inbox.id).unwrap(); // [second, first]

    srv.lock().unwrap().offline = true;
    ops::set_flags(&env.store, &[ids[0]], Flags::SEEN | Flags::FLAGGED, Flags::empty()).unwrap();
    ops::move_messages(&env.store, &[ids[1]], archive.id).unwrap();
    // Locally visible right away.
    assert_eq!(env.folder("INBOX").total, 1);
    assert_eq!(env.folder("Archive").total, 2);
    // Sync fails while offline and keeps the queue.
    assert!(env.sync(&mut Fake(srv.clone())).await.is_err());
    assert_eq!(env.store.pending_ops().unwrap().len(), 2);

    srv.lock().unwrap().offline = false;
    let report = env.sync(&mut Fake(srv.clone())).await.unwrap();
    assert_eq!(report.done, 2);
    assert!(env.store.pending_ops().unwrap().is_empty());
    {
        let s = srv.lock().unwrap();
        assert_eq!(s.folders["INBOX"].msgs[&2].flags, Flags::SEEN | Flags::FLAGGED);
        assert!(!s.folders["INBOX"].msgs.contains_key(&1));
        assert_eq!(s.folders["Archive"].msgs.len(), 2);
    }
    // The moved message got its new UID from the move and wasn't duplicated.
    let archive = env.folder("Archive");
    assert_eq!(archive.total, 2);
    assert_eq!(env.store.message(ids[1]).unwrap().unwrap().uid, Some(2));
    let m = env.store.message(ids[0]).unwrap().unwrap();
    assert!(m.flags.contains(Flags::FLAGGED));
}

#[tokio::test]
async fn delete_goes_to_trash_then_expunges() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    let inbox = env.folder("INBOX");
    let id = env.store.ids_by_date(inbox.id).unwrap()[0];
    ops::delete(&env.store, &[id]).unwrap();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    assert_eq!(srv.lock().unwrap().folders["Trash"].msgs.len(), 1);
    let trash = env.folder("Trash");
    assert_eq!(trash.total, 1);
    ops::delete(&env.store, &[id]).unwrap();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    assert!(srv.lock().unwrap().folders["Trash"].msgs.is_empty());
    assert_eq!(env.folder("Trash").total, 0);
}

#[tokio::test]
async fn append_is_adopted() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    let archive = env.folder("Archive");
    let raw = b"From: me@example.org\r\nSubject: sent\r\nMessage-ID: <s@x>\r\n\r\nhi\r\n";
    ops::append(&env.store, &env.blobs, archive.id, raw, Flags::SEEN).unwrap();
    assert_eq!(env.folder("Archive").total, 2);
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    assert_eq!(env.folder("Archive").total, 2);
    assert_eq!(env.store.uids(archive.id).unwrap(), vec![1, 2]);
}

#[tokio::test]
async fn queued_ops_are_dropped_after_uidvalidity_change() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    let inbox = env.folder("INBOX");
    let newest = env.store.ids_by_date(inbox.id).unwrap()[0]; // UID 2
    srv.lock().unwrap().offline = true;
    ops::set_flags(&env.store, &[newest], Flags::FLAGGED, Flags::empty()).unwrap();
    // Meanwhile the server rebuilds INBOX: the same UIDs now mean other mail.
    {
        let mut s = srv.lock().unwrap();
        s.offline = false;
        let f = s.folders.get_mut("INBOX").unwrap();
        f.uidvalidity = 9;
        f.msgs.clear();
        f.uidnext = 1;
        s.add("INBOX", "innocent", "i@x");
        s.add("INBOX", "bystander", "b@x");
    }
    let report = env.sync(&mut Fake(srv.clone())).await.unwrap();
    assert_eq!(report.dropped.len(), 1, "{report:?}");
    assert_eq!(srv.lock().unwrap().folders["INBOX"].msgs[&2].flags, Flags::empty());
    assert!(env.store.pending_ops().unwrap().is_empty());
    assert_eq!(env.folder("INBOX").total, 2);
}

#[tokio::test]
async fn refused_move_leaves_no_ghost() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    let inbox = env.folder("INBOX");
    let archive = env.folder("Archive");
    let id = env.store.ids_by_date(inbox.id).unwrap()[0];
    ops::move_messages(&env.store, &[id], archive.id).unwrap();
    // The target disappears on the server before the move is replayed.
    srv.lock().unwrap().folders.remove("Archive");
    let report = env.sync(&mut Fake(srv.clone())).await.unwrap();
    assert_eq!(report.dropped.len(), 1, "{report:?}");
    // The message is back where the server has it, exactly once.
    let inbox = env.folder("INBOX");
    assert_eq!(inbox.total, 2);
    assert_eq!(env.store.uids(inbox.id).unwrap(), vec![1, 2]);
    assert!(env.store.message(id).unwrap().is_none());
}

#[tokio::test]
async fn offline_folder_creation_and_move() {
    let srv = server();
    let env = Env::new();
    env.sync(&mut Fake(srv.clone())).await.unwrap();
    let inbox = env.folder("INBOX");
    let id = env.store.ids_by_date(inbox.id).unwrap()[0];
    srv.lock().unwrap().offline = true;
    let target = ops::ensure_folder(&env.store, "Archive/2026", Some("/")).unwrap();
    assert_eq!(ops::ensure_folder(&env.store, "Archive/2026", Some("/")).unwrap(), target);
    ops::move_messages(&env.store, &[id], target).unwrap();
    assert_eq!(env.folder("Archive/2026").total, 1);
    srv.lock().unwrap().offline = false;
    let report = env.sync(&mut Fake(srv.clone())).await.unwrap();
    assert_eq!(report.done, 2, "{report:?}");
    assert_eq!(srv.lock().unwrap().folders["Archive/2026"].msgs.len(), 1);
    assert_eq!(env.folder("Archive/2026").total, 1);
    assert_eq!(env.folder("INBOX").total, 1);
}

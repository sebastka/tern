//! Integration tests against a real IMAP server (see `testenv/compose.yaml`).
//!
//! Run with `TERN_TEST_IMAP=127.0.0.1:31143 cargo test -p tern-imap`. Without
//! the variable the tests return early. Each test logs in as a fresh user, so
//! it starts with an empty mailbox.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tern_core::backend::{MailBackend, WaitOutcome};
use tern_core::blobs::BlobStore;
use tern_core::store::Store;
use tern_core::sync::{SyncContext, SyncEvent};
use tern_core::*;
use tern_imap::{ImapBackend, ImapSettings, Security};

fn settings(user: &str) -> Option<ImapSettings> {
    let addr = std::env::var("TERN_TEST_IMAP").ok()?;
    let (host, port) = addr.rsplit_once(':')?;
    Some(ImapSettings {
        host: host.into(),
        port: port.parse().ok()?,
        security: Security::Plaintext,
        username: format!("{user}-{}", std::process::id()),
        password: "pass".into(),
    })
}

fn msg(subject: &str, mid: &str, extra: &str) -> Vec<u8> {
    format!(
        "From: Ann <ann@example.org>\r\nTo: bob@example.org\r\nSubject: {subject}\r\n\
         Message-ID: <{mid}>\r\nDate: Sat, 04 Oct 2026 10:00:00 +0000\r\n{extra}\r\nHello {subject}\r\n"
    )
    .into_bytes()
}

struct Env {
    _dir: tempfile::TempDir,
    store: Store,
    blobs: BlobStore,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self {
            store: Store::open(&dir.path().join("store.sqlite")).unwrap(),
            blobs: BlobStore::open(dir.path().join("blobs"), Some(3)).unwrap(),
            _dir: dir,
        }
    }

    async fn sync(&self, b: &mut ImapBackend) -> ops::ReplayReport {
        let events = Arc::new(Mutex::new(Vec::<SyncEvent>::new()));
        let ev = events.clone();
        let sink = move |e| ev.lock().unwrap().push(e);
        let ctx = SyncContext { store: &self.store, blobs: &self.blobs, events: &sink, exclude: &[] };
        ctx.sync_account(b).await.unwrap()
    }

    fn folder(&self, name: &str) -> Folder {
        self.store.folder_by_name(name).unwrap().unwrap()
    }
}

#[tokio::test]
async fn full_cycle() {
    let Some(s) = settings("cycle") else { return };
    let mut b = ImapBackend::new(s.clone());
    b.connect().await.unwrap();
    assert!(b.caps().condstore && b.caps().move_ && b.caps().uidplus && b.caps().idle);

    // Seed through APPEND (also checks APPENDUID).
    let uid = b.append("INBOX", Flags::empty(), &msg("one", "1@t", "")).await.unwrap();
    assert_eq!(uid, Some(1));
    b.append("INBOX", Flags::SEEN, &msg("two", "2@t", "References: <1@t>\r\n")).await.unwrap();

    let env = Env::new();
    env.sync(&mut b).await;
    let inbox = env.folder("INBOX");
    assert_eq!((inbox.total, inbox.unread), (2, 1));
    assert_eq!(env.folder("Trash").role, Some(FolderRole::Trash));
    assert_eq!(env.folder("Sent").role, Some(FolderRole::Sent));
    let ids = env.store.ids_by_date(inbox.id).unwrap();
    let m = env.store.message(ids[0]).unwrap().unwrap();
    let raw = env.blobs.get(m.blob.as_deref().unwrap()).unwrap();
    assert!(String::from_utf8_lossy(&raw).contains("Hello"));

    // Offline-style ops replayed against the real server.
    let one = ids.iter().copied().find(|&i| env.store.message(i).unwrap().unwrap().uid == Some(1)).unwrap();
    ops::set_flags(&env.store, &[one], Flags::SEEN | Flags::FLAGGED, Flags::empty()).unwrap();
    let report = env.sync(&mut b).await;
    assert_eq!(report.done, 1);
    let flags = b.fetch_flags("INBOX", None).await.unwrap();
    assert!(flags.iter().find(|f| f.uid == 1).unwrap().flags.contains(Flags::FLAGGED));

    ops::delete(&env.store, &[one]).unwrap(); // → Trash via UID MOVE
    env.sync(&mut b).await;
    let trash = env.folder("Trash");
    assert_eq!(trash.total, 1);
    // COPYUID gave the moved message its new UID directly.
    assert_eq!(env.store.message(one).unwrap().unwrap().uid, Some(1));
    assert_eq!(env.folder("INBOX").total, 1);

    // Flag change from "another client" arrives via CONDSTORE.
    b.select("INBOX").await.unwrap();
    b.store_flags("INBOX", &[2], Flags::empty(), Flags::SEEN).await.unwrap();
    env.sync(&mut b).await;
    assert_eq!(env.folder("INBOX").unread, 1);

    // Permanent delete from Trash.
    ops::delete(&env.store, &[one]).unwrap();
    env.sync(&mut b).await;
    b.select("Trash").await.unwrap();
    assert!(b.uids("Trash").await.unwrap().is_empty());
    b.logout().await;
}

#[tokio::test]
async fn idle_wakes_on_new_mail() {
    let Some(s) = settings("idle") else { return };
    let mut idler = ImapBackend::new(s.clone());
    let mut writer = ImapBackend::new(s);
    idler.connect().await.unwrap();
    writer.connect().await.unwrap();

    let wait = tokio::spawn(async move {
        let r = idler.wait_for_changes("INBOX", Duration::from_secs(20)).await;
        (r, idler)
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    writer.append("INBOX", Flags::empty(), &msg("ping", "p@t", "")).await.unwrap();
    let (r, mut idler) = tokio::time::timeout(Duration::from_secs(10), wait).await.unwrap().unwrap();
    assert_eq!(r.unwrap(), WaitOutcome::Changed);
    // Session is usable after IDLE.
    idler.select("INBOX").await.unwrap();
    assert_eq!(idler.uids("INBOX").await.unwrap(), vec![1]);
}

#[tokio::test]
async fn wrong_password_is_auth_error() {
    let Some(mut s) = settings("auth") else { return };
    s.password = "wrong".into();
    let err = ImapBackend::new(s).connect().await.unwrap_err();
    assert!(matches!(err, BackendError::Auth(_)), "{err:?}");
}

#[tokio::test]
async fn tls_verifies_certificates() {
    // The test server's certificate is self-signed: both implicit TLS and
    // STARTTLS must refuse it (and say why), never fall back to plaintext.
    let Some(mut s) = settings("tls") else { return };
    s.port = 31993;
    s.security = Security::Implicit;
    let err = ImapBackend::new(s.clone()).connect().await.unwrap_err();
    assert!(matches!(&err, BackendError::Connection(m) if m.contains("certificate")), "{err:?}");
    s.port = 31143;
    s.security = Security::StartTls;
    let err = ImapBackend::new(s).connect().await.unwrap_err();
    assert!(matches!(&err, BackendError::Connection(m) if m.contains("certificate")), "{err:?}");
}

#[tokio::test]
async fn create_nested_and_unicode_folders() {
    let Some(s) = settings("create") else { return };
    let mut b = ImapBackend::new(s);
    b.create_folder("Archive/2026").await.unwrap();
    b.create_folder("Archive/2026").await.unwrap(); // idempotent
    b.create_folder("Entwürfe/Ärger").await.unwrap();
    let names: Vec<String> = b.list_folders().await.unwrap().into_iter().map(|f| f.name).collect();
    for n in ["Archive", "Archive/2026", "Entwürfe", "Entwürfe/Ärger"] {
        assert!(names.iter().any(|x| x == n), "{n} missing in {names:?}");
    }
    b.append("Archive/2026", Flags::SEEN, &msg("old", "o@t", "")).await.unwrap();
    b.select("Archive/2026").await.unwrap();
    assert_eq!(b.uids("Archive/2026").await.unwrap().len(), 1);
}

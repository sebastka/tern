//! End-to-end through the `App` facade against the test servers
//! (`testenv/compose.yaml`). Run with
//! `TERN_TEST_IMAP=127.0.0.1:31143 TERN_TEST_SMTP=127.0.0.1:1025 cargo test -p tern-app --test e2e`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use tern_app::*;
use tern_core::Flags;
use tern_core::backend::MailBackend;
use tern_imap::{ImapBackend, ImapSettings, Security};

struct Env {
    _root: tempfile::TempDir,
    app: App,
    rx: mpsc::Receiver<Event>,
}

fn wait_for(rx: &mpsc::Receiver<Event>, what: &str, mut f: impl FnMut(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(e) if f(&e) => return e,
            Ok(_) => {}
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }
}

fn setup(user: &str) -> Option<(Env, ImapSettings)> {
    let imap = std::env::var("TERN_TEST_IMAP").ok()?;
    let smtp = std::env::var("TERN_TEST_SMTP").ok()?;
    let (ih, ip) = imap.rsplit_once(':')?;
    let (sh, sp) = smtp.rsplit_once(':')?;
    let root = tempfile::tempdir().ok()?;
    let dirs = tern_config::Dirs::rooted(root.path());
    let acc_dir = dirs.config.join("profiles/test/accounts");
    std::fs::create_dir_all(&acc_dir).ok()?;
    let username = format!("{user}-{}", std::process::id());
    std::fs::write(
        acc_dir.join("local.toml"),
        format!(
            r#"name = "Local"
email = "{username}@example.org"
display_name = "Tester"
archive = "Archive/{{year}}"
[imap]
host = "{ih}"
port = {ip}
tls = "insecure-plaintext"
username = "{username}"
password.command = "echo pass"
[smtp]
host = "{sh}"
port = {sp}
tls = "insecure-plaintext"
username = "{username}"
password.command = "echo pass"
"#
        ),
    )
    .ok()?;
    let (tx, rx) = mpsc::channel();
    let app = App::with_dirs(dirs, move |e| {
        let _ = tx.send(e);
    });
    let settings = ImapSettings {
        host: ih.into(),
        port: ip.parse().ok()?,
        security: Security::Plaintext,
        username,
        password: "pass".into(),
    };
    Some((Env { _root: root, app, rx }, settings))
}

#[test]
fn sync_read_and_send() {
    let Some((env, settings)) = setup("e2e") else { return };
    // Seed the server.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let mut b = ImapBackend::new(settings.clone());
        let msg = b"From: Ann <ann@example.org>\r\nTo: tester@example.org\r\nSubject: Welcome\r\n\
Message-ID: <w@example.org>\r\nDate: Sat, 04 Oct 2026 10:00:00 +0000\r\nContent-Type: text/html\r\n\r\n\
<p>Hello <b>there</b><img src=\"https://tracker.example/p.gif\"></p>\r\n";
        b.append("INBOX", Flags::empty(), msg).await.unwrap();
    });

    let info = env.app.startup_info(None);
    assert_eq!(info.auto_profile.as_deref(), Some("test"));
    env.app.open_profile("test").unwrap();
    // A second instance can't open the same profile.
    let (tx2, _rx2) = mpsc::channel();
    let other = App::with_dirs(tern_config::Dirs::rooted(env._root.path()), move |e| {
        let _ = tx2.send(e);
    });
    assert!(matches!(other.open_profile("test"), Err(OpenError::AlreadyRunning(_))));
    drop(other);

    wait_for(&env.rx, "account online", |e| matches!(e, Event::AccountStatus { state: AccountState::Online, .. }));
    let tree = env.app.folder_tree();
    assert_eq!(tree[0].name, "Local");
    let inbox = tree.iter().find(|n| n.role == "inbox").expect("inbox in tree");
    assert_eq!(inbox.unread, 1);

    let count =
        env.app.open_list(FolderKey { account: "local".into(), folder: inbox.folder }, true, "", Default::default());
    assert_eq!(count, 1);
    let rows = env.app.list_rows(0, 50);
    assert_eq!(rows[0].subject, "Welcome");
    assert!(rows[0].unread);

    env.app.open_message(rows[0].key.clone(), false);
    let Event::MessageLoaded(view) = wait_for(&env.rx, "message", |e| matches!(e, Event::MessageLoaded(_))) else {
        unreachable!()
    };
    assert!(view.has_html && view.has_remote_content && !view.remote_allowed);
    assert!(view.text.contains("Hello"));
    let (mime, html) = env.app.resource(&view.url).unwrap();
    assert!(mime.starts_with("text/html"));
    let html = String::from_utf8(html).unwrap();
    assert!(html.contains("<b>there</b>") && html.contains("default-src 'none'"));

    // Mark read: local first, then replayed.
    env.app.mark_read(std::slice::from_ref(&rows[0].key), true);
    wait_for(&env.rx, "list refresh", |e| matches!(e, Event::ListChanged { .. }));
    assert!(!env.app.list_rows(0, 1)[0].unread);

    // Reply and send; a copy lands in Sent on the server.
    env.app.prepare_reply(rows[0].key.clone(), ReplyMode::Reply);
    let Event::ComposeReady(mut draft) = wait_for(&env.rx, "draft", |e| matches!(e, Event::ComposeReady(_))) else {
        unreachable!()
    };
    assert_eq!(draft.subject, "Re: Welcome");
    assert_eq!(draft.to, "\"Ann\" <ann@example.org>");
    draft.body = format!("Thanks!{}", draft.body);
    let request = env.app.send(draft);
    let Event::DraftQueued { request: r, error } =
        wait_for(&env.rx, "queued", |e| matches!(e, Event::DraftQueued { .. }))
    else {
        unreachable!()
    };
    assert_eq!((r, error.as_str()), (request, ""));
    // A draft that can't be built is reported, not lost.
    let bad = env.app.send(Draft { account: "local".into(), to: "nonsense".into(), ..Default::default() });
    let Event::DraftQueued { request: r, error } =
        wait_for(&env.rx, "rejected", |e| matches!(e, Event::DraftQueued { .. }))
    else {
        unreachable!()
    };
    assert_eq!(r, bad);
    assert!(!error.is_empty());
    let Event::SendResult { ok, text } = wait_for(&env.rx, "send", |e| matches!(e, Event::SendResult { .. })) else {
        unreachable!()
    };
    assert!(ok, "{text}");

    let deadline = Instant::now() + Duration::from_secs(20);
    let sent_on_server = loop {
        let n = rt.block_on(async {
            let mut b = ImapBackend::new(settings.clone());
            b.select("Sent").await.unwrap();
            b.uids("Sent").await.unwrap().len()
        });
        if n > 0 || Instant::now() > deadline {
            break n;
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    assert_eq!(sent_on_server, 1);
    // \Answered was set on the original on the server.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let answered = rt.block_on(async {
            let mut b = ImapBackend::new(settings.clone());
            let f = b.fetch_flags("INBOX", None).await.unwrap();
            f[0].flags.contains(Flags::ANSWERED) && f[0].flags.contains(Flags::SEEN)
        });
        if answered {
            break;
        }
        assert!(Instant::now() < deadline, "flags not replayed");
        std::thread::sleep(Duration::from_millis(300));
    }

    // Archive: Archive/<year of the message> is created and the message moved.
    assert!(env.app.accounts()[0].can_archive);
    env.app.archive_messages(std::slice::from_ref(&rows[0].key));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let archived = rt.block_on(async {
            let mut b = ImapBackend::new(settings.clone());
            if b.select("Archive/2026").await.is_err() {
                return 0;
            }
            b.uids("Archive/2026").await.unwrap().len()
        });
        if archived == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "message not archived on the server");
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(env.app.folder_tree().iter().any(|n| n.path == "Archive/2026"));
}

//! Submission against Mailpit (see `testenv/compose.yaml`). Run with
//! `TERN_TEST_SMTP=127.0.0.1:1025 TERN_TEST_MAILPIT_API=127.0.0.1:8025`.

use std::io::{Read, Write};

use tern_core::model::Address;
use tern_smtp::mime::{self, Headers};
use tern_smtp::{Security, SendError, Sender, SmtpSettings};

fn http_get(addr: &str, path: &str) -> String {
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    write!(s, "GET {path} HTTP/1.0\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[tokio::test]
async fn send_through_mailpit() {
    let (Ok(smtp), Ok(api)) = (std::env::var("TERN_TEST_SMTP"), std::env::var("TERN_TEST_MAILPIT_API")) else {
        return;
    };
    let (host, port) = smtp.rsplit_once(':').unwrap();
    let sender = Sender::new(&SmtpSettings {
        host: host.into(),
        port: port.parse().unwrap(),
        security: Security::Plaintext,
        username: "u".into(),
        password: "p".into(),
    })
    .unwrap();

    let token = mime::random_token();
    let h = Headers {
        from: Address { name: Some("Ann".into()), email: "ann@example.org".into() },
        to: vec![Address { name: None, email: "bob@example.org".into() }],
        subject: format!("tern test {token}"),
        message_id: format!("{token}@example.org"),
        date: "Sat, 4 Oct 2026 10:00:00 +0200".into(),
        ..Default::default()
    };
    let raw = mime::message(&h, &mime::body_entity("Hello Bob", &[]));
    sender.send("ann@example.org", &["bob@example.org".into(), "hidden@example.org".into()], &raw).await.unwrap();

    let found = http_get(&api, &format!("/api/v1/search?query=subject:%22{token}%22"));
    assert!(found.contains(&token), "{found}");
    // Bcc recipient got it through the envelope only.
    assert!(found.contains("hidden@example.org"), "{found}");
    assert!(!String::from_utf8_lossy(&raw).contains("hidden"));

    let err = sender.send("not an address", &["b@example.org".into()], &raw).await.unwrap_err();
    assert!(matches!(err, SendError::Permanent(_)));
}

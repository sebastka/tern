//! PGP/MIME round trip through our own MIME writer and the renderer, with a
//! throwaway gpg keyring. Skipped when gpg isn't installed.

use std::process::Command;

use tern_app::compose::{self, Sender};
use tern_app::render::{RenderOptions, render};
use tern_app::{Draft, SignatureState};
use tern_core::Address;
use tern_pgp::Gpg;

fn keyring() -> Option<(tempfile::TempDir, Gpg, String)> {
    Command::new("gpg").arg("--version").output().ok()?;
    let home = tempfile::Builder::new().prefix("tp").tempdir_in("/tmp").ok()?;
    let ok = Command::new("gpg")
        .arg("--homedir")
        .arg(home.path())
        .args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""])
        .args(["--quick-gen-key", "Me <me@example.org>", "default", "default", "never"])
        .output()
        .ok()?
        .status
        .success();
    assert!(ok);
    let gpg = Gpg::new("gpg").with_homedir(home.path());
    let out =
        Command::new("gpg").arg("--homedir").arg(home.path()).args(["--with-colons", "--list-keys"]).output().ok()?;
    let fpr =
        String::from_utf8_lossy(&out.stdout).lines().find(|l| l.starts_with("fpr:"))?.split(':').nth(9)?.to_owned();
    Some((home, gpg, fpr))
}

async fn roundtrip(gpg: &Gpg, key: &str, sign: bool, encrypt: bool) -> tern_app::render::Rendered {
    let sender =
        Sender { from: Address { name: Some("Me".into()), email: "me@example.org".into() }, pgp_key: Some(key) };
    let draft = Draft {
        to: "me@example.org".into(),
        subject: "Secret plans".into(),
        body: "Grüße! The plan is ready.\nFrom here on, it's secret.\n".into(),
        sign,
        encrypt,
        ..Default::default()
    };
    let built = compose::build(&draft, &sender, gpg, None).await.unwrap();
    let raw = String::from_utf8(built.raw.clone()).unwrap();
    if encrypt {
        assert!(raw.contains("multipart/encrypted") && !raw.contains("plan is ready"));
    } else if sign {
        assert!(raw.contains("multipart/signed"));
    }
    let opts = RenderOptions { gpg, allow_remote: false, url_base: "tern-msg:/a/1".into() };
    render(&built.raw, &opts).await
}

#[tokio::test]
async fn sign_encrypt_render() {
    let Some((home, gpg, fpr)) = keyring() else { return };

    let r = roundtrip(&gpg, &fpr, true, false).await;
    assert_eq!(r.signature, SignatureState::Good, "{}", r.signature_text);
    assert!(r.text.contains("Grüße! The plan is ready."), "{}", r.text);
    assert!(r.text.contains("From here on"), "{}", r.text);
    assert!(!r.encrypted);

    let r = roundtrip(&gpg, &fpr, false, true).await;
    assert!(r.encrypted && !r.decryption_failed);
    assert_eq!(r.signature, SignatureState::None);
    assert!(r.text.contains("plan is ready"));

    let r = roundtrip(&gpg, &fpr, true, true).await;
    assert!(r.encrypted);
    assert_eq!(r.signature, SignatureState::Good, "{}", r.signature_text);

    // Tampering with a signed message is detected.
    let sender = Sender { from: Address { name: None, email: "me@example.org".into() }, pgp_key: Some(&fpr) };
    let draft = Draft {
        to: "me@example.org".into(),
        subject: "s".into(),
        body: "original".into(),
        sign: true,
        ..Default::default()
    };
    let built = compose::build(&draft, &sender, &gpg, None).await.unwrap();
    let tampered = String::from_utf8(built.raw).unwrap().replace("original", "forged!!");
    let opts = RenderOptions { gpg: &gpg, allow_remote: false, url_base: "tern-msg:/a/1".into() };
    let r = render(tampered.as_bytes(), &opts).await;
    assert_eq!(r.signature, SignatureState::Bad, "{}", r.signature_text);

    let _ = Command::new("gpgconf").arg("--homedir").arg(home.path()).args(["--kill", "all"]).output();
}

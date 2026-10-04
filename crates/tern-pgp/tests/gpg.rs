//! Round trips through a real `gpg` with a throwaway keyring. Skipped when
//! gpg isn't installed.

use std::process::Command;

use tern_pgp::{Gpg, PgpError, Signature};

fn setup() -> Option<(tempfile::TempDir, Gpg, String)> {
    Command::new("gpg").arg("--version").output().ok()?;
    let home = tempfile::Builder::new().prefix("tg").tempdir_in("/tmp").ok()?;
    let ok = Command::new("gpg")
        .args(["--homedir"])
        .arg(home.path())
        .args(["--batch", "--pinentry-mode", "loopback", "--passphrase", ""])
        .args(["--quick-gen-key", "Test User <test@example.org>", "default", "default", "never"])
        .output()
        .ok()?
        .status
        .success();
    assert!(ok, "key generation failed");
    let gpg = Gpg::new("gpg").with_homedir(home.path());
    Some((home, gpg, "test@example.org".into()))
}

fn kill_agent(home: &tempfile::TempDir) {
    let _ = Command::new("gpgconf").arg("--homedir").arg(home.path()).args(["--kill", "all"]).output();
}

#[tokio::test]
async fn sign_verify_encrypt_decrypt() {
    let Some((home, gpg, me)) = setup() else { return };

    let keys = gpg.keys_for(&me).await.unwrap();
    assert_eq!(keys.len(), 1);
    assert!(keys[0].can_encrypt);
    let fpr = keys[0].fingerprint.clone();
    assert_eq!(gpg.resolve_recipients(std::slice::from_ref(&me)).await.unwrap(), vec![fpr.clone()]);
    match gpg.resolve_recipients(&["nobody@example.org".into()]).await {
        Err(PgpError::MissingKeys(m)) => assert_eq!(m, "nobody@example.org"),
        other => panic!("{other:?}"),
    }

    let data = b"Content-Type: text/plain\r\n\r\nhello\r\n";
    let sig = gpg.sign_detached(data, &fpr).await.unwrap();
    assert!(sig.starts_with(b"-----BEGIN PGP SIGNATURE-----"));
    let v = gpg.verify_detached(data, &sig).await.unwrap();
    assert!(v.is_good(), "{v:?}");
    let v = gpg.verify_detached(b"tampered", &sig).await.unwrap();
    assert!(matches!(v, Signature::Bad { .. }), "{v:?}");

    let ct = gpg.encrypt(data, std::slice::from_ref(&fpr), &[], Some(&fpr), None).await.unwrap();
    assert!(ct.starts_with(b"-----BEGIN PGP MESSAGE-----"));
    let d = gpg.decrypt(&ct).await.unwrap();
    assert_eq!(d.plaintext, data);
    assert!(d.signature.as_ref().is_some_and(Signature::is_good), "{:?}", d.signature);

    let ct = gpg.encrypt(b"x", &[], std::slice::from_ref(&fpr), None, None).await.unwrap();
    assert!(gpg.decrypt(&ct).await.unwrap().signature.is_none());
    assert!(matches!(gpg.decrypt(b"garbage").await, Err(PgpError::Decrypt(_))));

    kill_agent(&home);
}

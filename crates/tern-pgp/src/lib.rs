//! OpenPGP through the system `gpg` binary (ARCHITECTURE.md §11).
//!
//! Every invocation uses `--batch --no-tty --status-fd 2 --with-colons` and
//! only the machine-readable `[GNUPG:]` status lines are interpreted. The
//! user's keyring, gpg-agent, smartcards and pinentry are used as configured.
//! Plaintext only ever lives in memory: it goes through pipes, never files.

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::debug;

#[derive(Debug, thiserror::Error)]
pub enum PgpError {
    #[error("cannot run {0}: {1}")]
    Spawn(String, std::io::Error),
    #[error("gpg failed: {0}")]
    Failed(String),
    #[error("no public key usable for encryption to: {0}")]
    MissingKeys(String),
    #[error("decryption failed: {0}")]
    Decrypt(String),
}

pub type Result<T> = std::result::Result<T, PgpError>;

/// Trust level of a valid signature's key, from the `TRUST_*` status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    Undefined,
    Never,
    Marginal,
    Full,
    Ultimate,
}

/// Result of checking one signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signature {
    Good {
        fingerprint: String,
        user_id: String,
        trust: Trust,
    },
    /// Cryptographically correct, but the key or signature expired, or the
    /// key was revoked.
    Problem {
        fingerprint: String,
        user_id: String,
        reason: String,
    },
    Bad {
        key_id: String,
        user_id: String,
    },
    /// We don't have the public key.
    UnknownKey {
        key_id: String,
    },
    Error(String),
}

impl Signature {
    pub fn is_good(&self) -> bool {
        matches!(self, Self::Good { .. })
    }
}

#[derive(Debug, Clone)]
pub struct Decrypted {
    pub plaintext: Vec<u8>,
    /// Present when the message was also signed.
    pub signature: Option<Signature>,
}

/// One status line: keyword and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub keyword: String,
    pub args: Vec<String>,
}

/// Extract `[GNUPG:]` status lines from gpg's stderr (status-fd 2). Other
/// lines are human-readable messages and ignored.
pub fn parse_status(stderr: &[u8]) -> Vec<Status> {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter_map(|l| l.strip_prefix("[GNUPG:] "))
        .map(|l| {
            let mut it = l.split(' ');
            let keyword = it.next().unwrap_or("").to_owned();
            // The last argument of GOODSIG & co. is a user id with spaces.
            let rest: Vec<&str> = it.collect();
            let args = match keyword.as_str() {
                "GOODSIG" | "BADSIG" | "EXPSIG" | "EXPKEYSIG" | "REVKEYSIG" if rest.len() > 1 => {
                    vec![rest[0].to_owned(), rest[1..].join(" ")]
                }
                _ => rest.iter().map(|s| (*s).to_owned()).collect(),
            };
            Status { keyword, args }
        })
        .collect()
}

/// Interpret signature statuses. Returns `None` if no signature was checked.
pub fn signature_from_status(st: &[Status]) -> Option<Signature> {
    let find = |k: &str| st.iter().find(|s| s.keyword == k);
    let arg = |s: &Status, i: usize| s.args.get(i).cloned().unwrap_or_default();
    let fingerprint = find("VALIDSIG").map(|s| arg(s, 0)).unwrap_or_default();
    if let Some(s) = find("GOODSIG") {
        let trust = st
            .iter()
            .find_map(|s| match s.keyword.as_str() {
                "TRUST_UNDEFINED" => Some(Trust::Undefined),
                "TRUST_NEVER" => Some(Trust::Never),
                "TRUST_MARGINAL" => Some(Trust::Marginal),
                "TRUST_FULLY" => Some(Trust::Full),
                "TRUST_ULTIMATE" => Some(Trust::Ultimate),
                _ => None,
            })
            .unwrap_or(Trust::Undefined);
        let fingerprint = if fingerprint.is_empty() { arg(s, 0) } else { fingerprint };
        return Some(Signature::Good { fingerprint, user_id: arg(s, 1), trust });
    }
    for (k, reason) in [("EXPSIG", "signature expired"), ("EXPKEYSIG", "key expired"), ("REVKEYSIG", "key revoked")] {
        if let Some(s) = find(k) {
            let fingerprint = if fingerprint.is_empty() { arg(s, 0) } else { fingerprint.clone() };
            return Some(Signature::Problem { fingerprint, user_id: arg(s, 1), reason: reason.into() });
        }
    }
    if let Some(s) = find("BADSIG") {
        return Some(Signature::Bad { key_id: arg(s, 0), user_id: arg(s, 1) });
    }
    if let Some(s) = find("ERRSIG") {
        // ERRSIG <keyid> <pkalgo> <hashalgo> <sig_class> <time> <rc> <fpr>
        return Some(if arg(s, 5) == "9" || find("NO_PUBKEY").is_some() {
            Signature::UnknownKey { key_id: arg(s, 0) }
        } else {
            Signature::Error(format!("cannot check signature (rc {})", arg(s, 5)))
        });
    }
    None
}

/// A gpg key from `--list-keys --with-colons`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    pub fingerprint: String,
    pub user_ids: Vec<String>,
    /// Usable for encryption: not expired, revoked or disabled, and has an
    /// encryption-capable (sub)key.
    pub can_encrypt: bool,
}

/// Parse `--with-colons` key listings.
pub fn parse_keys(colons: &str) -> Vec<KeyInfo> {
    let mut keys: Vec<KeyInfo> = Vec::new();
    let mut want_fpr = false;
    for line in colons.lines() {
        let f: Vec<&str> = line.split(':').collect();
        match f.first().copied() {
            Some("pub") => {
                let validity = f.get(1).copied().unwrap_or("");
                // Field 12: key capabilities; capital letters cover the whole
                // key including subkeys. Field 1: e=expired r=revoked etc.
                let caps = f.get(11).copied().unwrap_or("");
                let bad = matches!(validity, "e" | "r" | "d" | "i" | "n");
                keys.push(KeyInfo {
                    fingerprint: String::new(),
                    user_ids: vec![],
                    can_encrypt: !bad && caps.contains('E'),
                });
                want_fpr = true;
            }
            Some("fpr") if want_fpr => {
                if let Some(k) = keys.last_mut() {
                    k.fingerprint = f.get(9).copied().unwrap_or("").to_owned();
                }
                want_fpr = false;
            }
            Some("sub") => want_fpr = false,
            Some("uid") => {
                if let Some(k) = keys.last_mut() {
                    k.user_ids.push(f.get(9).copied().unwrap_or("").to_owned());
                }
            }
            _ => {}
        }
    }
    keys
}

#[derive(Debug, Clone)]
pub struct Gpg {
    program: String,
    /// Optional `--homedir` (tests).
    homedir: Option<PathBuf>,
    /// Allow `--locate-keys` (WKD) for missing recipient keys.
    wkd: bool,
}

struct Output {
    ok: bool,
    stdout: Vec<u8>,
    status: Vec<Status>,
    stderr_text: String,
}

impl Gpg {
    pub fn new(program: impl Into<String>) -> Self {
        Self { program: program.into(), homedir: None, wkd: false }
    }

    pub fn with_homedir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.homedir = Some(dir.into());
        self
    }

    pub fn with_wkd(mut self, enabled: bool) -> Self {
        self.wkd = enabled;
        self
    }

    async fn run(&self, args: &[&str], input: &[u8]) -> Result<Output> {
        let mut cmd = Command::new(&self.program);
        cmd.args(["--batch", "--no-tty", "--status-fd", "2", "--with-colons"]);
        if let Some(h) = &self.homedir {
            cmd.arg("--homedir").arg(h);
        }
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        debug!(?args, "gpg");
        let mut child = cmd.spawn().map_err(|e| PgpError::Spawn(self.program.clone(), e))?;
        let mut stdin = child.stdin.take().expect("piped");
        let input = input.to_vec();
        // Feed stdin concurrently: gpg may fill stdout before reading all
        // input, which would deadlock a sequential write-then-read.
        let writer = tokio::spawn(async move {
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        });
        let out = child.wait_with_output().await.map_err(|e| PgpError::Spawn(self.program.clone(), e))?;
        let _ = writer.await;
        let stderr_text = String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|l| !l.starts_with("[GNUPG:]"))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Output { ok: out.status.success(), stdout: out.stdout, status: parse_status(&out.stderr), stderr_text })
    }

    /// Decrypt (and verify, if signed) an OpenPGP message.
    pub async fn decrypt(&self, ciphertext: &[u8]) -> Result<Decrypted> {
        let out = self.run(&["--decrypt"], ciphertext).await?;
        let has = |k: &str| out.status.iter().any(|s| s.keyword == k);
        if !has("DECRYPTION_OKAY") || has("DECRYPTION_FAILED") {
            let reason = if has("NO_SECKEY") {
                "no secret key for this message".to_owned()
            } else if out.stderr_text.is_empty() {
                "unknown error".to_owned()
            } else {
                out.stderr_text
            };
            return Err(PgpError::Decrypt(reason));
        }
        Ok(Decrypted { plaintext: out.stdout, signature: signature_from_status(&out.status) })
    }

    /// Verify a detached signature over `data`.
    pub async fn verify_detached(&self, data: &[u8], signature: &[u8]) -> Result<Signature> {
        // gpg needs one of the two from a file; the signature isn't secret.
        let mut sig =
            tempfile::Builder::new().prefix("tern-sig").tempfile().map_err(|e| PgpError::Failed(e.to_string()))?;
        std::io::Write::write_all(&mut sig, signature).map_err(|e| PgpError::Failed(e.to_string()))?;
        let path = sig.path().to_string_lossy().into_owned();
        let out = self.run(&["--verify", &path, "-"], data).await?;
        Ok(signature_from_status(&out.status).unwrap_or(Signature::Error(out.stderr_text)))
    }

    /// Verify an inline (cleartext-signed or opaque) message; returns the
    /// signed content and the signature result.
    pub async fn verify_inline(&self, message: &[u8]) -> Result<(Vec<u8>, Signature)> {
        let out = self.run(&["--decrypt"], message).await?;
        let sig = signature_from_status(&out.status).unwrap_or(Signature::Error(out.stderr_text));
        Ok((out.stdout, sig))
    }

    /// ASCII-armored detached signature (for PGP/MIME, SHA-256).
    pub async fn sign_detached(&self, data: &[u8], key: &str) -> Result<Vec<u8>> {
        let out = self.run(&["--armor", "--detach-sign", "--digest-algo", "SHA256", "--local-user", key], data).await?;
        if !out.ok || !out.status.iter().any(|s| s.keyword == "SIG_CREATED") {
            return Err(PgpError::Failed(out.stderr_text));
        }
        Ok(out.stdout)
    }

    /// ASCII-armored encryption to `recipients` (fingerprints), and to
    /// `hidden` (Bcc: their key ids are not visible to other recipients),
    /// optionally signed with `sign_key`. `self_key` is added as a hidden
    /// recipient so the sender can read their own sent copy.
    ///
    /// Keys are used regardless of their certification ("trust model
    /// always"): callers pass fingerprints they selected from the user's own
    /// keyring (see [`Self::resolve_recipients`]). Otherwise gpg in batch mode
    /// refuses every key the user hasn't signed, which is most of them.
    pub async fn encrypt(
        &self,
        data: &[u8],
        recipients: &[String],
        hidden: &[String],
        sign_key: Option<&str>,
        self_key: Option<&str>,
    ) -> Result<Vec<u8>> {
        let mut args: Vec<&str> = vec!["--armor", "--encrypt", "--trust-model", "always"];
        for r in recipients {
            args.extend(["--recipient", r]);
        }
        for r in hidden.iter().map(String::as_str).chain(self_key) {
            args.extend(["--hidden-recipient", r]);
        }
        if let Some(k) = sign_key {
            args.extend(["--sign", "--digest-algo", "SHA256", "--local-user", k]);
        }
        let out = self.run(&args, data).await?;
        let invalid: Vec<String> =
            out.status.iter().filter(|s| s.keyword == "INV_RECP").filter_map(|s| s.args.get(1).cloned()).collect();
        if !invalid.is_empty() {
            return Err(PgpError::MissingKeys(invalid.join(", ")));
        }
        if !out.ok || !out.status.iter().any(|s| s.keyword == "END_ENCRYPTION") {
            return Err(PgpError::Failed(out.stderr_text));
        }
        Ok(out.stdout)
    }

    /// Public keys matching an address (exact `<address>` match).
    pub async fn keys_for(&self, address: &str) -> Result<Vec<KeyInfo>> {
        let pattern = format!("<{address}>");
        let mode = if self.wkd { "--locate-keys" } else { "--list-keys" };
        let out = self.run(&[mode, &pattern], &[]).await?;
        Ok(parse_keys(&String::from_utf8_lossy(&out.stdout)))
    }

    /// For each address, a fingerprint of a key usable for encryption.
    /// `Err(MissingKeys)` lists the addresses without one.
    pub async fn resolve_recipients(&self, addresses: &[String]) -> Result<Vec<String>> {
        let mut found = Vec::new();
        let mut missing = Vec::new();
        for a in addresses {
            match self.keys_for(a).await?.into_iter().find(|k| k.can_encrypt) {
                Some(k) => found.push(k.fingerprint),
                None => missing.push(a.clone()),
            }
        }
        if missing.is_empty() { Ok(found) } else { Err(PgpError::MissingKeys(missing.join(", "))) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing() {
        let stderr = b"gpg: Signature made ...\n\
[GNUPG:] NEWSIG\n\
[GNUPG:] GOODSIG 1234ABCD Ann Example <ann@example.org>\n\
[GNUPG:] VALIDSIG ABCDEF0123456789 2026-10-04 1791100000 0 4 0 22 8 00 ABCDEF0123456789\n\
[GNUPG:] TRUST_ULTIMATE 0 pgp\n";
        let st = parse_status(stderr);
        assert_eq!(st.len(), 4);
        assert_eq!(st[1].args, vec!["1234ABCD", "Ann Example <ann@example.org>"]);
        assert_eq!(
            signature_from_status(&st),
            Some(Signature::Good {
                fingerprint: "ABCDEF0123456789".into(),
                user_id: "Ann Example <ann@example.org>".into(),
                trust: Trust::Ultimate
            })
        );
        let st = parse_status(b"[GNUPG:] ERRSIG 1234ABCD 1 8 00 1791100000 9 -\n[GNUPG:] NO_PUBKEY 1234ABCD\n");
        assert_eq!(signature_from_status(&st), Some(Signature::UnknownKey { key_id: "1234ABCD".into() }));
        assert_eq!(signature_from_status(&parse_status(b"[GNUPG:] DECRYPTION_OKAY\n")), None);
    }

    #[test]
    fn key_listing() {
        let colons = "tru::1:1791100000:0:3:1:5\n\
pub:u:255:22:AAAA:1791100000:::u:::scESC:::::ed25519:::0:\n\
fpr:::::::::FPRAAAA:\n\
uid:u::::1791100000::HASH::Ann <ann@example.org>::::::::::0:\n\
sub:u:255:18:BBBB:1791100000::::::e:::::cv25519::\n\
fpr:::::::::FPRBBBB:\n\
pub:e:255:22:CCCC:1600000000:1700000000::u:::sc:::::ed25519:::0:\n\
fpr:::::::::FPRCCCC:\n";
        let keys = parse_keys(colons);
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].fingerprint, "FPRAAAA");
        assert!(keys[0].can_encrypt);
        assert_eq!(keys[0].user_ids, vec!["Ann <ann@example.org>"]);
        assert!(!keys[1].can_encrypt);
    }
}

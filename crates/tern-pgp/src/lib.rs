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

/// Where a secret (sub)key is, from field 15 of `--list-secret-keys
/// --with-colons`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretLocation {
    /// In the local keyring.
    Local,
    /// Only a stub: the secret part is kept offline.
    Offline,
    /// On a smartcard (OpenPGP card serial number, as printed on the card).
    Card(String),
}

impl std::fmt::Display for SecretLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "secret key on this computer"),
            Self::Offline => write!(f, "secret key kept offline (only a stub here)"),
            Self::Card(serial) => write!(f, "secret key on smartcard {serial}"),
        }
    }
}

/// The serial printed on an OpenPGP card, from its application id
/// (`D276000124 01 0304 0006 40146786 0000` → `40146786`).
fn card_serial(aid: &str) -> String {
    match aid.get(20..28) {
        Some(serial) if aid.len() == 32 && aid.starts_with("D276000124") => serial.to_owned(),
        _ => aid.to_owned(),
    }
}

/// One secret (sub)key from `--list-secret-keys --with-colons`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretKey {
    /// Long key id.
    pub key_id: String,
    /// Its own capabilities (lowercase letters: `s`, `e`, `a`, `c`).
    pub caps: String,
    /// Not expired or revoked.
    pub valid: bool,
    pub location: SecretLocation,
}

/// Parse `sec`/`ssb` lines of a secret key listing.
pub fn parse_secret_keys(colons: &str) -> Vec<SecretKey> {
    colons
        .lines()
        .map(|l| l.split(':').collect::<Vec<_>>())
        .filter(|f| matches!(f.first().copied(), Some("sec" | "ssb")))
        .map(|f| {
            let field = |i: usize| f.get(i).copied().unwrap_or("");
            let location = match field(14) {
                "#" => SecretLocation::Offline,
                "" | "+" => SecretLocation::Local,
                serial => SecretLocation::Card(card_serial(serial)),
            };
            SecretKey {
                key_id: field(4).to_owned(),
                caps: field(11).chars().filter(char::is_ascii_lowercase).collect(),
                valid: !matches!(field(1), "e" | "r" | "d" | "i" | "n"),
                location,
            }
        })
        .collect()
}

/// What's wrong with a key configured as an account's own key (`pgp.key`).
/// `public` and `secret` are `--list-keys` / `--list-secret-keys` colon
/// listings for the configured key id; `email` is the account's address.
/// Empty if the key is fine.
pub fn own_key_problems(public: &str, secret: &str, email: &str, need_sign: bool) -> Vec<String> {
    let mut problems = Vec::new();
    let keys = parse_keys(public);
    let validity = public.lines().find(|l| l.starts_with("pub:")).and_then(|l| l.split(':').nth(1)).unwrap_or("");
    match keys.len() {
        0 => {
            problems.push("not found in the gpg keyring".to_owned());
            return problems;
        }
        1 => {}
        n => {
            problems.push(format!("matches {n} keys; use the full 40-digit fingerprint"));
            return problems;
        }
    }
    let key = &keys[0];
    match validity {
        "e" => problems.push("the key has expired".to_owned()),
        "r" => problems.push("the key is revoked".to_owned()),
        "d" => problems.push("the key is disabled".to_owned()),
        "i" | "n" => problems.push("gpg considers the key invalid".to_owned()),
        _ => {}
    }
    let wanted = format!("<{}>", email.to_ascii_lowercase());
    if !key.user_ids.iter().any(|u| u.to_ascii_lowercase().contains(&wanted)) {
        problems.push(format!(
            "has no user id for {email} (it has: {}); is this the key of another account?",
            key.user_ids.join(", ")
        ));
    }
    let unusable = matches!(validity, "e" | "r" | "d" | "i" | "n");
    if !unusable && !key.can_encrypt {
        problems.push("has no valid encryption subkey: encrypted mail can't include your own copy".to_owned());
    }
    let secret = parse_secret_keys(secret);
    let usable =
        |cap: char| secret.iter().any(|k| k.valid && k.caps.contains(cap) && k.location != SecretLocation::Offline);
    if need_sign && !usable('s') {
        problems.push("no usable secret signing key here, but sign_by_default = true".to_owned());
    }
    if !usable('e') {
        problems.push("no usable secret encryption key here: encrypted mail to this account can't be read".to_owned());
    }
    problems
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
            return Err(PgpError::Decrypt(self.explain_decrypt_failure(&out).await));
        }
        Ok(Decrypted { plaintext: out.stdout, signature: signature_from_status(&out.status) })
    }

    /// Which keys the message is encrypted to and where their secret keys
    /// are (here, offline, on which smartcard), plus gpg's own message.
    async fn explain_decrypt_failure(&self, out: &Output) -> String {
        let mut recipients: Vec<String> = Vec::new();
        for s in out.status.iter().filter(|s| s.keyword == "ENC_TO") {
            let Some(id) = s.args.first() else { continue };
            let line = if id.trim_start_matches('0').is_empty() {
                "a hidden recipient (key id not shown)".to_owned()
            } else {
                let secret = match self.run(&["--list-secret-keys", id], &[]).await {
                    Ok(o) => parse_secret_keys(&String::from_utf8_lossy(&o.stdout))
                        .into_iter()
                        .find(|k| k.key_id.eq_ignore_ascii_case(id)),
                    Err(_) => None,
                };
                match secret {
                    Some(SecretKey { location: SecretLocation::Card(serial), .. }) => {
                        format!("0x{id}: secret key on smartcard {serial} (is it inserted and unlocked?)")
                    }
                    Some(k) => format!("0x{id}: {}", k.location),
                    None => format!("0x{id}: no secret key for it on this computer"),
                }
            };
            recipients.push(line);
        }
        let gpg_says = if out.stderr_text.trim().is_empty() { "unknown error" } else { out.stderr_text.trim() };
        if recipients.is_empty() {
            return gpg_says.to_owned();
        }
        format!("The message is encrypted to:\n  {}\n\ngpg: {gpg_says}", recipients.join("\n  "))
    }

    /// Check the account's own key (`pgp.key`), see [`own_key_problems`].
    /// `Err` only if gpg can't be run at all.
    pub async fn check_own_key(&self, key: &str, email: &str, need_sign: bool) -> Result<Vec<String>> {
        let public = self.run(&["--list-keys", key], &[]).await?;
        let secret = self.run(&["--list-secret-keys", key], &[]).await?;
        Ok(own_key_problems(
            &String::from_utf8_lossy(&public.stdout),
            &String::from_utf8_lossy(&secret.stdout),
            email,
            need_sign,
        ))
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
    /// optionally signed with `sign_key`. `self_key` is added as a normal
    /// recipient so the sender can read their own sent copy: hiding it would
    /// protect nothing (the sender's key is public), but every reader's gpg
    /// would have to try all its secret keys on the anonymous packet,
    /// prompting for each smartcard.
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
        for r in recipients.iter().map(String::as_str).chain(self_key) {
            args.extend(["--recipient", r]);
        }
        for r in hidden {
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

    // Shaped like a real key: primary key offline, subkeys on a YubiKey.
    const PUBLIC: &str = "pub:u:255:22:C74C02E66D0CBECF:1789746560:1852818560::u:::cESCA:::::ed25519:::0:\n\
fpr:::::::::0B25B26C537B40B5B208F3A6C74C02E66D0CBECF:\n\
uid:u::::1789746560::HASH::Sebastian Karlsen <sebastian@karlsen.fr>::::::::::0:\n\
sub:u:255:22:35495314BE571DA3:1789746672:1821282672:::::s:::::ed25519::\n\
sub:u:255:18:5C6776D77A0675AE:1789746688:1821282688:::::e:::::cv25519::\n";
    const SECRET: &str = "sec:u:255:22:C74C02E66D0CBECF:1789746560:1852818560::u:::cESCA:::#:::ed25519:::0:\n\
fpr:::::::::0B25B26C537B40B5B208F3A6C74C02E66D0CBECF:\n\
ssb:u:255:22:35495314BE571DA3:1789746672:1821282672:::::s:::D2760001240100000006401467850000:::ed25519::\n\
ssb:u:255:18:5C6776D77A0675AE:1789746688:1821282688:::::e:::D2760001240100000006401467850000:::cv25519::\n";

    #[test]
    fn secret_key_locations() {
        let keys = parse_secret_keys(SECRET);
        assert_eq!(keys.len(), 3);
        assert_eq!(keys[0].location, SecretLocation::Offline);
        assert_eq!(keys[0].caps, "c");
        assert_eq!(keys[2].key_id, "5C6776D77A0675AE");
        assert_eq!(keys[2].caps, "e");
        assert_eq!(keys[2].location, SecretLocation::Card("40146785".into()));
        let local = "ssb:u:255:18:AA:1:2:::::e:::+:::cv25519::\nssb:u:255:18:BB:1:2:::::e::::::cv25519::\n";
        assert!(parse_secret_keys(local).iter().all(|k| k.location == SecretLocation::Local));
    }

    #[test]
    fn own_key_checks() {
        assert!(own_key_problems(PUBLIC, SECRET, "sebastian@karlsen.fr", true).is_empty());
        assert!(own_key_problems(PUBLIC, SECRET, "Sebastian@Karlsen.FR", false).is_empty());
        // Key of another account.
        let p = own_key_problems(PUBLIC, SECRET, "sebastian@corp.inbox.com", false);
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("no user id for sebastian@corp.inbox.com"), "{p:?}");
        // Not in the keyring at all.
        assert_eq!(own_key_problems("", "", "a@b.c", false), ["not found in the gpg keyring"]);
        // Public key only: can't read encrypted mail, can't sign.
        let p = own_key_problems(PUBLIC, "", "sebastian@karlsen.fr", true);
        assert_eq!(p.len(), 2, "{p:?}");
        // Everything offline counts as missing.
        let offline = SECRET.replace("D2760001240100000006401467850000", "#");
        assert_eq!(own_key_problems(PUBLIC, &offline, "sebastian@karlsen.fr", false).len(), 1);
        // Expired: reported once, not also as "no encryption subkey".
        let expired = PUBLIC.replacen("pub:u:", "pub:e:", 1);
        let p = own_key_problems(&expired, SECRET, "sebastian@karlsen.fr", false);
        assert_eq!(p, ["the key has expired"]);
        // An ambiguous id.
        let two = format!("{PUBLIC}{}", PUBLIC.replace("C74C02E66D0CBECF", "1111111111111111"));
        assert!(own_key_problems(&two, SECRET, "sebastian@karlsen.fr", false)[0].contains("matches 2 keys"));
    }
}

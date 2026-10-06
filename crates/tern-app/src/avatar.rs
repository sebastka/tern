//! Sender avatars from WebFinger (RFC 7033) and Libravatar, cached per
//! profile in `$XDG_CACHE_HOME` (ARCHITECTURE.md §7: regenerable data).
//!
//! A lookup reveals to the sender's domain or the avatar service that the
//! user opened a message from that address, so `tern-app` only calls this
//! when `[avatars] lookup` allows it. Results, including "nothing found",
//! are cached so each address is looked up rarely.
//!
//! Everything fetched is hostile input: HTTPS only, few redirects, short
//! timeouts, size limits, and only PNG/JPEG/GIF (checked by signature) is
//! handed to the frontend.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};
use tern_config::AvatarSource;
use tracing::debug;

/// Larger avatars are refused.
const MAX_IMAGE: u64 = 512 * 1024;
const MAX_JSON: u64 = 64 * 1024;
/// A found avatar is refreshed after this.
const FOUND_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// "Nothing found" is remembered this long.
const MISSING_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
const AVATAR_REL: &str = "http://webfinger.net/rel/avatar";
const LIBRAVATAR: &str = "https://seccdn.libravatar.org/avatar";

/// What the cache knows about an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached {
    /// The cached image (possibly old).
    pub image: Option<Vec<u8>>,
    /// Unknown, or old enough to look up again.
    pub stale: bool,
}

/// Result of one lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    Found(Vec<u8>),
    /// Every source answered: there is none.
    NotFound,
    /// Network trouble: try again later, cache nothing.
    Failed,
}

pub struct Avatars {
    dir: PathBuf,
    agent: OnceLock<ureq::Agent>,
    /// Addresses being looked up right now.
    inflight: Mutex<HashSet<String>>,
}

/// Addresses are compared lowercased (as Libravatar hashes them).
pub fn normalize(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// A plausible DNS name: it goes into a URL, so nothing else may pass.
fn valid_domain(d: &str) -> bool {
    (1..=253).contains(&d.len())
        && d.contains('.')
        && d.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn percent(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// PNG, JPEG or GIF, by signature (the content type alone isn't trusted).
pub fn is_image(data: &[u8]) -> bool {
    data.starts_with(b"\x89PNG\r\n\x1a\n") || data.starts_with(b"\xff\xd8\xff") || data.starts_with(b"GIF8")
}

/// The avatar link of a WebFinger answer, if it's HTTPS.
fn webfinger_avatar(json: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    v.get("links")?.as_array()?.iter().find_map(|l| {
        let href = l.get("href")?.as_str()?;
        (l.get("rel")?.as_str()? == AVATAR_REL && href.starts_with("https://")).then(|| href.to_owned())
    })
}

/// The best SRV target, as `host` or `host:port` (port 443 is implied).
/// Lowest priority wins, then highest weight; the name must be a plain DNS
/// name, since it goes into a URL.
fn srv_server(records: &[(u16, u16, u16, String)]) -> Option<String> {
    let (_, _, port, target) =
        records.iter().min_by_key(|(priority, weight, _, _)| (*priority, std::cmp::Reverse(*weight)))?;
    let host = target.trim_end_matches('.').to_ascii_lowercase();
    if !valid_domain(&host) || *port == 0 {
        return None;
    }
    Some(if *port == 443 { host } else { format!("{host}:{port}") })
}

/// The domain's own Libravatar server (Libravatar federation): the
/// `_avatars-sec._tcp.<domain>` SRV record, HTTPS only (the plain-HTTP
/// `_avatars._tcp` variant is ignored). Uses the system resolver
/// configuration. Blocking.
fn libravatar_server(domain: &str) -> Option<String> {
    use hickory_resolver::proto::rr::RData;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    let records = rt.block_on(async {
        let resolver = hickory_resolver::Resolver::builder_tokio().ok()?.build().ok()?;
        let lookup = resolver.srv_lookup(format!("_avatars-sec._tcp.{domain}.")).await.ok()?;
        Some(
            lookup
                .answers()
                .iter()
                .filter_map(|r| match &r.data {
                    RData::SRV(s) => Some((s.priority, s.weight, s.port, s.target.to_ascii())),
                    _ => None,
                })
                .collect::<Vec<_>>(),
        )
    })?;
    let server = srv_server(&records);
    debug!(%domain, ?server, "libravatar SRV");
    server
}

fn fresh(path: &Path, ttl: Duration) -> Option<bool> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).map(|age| age < ttl).unwrap_or(true))
}

impl Avatars {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, agent: OnceLock::new(), inflight: Mutex::new(HashSet::new()) }
    }

    fn path(&self, email: &str) -> PathBuf {
        self.dir.join(sha256_hex(&normalize(email)))
    }

    pub fn cached(&self, email: &str) -> Cached {
        let img = self.path(email);
        let none = img.with_extension("none");
        if let Some(is_fresh) = fresh(&img, FOUND_TTL)
            && let Ok(data) = std::fs::read(&img)
            && is_image(&data)
        {
            return Cached { image: Some(data), stale: !is_fresh };
        }
        Cached { image: None, stale: fresh(&none, MISSING_TTL) != Some(true) }
    }

    /// Claim the lookup of `email`; false if one is already running.
    pub fn begin(&self, email: &str) -> bool {
        self.inflight.lock().unwrap_or_else(|p| p.into_inner()).insert(normalize(email))
    }

    /// Look `email` up (blocking: run off the async runtime), cache the
    /// result and end the claim taken with [`Self::begin`].
    pub fn lookup(&self, email: &str, sources: &[AvatarSource]) -> Lookup {
        let email = normalize(email);
        let result = self.fetch(&email, sources);
        let img = self.path(&email);
        let none = img.with_extension("none");
        let _ = std::fs::create_dir_all(&self.dir);
        match &result {
            Lookup::Found(data) => {
                // Atomic: a reader never sees half an image.
                let tmp = img.with_extension("tmp");
                if std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, &img)).is_ok() {
                    let _ = std::fs::remove_file(&none);
                }
            }
            Lookup::NotFound => {
                let _ = std::fs::write(&none, b"");
                let _ = std::fs::remove_file(&img);
            }
            Lookup::Failed => {}
        }
        self.inflight.lock().unwrap_or_else(|p| p.into_inner()).remove(&email);
        result
    }

    fn agent(&self) -> &ureq::Agent {
        self.agent.get_or_init(|| {
            // The system certificate store, like IMAP and SMTP.
            let certs: Vec<ureq::tls::Certificate<'static>> = rustls_native_certs::load_native_certs()
                .certs
                .iter()
                .map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned())
                .collect();
            let tls = ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::new_with_certs(&certs))
                // ring, like the rest of Tern's TLS.
                .unversioned_rustls_crypto_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
                .build();
            ureq::Agent::config_builder()
                .tls_config(tls)
                .https_only(true)
                .max_redirects(3)
                .timeout_global(Some(Duration::from_secs(8)))
                .user_agent("Tern")
                .build()
                .into()
        })
    }

    /// GET `url`: Ok(Some(body)) on success, Ok(None) on an HTTP error
    /// status (the resource isn't there), Err on network trouble.
    fn get(&self, url: &str, accept: &str, limit: u64) -> Result<Option<(String, Vec<u8>)>, ()> {
        match self.agent().get(url).header("Accept", accept).call() {
            Ok(mut resp) => {
                let ct = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                match resp.body_mut().with_config().limit(limit).read_to_vec() {
                    Ok(body) => Ok(Some((ct, body))),
                    Err(e) => {
                        debug!(%url, "avatar: {e}");
                        Err(())
                    }
                }
            }
            Err(ureq::Error::StatusCode(code)) => {
                debug!(%url, code, "avatar: not found");
                Ok(None)
            }
            Err(e) => {
                debug!(%url, "avatar: {e}");
                Err(())
            }
        }
    }

    fn image(&self, url: &str) -> Result<Option<Vec<u8>>, ()> {
        Ok(self
            .get(url, "image/png, image/jpeg, image/gif", MAX_IMAGE)?
            .filter(|(ct, data)| ct.starts_with("image/") && is_image(data))
            .map(|(_, data)| data))
    }

    fn fetch(&self, email: &str, sources: &[AvatarSource]) -> Lookup {
        let Some((_, domain)) = email.rsplit_once('@') else { return Lookup::NotFound };
        let mut failed = false;
        for source in sources {
            match source {
                AvatarSource::Webfinger => {
                    if !valid_domain(domain) {
                        continue;
                    }
                    let url = format!(
                        "https://{domain}/.well-known/webfinger?resource={}&rel={}",
                        percent(&format!("acct:{email}")),
                        percent(AVATAR_REL)
                    );
                    // Most mail domains don't serve WebFinger at all; a
                    // refused connection means "none", not "try later".
                    let Ok(Some((_, json))) = self.get(&url, "application/jrd+json, application/json", MAX_JSON) else {
                        continue;
                    };
                    if let Some(href) = webfinger_avatar(&json) {
                        match self.image(&href) {
                            Ok(Some(img)) => return Lookup::Found(img),
                            Ok(None) => {}
                            Err(()) => failed = true,
                        }
                    }
                }
                AvatarSource::Libravatar => {
                    // Federation: the domain's own server if it publishes
                    // one, else libravatar.org.
                    let base = valid_domain(domain)
                        .then(|| libravatar_server(domain))
                        .flatten()
                        .map(|server| format!("https://{server}/avatar"))
                        .unwrap_or_else(|| LIBRAVATAR.to_owned());
                    let url = format!("{base}/{}?s=128&d=404", sha256_hex(email));
                    match self.image(&url) {
                        Ok(Some(img)) => return Lookup::Found(img),
                        Ok(None) => {}
                        Err(()) => failed = true,
                    }
                }
            }
        }
        if failed { Lookup::Failed } else { Lookup::NotFound }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashing_matches_libravatar() {
        // Trimmed and lowercased, then SHA-256 (as `sha256sum` computes it).
        assert_eq!(
            sha256_hex(&normalize(" User@Example.com ")),
            "b4c9a289323b21a01c3e940f150eb9b8c542587f1abfd8f0e1cc1ffc5e475514"
        );
    }

    #[test]
    fn domains_are_checked() {
        assert!(valid_domain("example.org") && valid_domain("mail-1.example.co.uk"));
        for bad in ["localhost", "", "a..b", "-a.org", "a.org/x", "a.org?x", "evil.org#", "ex ample.org", "a_b.org"] {
            assert!(!valid_domain(bad), "{bad}");
        }
    }

    #[test]
    fn srv_targets() {
        let rec = |p, w, port, t: &str| (p, w, port, t.to_owned());
        assert_eq!(srv_server(&[rec(0, 0, 443, "www.karlsen.fr.")]).as_deref(), Some("www.karlsen.fr"));
        assert_eq!(
            srv_server(&[rec(10, 5, 443, "b.example."), rec(0, 1, 8443, "A.Example.")]).as_deref(),
            Some("a.example:8443")
        );
        assert_eq!(
            srv_server(&[rec(0, 1, 443, "x.example."), rec(0, 9, 443, "y.example.")]).as_deref(),
            Some("y.example")
        );
        // Nothing usable: no record, a weird name, port 0.
        assert_eq!(srv_server(&[]), None);
        assert_eq!(srv_server(&[rec(0, 0, 443, "evil.example/x.")]), None);
        assert_eq!(srv_server(&[rec(0, 0, 0, "a.example.")]), None);
    }

    #[test]
    fn webfinger_links() {
        let json = br#"{"subject":"acct:a@b.c","links":[
            {"rel":"self","href":"https://b.c/users/a"},
            {"rel":"http://webfinger.net/rel/avatar","type":"image/png","href":"https://b.c/a.png"}]}"#;
        assert_eq!(webfinger_avatar(json).as_deref(), Some("https://b.c/a.png"));
        let http = br#"{"links":[{"rel":"http://webfinger.net/rel/avatar","href":"http://b.c/a.png"}]}"#;
        assert_eq!(webfinger_avatar(http), None);
        assert_eq!(webfinger_avatar(b"not json"), None);
    }

    #[test]
    fn image_signatures() {
        assert!(is_image(b"\x89PNG\r\n\x1a\n...."));
        assert!(is_image(b"\xff\xd8\xff\xe0..JFIF"));
        assert!(is_image(b"GIF89a"));
        assert!(!is_image(b"<svg xmlns=..."));
        assert!(!is_image(b"<html>"));
    }

    /// Real lookups; needs the network: `cargo test -p tern-app -- --ignored avatar`.
    #[test]
    #[ignore]
    fn network_lookups() {
        let t = tempfile::tempdir().unwrap();
        let a = Avatars::new(t.path().to_owned());
        // A public fediverse account: WebFinger with an avatar link.
        match a.lookup("Gargron@mastodon.social", &[AvatarSource::Webfinger]) {
            Lookup::Found(img) => assert!(is_image(&img)),
            other => panic!("expected an avatar, got {other:?}"),
        }
        // A domain with its own WebFinger and Libravatar server (SRV
        // `_avatars-sec._tcp.karlsen.fr`), used with its owner's permission.
        assert_eq!(libravatar_server("karlsen.fr").as_deref(), Some("www.karlsen.fr"));
        for source in [AvatarSource::Webfinger, AvatarSource::Libravatar] {
            match a.lookup("sebastian@karlsen.fr", &[source]) {
                Lookup::Found(img) => assert!(is_image(&img)),
                other => panic!("{source:?}: expected an avatar, got {other:?}"),
            }
        }
        // An address nobody has: Libravatar answers 404 (d=404).
        let nobody = "nobody.tern-test.4f9c2@example.org";
        assert_eq!(a.lookup(nobody, &[AvatarSource::Libravatar]), Lookup::NotFound);
        assert_eq!(a.cached(nobody), Cached { image: None, stale: false });
    }

    #[test]
    fn cache_found_missing_and_stale() {
        let t = tempfile::tempdir().unwrap();
        let a = Avatars::new(t.path().join("avatars"));
        assert_eq!(a.cached("x@y.org"), Cached { image: None, stale: true });
        // Simulate lookups without network: write as `lookup` would.
        std::fs::create_dir_all(t.path().join("avatars")).unwrap();
        let img = a.path("X@Y.org");
        std::fs::write(&img, b"\x89PNG\r\n\x1a\nrest").unwrap();
        let c = a.cached("x@y.org");
        assert!(c.image.is_some() && !c.stale);
        std::fs::remove_file(&img).unwrap();
        std::fs::write(img.with_extension("none"), b"").unwrap();
        assert_eq!(a.cached("x@y.org"), Cached { image: None, stale: false });
        // A non-image in the cache is ignored.
        std::fs::write(&img, b"<html>").unwrap();
        assert_eq!(a.cached("x@y.org").image, None);
        // One lookup at a time per address.
        assert!(a.begin("x@y.org"));
        assert!(!a.begin("X@y.org"));
    }
}

//! Outgoing mail for Tern: MIME composition, the outbox, and SMTP submission
//! through `lettre` (rustls, system trust store).

pub mod mime;
pub mod outbox;

use std::fmt;
use std::str::FromStr;

use lettre::address::Envelope;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

pub use outbox::{Outbox, OutboxItem, OutboxMeta};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    Implicit,
    StartTls,
    /// Only for local test servers (enforced by config validation).
    Plaintext,
}

#[derive(Clone)]
pub struct SmtpSettings {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    pub password: String,
}

impl fmt::Debug for SmtpSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("security", &self.security)
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// Worth retrying later (network, 4xx).
    #[error("temporary failure: {0}")]
    Transient(String),
    /// The server refused (5xx, bad address, authentication).
    #[error("rejected: {0}")]
    Permanent(String),
}

pub struct Sender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl Sender {
    pub fn new(s: &SmtpSettings) -> Result<Self, SendError> {
        let perm = |e: lettre::transport::smtp::Error| SendError::Permanent(e.to_string());
        let tls = || TlsParameters::new(s.host.clone()).map_err(perm);
        let builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&s.host).port(s.port);
        let builder = match s.security {
            Security::Implicit => builder.tls(Tls::Wrapper(tls()?)),
            Security::StartTls => builder.tls(Tls::Required(tls()?)),
            Security::Plaintext => builder.tls(Tls::None),
        };
        let transport = builder
            .credentials(Credentials::new(s.username.clone(), s.password.clone()))
            .timeout(Some(std::time::Duration::from_secs(120)))
            .build();
        Ok(Self { transport })
    }

    /// Submit a raw message to the given envelope.
    pub async fn send(&self, from: &str, recipients: &[String], raw: &[u8]) -> Result<(), SendError> {
        let addr = |a: &str| {
            lettre::Address::from_str(a).map_err(|e| SendError::Permanent(format!("invalid address {a:?}: {e}")))
        };
        let to = recipients.iter().map(|r| addr(r)).collect::<Result<Vec<_>, _>>()?;
        let envelope = Envelope::new(Some(addr(from)?), to).map_err(|e| SendError::Permanent(e.to_string()))?;
        self.transport.send_raw(&envelope, raw).await.map(|_| ()).map_err(|e| {
            if e.is_permanent() || e.is_client() {
                SendError::Permanent(e.to_string())
            } else {
                SendError::Transient(e.to_string())
            }
        })
    }
}

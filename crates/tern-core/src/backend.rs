//! The protocol abstraction the sync engine drives (ARCHITECTURE.md §4, §12).
//!
//! Folders are addressed by their server-side name and messages by a 32-bit
//! UID within a folder. A future JMAP backend will map its string ids onto
//! this through a small id table (see DECISIONS.md).

use std::time::Duration;

use async_trait::async_trait;

use crate::model::*;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// Network or connection problem; the operation can be retried later.
    #[error("connection: {0}")]
    Connection(String),
    /// Authentication failed; retrying with the same password won't help.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The server rejected the command (NO/BAD), e.g. the folder doesn't exist.
    #[error("server refused: {0}")]
    Refused(String),
    #[error("protocol: {0}")]
    Protocol(String),
}

impl BackendError {
    /// Whether retrying the same operation later can succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Connection(_))
    }
}

pub type BackendResult<T> = std::result::Result<T, BackendError>;

/// What happened while waiting for changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The server announced a change (new mail, expunge, flags).
    Changed,
    /// Nothing happened before the timeout.
    Timeout,
}

#[async_trait]
pub trait MailBackend: Send {
    fn caps(&self) -> BackendCaps;

    async fn list_folders(&mut self) -> BackendResult<Vec<RemoteFolder>>;

    /// Select a folder (read-write) and report its state.
    async fn select(&mut self, folder: &str) -> BackendResult<FolderStatus>;

    /// All UIDs currently in the selected folder.
    async fn uids(&mut self, folder: &str) -> BackendResult<Vec<u32>>;

    /// Headers, flags and sizes for the given UIDs of the selected folder.
    async fn fetch_headers(&mut self, folder: &str, uids: &[u32]) -> BackendResult<Vec<RemoteHeader>>;

    /// Flags of all messages, or only those changed since `modseq`
    /// (CONDSTORE).
    async fn fetch_flags(&mut self, folder: &str, changed_since: Option<u64>) -> BackendResult<Vec<RemoteFlags>>;

    /// Complete raw messages for the given UIDs.
    async fn fetch_bodies(&mut self, folder: &str, uids: &[u32]) -> BackendResult<Vec<(u32, Vec<u8>)>>;

    async fn store_flags(&mut self, folder: &str, uids: &[u32], add: Flags, remove: Flags) -> BackendResult<()>;

    /// Move messages. Returns the new UIDs in the target, in the order of
    /// `uids`, when the server reports them (UIDPLUS).
    async fn move_messages(&mut self, from: &str, uids: &[u32], to: &str) -> BackendResult<Vec<Option<u32>>>;

    /// Permanently remove messages.
    async fn expunge(&mut self, folder: &str, uids: &[u32]) -> BackendResult<()>;

    /// Create a folder (and subscribe to it). Succeeds if it already exists.
    async fn create_folder(&mut self, name: &str) -> BackendResult<()>;

    /// Upload a message. Returns its UID when reported (UIDPLUS).
    async fn append(&mut self, folder: &str, flags: Flags, raw: &[u8]) -> BackendResult<Option<u32>>;

    /// Block until the folder changes or `timeout` passes (IMAP IDLE). Backends
    /// without push support sleep for `timeout` and report `Timeout`.
    async fn wait_for_changes(&mut self, folder: &str, timeout: Duration) -> BackendResult<WaitOutcome>;

    /// Polite disconnect.
    async fn logout(&mut self);
}

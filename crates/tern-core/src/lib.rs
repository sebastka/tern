//! Tern core: domain model, local mail store and sync engine.
//!
//! Frontends never use this crate directly; they go through `tern-app`.

pub mod backend;
pub mod blobs;
pub mod headers;
pub mod lock;
pub mod model;
pub mod ops;
pub mod store;
pub mod sync;
pub mod thread;

pub use backend::{BackendError, MailBackend};
pub use model::*;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error("database: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("storage: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

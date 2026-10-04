//! The outbox (ARCHITECTURE.md §7, §9): outgoing messages are written here
//! first, durably, and removed only after a successful SMTP submission.
//!
//! `outbox/<id>.eml` holds the exact message; `outbox/<id>.json` holds the
//! SMTP envelope (Bcc recipients are not in the headers) and failure state.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxMeta {
    pub from: String,
    pub recipients: Vec<String>,
    pub subject: String,
    /// Unix seconds.
    pub queued_at: i64,
    pub attempts: u32,
    pub last_error: Option<String>,
    /// The server rejected the message permanently; it stays in the outbox
    /// for the user to inspect but is not retried.
    pub failed: bool,
    /// Also store a copy in the Sent folder after sending.
    pub save_to_sent: bool,
    /// Local id of the message this replies to; marked \Answered once sent.
    #[serde(default)]
    pub reply_to_message: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    pub id: String,
    pub meta: OutboxMeta,
}

#[derive(Debug, Clone)]
pub struct Outbox {
    dir: PathBuf,
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

impl Outbox {
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn eml(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.eml"))
    }

    fn json(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Queue a message; returns its id. Durable when this returns.
    pub fn enqueue(&self, raw: &[u8], meta: &OutboxMeta) -> std::io::Result<String> {
        let id = format!("{}-{}", meta.queued_at, &crate::mime::random_token()[..12]);
        // Message first: an .eml without .json is ignored and cleaned later,
        // a .json without .eml can't happen.
        write_atomic(&self.eml(&id), raw)?;
        write_atomic(&self.json(&id), &serde_json::to_vec_pretty(meta)?)?;
        Ok(id)
    }

    /// Queued items, oldest first.
    pub fn list(&self) -> std::io::Result<Vec<OutboxItem>> {
        let mut items = Vec::new();
        for e in fs::read_dir(&self.dir)?.flatten() {
            let path = e.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else { continue };
            if !self.eml(&id).exists() {
                continue;
            }
            match fs::read(&path).map(|b| serde_json::from_slice::<OutboxMeta>(&b)) {
                Ok(Ok(meta)) => items.push(OutboxItem { id, meta }),
                _ => tracing::warn!(?path, "unreadable outbox metadata"),
            }
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(items)
    }

    pub fn message(&self, id: &str) -> std::io::Result<Vec<u8>> {
        fs::read(self.eml(id))
    }

    pub fn update(&self, item: &OutboxItem) -> std::io::Result<()> {
        write_atomic(&self.json(&item.id), &serde_json::to_vec_pretty(&item.meta)?)
    }

    pub fn remove(&self, id: &str) -> std::io::Result<()> {
        fs::remove_file(self.json(id))?;
        fs::remove_file(self.eml(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enqueue_list_remove() {
        let t = tempfile::tempdir().unwrap();
        let o = Outbox::open(t.path().join("outbox")).unwrap();
        let meta = OutboxMeta {
            from: "a@x".into(),
            recipients: vec!["b@x".into(), "secret-bcc@x".into()],
            subject: "s".into(),
            queued_at: 1,
            attempts: 0,
            last_error: None,
            failed: false,
            save_to_sent: true,
            reply_to_message: None,
        };
        let id = o.enqueue(b"raw", &meta).unwrap();
        let mut items = o.list().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(o.message(&id).unwrap(), b"raw");
        items[0].meta.attempts = 2;
        o.update(&items[0]).unwrap();
        assert_eq!(o.list().unwrap()[0].meta.attempts, 2);
        o.remove(&id).unwrap();
        assert!(o.list().unwrap().is_empty());
    }
}

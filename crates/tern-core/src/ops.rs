//! Offline operation queue (ARCHITECTURE.md §9).
//!
//! Every user action is applied to the local store first, recorded as a
//! [`PendingOp`] and replayed against the server when online. Ops reference
//! folders by server name so they survive folder table changes.
//!
//! Conflict policy: flags are server-wins (a failed flag op is dropped after
//! a few attempts and the next sync restores the server state); moves the
//! user made are local-wins (retried until the server accepts them, unless
//! the source message is gone).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::backend::{BackendError, MailBackend};
use crate::blobs::BlobStore;
use crate::model::*;
use crate::store::Store;
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingOp {
    SetFlags {
        folder: String,
        uids: Vec<u32>,
        messages: Vec<MessageId>,
        add: Flags,
        remove: Flags,
        /// UIDVALIDITY the UIDs belong to; the op is dropped if it changed.
        #[serde(default)]
        uidvalidity: Option<u32>,
    },
    Move {
        from: String,
        to: String,
        uids: Vec<u32>,
        messages: Vec<MessageId>,
        #[serde(default)]
        uidvalidity: Option<u32>,
    },
    Expunge {
        folder: String,
        uids: Vec<u32>,
        #[serde(default)]
        uidvalidity: Option<u32>,
    },
    Append {
        folder: String,
        blob: String,
        flags: Flags,
        message: Option<MessageId>,
    },
    CreateFolder {
        name: String,
    },
}

/// Permanent failures before an op is dropped.
const MAX_ATTEMPTS: u32 = 5;

/// Group messages by folder, keeping only those that exist.
fn by_folder(store: &Store, ids: &[MessageId]) -> Result<BTreeMap<FolderId, Vec<MessageSummary>>> {
    let mut map: BTreeMap<FolderId, Vec<MessageSummary>> = BTreeMap::new();
    for m in store.messages(ids)? {
        map.entry(m.folder_id).or_default().push(m);
    }
    Ok(map)
}

fn folder_name(store: &Store, id: FolderId) -> Result<String> {
    Ok(folder_info(store, id)?.0)
}

/// Name and UIDVALIDITY of a folder.
fn folder_info(store: &Store, id: FolderId) -> Result<(String, Option<u32>)> {
    store.folder(id)?.map(|f| (f.name, f.uidvalidity)).ok_or_else(|| Error::Other(format!("folder {id} not found")))
}

/// Change flags locally and queue the change.
pub fn set_flags(store: &Store, ids: &[MessageId], add: Flags, remove: Flags) -> Result<()> {
    for (folder, msgs) in by_folder(store, ids)? {
        let ids: Vec<_> = msgs.iter().map(|m| m.id).collect();
        store.set_flags_local(&ids, add, remove)?;
        // Messages without a UID only exist locally so far; the flags travel
        // with their Append op or are lost to server-wins on adoption.
        let (uids, messages): (Vec<u32>, Vec<MessageId>) = msgs.iter().filter_map(|m| m.uid.map(|u| (u, m.id))).unzip();
        if !uids.is_empty() {
            let (folder, uidvalidity) = folder_info(store, folder)?;
            store.push_op(&PendingOp::SetFlags { folder, uids, messages, add, remove, uidvalidity })?;
        }
    }
    Ok(())
}

/// Move messages locally and queue the move.
pub fn move_messages(store: &Store, ids: &[MessageId], to: FolderId) -> Result<()> {
    let to_name = folder_name(store, to)?;
    for (folder, msgs) in by_folder(store, ids)? {
        if folder == to {
            continue;
        }
        if msgs.iter().any(|m| m.uid.is_none()) {
            return Err(Error::Other("a selected message is not on the server yet; try again after sync".into()));
        }
        let uids: Vec<u32> = msgs.iter().filter_map(|m| m.uid).collect();
        let messages: Vec<MessageId> = msgs.iter().map(|m| m.id).collect();
        let (from, uidvalidity) = folder_info(store, folder)?;
        store.move_local(&messages, to)?;
        store.push_op(&PendingOp::Move { from, to: to_name.clone(), uids, messages, uidvalidity })?;
    }
    Ok(())
}

/// Delete: move to Trash, or remove permanently when already in Trash (or
/// when the account has no Trash folder).
pub fn delete(store: &Store, ids: &[MessageId]) -> Result<()> {
    let trash = store.folder_by_role(FolderRole::Trash)?;
    for (folder, msgs) in by_folder(store, ids)? {
        let ids: Vec<_> = msgs.iter().map(|m| m.id).collect();
        match &trash {
            Some(t) if t.id != folder => move_messages(store, &ids, t.id)?,
            _ => {
                let uids: Vec<u32> = msgs.iter().filter_map(|m| m.uid).collect();
                let (name, uidvalidity) = folder_info(store, folder)?;
                store.delete_local(&ids)?;
                if !uids.is_empty() {
                    store.push_op(&PendingOp::Expunge { folder: name, uids, uidvalidity })?;
                }
            }
        }
    }
    Ok(())
}

/// The folder named `name`, created locally and queued for creation on the
/// server if it doesn't exist yet. Messages can be moved into it right away;
/// the queue keeps the order (create first, then move).
pub fn ensure_folder(store: &Store, name: &str, delimiter: Option<&str>) -> Result<FolderId> {
    if let Some(f) = store.folder_by_name(name)? {
        return Ok(f.id);
    }
    let id = store.create_folder_local(name, delimiter)?;
    store.push_op(&PendingOp::CreateFolder { name: name.to_owned() })?;
    Ok(id)
}

/// Store a message locally in `folder` and queue its upload (sent copies).
pub fn append(store: &Store, blobs: &BlobStore, folder: FolderId, raw: &[u8], flags: Flags) -> Result<MessageId> {
    let blob = blobs.put(raw)?;
    blobs.sync()?;
    let env = crate::headers::parse_envelope(raw, Some(now()));
    let id = store.insert_local(folder, &blob, flags, raw.len() as u32, &env)?;
    store.push_op(&PendingOp::Append { folder: folder_name(store, folder)?, blob, flags, message: Some(id) })?;
    Ok(id)
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Outcome of a replay run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    pub done: usize,
    /// Ops dropped after repeated permanent failures, with the last error.
    pub dropped: Vec<String>,
    /// Ops still queued (we went offline).
    pub remaining: usize,
}

/// Replay queued ops in order. Stops at the first transient (network) error
/// so ordering is preserved; the rest waits for the next run.
pub async fn replay(store: &Store, blobs: &BlobStore, backend: &mut dyn MailBackend) -> Result<ReplayReport> {
    let ops = store.pending_ops()?;
    let mut report = ReplayReport::default();
    for (i, (id, op)) in ops.iter().enumerate() {
        debug!(?op, "replaying");
        match run_op(store, blobs, backend, op).await {
            Ok(()) => {
                store.remove_op(*id)?;
                report.done += 1;
            }
            Err(Error::Backend(e)) if e.is_transient() => {
                report.remaining = ops.len() - i;
                return Err(Error::Backend(e));
            }
            Err(e) => {
                let attempts = store.op_failed(*id, &e.to_string())?;
                warn!(%e, attempts, "pending op failed");
                if attempts >= MAX_ATTEMPTS || matches!(e, Error::Backend(BackendError::Refused(_))) {
                    store.remove_op(*id)?;
                    release(store, op)?;
                    report.dropped.push(format!("{}: {e}", describe(op)));
                }
            }
        }
    }
    Ok(report)
}

/// Undo the local side of a dropped op so the next sync shows the server
/// state again (server wins).
fn release(store: &Store, op: &PendingOp) -> Result<()> {
    match op {
        PendingOp::SetFlags { folder, messages, .. } => {
            for &m in messages {
                store.confirm_local(m, None)?;
            }
            // Flag changes skipped meanwhile are older than the folder's
            // HIGHESTMODSEQ now: force a full flag fetch.
            store.forget_modseq(folder)?;
        }
        // The server still has the originals in the source folder; the next
        // sync brings them back there. Drop the placeholders in the target.
        PendingOp::Move { messages, .. } => store.delete_placeholders(messages)?,
        PendingOp::Append { message: Some(m), .. } => store.delete_placeholders(std::slice::from_ref(m))?,
        _ => {}
    }
    Ok(())
}

fn describe(op: &PendingOp) -> String {
    match op {
        PendingOp::SetFlags { folder, uids, .. } => format!("flag change of {} message(s) in {folder}", uids.len()),
        PendingOp::Move { from, to, uids, .. } => format!("move of {} message(s) from {from} to {to}", uids.len()),
        PendingOp::Expunge { folder, uids, .. } => format!("deletion of {} message(s) in {folder}", uids.len()),
        PendingOp::Append { folder, .. } => format!("upload to {folder}"),
        PendingOp::CreateFolder { name } => format!("creation of folder {name}"),
    }
}

/// Select `folder` and make sure its UIDs still mean what they meant when
/// the op was queued.
async fn select_checked(backend: &mut dyn MailBackend, folder: &str, uidvalidity: Option<u32>) -> Result<()> {
    let st = backend.select(folder).await?;
    match uidvalidity {
        Some(v) if v != st.uidvalidity => Err(Error::Backend(BackendError::Refused(format!(
            "{folder} was reset on the server (UIDVALIDITY changed)"
        )))),
        _ => Ok(()),
    }
}

async fn run_op(store: &Store, blobs: &BlobStore, backend: &mut dyn MailBackend, op: &PendingOp) -> Result<()> {
    match op {
        PendingOp::SetFlags { folder, uids, messages, add, remove, uidvalidity } => {
            select_checked(backend, folder, *uidvalidity).await?;
            backend.store_flags(folder, uids, *add, *remove).await?;
            for &m in messages {
                store.confirm_local(m, None)?;
            }
        }
        PendingOp::Move { from, to, uids, messages, uidvalidity } => {
            select_checked(backend, from, *uidvalidity).await?;
            let new = backend.move_messages(from, uids, to).await?;
            for (i, &m) in messages.iter().enumerate() {
                let uid = new.get(i).copied().flatten();
                // Without a UID from the server and without a Message-ID the
                // copy can never be matched up: let the next sync bring the
                // server's copy instead of keeping a duplicate.
                if uid.is_none() && store.message(m)?.is_some_and(|s| s.envelope.message_id.is_none()) {
                    store.delete_placeholders(&[m])?;
                } else {
                    store.confirm_local(m, uid)?;
                }
            }
        }
        PendingOp::Expunge { folder, uids, uidvalidity } => {
            select_checked(backend, folder, *uidvalidity).await?;
            backend.expunge(folder, uids).await?;
        }
        PendingOp::CreateFolder { name } => backend.create_folder(name).await?,
        PendingOp::Append { folder, blob, flags, message } => {
            let raw = blobs.get(blob)?;
            let uid = backend.append(folder, *flags, &raw).await?;
            if let Some(m) = message {
                store.confirm_local(*m, uid)?;
            }
        }
    }
    Ok(())
}

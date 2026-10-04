//! Folder synchronization (ARCHITECTURE.md §9).
//!
//! Per folder:
//! 1. UIDVALIDITY changed → drop the folder's index.
//! 2. Membership: if EXISTS and UIDNEXT are unchanged nothing was added or
//!    expunged; otherwise diff the UID lists.
//! 3. Flags: CONDSTORE `CHANGEDSINCE` when available, else all flags.
//! 4. Headers of new messages first (newest first), bodies later in a separate
//!    pass, so the list fills quickly.

use std::collections::HashSet;

use tracing::{debug, info};

use crate::Result;
use crate::backend::MailBackend;
use crate::blobs::BlobStore;
use crate::headers::parse_envelope;
use crate::model::*;
use crate::store::Store;

/// Progress and change notifications from the sync engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEvent {
    FolderListChanged,
    /// Messages were added, removed or changed in a folder.
    FolderChanged(FolderId),
    /// Header or body download progress for a folder.
    Progress {
        folder: FolderId,
        phase: SyncPhase,
        done: u32,
        total: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    Headers,
    Bodies,
}

pub struct SyncContext<'a> {
    pub store: &'a Store,
    pub blobs: &'a BlobStore,
    pub events: &'a (dyn Fn(SyncEvent) + Send + Sync),
    /// Folder names never synced.
    pub exclude: &'a [String],
}

const HEADER_BATCH: usize = 250;
const BODY_BATCH: usize = 25;

impl SyncContext<'_> {
    fn emit(&self, e: SyncEvent) {
        (self.events)(e);
    }

    fn excluded(&self, name: &str) -> bool {
        self.exclude.iter().any(|e| e == name)
    }

    /// Refresh the folder list.
    pub async fn sync_folder_list(&self, backend: &mut dyn MailBackend) -> Result<()> {
        let mut remote = backend.list_folders().await?;
        for f in &mut remote {
            if f.role.is_none() {
                f.role = FolderRole::guess_from_name(&f.name);
            }
        }
        if self.store.sync_folder_list(&remote)? {
            self.emit(SyncEvent::FolderListChanged);
        }
        Ok(())
    }

    /// Folders to sync, INBOX first, then by name.
    pub fn sync_order(&self) -> Result<Vec<Folder>> {
        let mut folders: Vec<Folder> =
            self.store.folders()?.into_iter().filter(|f| f.selectable && !self.excluded(&f.name)).collect();
        folders.sort_by_key(|f| (f.role != Some(FolderRole::Inbox), f.name.clone()));
        Ok(folders)
    }

    /// Bring one folder's index (membership, headers, flags) up to date.
    pub async fn sync_folder(&self, backend: &mut dyn MailBackend, folder: &Folder) -> Result<()> {
        let st = backend.select(&folder.name).await?;
        // After select: the backend may only now have connected and learned
        // the server's capabilities.
        let caps = backend.caps();
        let mut folder = folder.clone();
        let mut changed = false;

        if folder.uidvalidity.is_some_and(|v| v != st.uidvalidity) {
            info!(folder = %folder.name, "UIDVALIDITY changed, resyncing folder");
            self.store.reset_folder(folder.id)?;
            folder.uidvalidity = None;
            folder.highestmodseq = None;
            folder.uidnext = None;
            changed = true;
        }

        let local = self.store.uids(folder.id)?;
        let unchanged_membership = folder.uidvalidity == Some(st.uidvalidity)
            && local.len() as u32 == st.exists
            && st.uidnext.is_some()
            && folder.uidnext == st.uidnext;
        let mut new_uids = Vec::new();
        if !unchanged_membership {
            let remote = backend.uids(&folder.name).await?;
            let remote_set: HashSet<u32> = remote.iter().copied().collect();
            let local_set: HashSet<u32> = local.iter().copied().collect();
            let gone: Vec<u32> = local.iter().copied().filter(|u| !remote_set.contains(u)).collect();
            if !gone.is_empty() {
                debug!(folder = %folder.name, n = gone.len(), "expunged");
                self.store.remove_uids(folder.id, &gone)?;
                changed = true;
            }
            new_uids = remote.into_iter().filter(|u| !local_set.contains(u)).collect();
            new_uids.sort_unstable_by(|a, b| b.cmp(a)); // newest first
        }

        if changed {
            self.emit(SyncEvent::FolderChanged(folder.id));
        }

        let total = new_uids.len() as u32;
        let mut done = 0;
        for chunk in new_uids.chunks(HEADER_BATCH) {
            let headers = backend.fetch_headers(&folder.name, chunk).await?;
            let items: Vec<_> = headers
                .into_iter()
                .map(|h| {
                    let env = parse_envelope(&h.header, h.internal_date);
                    (h, env)
                })
                .collect();
            self.store.insert_headers(folder.id, &items)?;
            done += chunk.len() as u32;
            self.emit(SyncEvent::Progress { folder: folder.id, phase: SyncPhase::Headers, done, total });
            self.emit(SyncEvent::FolderChanged(folder.id));
        }

        // Flags of messages we already had. New ones came with their flags.
        let modseq_unchanged = caps.condstore && st.highestmodseq.is_some() && folder.highestmodseq == st.highestmodseq;
        if !local.is_empty() && !modseq_unchanged {
            let since = if caps.condstore { folder.highestmodseq } else { None };
            let flags = backend.fetch_flags(&folder.name, since).await?;
            if self.store.apply_remote_flags(folder.id, &flags)? > 0 {
                self.emit(SyncEvent::FolderChanged(folder.id));
            }
        }

        self.store.set_folder_state(folder.id, st.uidvalidity, st.uidnext, st.highestmodseq)?;
        Ok(())
    }

    /// Download missing bodies of one folder, newest first, in batches. Each
    /// batch is flushed to disk once before the index references it.
    pub async fn fetch_bodies(&self, backend: &mut dyn MailBackend, folder: &Folder) -> Result<()> {
        let total = self.store.count_missing_bodies(folder.id)?;
        if total == 0 {
            return Ok(());
        }
        backend.select(&folder.name).await?;
        let mut done = 0;
        loop {
            let missing = self.store.missing_bodies(folder.id, BODY_BATCH)?;
            if missing.is_empty() {
                break;
            }
            let uids: Vec<u32> = missing.iter().map(|(_, u)| *u).collect();
            let bodies = backend.fetch_bodies(&folder.name, &uids).await?;
            let mut stored = Vec::with_capacity(bodies.len());
            for (uid, raw) in &bodies {
                stored.push((*uid, self.blobs.put(raw)?));
            }
            self.blobs.sync()?;
            self.store.set_blobs(folder.id, &stored)?;
            // Messages that vanished between header and body fetch would loop
            // forever; drop them from this pass.
            let got: HashSet<u32> = bodies.iter().map(|(u, _)| *u).collect();
            let vanished: Vec<u32> = uids.iter().copied().filter(|u| !got.contains(u)).collect();
            if !vanished.is_empty() {
                self.store.remove_uids(folder.id, &vanished)?;
            }
            done += uids.len() as u32;
            self.emit(SyncEvent::Progress {
                folder: folder.id,
                phase: SyncPhase::Bodies,
                done: done.min(total),
                total,
            });
        }
        Ok(())
    }

    /// Full account sync: replay queued ops, refresh folders, index every
    /// folder, then download bodies.
    pub async fn sync_account(&self, backend: &mut dyn MailBackend) -> Result<crate::ops::ReplayReport> {
        let report = crate::ops::replay(self.store, self.blobs, backend).await?;
        self.sync_folder_list(backend).await?;
        let folders = self.sync_order()?;
        for f in &folders {
            self.sync_folder(backend, f).await?;
        }
        for f in &folders {
            self.fetch_bodies(backend, f).await?;
        }
        Ok(report)
    }
}

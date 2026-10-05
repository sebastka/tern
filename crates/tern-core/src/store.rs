//! Per-account SQLite index (ARCHITECTURE.md §8): folders, message envelopes,
//! flags, sync state and the offline operation queue.
//!
//! One connection behind a mutex. SQLite runs in WAL mode; all transactions
//! are short, so the UI thread can read while sync writes between batches.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::Result;
use crate::model::*;
use crate::ops::PendingOp;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = r#"
CREATE TABLE folders (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE,
    delimiter     TEXT,
    role          TEXT,
    selectable    INTEGER NOT NULL DEFAULT 1,
    uidvalidity   INTEGER,
    uidnext       INTEGER,
    highestmodseq INTEGER
);

CREATE TABLE messages (
    id            INTEGER PRIMARY KEY,
    folder_id     INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
    uid           INTEGER,             -- NULL: created locally, not yet on the server
    blob          TEXT,                -- NULL: body not downloaded yet
    flags         INTEGER NOT NULL DEFAULT 0,
    keywords      TEXT NOT NULL DEFAULT '',
    modseq        INTEGER,
    size          INTEGER NOT NULL DEFAULT 0,
    date          INTEGER NOT NULL DEFAULT 0,
    from_json     TEXT NOT NULL DEFAULT '[]',
    to_json       TEXT NOT NULL DEFAULT '[]',
    cc_json       TEXT NOT NULL DEFAULT '[]',
    from_text     TEXT NOT NULL DEFAULT '',
    to_text       TEXT NOT NULL DEFAULT '',
    subject       TEXT NOT NULL DEFAULT '',
    message_id    TEXT,
    in_reply_to   TEXT,
    refs          TEXT NOT NULL DEFAULT '',
    has_attachments INTEGER NOT NULL DEFAULT 0,
    encrypted     INTEGER NOT NULL DEFAULT 0,
    local_change  INTEGER NOT NULL DEFAULT 0  -- number of queued ops touching it
);
CREATE UNIQUE INDEX messages_folder_uid ON messages(folder_id, uid) WHERE uid IS NOT NULL;
CREATE INDEX messages_folder_date ON messages(folder_id, date DESC, id DESC);
CREATE INDEX messages_msgid ON messages(message_id);
CREATE INDEX messages_blob ON messages(blob);

CREATE TABLE pending_ops (
    id         INTEGER PRIMARY KEY,
    created    INTEGER NOT NULL,
    op         TEXT NOT NULL,          -- JSON PendingOp
    attempts   INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
);

-- Fast path search over envelope fields (§13).
CREATE VIRTUAL TABLE messages_fts USING fts5(
    subject, from_text, to_text, content='messages', content_rowid='id',
    tokenize='unicode61 remove_diacritics 2'
);
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
    INSERT INTO messages_fts(rowid, subject, from_text, to_text)
    VALUES (new.id, new.subject, new.from_text, new.to_text);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, subject, from_text, to_text)
    VALUES ('delete', old.id, old.subject, old.from_text, old.to_text);
END;
CREATE TRIGGER messages_au AFTER UPDATE OF subject, from_text, to_text ON messages BEGIN
    INSERT INTO messages_fts(messages_fts, rowid, subject, from_text, to_text)
    VALUES ('delete', old.id, old.subject, old.from_text, old.to_text);
    INSERT INTO messages_fts(rowid, subject, from_text, to_text)
    VALUES (new.id, new.subject, new.from_text, new.to_text);
END;
"#;

/// Columns selected for [`MessageSummary`], in [`summary_from_row`] order.
const SUMMARY_COLS: &str = "id, folder_id, uid, blob, flags, size, date, from_json, to_json, \
    cc_json, subject, message_id, in_reply_to, refs, has_attachments, encrypted";

/// Inputs for threading one message.
#[derive(Debug, Clone)]
pub struct ThreadInput {
    pub id: MessageId,
    pub date: i64,
    pub subject: String,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

/// Sortable fields of a message (see [`Store::sort_inputs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortInput {
    pub id: MessageId,
    pub date: i64,
    pub subject: String,
    /// Senders as indexed for search: first name (or address) first.
    pub from: String,
    /// To and Cc recipients, same format.
    pub to: String,
    pub size: u32,
    pub flags: Flags,
    pub has_attachments: bool,
    pub encrypted: bool,
}

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"))?;
        } else if version > SCHEMA_VERSION {
            return Err(crate::Error::Other(format!(
                "store schema version {version} is newer than this build supports ({SCHEMA_VERSION})"
            )));
        }
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave SQLite inconsistent
        // (transactions roll back on drop), so poisoning is ignored.
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    // ---------------------------------------------------------------- folders

    /// Make the folder table match the server list. Returns true if anything
    /// changed. Folders missing on the server are deleted with their messages.
    pub fn sync_folder_list(&self, remote: &[RemoteFolder]) -> Result<bool> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut changed = false;
        let existing: HashMap<String, (Option<String>, Option<String>, bool)> = {
            let mut st = tx.prepare("SELECT name, delimiter, role, selectable FROM folders")?;
            st.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?, r.get(3)?))))?
                .collect::<rusqlite::Result<_>>()?
        };
        for f in remote {
            let role = f.role.map(|r| r.as_str().to_owned());
            let new = (f.delimiter.clone(), role.clone(), f.selectable);
            match existing.get(&f.name) {
                Some(old) if *old == new => {}
                Some(_) => {
                    tx.execute(
                        "UPDATE folders SET delimiter = ?2, role = ?3, selectable = ?4 WHERE name = ?1",
                        params![f.name, f.delimiter, role, f.selectable],
                    )?;
                    changed = true;
                }
                None => {
                    tx.execute(
                        "INSERT INTO folders (name, delimiter, role, selectable) VALUES (?1, ?2, ?3, ?4)",
                        params![f.name, f.delimiter, role, f.selectable],
                    )?;
                    changed = true;
                }
            }
        }
        let remote_names: HashSet<&str> = remote.iter().map(|f| f.name.as_str()).collect();
        for name in existing.keys().filter(|n| !remote_names.contains(n.as_str())) {
            tx.execute("DELETE FROM folders WHERE name = ?1", [name])?;
            changed = true;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Add a folder that exists only locally so far (its creation on the
    /// server is queued). Returns its id.
    pub fn create_folder_local(&self, name: &str, delimiter: Option<&str>) -> Result<FolderId> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR IGNORE INTO folders (name, delimiter, role, selectable) VALUES (?1, ?2, NULL, 1)",
            params![name, delimiter],
        )?;
        Ok(conn.query_row("SELECT id FROM folders WHERE name = ?1", [name], |r| r.get(0))?)
    }

    pub fn folders(&self) -> Result<Vec<Folder>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT f.id, f.name, f.delimiter, f.role, f.selectable, f.uidvalidity, f.uidnext,
                    f.highestmodseq,
                    (SELECT count(*) FROM messages m WHERE m.folder_id = f.id AND (m.flags & 8) = 0),
                    (SELECT count(*) FROM messages m WHERE m.folder_id = f.id AND (m.flags & 9) = 0)
             FROM folders f ORDER BY f.name",
        )?;
        let rows = st.query_map([], folder_from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    }

    pub fn folder(&self, id: FolderId) -> Result<Option<Folder>> {
        Ok(self.folders()?.into_iter().find(|f| f.id == id))
    }

    pub fn folder_by_name(&self, name: &str) -> Result<Option<Folder>> {
        Ok(self.folders()?.into_iter().find(|f| f.name == name))
    }

    pub fn folder_by_role(&self, role: FolderRole) -> Result<Option<Folder>> {
        Ok(self.folders()?.into_iter().find(|f| f.role == Some(role)))
    }

    pub fn set_folder_state(
        &self,
        id: FolderId,
        uidvalidity: u32,
        uidnext: Option<u32>,
        highestmodseq: Option<u64>,
    ) -> Result<()> {
        self.conn().execute(
            "UPDATE folders SET uidvalidity = ?2, uidnext = ?3, highestmodseq = ?4 WHERE id = ?1",
            params![id, uidvalidity, uidnext, highestmodseq.map(|m| m as i64)],
        )?;
        Ok(())
    }

    /// UIDVALIDITY changed: forget all server-side messages of the folder.
    /// Messages created locally (no UID yet) are kept.
    pub fn reset_folder(&self, id: FolderId) -> Result<()> {
        let conn = self.conn();
        conn.execute("DELETE FROM messages WHERE folder_id = ?1 AND uid IS NOT NULL", [id])?;
        conn.execute(
            "UPDATE folders SET uidvalidity = NULL, uidnext = NULL, highestmodseq = NULL WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }

    // --------------------------------------------------------------- messages

    /// UIDs of messages known locally in a folder, ascending.
    pub fn uids(&self, folder: FolderId) -> Result<Vec<u32>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT uid FROM messages WHERE folder_id = ?1 AND uid IS NOT NULL ORDER BY uid")?;
        let v = st.query_map([folder], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    /// Insert header-phase results. Messages that already exist (same UID) are
    /// skipped. A local copy without UID and with the same Message-ID (from an
    /// offline move or append) is adopted instead of duplicated.
    pub fn insert_headers(&self, folder: FolderId, items: &[(RemoteHeader, Envelope)]) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut inserted = 0;
        for (h, env) in items {
            if let Some(mid) = &env.message_id {
                let adopted = tx.execute(
                    "UPDATE messages SET uid = ?3, flags = ?4, keywords = ?5, modseq = ?6, size = ?7,
                         local_change = 0
                     WHERE id = (SELECT id FROM messages
                                 WHERE folder_id = ?1 AND uid IS NULL AND message_id = ?2 LIMIT 1)",
                    params![folder, mid, h.uid, h.flags.0, h.keywords.join(" "), h.modseq.map(|m| m as i64), h.size],
                )?;
                if adopted > 0 {
                    continue;
                }
            }
            inserted += insert_message(&tx, folder, Some(h.uid), None, h.flags, &h.keywords, h.modseq, h.size, env)?;
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// Insert a message that exists only locally so far (sent copy, offline
    /// append). Returns its id.
    pub fn insert_local(
        &self,
        folder: FolderId,
        blob: &str,
        flags: Flags,
        size: u32,
        env: &Envelope,
    ) -> Result<MessageId> {
        let conn = self.conn();
        insert_message(&conn, folder, None, Some(blob), flags, &[], None, size, env)?;
        let id = conn.last_insert_rowid();
        conn.execute("UPDATE messages SET local_change = 1 WHERE id = ?1", [id])?;
        Ok(id)
    }

    /// Remove messages expunged on the server. Returns the number removed.
    pub fn remove_uids(&self, folder: FolderId, uids: &[u32]) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut n = 0;
        {
            let mut st = tx.prepare("DELETE FROM messages WHERE folder_id = ?1 AND uid = ?2")?;
            for uid in uids {
                n += st.execute(params![folder, uid])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Apply server flag state (server wins), except for messages with a
    /// pending local change. Returns the number of rows changed.
    pub fn apply_remote_flags(&self, folder: FolderId, flags: &[RemoteFlags]) -> Result<usize> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let mut n = 0;
        {
            let mut st = tx.prepare(
                "UPDATE messages SET flags = ?3, keywords = ?4, modseq = coalesce(?5, modseq)
                 WHERE folder_id = ?1 AND uid = ?2 AND local_change = 0
                   AND (flags != ?3 OR keywords != ?4)",
            )?;
            for f in flags {
                n += st.execute(params![folder, f.uid, f.flags.0, f.keywords.join(" "), f.modseq.map(|m| m as i64)])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Messages whose body isn't downloaded yet, newest first.
    pub fn missing_bodies(&self, folder: FolderId, limit: usize) -> Result<Vec<(MessageId, u32)>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, uid FROM messages WHERE folder_id = ?1 AND blob IS NULL AND uid IS NOT NULL
             ORDER BY date DESC LIMIT ?2",
        )?;
        let v = st
            .query_map(params![folder, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn count_missing_bodies(&self, folder: FolderId) -> Result<u32> {
        Ok(self.conn().query_row(
            "SELECT count(*) FROM messages WHERE folder_id = ?1 AND blob IS NULL AND uid IS NOT NULL",
            [folder],
            |r| r.get(0),
        )?)
    }

    /// Record downloaded bodies (blobs must already be synced to disk).
    pub fn set_blobs(&self, folder: FolderId, items: &[(u32, String)]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare("UPDATE messages SET blob = ?3 WHERE folder_id = ?1 AND uid = ?2")?;
            for (uid, blob) in items {
                st.execute(params![folder, uid, blob])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn message(&self, id: MessageId) -> Result<Option<MessageSummary>> {
        let conn = self.conn();
        Ok(conn
            .query_row(&format!("SELECT {SUMMARY_COLS} FROM messages WHERE id = ?1"), [id], summary_from_row)
            .optional()?)
    }

    pub fn messages(&self, ids: &[MessageId]) -> Result<Vec<MessageSummary>> {
        ids.iter().filter_map(|&id| self.message(id).transpose()).collect()
    }

    pub fn count(&self, folder: FolderId) -> Result<u32> {
        Ok(self.conn().query_row("SELECT count(*) FROM messages WHERE folder_id = ?1", [folder], |r| r.get(0))?)
    }

    /// Message ids of a folder, newest first. This is the flat list order.
    pub fn ids_by_date(&self, folder: FolderId) -> Result<Vec<MessageId>> {
        let conn = self.conn();
        let mut st = conn
            .prepare("SELECT id FROM messages WHERE folder_id = ?1 AND (flags & 8) = 0 ORDER BY date DESC, id DESC")?;
        let v = st.query_map([folder], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn thread_inputs(&self, folder: FolderId) -> Result<Vec<ThreadInput>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, date, subject, message_id, in_reply_to, refs FROM messages
             WHERE folder_id = ?1 AND (flags & 8) = 0",
        )?;
        let v = st
            .query_map([folder], |r| {
                let refs: String = r.get(5)?;
                Ok(ThreadInput {
                    id: r.get(0)?,
                    date: r.get(1)?,
                    subject: r.get(2)?,
                    message_id: r.get(3)?,
                    in_reply_to: r.get(4)?,
                    references: refs.split_whitespace().map(str::to_owned).collect(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    /// The fields message lists can be sorted by, for all messages of a
    /// folder.
    pub fn sort_inputs(&self, folder: FolderId) -> Result<Vec<SortInput>> {
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT id, date, subject, from_text, to_text, size, flags, has_attachments, encrypted FROM messages
             WHERE folder_id = ?1 AND (flags & 8) = 0",
        )?;
        let v = st
            .query_map([folder], |r| {
                Ok(SortInput {
                    id: r.get(0)?,
                    date: r.get(1)?,
                    subject: r.get(2)?,
                    from: r.get(3)?,
                    to: r.get(4)?,
                    size: r.get(5)?,
                    flags: Flags(r.get(6)?),
                    has_attachments: r.get(7)?,
                    encrypted: r.get(8)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    /// Full-text search over subject/from/to (FTS5). `folder` restricts the
    /// result. Results are newest first.
    pub fn search(&self, folder: Option<FolderId>, query: &str, limit: usize) -> Result<Vec<MessageId>> {
        let q = fts_query(query);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let mut st = conn.prepare(
            "SELECT m.id FROM messages_fts f JOIN messages m ON m.id = f.rowid
             WHERE messages_fts MATCH ?1 AND (?2 IS NULL OR m.folder_id = ?2) AND (m.flags & 8) = 0
             ORDER BY m.date DESC LIMIT ?3",
        )?;
        let v = st.query_map(params![q, folder, limit as i64], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    pub fn referenced_blobs(&self) -> Result<HashSet<String>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT DISTINCT blob FROM messages WHERE blob IS NOT NULL")?;
        let v = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(v)
    }

    // ------------------------------------------------- local (offline) changes

    /// Apply a flag change locally and mark the rows dirty.
    pub fn set_flags_local(&self, ids: &[MessageId], add: Flags, remove: Flags) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare(
                "UPDATE messages SET flags = (flags | ?2) & ~?3, local_change = local_change + 1 WHERE id = ?1",
            )?;
            for id in ids {
                st.execute(params![id, add.0, remove.0])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Move rows to another folder locally. They lose their UID until the
    /// server reports it (COPYUID or the next sync of the target).
    pub fn move_local(&self, ids: &[MessageId], to: FolderId) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare(
                "UPDATE messages SET folder_id = ?2, uid = NULL, modseq = NULL, local_change = local_change + 1
                 WHERE id = ?1",
            )?;
            for id in ids {
                st.execute(params![id, to])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_local(&self, ids: &[MessageId]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare("DELETE FROM messages WHERE id = ?1")?;
            for id in ids {
                st.execute([id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// After a successful replay: set the new UID (if known) and clear the
    /// dirty mark.
    pub fn confirm_local(&self, id: MessageId, new_uid: Option<u32>) -> Result<()> {
        let conn = self.conn();
        match new_uid {
            Some(uid) => {
                let n = conn.execute(
                    "UPDATE OR IGNORE messages SET uid = ?2, local_change = max(local_change - 1, 0) WHERE id = ?1",
                    params![id, uid],
                )?;
                if n == 0 {
                    // A sync already inserted the server copy under that UID;
                    // the local placeholder is redundant.
                    conn.execute("DELETE FROM messages WHERE id = ?1 AND uid IS NULL", [id])?;
                }
            }
            None => {
                conn.execute("UPDATE messages SET local_change = max(local_change - 1, 0) WHERE id = ?1", [id])?;
            }
        }
        Ok(())
    }

    /// Remove local-only copies (no UID) of the given messages.
    pub fn delete_placeholders(&self, ids: &[MessageId]) -> Result<()> {
        let conn = self.conn();
        for id in ids {
            conn.execute("DELETE FROM messages WHERE id = ?1 AND uid IS NULL", [id])?;
        }
        Ok(())
    }

    /// Forget a folder's HIGHESTMODSEQ so the next sync fetches all flags.
    pub fn forget_modseq(&self, folder: &str) -> Result<()> {
        self.conn().execute("UPDATE folders SET highestmodseq = NULL WHERE name = ?1", [folder])?;
        Ok(())
    }

    // ------------------------------------------------------------ pending ops

    pub fn push_op(&self, op: &PendingOp) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO pending_ops (created, op) VALUES (strftime('%s','now'), ?1)",
            [serde_json::to_string(op)?],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Pending ops in insertion order.
    pub fn pending_ops(&self) -> Result<Vec<(i64, PendingOp)>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, op FROM pending_ops ORDER BY id")?;
        let rows: Vec<(i64, String)> =
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        rows.into_iter().map(|(id, json)| Ok((id, serde_json::from_str(&json)?))).collect()
    }

    pub fn remove_op(&self, id: i64) -> Result<()> {
        self.conn().execute("DELETE FROM pending_ops WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Record a failed attempt. Returns the new attempt count.
    pub fn op_failed(&self, id: i64, error: &str) -> Result<u32> {
        let conn = self.conn();
        conn.execute(
            "UPDATE pending_ops SET attempts = attempts + 1, last_error = ?2 WHERE id = ?1",
            params![id, error],
        )?;
        Ok(conn.query_row("SELECT attempts FROM pending_ops WHERE id = ?1", [id], |r| r.get(0))?)
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_message(
    conn: &Connection,
    folder: FolderId,
    uid: Option<u32>,
    blob: Option<&str>,
    flags: Flags,
    keywords: &[String],
    modseq: Option<u64>,
    size: u32,
    env: &Envelope,
) -> Result<usize> {
    let text = |list: &[Address]| {
        list.iter()
            .map(|a| match &a.name {
                Some(n) => format!("{n} {}", a.email),
                None => a.email.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let to_and_cc: Vec<Address> = env.to.iter().chain(&env.cc).cloned().collect();
    Ok(conn.execute(
        "INSERT OR IGNORE INTO messages
         (folder_id, uid, blob, flags, keywords, modseq, size, date, from_json, to_json, cc_json,
          from_text, to_text, subject, message_id, in_reply_to, refs, has_attachments, encrypted)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            folder,
            uid,
            blob,
            flags.0,
            keywords.join(" "),
            modseq.map(|m| m as i64),
            size,
            env.date,
            serde_json::to_string(&env.from)?,
            serde_json::to_string(&env.to)?,
            serde_json::to_string(&env.cc)?,
            text(&env.from),
            text(&to_and_cc),
            env.subject,
            env.message_id,
            env.in_reply_to,
            env.references.join(" "),
            env.has_attachments,
            env.encrypted,
        ],
    )?)
}

fn folder_from_row(r: &Row<'_>) -> rusqlite::Result<Folder> {
    let role: Option<String> = r.get(3)?;
    Ok(Folder {
        id: r.get(0)?,
        name: r.get(1)?,
        delimiter: r.get(2)?,
        role: role.as_deref().and_then(FolderRole::parse),
        selectable: r.get(4)?,
        uidvalidity: r.get(5)?,
        uidnext: r.get(6)?,
        highestmodseq: r.get::<_, Option<i64>>(7)?.map(|m| m as u64),
        total: r.get(8)?,
        unread: r.get(9)?,
    })
}

fn summary_from_row(r: &Row<'_>) -> rusqlite::Result<MessageSummary> {
    let json = |i: usize| -> rusqlite::Result<Vec<Address>> {
        let s: String = r.get(i)?;
        Ok(serde_json::from_str(&s).unwrap_or_default())
    };
    let refs: String = r.get(13)?;
    Ok(MessageSummary {
        id: r.get(0)?,
        folder_id: r.get(1)?,
        uid: r.get(2)?,
        blob: r.get(3)?,
        flags: Flags(r.get(4)?),
        size: r.get(5)?,
        envelope: Envelope {
            date: r.get(6)?,
            from: json(7)?,
            to: json(8)?,
            cc: json(9)?,
            subject: r.get(10)?,
            message_id: r.get(11)?,
            in_reply_to: r.get(12)?,
            references: refs.split_whitespace().map(str::to_owned).collect(),
            has_attachments: r.get(14)?,
            encrypted: r.get(15)?,
        },
    })
}

/// Turn user input into a safe FTS5 query: every word becomes a quoted prefix
/// term, all terms must match.
fn fts_query(input: &str) -> String {
    input
        .split_whitespace()
        .map(|w| w.replace('"', ""))
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, role: Option<FolderRole>) -> RemoteFolder {
        RemoteFolder { name: name.into(), delimiter: Some("/".into()), role, selectable: true }
    }

    fn header(uid: u32, mid: &str, subject: &str) -> (RemoteHeader, Envelope) {
        (
            RemoteHeader {
                uid,
                flags: Flags::empty(),
                keywords: vec![],
                size: 100,
                modseq: Some(uid as u64),
                internal_date: None,
                header: vec![],
            },
            Envelope {
                date: uid as i64 * 1000,
                subject: subject.into(),
                message_id: Some(mid.into()),
                from: vec![Address { name: Some("Ann Example".into()), email: "ann@example.org".into() }],
                ..Default::default()
            },
        )
    }

    #[test]
    fn folder_list_sync() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.sync_folder_list(&[folder("INBOX", Some(FolderRole::Inbox)), folder("Old", None)]).unwrap());
        assert!(!s.sync_folder_list(&[folder("INBOX", Some(FolderRole::Inbox)), folder("Old", None)]).unwrap());
        assert!(s.sync_folder_list(&[folder("INBOX", Some(FolderRole::Inbox))]).unwrap());
        let f = s.folders().unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(s.folder_by_role(FolderRole::Inbox).unwrap().unwrap().name, "INBOX");
    }

    #[test]
    fn messages_flags_and_search() {
        let s = Store::open_in_memory().unwrap();
        s.sync_folder_list(&[folder("INBOX", None)]).unwrap();
        let inbox = s.folder_by_name("INBOX").unwrap().unwrap().id;
        let items = vec![header(1, "a@x", "Quarterly report"), header(2, "b@x", "Lunch?")];
        assert_eq!(s.insert_headers(inbox, &items).unwrap(), 2);
        assert_eq!(s.insert_headers(inbox, &items).unwrap(), 0);
        assert_eq!(s.uids(inbox).unwrap(), vec![1, 2]);
        assert_eq!(s.folders().unwrap()[0].unread, 2);

        let ids = s.ids_by_date(inbox).unwrap();
        assert_eq!(ids.len(), 2);
        let newest = s.message(ids[0]).unwrap().unwrap();
        assert_eq!(newest.envelope.subject, "Lunch?");

        // Local change wins until replayed; then the server state applies.
        s.set_flags_local(&[newest.id], Flags::SEEN, Flags::empty()).unwrap();
        let remote = [RemoteFlags { uid: 2, flags: Flags::empty(), keywords: vec![], modseq: None }];
        assert_eq!(s.apply_remote_flags(inbox, &remote).unwrap(), 0);
        s.confirm_local(newest.id, None).unwrap();
        assert_eq!(s.apply_remote_flags(inbox, &remote).unwrap(), 1);

        assert_eq!(s.search(Some(inbox), "quart", 10).unwrap().len(), 1);
        assert_eq!(s.search(None, "ann", 10).unwrap().len(), 2);
        assert_eq!(s.search(None, "\"", 10).unwrap().len(), 0);

        assert_eq!(s.remove_uids(inbox, &[1]).unwrap(), 1);
        assert_eq!(s.search(None, "quarterly", 10).unwrap().len(), 0);
    }

    #[test]
    fn local_move_is_adopted_by_message_id() {
        let s = Store::open_in_memory().unwrap();
        s.sync_folder_list(&[folder("INBOX", None), folder("Archive", None)]).unwrap();
        let inbox = s.folder_by_name("INBOX").unwrap().unwrap().id;
        let archive = s.folder_by_name("Archive").unwrap().unwrap().id;
        s.insert_headers(inbox, &[header(5, "m@x", "Hi")]).unwrap();
        let id = s.ids_by_date(inbox).unwrap()[0];
        s.move_local(&[id], archive).unwrap();
        assert!(s.uids(archive).unwrap().is_empty());
        // Next sync of Archive sees the message with a server UID.
        assert_eq!(s.insert_headers(archive, &[header(77, "m@x", "Hi")]).unwrap(), 0);
        assert_eq!(s.uids(archive).unwrap(), vec![77]);
        assert_eq!(s.message(id).unwrap().unwrap().uid, Some(77));
    }
}

//! Event hub: background tasks report changes here; a ticker coalesces them
//! into frontend events, so a sync that touches a folder hundreds of times
//! produces a few list refreshes, not hundreds.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crate::types::{AccountState, Event, MessageKey};

pub type Sink = Arc<dyn Fn(Event) + Send + Sync>;

#[derive(Default)]
pub struct Dirty {
    pub tree: bool,
    pub folders: BTreeSet<(String, i64)>,
    /// (account, folder label) → (done, total)
    pub progress: BTreeMap<(String, String), (u32, u32)>,
    pub bodies: Vec<MessageKey>,
}

pub struct Hub {
    pub sink: Sink,
    dirty: Mutex<Dirty>,
}

impl Hub {
    pub fn new(sink: Sink) -> Self {
        Self { sink, dirty: Mutex::new(Dirty::default()) }
    }

    fn with(&self, f: impl FnOnce(&mut Dirty)) {
        f(&mut self.dirty.lock().unwrap_or_else(|p| p.into_inner()));
    }

    pub fn take(&self) -> Dirty {
        std::mem::take(&mut self.dirty.lock().unwrap_or_else(|p| p.into_inner()))
    }

    pub fn emit(&self, e: Event) {
        (self.sink)(e);
    }

    pub fn tree_dirty(&self) {
        self.with(|d| d.tree = true);
    }

    /// Messages of a folder changed (counts too).
    pub fn folder_dirty(&self, account: &str, folder: i64) {
        self.with(|d| {
            d.tree = true;
            d.folders.insert((account.to_owned(), folder));
        });
    }

    pub fn progress(&self, account: &str, folder: String, done: u32, total: u32) {
        self.with(|d| {
            d.progress.insert((account.to_owned(), folder), (done, total));
        });
    }

    /// Signal the end of a sync pass (clears the progress display).
    pub fn progress_done(&self, account: &str) {
        self.with(|d| {
            d.progress.insert((account.to_owned(), String::new()), (0, 0));
        });
    }

    pub fn body_ready(&self, key: MessageKey) {
        self.with(|d| d.bodies.push(key));
    }

    pub fn account_status(&self, account: &str, state: AccountState, text: String) {
        self.emit(Event::AccountStatus { account: account.to_owned(), state, text });
    }

    pub fn sent(&self, ok: bool, text: String) {
        self.emit(Event::SendResult { ok, text });
    }

    pub fn error(&self, text: String) {
        self.emit(Event::Error { text });
    }
}

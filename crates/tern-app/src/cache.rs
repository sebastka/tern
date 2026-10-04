//! Rendered messages kept in memory, bounded by size (`[memory]
//! message_cache_mb`). Decrypted content only ever lives here.

use std::sync::Arc;

use crate::render::Rendered;
use crate::types::MessageKey;

impl Rendered {
    /// Approximate heap size: documents, text, attachments, inline parts.
    pub fn approx_bytes(&self) -> usize {
        let strings = [&self.subject, &self.from, &self.to, &self.cc, &self.text_doc, &self.text]
            .iter()
            .map(|s| s.len())
            .sum::<usize>();
        strings
            + self.html_doc.as_ref().map_or(0, String::len)
            + self.attachments.iter().map(|(a, d)| d.len() + a.filename.len() + a.content_type.len()).sum::<usize>()
            + self.cids.iter().map(|(k, (t, d))| k.len() + t.len() + d.len()).sum::<usize>()
    }
}

#[derive(Default)]
pub struct RenderCache {
    /// Least recently inserted first.
    entries: Vec<(MessageKey, Arc<Rendered>, usize)>,
}

impl RenderCache {
    pub fn get(&self, key: &MessageKey) -> Option<Arc<Rendered>> {
        self.entries.iter().find(|(k, _, _)| k == key).map(|(_, r, _)| r.clone())
    }

    pub fn bytes(&self) -> usize {
        self.entries.iter().map(|(_, _, b)| b).sum()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Insert (or refresh) a message, then evict the oldest entries until
    /// the cache fits `limit` bytes. The inserted message and `keep` (the
    /// one on screen) are never evicted, even if they alone exceed it.
    pub fn insert(&mut self, key: MessageKey, r: Arc<Rendered>, limit: usize, keep: Option<&MessageKey>) {
        self.entries.retain(|(k, _, _)| *k != key);
        let size = r.approx_bytes();
        self.entries.push((key.clone(), r, size));
        while self.bytes() > limit {
            let Some(i) = self.entries.iter().position(|(k, _, _)| *k != key && Some(k) != keep) else {
                break;
            };
            self.entries.remove(i);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(bytes: usize) -> Arc<Rendered> {
        Arc::new(Rendered { text: "x".repeat(bytes), ..Default::default() })
    }

    fn key(id: i64) -> MessageKey {
        MessageKey { account: "a".into(), id }
    }

    #[test]
    fn evicts_oldest_by_size() {
        let mut c = RenderCache::default();
        for id in 1..=5 {
            c.insert(key(id), rendered(100), 300, None);
        }
        assert_eq!(c.len(), 3);
        assert!(c.get(&key(1)).is_none() && c.get(&key(2)).is_none());
        assert!(c.get(&key(5)).is_some());
        assert!(c.bytes() <= 300);
    }

    #[test]
    fn keeps_current_and_new_even_if_too_big() {
        let mut c = RenderCache::default();
        c.insert(key(1), rendered(100), 300, None);
        c.insert(key(2), rendered(100), 300, Some(&key(1)));
        // A huge message: everything else goes, except the one on screen.
        c.insert(key(3), rendered(10_000), 300, Some(&key(1)));
        assert!(c.get(&key(1)).is_some() && c.get(&key(3)).is_some());
        assert!(c.get(&key(2)).is_none());
        // Re-inserting refreshes instead of duplicating.
        c.insert(key(3), rendered(50), 300, None);
        assert_eq!(c.len(), 2);
    }
}

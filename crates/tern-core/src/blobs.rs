//! Immutable, content-addressed message blobs (ARCHITECTURE.md §8).
//!
//! `blobs/<hh>/<blake3-hex>.eml.zst` (or `.eml` when compression is off). The
//! hash is over the *uncompressed* bytes, so the same message in two folders
//! is stored once regardless of compression settings.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone)]
pub struct BlobStore {
    root: PathBuf,
    /// zstd level, or `None` to store plain `.eml`.
    compression: Option<i32>,
}

/// BLAKE3 of the uncompressed bytes, lowercase hex.
pub type BlobHash = String;

pub fn hash(raw: &[u8]) -> BlobHash {
    blake3::hash(raw).to_hex().to_string()
}

fn valid_hash(h: &str) -> bool {
    h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

impl BlobStore {
    pub fn open(root: impl Into<PathBuf>, compression: Option<i32>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join("tmp"))?;
        Ok(Self { root, compression })
    }

    fn dir(&self, h: &str) -> PathBuf {
        self.root.join(&h[..2])
    }

    fn path_zst(&self, h: &str) -> PathBuf {
        self.dir(h).join(format!("{h}.eml.zst"))
    }

    fn path_plain(&self, h: &str) -> PathBuf {
        self.dir(h).join(format!("{h}.eml"))
    }

    pub fn contains(&self, h: &str) -> bool {
        valid_hash(h) && (self.path_zst(h).exists() || self.path_plain(h).exists())
    }

    /// Store `raw` and return its hash. Not durable until [`Self::sync`] is
    /// called: callers write a batch of blobs, sync once, then commit the
    /// SQLite transaction that references them.
    pub fn put(&self, raw: &[u8]) -> io::Result<BlobHash> {
        let h = hash(raw);
        if self.contains(&h) {
            return Ok(h);
        }
        let (final_path, data): (PathBuf, std::borrow::Cow<'_, [u8]>) = match self.compression {
            Some(level) => (self.path_zst(&h), zstd::bulk::compress(raw, level)?.into()),
            None => (self.path_plain(&h), raw.into()),
        };
        fs::create_dir_all(self.dir(&h))?;
        let tmp = self.root.join("tmp").join(format!("{h}.{}", std::process::id()));
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&data)?;
        }
        fs::rename(&tmp, &final_path)?;
        Ok(h)
    }

    /// Flush all written blobs to disk with a single `syncfs`.
    pub fn sync(&self) -> io::Result<()> {
        let dir = File::open(&self.root)?;
        rustix::fs::syncfs(&dir).map_err(io::Error::from)
    }

    /// Read and decompress a blob.
    pub fn get(&self, h: &str) -> io::Result<Vec<u8>> {
        if !valid_hash(h) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid blob hash"));
        }
        match File::open(self.path_zst(h)) {
            Ok(f) => {
                let mut out = Vec::new();
                zstd::stream::read::Decoder::new(f)?.read_to_end(&mut out)?;
                Ok(out)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => fs::read(self.path_plain(h)),
            Err(e) => Err(e),
        }
    }

    /// Delete blobs not in `referenced` whose files are older than `grace`.
    /// Returns the number of deleted blobs.
    pub fn gc(&self, referenced: &HashSet<String>, grace: Duration) -> io::Result<usize> {
        let cutoff = SystemTime::now() - grace;
        let mut deleted = 0;
        for shard in fs::read_dir(&self.root)?.flatten() {
            let name = shard.file_name();
            let name = name.to_string_lossy();
            if name.len() != 2 || !shard.path().is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path())?.flatten() {
                let path = entry.path();
                let Some(h) = blob_hash_of(&path) else { continue };
                if referenced.contains(h) {
                    continue;
                }
                let old = entry.metadata()?.modified().map(|m| m < cutoff).unwrap_or(false);
                if old {
                    fs::remove_file(&path)?;
                    deleted += 1;
                }
            }
        }
        // Leftovers from crashed writes.
        for entry in fs::read_dir(self.root.join("tmp"))?.flatten() {
            if entry.metadata()?.modified().map(|m| m < cutoff).unwrap_or(false) {
                let _ = fs::remove_file(entry.path());
            }
        }
        Ok(deleted)
    }
}

fn blob_hash_of(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    let h = name.strip_suffix(".eml.zst").or_else(|| name.strip_suffix(".eml"))?;
    valid_hash(h).then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_dedup_and_mixed_formats() {
        let t = tempfile::tempdir().unwrap();
        let zst = BlobStore::open(t.path(), Some(3)).unwrap();
        let raw = b"Subject: hi\r\n\r\nhello hello hello hello\r\n".repeat(20);
        let h = zst.put(&raw).unwrap();
        assert_eq!(zst.put(&raw).unwrap(), h);
        assert_eq!(zst.get(&h).unwrap(), raw);
        zst.sync().unwrap();

        // A store with compression off still reads compressed blobs, and
        // doesn't rewrite them.
        let plain = BlobStore::open(t.path(), None).unwrap();
        assert_eq!(plain.get(&h).unwrap(), raw);
        let h2 = plain.put(b"other").unwrap();
        assert!(plain.path_plain(&h2).exists());
        assert_eq!(zst.get(&h2).unwrap(), b"other");
    }

    #[test]
    fn gc_respects_references_and_grace() {
        let t = tempfile::tempdir().unwrap();
        let s = BlobStore::open(t.path(), Some(1)).unwrap();
        let keep = s.put(b"keep").unwrap();
        let drop = s.put(b"drop").unwrap();
        let refs: HashSet<String> = [keep.clone()].into();
        assert_eq!(s.gc(&refs, Duration::from_secs(3600)).unwrap(), 0);
        assert_eq!(s.gc(&refs, Duration::ZERO).unwrap(), 1);
        assert!(s.contains(&keep));
        assert!(!s.contains(&drop));
        assert!(s.get("../../etc/passwd").is_err());
    }
}

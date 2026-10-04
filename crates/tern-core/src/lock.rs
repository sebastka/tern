//! Single instance per profile (ARCHITECTURE.md §6): an exclusive lock on
//! `$XDG_RUNTIME_DIR/tern/<profile>.lock`, held for the process lifetime.
//! The kernel releases it if the process dies, so stale locks can't happen.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct ProfileLock {
    _file: File,
    path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("profile is already open in another Tern process")]
    Held,
    #[error("cannot create lock file {0}: {1}")]
    Io(PathBuf, io::Error),
}

impl ProfileLock {
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        let io_err = |e| LockError::Io(path.to_owned(), e);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io_err)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            }
        }
        let mut file =
            OpenOptions::new().create(true).truncate(false).read(true).write(true).open(path).map_err(io_err)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(LockError::Held),
            Err(TryLockError::Error(e)) => return Err(io_err(e)),
        }
        // Informational only; the lock itself is what counts.
        file.set_len(0).ok();
        let _ = writeln!(file, "{}", std::process::id());
        Ok(Self { _file: file, path: path.to_owned() })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_fails_until_released() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("run/tern/p.lock");
        let a = ProfileLock::acquire(&p).unwrap();
        assert!(matches!(ProfileLock::acquire(&p), Err(LockError::Held)));
        drop(a);
        ProfileLock::acquire(&p).unwrap();
    }
}

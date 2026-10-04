//! Hot reload: watch the config directory and call back after changes settle.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

/// Keeps the watcher alive; dropping it stops watching.
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
}

const DEBOUNCE: Duration = Duration::from_millis(400);

impl ConfigWatcher {
    /// Watch `config_dir` recursively. `on_change` runs on a background thread
    /// once no further events arrived for a short while (editors often write a
    /// file in several steps).
    pub fn spawn(config_dir: &Path, on_change: impl Fn() + Send + 'static) -> notify::Result<Self> {
        std::fs::create_dir_all(config_dir).ok();
        let (tx, rx) = mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && relevant(&ev.paths)
                && !matches!(ev.kind, notify::EventKind::Access(_))
            {
                let _ = tx.send(());
            }
        })?;
        watcher.watch(config_dir, RecursiveMode::Recursive)?;
        std::thread::Builder::new()
            .name("tern-config-watch".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    // Swallow the burst.
                    while rx.recv_timeout(DEBOUNCE).is_ok() {}
                    on_change();
                }
            })
            .expect("spawn config watcher thread");
        Ok(Self { _watcher: watcher })
    }
}

fn relevant(paths: &[PathBuf]) -> bool {
    // Ignore editor swap/backup files; directories (profile added) count.
    paths.iter().any(|p| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        !(name.starts_with('.') || name.ends_with('~') || name.ends_with(".swp"))
    })
}

//! XDG base directory resolution (ARCHITECTURE.md §7).
//!
//! Implemented directly from the freedesktop spec instead of through a crate,
//! because we need `XDG_RUNTIME_DIR` and `XDG_STATE_HOME`, and want the exact
//! fallback rules of the spec.

use std::path::{Path, PathBuf};

const APP: &str = "tern";

/// Resolved base directories for Tern. All paths already include the `tern`
/// component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dirs {
    pub config: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub runtime: PathBuf,
}

impl Dirs {
    /// Resolve from the process environment.
    pub fn from_env() -> Self {
        Self::resolve(|k| std::env::var_os(k).map(PathBuf::from))
    }

    /// Resolve with a custom variable lookup (used by tests).
    pub fn resolve(var: impl Fn(&str) -> Option<PathBuf>) -> Self {
        let home = var("HOME").unwrap_or_else(|| PathBuf::from("/"));
        // The spec says relative paths are invalid and must be ignored.
        let base = |name: &str, default: &str| -> PathBuf {
            var(name).filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(default)).join(APP)
        };
        let runtime = var("XDG_RUNTIME_DIR").filter(|p| p.is_absolute()).map(|p| p.join(APP)).unwrap_or_else(|| {
            // Fallback when no session runtime dir exists (e.g. ssh without
            // systemd-logind): a per-user directory in the temp dir.
            let user = var("USER").map(|u| u.to_string_lossy().into_owned()).unwrap_or_else(|| "unknown".into());
            std::env::temp_dir().join(format!("{APP}-{user}"))
        });
        Self {
            config: base("XDG_CONFIG_HOME", ".config"),
            data: base("XDG_DATA_HOME", ".local/share"),
            state: base("XDG_STATE_HOME", ".local/state"),
            cache: base("XDG_CACHE_HOME", ".cache"),
            runtime,
        }
    }

    /// A layout rooted in a single directory, for tests and `--root` style
    /// isolated runs.
    pub fn rooted(root: &Path) -> Self {
        Self {
            config: root.join("config").join(APP),
            data: root.join("data").join(APP),
            state: root.join("state").join(APP),
            cache: root.join("cache").join(APP),
            runtime: root.join("runtime").join(APP),
        }
    }

    pub fn profiles_config(&self) -> PathBuf {
        self.config.join("profiles")
    }

    pub fn profile_data(&self, profile: &str) -> PathBuf {
        self.data.join("profiles").join(profile)
    }

    pub fn profile_cache(&self, profile: &str) -> PathBuf {
        self.cache.join("profiles").join(profile)
    }

    pub fn logs(&self) -> PathBuf {
        self.state.join("logs")
    }

    pub fn lock_file(&self, profile: &str) -> PathBuf {
        self.runtime.join(format!("{profile}.lock"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_and_overrides() {
        let env: HashMap<&str, &str> = [
            ("HOME", "/home/u"),
            ("XDG_DATA_HOME", "/data"),
            ("XDG_CACHE_HOME", "relative/ignored"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]
        .into();
        let d = Dirs::resolve(|k| env.get(k).map(PathBuf::from));
        assert_eq!(d.config, PathBuf::from("/home/u/.config/tern"));
        assert_eq!(d.data, PathBuf::from("/data/tern"));
        assert_eq!(d.cache, PathBuf::from("/home/u/.cache/tern"));
        assert_eq!(d.state, PathBuf::from("/home/u/.local/state/tern"));
        assert_eq!(d.lock_file("work"), PathBuf::from("/run/user/1000/tern/work.lock"));
    }
}

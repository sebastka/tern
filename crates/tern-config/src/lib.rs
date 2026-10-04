//! Tern configuration: TOML files under `$XDG_CONFIG_HOME/tern/`, never written
//! by the application (ARCHITECTURE.md §5).

pub mod load;
pub mod model;
pub mod secrets;
pub mod watch;
pub mod xdg;

pub use load::{
    Account, ConfigErrors, ConfigIssue, MAX_SIGNATURE_BYTES, Profile, is_valid_id, list_profiles, load_global,
    load_profile, resolve_path, signature_format,
};
pub use model::*;
pub use watch::ConfigWatcher;
pub use xdg::Dirs;

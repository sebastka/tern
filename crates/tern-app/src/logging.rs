//! Logging with `tracing` to `$XDG_STATE_HOME/tern/logs/` (ARCHITECTURE.md §4).
//! `TERN_LOG` sets the filter (default `info`); `TERN_LOG_STDERR=1` also
//! logs to stderr.

use std::path::Path;
use std::sync::OnceLock;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

static GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

pub fn init(log_dir: &Path) {
    if GUARD.get().is_some() {
        return;
    }
    let _ = std::fs::create_dir_all(log_dir);
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("tern")
        .filename_suffix("log")
        .max_log_files(7)
        .build(log_dir);
    let Ok(appender) = appender else { return };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env("TERN_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = tracing_subscriber::fmt::layer().with_writer(writer).with_ansi(false);
    let stderr_layer =
        std::env::var_os("TERN_LOG_STDERR").map(|_| tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    if tracing_subscriber::registry().with(filter).with(file_layer).with(stderr_layer).try_init().is_ok() {
        let _ = GUARD.set(guard);
    }
}

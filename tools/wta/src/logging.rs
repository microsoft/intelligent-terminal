use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Per-PID helper log file prefix. The `main_helper-{pid}` process label
/// (see `main::process_label`) lands here, e.g. `wta-main_helper-12345.log`.
const HELPER_LOG_PREFIX: &str = "wta-main_helper-";
/// Per-PID helper logs older than this are reclaimed by [`housekeeping`].
const HELPER_RETENTION_DAYS: u64 = 3;
/// Daily-rotated `wta-cli.log` files kept by the appender; older ones are
/// deleted natively by `tracing_appender` (`Builder::max_log_files`).
const CLI_MAX_LOG_FILES: usize = 3;
/// Marker file recording the build version whose logs currently occupy the
/// directory. A mismatch triggers a one-time wipe of the prior build's logs.
const VERSION_MARKER: &str = ".wta-log-version";

/// Returns the default `EnvFilter` directive to use when neither `WTA_LOG` nor
/// `RUST_LOG` is set.
///
/// `debug_assertions` is passed in (rather than read from `cfg!`) so that the
/// release-build branch can be unit-tested even when the test binary itself is
/// compiled in debug mode.
pub(crate) fn default_filter_directive(debug_assertions: bool) -> &'static str {
    if debug_assertions {
        // Verbose for developers iterating on the code.
        "debug"
    } else {
        // Shipping release binaries log at info: enough to follow lifecycle
        // and connection flow out of the box, without the noisy debug traces.
        // Users can still opt into more via `WTA_LOG=debug|trace` / `RUST_LOG`.
        "info"
    }
}

pub fn init(process: &str) -> WorkerGuard {
    // Logs are transient diagnostics → the cache (`LocalCache\Local`) root,
    // not the persistent `LocalState` state root.
    let log_dir = crate::runtime_paths::intelligent_terminal_local_root()
        .map(|r| r.join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("IntelligentTerminal").join("logs"));
    let _ = std::fs::create_dir_all(&log_dir);

    // Reclaim disk BEFORE opening our own appender so a version-upgrade wipe
    // can't race against the file we're about to create.
    housekeeping(&log_dir, process);

    // The short-lived `cli` process is the only high-frequency writer, so it
    // gets daily rotation with native retention; every other process writes a
    // single, never-rotated file (`wta-<process>.log`).
    let (non_blocking, guard) = if process == "cli" {
        let appender = rolling::Builder::new()
            .rotation(rolling::Rotation::DAILY)
            .filename_prefix("wta-cli")
            .filename_suffix("log")
            .max_log_files(CLI_MAX_LOG_FILES)
            .build(&log_dir)
            // Fall back to a single non-rotating file if the builder rejects
            // the directory for any reason — logging must never panic startup.
            .unwrap_or_else(|_| rolling::never(&log_dir, "wta-cli.log"));
        tracing_appender::non_blocking(appender)
    } else {
        let file_name = format!("wta-{process}.log");
        tracing_appender::non_blocking(rolling::never(&log_dir, &file_name))
    };

    let default_level = default_filter_directive(cfg!(debug_assertions));

    let filter = EnvFilter::try_from_env("WTA_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(default_level));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            fmt::layer()
                .with_writer(non_blocking)
                .with_ansi(false)
                .with_target(true)
                .with_timer(fmt::time::SystemTime),
        )
        .init();

    guard
}

/// Filesystem upkeep run once per process at logging init, before our own
/// appender opens.
///
/// Two jobs that no rolling library covers, because they span *distinct
/// filenames* rather than one appender's rotation set:
///
///   1. On a build-version change, delete the previous build's `wta-*.log`.
///   2. Reclaim per-PID helper logs older than [`HELPER_RETENTION_DAYS`].
///
/// Both use plain `remove_file`. This is safe to run from several
/// concurrently-starting processes after an upgrade: `remove_file` is
/// idempotent, and any file still held open by a live process fails to
/// delete on Windows (sharing violation) and is simply skipped.
fn housekeeping(log_dir: &Path, process: &str) {
    version_cleanup(log_dir);
    // Only long-lived / relevant processes scan for stale helper files; the
    // high-frequency `cli` path must not pay a directory scan on every call.
    if process == "main_master" || process.starts_with("main_helper") {
        prune_stale_helper_logs(log_dir);
    }
}

/// On a build-version change (or first run), delete the prior build's
/// `wta-*.log` files, then stamp the directory with the current version.
fn version_cleanup(log_dir: &Path) {
    let current = env!("CARGO_PKG_VERSION");
    let marker = log_dir.join(VERSION_MARKER);
    if std::fs::read_to_string(&marker)
        .ok()
        .as_deref()
        .map(str::trim)
        == Some(current)
    {
        // Common case: same build already owns this directory.
        return;
    }

    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Our own logs all start with `wta-`. Leave `wta-agent-pane.log`
            // alone — it's written by the C++ side, not us.
            if name.starts_with("wta-") && name != "wta-agent-pane.log" {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    let _ = std::fs::write(&marker, current);
}

/// Delete per-PID helper logs whose mtime is older than
/// [`HELPER_RETENTION_DAYS`]. Per-PID filenames (`wta-main_helper-{pid}.log`)
/// accumulate as tabs open/close and are not part of any appender's rotation
/// set, so retention has to be done by hand.
fn prune_stale_helper_logs(log_dir: &Path) {
    let Some(cutoff) = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(HELPER_RETENTION_DAYS * 24 * 60 * 60))
    else {
        return;
    };

    if let Ok(entries) = std::fs::read_dir(log_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with(HELPER_LOG_PREFIX) {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|m| m.modified())
                .map(|mtime| mtime < cutoff)
                .unwrap_or(false);
            if stale {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::filter::LevelFilter;

    #[test]
    fn debug_build_default_is_debug() {
        assert_eq!(default_filter_directive(true), "debug");
    }

    #[test]
    fn release_build_default_is_info() {
        assert_eq!(default_filter_directive(false), "info");
    }

    #[test]
    fn release_default_filter_enables_info() {
        // The EnvFilter built from the release default must enable info (and
        // warn/error), so shipping builds have useful logs without WTA_LOG.
        let filter = EnvFilter::new(default_filter_directive(false));
        assert_eq!(filter.max_level_hint(), Some(LevelFilter::INFO));
    }

    #[test]
    fn debug_default_filter_enables_debug() {
        let filter = EnvFilter::new(default_filter_directive(true));
        assert_eq!(filter.max_level_hint(), Some(LevelFilter::DEBUG));
    }

    #[test]
    fn version_cleanup_wipes_on_change_and_preserves_agent_pane() {
        let dir = std::env::temp_dir().join(format!("wta-log-housekeeping-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Seed a stale-version marker + some logs.
        std::fs::write(dir.join(VERSION_MARKER), "0.0.0-old").unwrap();
        std::fs::write(dir.join("wta-main_master.log"), "old").unwrap();
        std::fs::write(dir.join("wta-cli.log.2020-01-01"), "old").unwrap();
        std::fs::write(dir.join("wta-agent-pane.log"), "cpp-owned").unwrap();
        std::fs::write(dir.join("hook-trace.log"), "hooks").unwrap();

        version_cleanup(&dir);

        // Our logs wiped; C++ and hook logs untouched; marker rewritten.
        assert!(!dir.join("wta-main_master.log").exists());
        assert!(!dir.join("wta-cli.log.2020-01-01").exists());
        assert!(dir.join("wta-agent-pane.log").exists());
        assert!(dir.join("hook-trace.log").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join(VERSION_MARKER)).unwrap(),
            env!("CARGO_PKG_VERSION")
        );

        // Second run with matching version must NOT wipe freshly written logs.
        std::fs::write(dir.join("wta-main_master.log"), "new").unwrap();
        version_cleanup(&dir);
        assert!(dir.join("wta-main_master.log").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}

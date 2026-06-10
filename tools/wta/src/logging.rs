use std::path::Path;
use std::sync::{Mutex, OnceLock};
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

/// Holds the non-blocking appender's `WorkerGuard` for the whole process.
///
/// Stored in a global (not a `main()` local) so [`shutdown_flush`] can drop it
/// — flushing the appender — before any `std::process::exit`, which would
/// otherwise skip the `Drop` and lose the final buffered log records.
static GUARD: OnceLock<Mutex<Option<WorkerGuard>>> = OnceLock::new();

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

/// Replace the current user's home / app-data prefixes in a path with stable
/// placeholders, so fixed tool / runtime paths can be logged at default levels
/// without leaking the Windows username.
///
/// Only the user-profile prefix is personal in these paths (e.g.
/// `C:\Users\<name>\AppData\Local\…\config.json`); the rest is a fixed,
/// non-personal tool layout that stays useful for diagnostics. Use this for
/// *known* tool/runtime paths only — for arbitrary paths the agent reads or
/// writes (which can reveal user file/folder names beyond the prefix), keep
/// the full path on a trace-only `*.content` target instead.
///
/// Prefixes are tried most-specific first (`LOCALAPPDATA` is itself under
/// `USERPROFILE`) and matched case-insensitively (Windows paths).
pub(crate) fn redact_user_path(path: impl AsRef<std::path::Path>) -> String {
    let s = path.as_ref().to_string_lossy().into_owned();
    for (var, placeholder) in [
        ("LOCALAPPDATA", "%LOCALAPPDATA%"),
        ("APPDATA", "%APPDATA%"),
        ("USERPROFILE", "%USERPROFILE%"),
    ] {
        if let Some(prefix) = std::env::var_os(var) {
            let prefix = prefix.to_string_lossy();
            if prefix.is_empty() {
                continue;
            }
            // `get(..len)` is None when `len` is not a char boundary, so this
            // never panics on a non-ASCII username.
            if let Some(head) = s.get(..prefix.len()) {
                if head.eq_ignore_ascii_case(&prefix) {
                    return format!("{placeholder}{}", &s[prefix.len()..]);
                }
            }
        }
    }
    s
}

/// Root of the WTA log tree: `<local_root>/logs` (or a temp-dir fallback).
fn logs_root() -> std::path::PathBuf {
    crate::runtime_paths::intelligent_terminal_local_root()
        .map(|r| r.join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("IntelligentTerminal").join("logs"))
}

/// The directory log files are written to: `<root>/logs/<pkgver>` when
/// packaged, `<root>/logs` when unpackaged.
///
/// Shared so every writer agrees: `init` (this process's appender) and
/// `spawn.rs` (which hands it to agent-CLI PowerShell hooks via
/// `WTA_HOOK_LOG_DIR`) both resolve through here.
pub(crate) fn log_dir() -> std::path::PathBuf {
    let root = logs_root();
    match package_version() {
        Some(v) => root.join(v),
        None => root,
    }
}

pub fn init(process: &str) {
    let logs_root = logs_root();

    // Per-version subdirectory: each build's logs are stored separately so an
    // upgrade can drop the prior version's logs wholesale — we keep only the
    // current version's dir (see `prune_old_version_dirs`). This is also what
    // makes cleanup lock-free: the live (current-version) dir is never a
    // deletion target, so no process can delete a file another is still writing.
    //
    // The version key is the *package* version (GetCurrentPackageId), shared at
    // runtime with the C++ agent-pane logger and the PowerShell hooks so all
    // three writers land in the same `logs\<pkgver>\` folder. Unpackaged
    // (dev-from-cargo / tests) has no package identity → logs go flat.
    let version_dir = package_version();
    let log_dir = match &version_dir {
        Some(v) => logs_root.join(v),
        None => logs_root.clone(),
    };
    let _ = std::fs::create_dir_all(&log_dir);

    // Reclaim disk BEFORE opening our own appender.
    housekeeping(&logs_root, &log_dir, version_dir.as_deref(), process);

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

    // Stash the guard globally so `shutdown_flush` can drop it on exit.
    let _ = GUARD.set(Mutex::new(Some(guard)));
}

/// The current process's package version as `"Major.Minor.Build.Revision"`
/// (e.g. `"0.8.0.2"`), or `None` when the process has no package identity
/// (unpackaged dev runs / tests).
///
/// This is the shared per-version-dir key: the C++ side reads the same value
/// via `GetCurrentPackageId` in `IntelligentTerminalPaths.h`, so the Rust
/// processes, the C++ agent-pane logger, and (through `WTA_HOOK_LOG_DIR`) the
/// PowerShell hooks all resolve to the same `logs\<pkgver>\` folder.
pub(crate) fn package_version() -> Option<String> {
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::Storage::Packaging::Appx::{GetCurrentPackageId, PACKAGE_ID};

    unsafe {
        // First call sizes the buffer. A packaged process returns
        // ERROR_INSUFFICIENT_BUFFER and fills `len`; unpackaged returns
        // APPMODEL_ERROR_NO_PACKAGE (any other rc means "no usable identity").
        let mut len: u32 = 0;
        if GetCurrentPackageId(&mut len, std::ptr::null_mut()) != ERROR_INSUFFICIENT_BUFFER
            || len == 0
        {
            return None;
        }
        // PACKAGE_ID holds a u64 + pointers, so back it with `u64` storage to
        // guarantee 8-byte alignment (a `Vec<u8>` is only 1-aligned).
        let words = (len as usize + 7) / 8;
        let mut buf = vec![0u64; words.max(1)];
        if GetCurrentPackageId(&mut len, buf.as_mut_ptr() as *mut u8) != 0 {
            return None; // not ERROR_SUCCESS
        }
        let id = &*(buf.as_ptr() as *const PACKAGE_ID);
        // PACKAGE_VERSION { Anonymous: union { Version: u64, Anonymous: { Revision, Build, Minor, Major } } }
        let v = id.version.Anonymous.Anonymous;
        Some(format!("{}.{}.{}.{}", v.Major, v.Minor, v.Build, v.Revision))
    }
}

/// Flush and release the file appender. Must be called once before any
/// `std::process::exit` and at the end of `main()`.
///
/// The non-blocking appender only flushes its buffered records when its
/// `WorkerGuard` is dropped. The guard lives in a `static` ([`GUARD`]) — and
/// `static`s never run `Drop` at process teardown — so this explicit
/// take-and-drop is the single flush point for *every* exit path, including
/// the `process::exit` calls that bypass normal stack unwinding. Idempotent:
/// a second call finds the guard already taken and is a no-op.
pub fn shutdown_flush() {
    if let Some(slot) = GUARD.get() {
        if let Ok(mut guard) = slot.lock() {
            guard.take(); // drop the WorkerGuard -> blocks until appender drains
        }
    }
}

/// Filesystem upkeep run once per process at logging init, before our own
/// appender opens.
///
/// 1. Cap the number of retained per-version log dirs (drops older builds'
///    logs after an upgrade).
/// 2. Reclaim per-PID helper logs older than [`HELPER_RETENTION_DAYS`] within
///    the current version's dir.
fn housekeeping(logs_root: &Path, log_dir: &Path, current_version: Option<&str>, process: &str) {
    // Only meaningful when packaged (there are per-version subdirs to cap);
    // unpackaged dev/tests write flat and have nothing to prune here.
    if let Some(current) = current_version {
        prune_old_version_dirs(logs_root, current);
    }
    // Only long-lived / relevant processes scan for stale helper files; the
    // high-frequency `cli` path must not pay a directory scan on every call.
    if process == "main_master" || process.starts_with("main_helper") {
        prune_stale_helper_logs(log_dir);
    }
}

/// Delete every per-version log subdir under `logs/` except the current
/// build's — we keep only the current version's logs, so on any start after an
/// upgrade the prior versions' dirs are removed wholesale.
///
/// The current dir is never a deletion target, so this needs no inter-process
/// lock even when several upgraded processes start at once: they only ever race
/// to delete the same *dead* (old-version) dirs, and `remove_dir_all` is
/// idempotent.
fn prune_old_version_dirs(logs_root: &Path, current: &str) {
    let Ok(entries) = std::fs::read_dir(logs_root) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            // Leave any flat files alone — only version subdirs are pruned.
            // (Post-unification all writers use the versioned dir, but a stray
            // pre-upgrade flat log must never be a deletion target here.)
            continue;
        }
        if entry.file_name().to_string_lossy() == current {
            continue; // never delete the live dir
        }
        let _ = std::fs::remove_dir_all(entry.path());
    }
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
    fn redact_user_path_strips_localappdata_prefix() {
        let local = std::env::var_os("LOCALAPPDATA");
        if let Some(local) = local {
            let local = local.to_string_lossy().into_owned();
            let full = std::path::Path::new(&local)
                .join("IntelligentTerminal")
                .join("master-pipe.txt");
            let redacted = redact_user_path(&full);
            assert!(
                redacted.starts_with("%LOCALAPPDATA%"),
                "expected placeholder prefix, got: {redacted}",
            );
            assert!(!redacted.contains(&local), "raw prefix leaked: {redacted}");
            assert!(redacted.ends_with("master-pipe.txt"));
        }
    }

    #[test]
    fn redact_user_path_leaves_unrelated_paths_untouched() {
        let p = std::path::Path::new(r"D:\unrelated\proj\file.rs");
        assert_eq!(redact_user_path(p), r"D:\unrelated\proj\file.rs");
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
    fn prune_keeps_only_current_version() {
        let root = std::env::temp_dir().join(format!("wta-version-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        let current = "9.9.9.9";
        std::fs::create_dir_all(root.join(current)).unwrap();
        // Several older version dirs, each with a log file inside.
        for v in ["0.0.1", "0.0.2", "0.0.3", "0.0.4", "0.0.5"] {
            let d = root.join(v);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("wta-main.log"), "x").unwrap();
        }
        // A flat non-dir file must be left untouched.
        std::fs::write(root.join("terminal-agent-pane.log"), "cpp").unwrap();

        prune_old_version_dirs(&root, current);

        // Current version survives; flat file untouched; every older version gone.
        assert!(root.join(current).exists());
        assert!(root.join("terminal-agent-pane.log").exists());
        for v in ["0.0.1", "0.0.2", "0.0.3", "0.0.4", "0.0.5"] {
            assert!(!root.join(v).exists(), "old version dir {v} must be deleted");
        }
        let dir_count = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .count();
        assert_eq!(dir_count, 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}

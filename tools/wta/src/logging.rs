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
        // Verbose for developers iterating on *our* code, but cap the
        // `agent_client_protocol` crate at `info`. At `debug` that crate dumps
        // every JSON-RPC message body verbatim — and logs each outgoing
        // response twice via its actor spans (`send_raw_message` +
        // `outgoing_protocol_actor`). For the `sessions/list` poll that
        // response is the whole session-registry snapshot (~27 KB), so a
        // routine debug session bloats `wta-main_master.log` to multiple GB, of
        // which ~99% is this one crate's wire trace. Capping at `info` drops
        // that debug/trace flood while still surfacing anything the crate logs
        // at info and above. Today the crate emits only `trace!`/`debug!` (no
        // info/warn/error), so this is behaviorally identical to `warn` but
        // reads as the minimal cap and is forward-safe if the crate later adds
        // info-level logs. WTA keeps its own dedicated ACP wire log
        // (`wta-acp-debug.log`) for deep debugging; opt the crate's trace back
        // in explicitly with `WTA_LOG=debug,agent_client_protocol=debug`.
        "debug,agent_client_protocol=info"
    } else {
        // Shipping release binaries log at info: enough to follow lifecycle
        // and connection flow out of the box, without the noisy debug traces.
        // Users can still opt into more via `WTA_LOG=debug|trace` / `RUST_LOG`.
        "info"
    }
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

/// Install a Windows console control handler that records the teardown
/// signal and drains the log appender before the OS terminates us.
///
/// The wta-**helper** runs as a ConPTY child of Windows Terminal (it's the
/// process rendered in the agent pane). When its pane/tab/window closes — or
/// the user logs off / shuts down — the OS delivers a control event
/// (`CTRL_CLOSE`/`CTRL_LOGOFF`/`CTRL_SHUTDOWN`) and then terminates it at the
/// end of a short grace window. Without a handler those deaths are invisible:
/// the process vanishes mid-stream and the non-blocking appender's last
/// buffered records are lost, because [`shutdown_flush`] never runs (the
/// `WorkerGuard` lives in a `static` and `static`s don't `Drop` at teardown).
/// That is exactly the "helper just stopped responding" signature where the
/// success path is logged exhaustively but the teardown path is silent and
/// the incident is undiagnosable.
///
/// This closes that gap for the helper: it logs WHICH control event tore the
/// process down and flushes so the final records (e.g. the transport-lost
/// WARN in `run_acp_client_over_pipe`) reach disk. The handler returns FALSE
/// so the default handler still runs and the process terminates as before —
/// we only ADD a log line + flush, never changing termination behavior. It's
/// installed process-wide (cheap and harmless), so any wta process that does
/// receive a console control event benefits.
///
/// Coverage limits — what this does NOT catch:
///   * The wta-**master** is spawned `CREATE_NO_WINDOW` and contained in a
///     Job Object with `KILL_ON_JOB_CLOSE` (see C++ `SharedWta`). Its normal
///     teardown is the parent dropping that job, which reaps the master like
///     a `TerminateProcess` — NO control event — so *this handler* does not
///     trace routine master teardown. That teardown is not unlogged overall,
///     though: the C++ parent (`SharedWta`) records both the deliberate
///     job-close and an unexpected exit to `terminal-agent-pane.log`. This
///     handler fires for the master only on genuine console signals
///     (logoff/shutdown), if delivered at all.
///   * A hard `TerminateProcess` (Task Manager "End task", `taskkill /F`, an
///     OS resource kill, or the Job-Object reap above) delivers no control
///     event and stays untraceable from inside the process.
///   * While the Ratatui TUI holds the console in raw mode, Ctrl+C arrives as
///     a key event (not `CTRL_C_EVENT`), so this handler doesn't normally see
///     it and doesn't alter the TUI's Ctrl+C behavior.
pub fn install_ctrl_handler() {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_INVALID_HANDLE};
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT,
        CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    // Returns `windows_sys`' `BOOL` (an `i32` alias) to match the
    // PHANDLER_ROUTINE signature: 0 == FALSE (fall through to default).
    unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
        let event = match ctrl_type {
            CTRL_C_EVENT => "CTRL_C",
            CTRL_BREAK_EVENT => "CTRL_BREAK",
            CTRL_CLOSE_EVENT => "CTRL_CLOSE",
            CTRL_LOGOFF_EVENT => "CTRL_LOGOFF",
            CTRL_SHUTDOWN_EVENT => "CTRL_SHUTDOWN",
            _ => "UNKNOWN",
        };
        tracing::warn!(
            target: "lifecycle",
            ctrl_type,
            event,
            "console control event received — process being torn down; flushing logs"
        );
        // Drain the appender so the line above (and any earlier buffered
        // records) hit disk before the grace window ends and we're killed.
        shutdown_flush();
        // FALSE → fall through to the default handler (terminate). We only
        // add logging + flush; termination behavior is unchanged.
        0
    }

    // SAFETY: `handler` is a valid `extern "system"` routine matching the
    // PHANDLER_ROUTINE signature; registering a control handler is a
    // process-global, thread-safe Win32 operation.
    unsafe {
        if SetConsoleCtrlHandler(Some(handler), 1) == 0 {
            // Capture the Win32 error immediately, before any other call (incl.
            // the logging macro's own work) can reset thread-last-error.
            let error_code = GetLastError();
            if error_code == ERROR_INVALID_HANDLE {
                // Expected for a windowless wta process (the CREATE_NO_WINDOW
                // master, a detached CLI invocation): there's no console to
                // signal, and teardown for those is covered elsewhere (the C++
                // side observes the master via its wait callback). Benign —
                // debug only, so it never spams release logs.
                tracing::debug!(
                    target: "lifecycle",
                    error_code,
                    "SetConsoleCtrlHandler: no console attached (expected for windowless process)"
                );
            } else {
                // Any other failure is the diagnostic feature itself failing to
                // arm where we DID expect a console (e.g. the helper) — warn so
                // release (info) logs explain why later teardown signals are
                // absent rather than leaving it a silent mystery.
                tracing::warn!(
                    target: "lifecycle",
                    error_code,
                    "SetConsoleCtrlHandler failed — teardown signals will not be logged"
                );
            }
        }
    }
}

/// Install a panic hook that records the panic to disk, then chains to the
/// previous hook.
///
/// A Rust panic otherwise writes only to stderr — invisible for a ConPTY-
/// hosted helper or a `CREATE_NO_WINDOW` master — and the non-blocking
/// appender's buffered tail is lost when a *fatal* panic kills the process
/// before the background worker drains it. So a panic is a "died for no
/// logged reason" blind spot. This closes it WITHOUT changing panic semantics
/// (it chains the previous hook, so unwind/abort and backtraces are
/// unchanged):
///   * a `tracing::error!` so the panic correlates in the normal log (this
///     drains fine for a *recovered* panic, e.g. behind a `catch_unwind`), and
///   * a synchronous append to `wta-panic.log`, independent of the async
///     appender, so the record reaches disk even when a fatal panic kills us.
///
/// It deliberately does NOT call [`shutdown_flush`]: that drops the appender
/// guard and would permanently kill logging after a recoverable panic. The
/// synchronous file write is the durable path instead.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Same payload extraction the rest of the codebase uses.
        let msg = info
            .payload()
            .downcast_ref::<&'static str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
            .unwrap_or("<non-string panic payload>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let thread_name = std::thread::current()
            .name()
            .unwrap_or("<unnamed>")
            .to_string();

        tracing::error!(
            target: "panic",
            message = %msg,
            location = %location,
            thread = %thread_name,
            "thread panicked"
        );

        // Guaranteed-on-disk backstop: a fatal main-thread panic unwinds past
        // main() without reaching any `shutdown_flush`, so the appender's
        // buffered tail (incl. the error above) can be lost. A synchronous
        // append here does not depend on the appender being alive.
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_dir().join("wta-panic.log"))
        {
            use std::io::Write;
            let _ = writeln!(
                f,
                "[{millis}ms] pid={} thread={thread_name} panicked at {location}: {msg}",
                std::process::id()
            );
        }

        prev(info);
    }));
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
        assert_eq!(
            default_filter_directive(true),
            "debug,agent_client_protocol=info"
        );
    }

    #[test]
    fn debug_build_default_caps_acp_crate_at_info() {
        // The debug default keeps our own code at `debug` but must cap the
        // noisy `agent_client_protocol` wire trace, or a routine debug run
        // balloons wta-main_master.log to multiple GB (see the directive doc).
        let directive = default_filter_directive(true);
        assert!(directive.starts_with("debug"));
        assert!(directive.contains("agent_client_protocol=info"));
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

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;
use time::{Date, OffsetDateTime};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    filter::filter_fn, fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter, Layer,
    Registry,
};

/// Per-PID helper log file prefix. The `main_helper-{pid}` process label
/// (see `main::process_label`) lands here, e.g.
/// `wta-main_helper-12345.<date>.log`.
const HELPER_LOG_PREFIX: &str = "wta-main_helper-";
/// Per-PID helper logs older than this are reclaimed by [`housekeeping`].
const HELPER_RETENTION_DAYS: u64 = 3;
/// Daily files kept for each Rust WTA log stream. Older matching files are
/// pruned when the appender is opened or rolls over. This is a best-effort
/// bound if concurrent writers race or filesystem deletion fails.
const LOG_MAX_FILES: usize = 3;
const ROLLOVER_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

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
        // routine debug session bloats `wta-main_master.<date>.log` to multiple
        // GB, of
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

fn explicitly_configures_acp_dependency(directives: &str) -> bool {
    directives.split(',').map(str::trim).any(|directive| {
        directive
            .strip_prefix("agent_client_protocol")
            .is_some_and(|suffix| {
                suffix.is_empty()
                    || suffix.starts_with('=')
                    || suffix.starts_with('[')
                    || suffix.starts_with("::")
            })
    })
}

fn apply_dependency_privacy_cap(
    filter: EnvFilter,
    directives: Option<&str>,
) -> impl Layer<Registry> {
    let explicit = directives.is_some_and(explicitly_configures_acp_dependency);
    // Intersect with the original filter instead of adding a directive: an
    // ACP=info override would enable logs even for `off` or `warn,wta=trace`.
    filter.and_then(filter_fn(move |metadata| {
        explicit
            || *metadata.level() <= tracing::Level::INFO
            || !(metadata.target() == "agent_client_protocol"
                || metadata.target().starts_with("agent_client_protocol::"))
    }))
}

fn configured_filter(default_directives: &str) -> impl Layer<Registry> {
    for variable in ["WTA_LOG", "RUST_LOG"] {
        if let Ok(directives) = std::env::var(variable) {
            if let Ok(filter) = EnvFilter::try_new(&directives) {
                return apply_dependency_privacy_cap(filter, Some(&directives));
            }
        }
    }

    apply_dependency_privacy_cap(EnvFilter::new(default_directives), None)
}

/// Root of the WTA log tree: `<local_root>/logs` (or a temp-dir fallback).
fn logs_root() -> std::path::PathBuf {
    crate::runtime_paths::intelligent_terminal_local_root()
        .map(|r| r.join("logs"))
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join("IntelligentTerminal")
                .join("logs")
        })
}

/// The directory log files are written to: `<root>/logs/<pkgver>` when
/// packaged, `<root>/logs` when unpackaged.
///
/// Shared so every Rust process agrees on the package-private log directory.
/// `spawn.rs` also hands it to pre-0.1.5 hook bundles during auto-upgrade.
pub(crate) fn log_dir() -> std::path::PathBuf {
    let root = logs_root();
    match package_version() {
        Some(v) => root.join(v),
        None => root,
    }
}

struct RolloverRetry {
    target_date: Date,
    retry_after: Instant,
}

enum LogWriter {
    Daily {
        file: File,
        log_dir: PathBuf,
        prefix: String,
        active_date: Date,
        rollover_retry: Option<RolloverRetry>,
    },
    Fixed(File),
}

impl LogWriter {
    fn new(process: &str, log_dir: &Path) -> io::Result<Self> {
        Self::daily(process, log_dir)
            .or_else(|_| open_append(&log_dir.join(format!("wta-{process}.log"))).map(Self::Fixed))
    }

    fn daily(process: &str, log_dir: &Path) -> io::Result<Self> {
        let active_date = OffsetDateTime::now_utc().date();
        let prefix = format!("wta-{process}");
        let active_path = dated_log_path(log_dir, &prefix, active_date);
        let file = open_append(&active_path)?;
        prune_log_stream(log_dir, &prefix, &active_path);
        Ok(Self::Daily {
            file,
            log_dir: log_dir.to_path_buf(),
            prefix,
            active_date,
            rollover_retry: None,
        })
    }
}

fn best_effort_stderr(args: std::fmt::Arguments<'_>) {
    let _ = io::stderr().lock().write_fmt(args);
}

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Daily {
                file,
                log_dir,
                prefix,
                active_date,
                rollover_retry,
            } => {
                let current_date = OffsetDateTime::now_utc().date();
                if current_date != *active_date {
                    let now = Instant::now();
                    let should_retry = rollover_retry.as_ref().is_none_or(|retry| {
                        retry.target_date != current_date || now >= retry.retry_after
                    });
                    if should_retry {
                        let next_path = dated_log_path(log_dir, prefix, current_date);
                        match open_append(&next_path) {
                            Ok(next_file) => {
                                *file = next_file;
                                *active_date = current_date;
                                *rollover_retry = None;
                                prune_log_stream(log_dir, prefix, &next_path);
                            }
                            Err(error) => {
                                *rollover_retry = Some(RolloverRetry {
                                    target_date: current_date,
                                    retry_after: now + ROLLOVER_RETRY_INTERVAL,
                                });
                                best_effort_stderr(format_args!("Log rollover failed: {error}\n"));
                            }
                        }
                    }
                }
                file.write(buf)
            }
            Self::Fixed(file) => file.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Daily { file, .. } | Self::Fixed(file) => file.flush(),
        }
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn dated_log_path(log_dir: &Path, prefix: &str, date: Date) -> PathBuf {
    log_dir.join(format!("{prefix}.{date}.log"))
}

fn exact_logs(dir: &Path, prefix: &str, active: &Path) -> io::Result<Vec<(Date, PathBuf)>> {
    let format = time::format_description::parse("[year]-[month]-[day]").unwrap();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_file() || path == active {
            continue;
        }
        let name = entry.file_name();
        let Some(date) = name
            .to_str()
            .and_then(|name| name.strip_prefix(prefix))
            .and_then(|name| name.strip_prefix('.'))
            .and_then(|name| name.strip_suffix(".log"))
            .and_then(|date| Date::parse(date, &format).ok())
        else {
            continue;
        };
        files.push((date, path));
    }
    Ok(files)
}

fn prune_log_stream(log_dir: &Path, prefix: &str, active_path: &Path) {
    let Ok(mut files) = exact_logs(log_dir, prefix, active_path).inspect_err(|error| {
        best_effort_stderr(format_args!("Log retention scan failed: {error}\n"))
    }) else {
        return;
    };
    files.sort_by_key(|(date, _)| *date);
    let remove_count = files.len().saturating_sub(LOG_MAX_FILES.saturating_sub(1));
    for (_, path) in files.into_iter().take(remove_count) {
        if let Err(error) = std::fs::remove_file(&path) {
            best_effort_stderr(format_args!(
                "Couldn't remove old log file {}: {error}\n",
                path.display()
            ));
        }
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
    // runtime with the C++ agent-pane logger so both writers land in the same
    // `logs\<pkgver>\` folder. Unpackaged
    // (dev-from-cargo / tests) has no package identity → logs go flat.
    let version_dir = package_version();
    let log_dir = match &version_dir {
        Some(v) => logs_root.join(v),
        None => logs_root.clone(),
    };
    let _ = std::fs::create_dir_all(&log_dir);

    // Reclaim disk BEFORE opening our own appender.
    housekeeping(&logs_root, &log_dir, version_dir.as_deref(), process);

    // Every Rust WTA stream uses the same daily UTC naming and bounded
    // retention. If the daily file cannot be initialized, keep the old
    // fixed-name fallback in the same directory.
    let writer =
        LogWriter::new(process, &log_dir).expect("failed to initialize daily or fixed log file");
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);

    let default_level = default_filter_directive(cfg!(debug_assertions));

    let filter = configured_filter(default_level);

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
/// processes and the C++ agent-pane logger resolve to the same
/// `logs\<pkgver>\` folder.
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
        Some(format!(
            "{}.{}.{}.{}",
            v.Major, v.Minor, v.Build, v.Revision
        ))
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
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
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
///   * a synchronous append to `wta-panic.<date>.log`, independent of the
///     async appender, so the record reaches disk even when a fatal panic
///     kills us.
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
        // write here does not depend on the normal appender being alive.
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let log_dir = log_dir();
        let record = format!(
            "[{millis}ms] pid={} thread={thread_name} panicked at {location}: {msg}",
            std::process::id()
        );
        let _ = write_panic_record(&log_dir, record.as_bytes());

        prev(info);
    }));
}

fn write_panic_record(log_dir: &Path, record: &[u8]) -> std::io::Result<()> {
    let mut writer = LogWriter::new("panic", log_dir)?;
    writer.write_all(record)?;
    writer.write_all(b"\n")?;
    writer.flush()
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
/// [`HELPER_RETENTION_DAYS`]. Each PID has its own daily-rotated
/// `wta-main_helper-{pid}.<date>.log` stream, so appender retention cannot
/// reclaim streams abandoned when helper processes exit.
///
/// Mtime is an activity heuristic, not a liveness check. A helper that stays
/// idle beyond the retention window can match; removal failures are ignored.
/// A liveness-aware policy belongs in a separate audit.
fn prune_stale_helper_logs(log_dir: &Path) {
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(
        HELPER_RETENTION_DAYS * 24 * 60 * 60,
    )) else {
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
    use std::io::Write;
    use std::sync::Arc;
    use time::Duration;
    use tracing_subscriber::filter::LevelFilter;

    #[derive(Clone)]
    struct SharedWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn capture_dependency_logs(filter: impl Layer<Registry> + Send + Sync + 'static) -> String {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = output.clone();
        let subscriber = tracing_subscriber::registry().with(filter).with(
            fmt::layer()
                .without_time()
                .with_ansi(false)
                .with_writer(move || SharedWriter(writer.clone())),
        );

        tracing::subscriber::with_default(subscriber, || {
            macro_rules! emit_levels {
                ($target:expr) => {
                    tracing::error!(target: $target, concat!($target, ":ERROR"));
                    tracing::warn!(target: $target, concat!($target, ":WARN"));
                    tracing::info!(target: $target, concat!($target, ":INFO"));
                    tracing::debug!(target: $target, concat!($target, ":DEBUG"));
                    tracing::trace!(target: $target, concat!($target, ":TRACE"));
                };
            }
            emit_levels!("agent_client_protocol");
            emit_levels!("agent_client_protocol::jsonrpc");
            emit_levels!("wta");
            emit_levels!("wta::logging");
            emit_levels!("unrelated");
            emit_levels!("agent_client_protocol_extra");
            emit_levels!("agent_client_protocol_extra::jsonrpc");
            emit_levels!(concat!("agent_client_protocol", "x"));
            let connection =
                tracing::info_span!(target: "agent_client_protocol::util", "connection");
            let _entered = connection.enter();
            emit_levels!("agent_client_protocol::jsonrpc");
            emit_levels!("wta");
        });

        let bytes = output.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    fn assert_logged_levels(log: &str, target: &str, expected: LevelFilter, directives: &str) {
        for level in [
            LevelFilter::ERROR,
            LevelFilter::WARN,
            LevelFilter::INFO,
            LevelFilter::DEBUG,
            LevelFilter::TRACE,
        ] {
            assert_eq!(
                log.contains(&format!(
                    "{target}:{}",
                    level.to_string().to_ascii_uppercase()
                )),
                level <= expected,
                "directives={directives}, target={target}, level={level}"
            );
        }
    }

    #[test]
    fn dependency_privacy_cap_preserves_global_verbosity() {
        for level in [
            LevelFilter::OFF,
            LevelFilter::ERROR,
            LevelFilter::WARN,
            LevelFilter::INFO,
            LevelFilter::DEBUG,
            LevelFilter::TRACE,
        ] {
            let directives = level.to_string();
            let log = capture_dependency_logs(apply_dependency_privacy_cap(
                EnvFilter::new(&directives),
                Some(&directives),
            ));
            for target in ["agent_client_protocol", "agent_client_protocol::jsonrpc"] {
                assert_logged_levels(&log, target, level.min(LevelFilter::INFO), &directives);
            }
            for target in ["wta", "wta::logging", "unrelated"] {
                assert_logged_levels(&log, target, level, &directives);
            }
        }
    }

    #[test]
    fn dependency_privacy_cap_preserves_scoped_verbosity() {
        for (directives, global, wta) in [
            ("off,wta=debug", LevelFilter::OFF, LevelFilter::DEBUG),
            ("off,wta=trace", LevelFilter::OFF, LevelFilter::TRACE),
            ("error,wta=debug", LevelFilter::ERROR, LevelFilter::DEBUG),
            ("warn,wta=debug", LevelFilter::WARN, LevelFilter::DEBUG),
            ("warn,wta=trace", LevelFilter::WARN, LevelFilter::TRACE),
            ("trace,wta=off", LevelFilter::TRACE, LevelFilter::OFF),
        ] {
            let log = capture_dependency_logs(apply_dependency_privacy_cap(
                EnvFilter::new(directives),
                Some(directives),
            ));
            for target in ["agent_client_protocol", "agent_client_protocol::jsonrpc"] {
                assert_logged_levels(&log, target, global.min(LevelFilter::INFO), directives);
            }
            for target in ["wta", "wta::logging"] {
                assert_logged_levels(&log, target, wta, directives);
            }
            assert_logged_levels(&log, "unrelated", global, directives);
        }
    }

    #[test]
    fn dependency_privacy_cap_preserves_explicit_overrides() {
        for directives in [
            "off,agent_client_protocol=debug",
            "warn,agent_client_protocol=trace",
            "info,agent_client_protocol",
            "debug,agent_client_protocol=off",
            "trace,agent_client_protocol=error",
            "trace,agent_client_protocol=warn",
            "trace,agent_client_protocol=info",
            "off,agent_client_protocol::jsonrpc=trace",
            "trace,agent_client_protocol::jsonrpc=warn",
            "debug,agent_client_protocol=off,agent_client_protocol::jsonrpc=trace",
        ] {
            assert!(explicitly_configures_acp_dependency(directives));
            let original = capture_dependency_logs(EnvFilter::new(directives));
            let capped = capture_dependency_logs(apply_dependency_privacy_cap(
                EnvFilter::new(directives),
                Some(directives),
            ));
            assert_eq!(capped, original, "directives={directives}");
        }
    }

    #[test]
    fn dependency_privacy_cap_preserves_span_scoped_overrides() {
        for (directives, level) in [
            (
                "off,agent_client_protocol[connection]=trace",
                LevelFilter::TRACE,
            ),
            (
                "off,agent_client_protocol::util[connection]=debug",
                LevelFilter::DEBUG,
            ),
            (
                "off,agent_client_protocol[connection]=info",
                LevelFilter::INFO,
            ),
            (
                "trace,agent_client_protocol=warn,agent_client_protocol[connection]=error",
                LevelFilter::WARN,
            ),
            (
                "trace,agent_client_protocol=error,agent_client_protocol[connection]=off",
                LevelFilter::ERROR,
            ),
            (
                "trace,agent_client_protocol=off,agent_client_protocol[connection]=off",
                LevelFilter::OFF,
            ),
        ] {
            let original = capture_dependency_logs(EnvFilter::try_new(directives).unwrap());
            assert_logged_levels(
                &original,
                "agent_client_protocol::jsonrpc",
                level,
                directives,
            );
            let capped = capture_dependency_logs(apply_dependency_privacy_cap(
                EnvFilter::try_new(directives).unwrap(),
                Some(directives),
            ));
            if directives.starts_with("off,") {
                assert!(original.contains("connection: agent_client_protocol::jsonrpc:"));
                assert!(capped.contains("connection: agent_client_protocol::jsonrpc:"));
            }
            // Concurrent subscribers can change whether EnvFilter's lower-level
            // span directives retain the INFO span. Compare every synthetic
            // event (target, level, order and duplicates), not span decoration.
            let original_events: Vec<_> = original
                .lines()
                .map(|line| line.rsplit_once(": ").expect("formatted event message").1)
                .collect();
            let capped_events: Vec<_> = capped
                .lines()
                .map(|line| line.rsplit_once(": ").expect("formatted event message").1)
                .collect();
            assert_eq!(capped_events, original_events, "directives={directives}");
        }
        for directives in [
            "trace,agent_client_protocol_extra[connection]=trace",
            "trace,agent_client_protocol_extra::util[connection]=trace",
            concat!("trace,agent_client_protocol", "x[connection]=trace"),
        ] {
            assert!(!explicitly_configures_acp_dependency(directives));
            let capped = capture_dependency_logs(apply_dependency_privacy_cap(
                EnvFilter::try_new(directives).unwrap(),
                Some(directives),
            ));
            assert_logged_levels(
                &capped,
                "agent_client_protocol::jsonrpc",
                LevelFilter::INFO,
                directives,
            );
            assert_logged_levels(&capped, "wta", LevelFilter::TRACE, directives);
        }
    }

    fn test_log_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{label}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn daily_log_count(root: &Path, prefix: &str) -> usize {
        exact_logs(root, prefix, root).unwrap().len()
    }

    fn daily_writer(root: &Path, date: Date, contents: &[u8]) -> (LogWriter, PathBuf) {
        let prefix = "wta-main".to_string();
        let path = dated_log_path(root, &prefix, date);
        std::fs::write(&path, contents).unwrap();
        let writer = LogWriter::Daily {
            file: open_append(&path).unwrap(),
            log_dir: root.to_path_buf(),
            prefix,
            active_date: date,
            rollover_retry: None,
        };
        (writer, path)
    }

    fn seed_future_logs(root: &Path, today: Date) -> [PathBuf; 3] {
        std::array::from_fn(|day| {
            let path = dated_log_path(root, "wta-main", today + Duration::days(day as i64 + 1));
            std::fs::write(&path, "future").unwrap();
            path
        })
    }

    fn read_log(path: impl AsRef<Path>) -> String {
        std::fs::read_to_string(path).unwrap()
    }

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
        // balloons wta-main_master.<date>.log to multiple GB (see the directive doc).
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
    fn global_debug_does_not_explicitly_enable_acp_dependency_payloads() {
        assert!(!explicitly_configures_acp_dependency("debug"));
        assert!(!explicitly_configures_acp_dependency(
            "debug,wta=trace,acp.content=trace"
        ));

        let filter = apply_dependency_privacy_cap(EnvFilter::new("debug"), Some("debug"));
        let log = capture_dependency_logs(filter);
        assert_logged_levels(&log, "agent_client_protocol", LevelFilter::INFO, "debug");
    }

    #[test]
    fn acp_dependency_payload_logging_requires_an_explicit_target() {
        assert!(explicitly_configures_acp_dependency(
            "debug, agent_client_protocol "
        ));
        assert!(explicitly_configures_acp_dependency(
            "debug,agent_client_protocol=debug"
        ));
        assert!(explicitly_configures_acp_dependency(
            "info,agent_client_protocol::jsonrpc=trace"
        ));

        let filter = apply_dependency_privacy_cap(
            EnvFilter::new("debug,agent_client_protocol=debug"),
            Some("debug,agent_client_protocol=debug"),
        );
        let log = capture_dependency_logs(filter);
        assert_logged_levels(
            &log,
            "agent_client_protocol",
            LevelFilter::DEBUG,
            "debug,agent_client_protocol=debug",
        );
    }

    #[test]
    fn dependency_privacy_cap_preserves_similar_targets() {
        for (directives, target) in [
            (
                "off,agent_client_protocol_extra=trace",
                "agent_client_protocol_extra",
            ),
            (
                "warn,agent_client_protocol_extra::jsonrpc=trace",
                "agent_client_protocol_extra::jsonrpc",
            ),
            (
                concat!("debug,agent_client_protocol", "x=trace"),
                concat!("agent_client_protocol", "x"),
            ),
        ] {
            assert!(!explicitly_configures_acp_dependency(directives));
            let original = capture_dependency_logs(EnvFilter::new(directives));
            let capped = capture_dependency_logs(apply_dependency_privacy_cap(
                EnvFilter::new(directives),
                Some(directives),
            ));
            assert_logged_levels(&capped, target, LevelFilter::TRACE, directives);
            for line in original.lines() {
                if line.contains("agent_client_protocol:DEBUG")
                    || line.contains("agent_client_protocol:TRACE")
                    || line.contains("agent_client_protocol::jsonrpc:DEBUG")
                    || line.contains("agent_client_protocol::jsonrpc:TRACE")
                {
                    assert!(!capped.contains(line), "directives={directives}");
                } else {
                    assert!(capped.contains(line), "directives={directives}");
                }
            }
        }
    }

    #[test]
    fn global_debug_filter_drops_acp_dependency_payload_bodies() {
        for directives in [
            "debug",
            "debug,agent_client_protocol_extra=debug",
            "debug,agent_client_protocol_extra::jsonrpc=trace",
            &format!("debug,agent_client_protocol{}=debug", 'x'),
        ] {
            assert!(!explicitly_configures_acp_dependency(directives));
            let output = Arc::new(Mutex::new(Vec::new()));
            let writer = output.clone();
            let subscriber = tracing_subscriber::registry()
                .with(apply_dependency_privacy_cap(
                    EnvFilter::new(directives),
                    Some(directives),
                ))
                .with(
                    fmt::layer()
                        .without_time()
                        .with_ansi(false)
                        .with_writer(move || SharedWriter(writer.clone())),
                );

            tracing::subscriber::with_default(subscriber, || {
                tracing::debug!(
                    target: "agent_client_protocol::jsonrpc::outgoing_actor",
                    prompt = "secret-prompt",
                    "outgoing request"
                );
                tracing::debug!(target: "wta_test", "visible WTA diagnostic");
            });

            let bytes = output.lock().unwrap().clone();
            let log = String::from_utf8(bytes).unwrap();
            assert!(log.contains("visible WTA diagnostic"));
            assert!(!log.contains("secret-prompt"));
            assert!(!log.contains("outgoing request"));
        }
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
            assert!(
                !root.join(v).exists(),
                "old version dir {v} must be deleted"
            );
        }
        let dir_count = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .count();
        assert_eq!(dir_count, 1);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn all_log_writers_use_daily_names_and_enforce_retention() {
        let scratch = test_log_dir("wta-all-stream-retention");
        const PROCESSES: &str = "main cli main_master main_helper-123 main_helper-1234 \
            delegate probe install-hooks panic";
        for process in PROCESSES.split_ascii_whitespace() {
            let prefix = format!("wta-{process}");
            for day in 1..=LOG_MAX_FILES + 2 {
                let path = scratch.join(format!("{prefix}.2000-01-{day:02}.log"));
                std::fs::write(path, "old").unwrap();
            }
        }
        const PRESERVED: &str =
            "wta-main.log wta-main.not-a-date.log wta-main.2000-01-01.extra.log";
        for name in PRESERVED.split_ascii_whitespace() {
            std::fs::write(scratch.join(name), "preserve").unwrap();
        }
        for process in PROCESSES.split_ascii_whitespace() {
            let prefix = format!("wta-{process}");
            let collision = match process {
                "main" => Some("wta-main_master"),
                "main_master" => Some("wta-main"),
                "main_helper-123" => Some("wta-main_helper-1234"),
                _ => None,
            }
            .map(|prefix| (prefix, daily_log_count(&scratch, prefix)));
            let writer = LogWriter::new(process, &scratch).unwrap();
            assert!(matches!(writer, LogWriter::Daily { .. }));
            assert_eq!(daily_log_count(&scratch, &prefix), LOG_MAX_FILES);
            if let Some((collision, count)) = collision {
                assert_eq!(daily_log_count(&scratch, collision), count);
            }
        }
        for name in PRESERVED.split_ascii_whitespace() {
            assert!(scratch.join(name).is_file(), "{name}");
        }

        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn writer_rollover_replaces_active_file_and_protects_new_file() {
        let scratch = test_log_dir("wta-writer-rollover");
        let today = OffsetDateTime::now_utc().date();
        let current_path = dated_log_path(&scratch, "wta-main", today);
        let (mut writer, old_path) =
            daily_writer(&scratch, today - Duration::days(10), b"old record\n");
        let future_logs = seed_future_logs(&scratch, today);
        writer.write_all(b"rolled record\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(read_log(&current_path), "rolled record\n");
        assert!(!old_path.exists());
        assert!(!future_logs[0].exists());
        assert!(future_logs[1].is_file());
        assert!(future_logs[2].is_file());
        assert_eq!(daily_log_count(&scratch, "wta-main"), LOG_MAX_FILES);

        drop(writer);
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn failed_rollover_keeps_old_active_file_and_skips_pruning() {
        let scratch = test_log_dir("wta-failed-rollover");
        let today = OffsetDateTime::now_utc().date();
        let old_date = today - Duration::days(10);
        let (mut writer, old_path) = daily_writer(&scratch, old_date, b"before failure\n");
        let current_path = dated_log_path(&scratch, "wta-main", today);
        std::fs::create_dir(&current_path).unwrap();
        let future_logs = seed_future_logs(&scratch, today);
        writer.write_all(b"after failure\n").unwrap();
        writer.flush().unwrap();
        let retry_after = match &writer {
            LogWriter::Daily {
                active_date,
                rollover_retry: Some(retry),
                ..
            } => {
                assert_eq!(*active_date, old_date);
                assert_eq!(retry.target_date, today);
                retry.retry_after
            }
            _ => panic!("failed rollover should retain retry state"),
        };
        writer.write_all(b"while throttled\n").unwrap();
        writer.flush().unwrap();
        assert!(matches!(
            &writer,
            LogWriter::Daily {
                rollover_retry: Some(retry),
                ..
            } if retry.retry_after == retry_after
        ));
        assert_eq!(
            read_log(&old_path),
            ["before failure", "after failure", "while throttled"].join("\n") + "\n"
        );
        assert!(future_logs.iter().all(|path| path.is_file()));
        assert_eq!(daily_log_count(&scratch, "wta-main"), 4);
        std::fs::remove_dir(&current_path).unwrap();
        writer.write_all(b"still throttled\n").unwrap();
        writer.flush().unwrap();
        assert!(!current_path.exists());
        assert_eq!(
            read_log(&old_path),
            [
                "before failure",
                "after failure",
                "while throttled",
                "still throttled",
            ]
            .join("\n")
                + "\n"
        );
        match &mut writer {
            LogWriter::Daily {
                rollover_retry: Some(retry),
                ..
            } => retry.retry_after = Instant::now(),
            _ => panic!("failed rollover should retain retry state"),
        }
        writer.write_all(b"after retry\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(read_log(&current_path), "after retry\n");
        assert!(matches!(
            &writer,
            LogWriter::Daily {
                active_date,
                rollover_retry: None,
                ..
            } if *active_date == today
        ));

        drop(writer);
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn retry_throttle_does_not_block_a_new_target_date() {
        let scratch = test_log_dir("wta-rollover-new-target");
        let today = OffsetDateTime::now_utc().date();
        let (mut writer, _) = daily_writer(&scratch, today - Duration::days(10), b"old record\n");
        let current_path = dated_log_path(&scratch, "wta-main", today);
        match &mut writer {
            LogWriter::Daily { rollover_retry, .. } => {
                *rollover_retry = Some(RolloverRetry {
                    target_date: today - Duration::days(1),
                    retry_after: Instant::now() + ROLLOVER_RETRY_INTERVAL,
                });
            }
            LogWriter::Fixed(_) => panic!("expected daily writer"),
        }

        writer.write_all(b"new target\n").unwrap();
        writer.flush().unwrap();

        assert_eq!(read_log(&current_path), "new target\n");
        assert!(matches!(
            &writer,
            LogWriter::Daily {
                active_date,
                rollover_retry: None,
                ..
            } if *active_date == today
        ));

        drop(writer);
        let _ = std::fs::remove_dir_all(scratch);
    }

    #[test]
    fn fixed_fallback_and_panic_double_open_failure_are_fallible() {
        let scratch = test_log_dir("wta-fixed-fallback");
        let daily = dated_log_path(&scratch, "wta-main", OffsetDateTime::now_utc().date());
        std::fs::create_dir(daily).unwrap();

        let mut writer = LogWriter::new("main", &scratch).unwrap();
        assert!(matches!(writer, LogWriter::Fixed(_)));
        writer.write_all(b"fixed record\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(read_log(scratch.join("wta-main.log")), "fixed record\n");
        drop(writer);

        let not_a_directory = scratch.join("not-a-directory");
        std::fs::write(&not_a_directory, "file").unwrap();
        let result =
            std::panic::catch_unwind(|| write_panic_record(&not_a_directory, b"panic record"));
        assert!(result.is_ok_and(|result| result.is_err()));

        let _ = std::fs::remove_dir_all(scratch);
    }
}

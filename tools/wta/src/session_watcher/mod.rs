//! session_watcher — turn each agent CLI's on-disk session records into the
//! crate's existing [`crate::agent_sessions::SessionEvent`]s, hook-free.
//!
//! The per-CLI `classify_*` functions are the pure, testable core: they take
//! one parsed record plus the session key and return zero or more
//! `SessionEvent`s. The watch actor ([`start`]) is the thin impure shell that
//! tails files (by byte offset — all four CLIs are append logs) and feeds
//! records through them. Path → session identity lives in [`discover`].

pub mod classify_claude;
pub mod classify_codex;
pub mod classify_copilot;
pub mod classify_gemini;
pub mod discover;

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;

use anyhow::Context;

/// Read the bytes appended to `path` since byte offset `from`, returning the
/// decoded text and the new end offset. Used for the append-only CLIs.
pub fn read_appended(path: &Path, from: u64) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len <= from {
        return Ok((String::new(), len));
    }
    file.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::with_capacity((len - from) as usize);
    file.take(len - from).read_to_end(&mut buf)?;
    Ok((String::from_utf8_lossy(&buf).into_owned(), len))
}

use crate::agent_sessions::{CliSource, SessionEvent};

/// One emitted event with its routing identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emitted {
    pub cli: CliSource,
    pub key: String,
    pub event: SessionEvent,
}

/// A watcher event scoped to the tracking generation that observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Observed {
    pub generation: u64,
    pub emitted: Emitted,
}

/// Per-file progress so we only classify new records.
#[derive(Default)]
pub(crate) struct Progress {
    /// Byte offset for append-only CLIs (all four are append logs).
    offset: u64,
    /// Gemini's canonical session id, resolved once from the file header's
    /// `sessionId` (the filename only carries the first 8 hex chars) and cached
    /// so every emitted event keys to the same id the registry binds. `None`
    /// until first read; falls back to the path-derived key if the header is
    /// unreadable.
    gemini_key: Option<String>,
    /// Set once if this file is a Codex multi-agent subagent fork — its records
    /// are then ignored wholesale (it inherits the parent's history and is not a
    /// user-facing session, so surfacing it would duplicate the parent's row).
    ignored: bool,
}

/// Process one changed file path into emitted events, advancing `progress`.
/// Pure w.r.t. everything except the on-disk file and the passed-in map.
pub fn process_change(path: &Path, progress: &mut HashMap<PathBuf, Progress>) -> Vec<Emitted> {
    let Some(disc) = discover::identify(path) else {
        return Vec::new();
    };
    let entry = progress.entry(path.to_path_buf()).or_default();
    if entry.ignored {
        // A Codex subagent fork detected on a previous read — skip wholesale.
        return Vec::new();
    }
    let mut out = Vec::new();

    match disc.cli {
        CliSource::Gemini => {
            // Gemini's `session-*.jsonl` is an append log (re-verified
            // 2026-06-14): single-message records and `$set` ops are appended at
            // the end; the only rewrites are full `$set.messages` snapshots at
            // start/resume, which `classify_record` skips so a resume can't
            // replay history. Read it by byte offset like the other CLIs.
            //
            // Canonical key = the header `sessionId` (the filename only carries
            // the first 8 hex chars). Resolve + cache it once; fall back to the
            // path-derived key until the header is readable.
            if entry.gemini_key.is_none() {
                entry.gemini_key = read_gemini_session_id(path);
            }
            let key = entry.gemini_key.clone().unwrap_or_else(|| disc.key.clone());

            let from = entry.offset;
            let Ok((text, len)) = read_appended(path, from) else {
                return out;
            };
            if len < from {
                entry.offset = len;
                return out;
            }
            let consumed = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
            entry.offset = from + consumed as u64;
            for line in text[..consumed].lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                for event in classify_gemini::classify_record(&val, &key) {
                    out.push(Emitted {
                        cli: CliSource::Gemini,
                        key: key.clone(),
                        event,
                    });
                }
            }
        }
        _ => {
            let from = entry.offset;
            let Ok((text, len)) = read_appended(path, from) else {
                return out;
            };
            if len < from {
                // File shrank/rotated — resync to the new end, drop nothing
                // further this tick.
                entry.offset = len;
                return out;
            }
            // Only consume through the last newline; a trailing partial line is
            // a record still being written — leave its bytes for the next tick.
            let consumed = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
            entry.offset = from + consumed as u64;
            for line in text[..consumed].lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                // A Codex multi-agent subagent fork carries `source.subagent` in
                // its session_meta (always the first record). Mark the file
                // ignored and drop everything: it inherits the parent's history,
                // so tracking it would duplicate the parent's row.
                if matches!(disc.cli, CliSource::Codex)
                    && classify_codex::record_is_subagent_meta(&val)
                {
                    entry.ignored = true;
                    return Vec::new();
                }
                let events = match disc.cli {
                    CliSource::Copilot => classify_copilot::classify(&val, &disc.key),
                    CliSource::Claude => classify_claude::classify(&val, &disc.key),
                    CliSource::Codex => classify_codex::classify(&val, &disc.key),
                    _ => Vec::new(),
                };
                for event in events {
                    out.push(Emitted {
                        cli: disc.cli.clone(),
                        key: disc.key.clone(),
                        event,
                    });
                }
            }
        }
    }
    out
}

/// The four watched roots under the user profile. Empty when `USERPROFILE` is
/// unset: returning relative `.copilot/...` paths would make the watcher
/// observe directories under the process CWD, so we disable the watcher cleanly
/// (watch nothing) instead.
pub fn watched_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) else {
        return Vec::new();
    };
    vec![
        home.join(".copilot").join("session-state"),
        home.join(".claude").join("projects"),
        home.join(".codex").join("sessions"),
        home.join(".gemini").join("tmp"),
    ]
}

/// Seed per-file progress to each existing session file's current end, so the
/// watcher only processes content appended *after* it starts. Without this, the
/// first `notify` event for a preexisting historical file (which the OS can
/// deliver spuriously — e.g. an indexer/AV touch, or a delayed
/// ReadDirectoryChangesW batch) would make `process_change` replay that file's
/// entire record stream from offset 0. Each replayed record revives its
/// historical Class-B session and re-broadcasts `sessions/changed`, flooding
/// master with thousands of redundant notifications and stalling live updates.
///
/// Files created *after* the watcher starts are not seeded (not present here),
/// so their first sighting is still read from offset 0 — correctly catching a
/// new session's opening `session_meta` / `task_started` records.
///
/// Apart from paths removed during traversal, errors must abort installation:
/// treating an unreadable old file as unseeded could replay it on a later event.
/// Returns false if a tracking transition cancels the traversal.
pub(crate) fn seed_existing_progress_in(
    roots: &[PathBuf],
    progress: &mut HashMap<PathBuf, Progress>,
    is_current: impl Fn() -> bool,
) -> std::io::Result<bool> {
    for root in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            if !is_current() {
                return Ok(false);
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            for entry in entries {
                if !is_current() {
                    return Ok(false);
                }
                let entry = entry?;
                let path = entry.path();
                match entry.file_type() {
                    Ok(ft) if ft.is_dir() => stack.push(path),
                    Ok(_) => {
                        let Some(_disc) = discover::identify(&path) else {
                            continue;
                        };
                        // All four CLIs are append logs — seed each file's
                        // progress to its current end so the watcher only
                        // processes content appended after it starts.
                        let offset = match std::fs::metadata(&path) {
                            Ok(metadata) => metadata.len(),
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                            Err(err) => return Err(err),
                        };
                        let prog = Progress {
                            offset,
                            ..Default::default()
                        };
                        progress.insert(path, prog);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                }
            }
        }
    }
    Ok(is_current())
}

/// Gemini's canonical session id, read from the file header's `sessionId`
/// field (the first line). `None` on any read/parse failure or if the header
/// lacks the field. Reads only the first line, not the whole (often large) file.
fn read_gemini_session_id(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    let val: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
    val.get("sessionId")
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

enum Message {
    SetEnabled {
        enabled: bool,
        generation: u64,
    },
    Notify {
        generation: u64,
        event: notify::Result<notify::Event>,
    },
    Shutdown,
}

struct DesiredWatch {
    enabled: AtomicBool,
    generation: AtomicU64,
}

impl DesiredWatch {
    fn new(enabled: bool, generation: u64) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            generation: AtomicU64::new(generation),
        }
    }

    fn set(&self, enabled: bool, generation: u64) {
        if self.matches(enabled, generation) {
            return;
        }
        // Publish cancellation before the new epoch. The master serializes
        // transitions and advances generation on every toggle.
        self.enabled.store(false, Ordering::SeqCst);
        self.generation.store(generation, Ordering::SeqCst);
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    fn matches(&self, enabled: bool, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
            && self.enabled.load(Ordering::SeqCst) == enabled
    }
}

/// Owns the watcher actor's lifetime. Dropping it explicitly stops observation.
pub(crate) struct WatchControl {
    inbox: Sender<Message>,
    desired: Arc<DesiredWatch>,
}

impl WatchControl {
    /// Queue a transition without blocking the caller on filesystem I/O.
    ///
    /// The owner advances `generation` whenever tracking changes and checks it
    /// again when receiving [`Observed`]. Installation failures are logged by
    /// the actor; a later enable request can retry them.
    pub fn set_enabled(&self, enabled: bool, generation: u64) -> anyhow::Result<()> {
        self.desired.set(enabled, generation);
        self.inbox
            .send(Message::SetEnabled {
                enabled,
                generation,
            })
            .map_err(|_| anyhow::anyhow!("session watcher actor has stopped"))
    }
}

impl Drop for WatchControl {
    fn drop(&mut self) {
        self.desired.enabled.store(false, Ordering::SeqCst);
        // notify itself retains an inbox sender, so channel disconnection
        // cannot be used to shut down an enabled watcher.
        let _ = self.inbox.send(Message::Shutdown);
    }
}

struct ActiveWatch {
    generation: u64,
    _watcher: notify::RecommendedWatcher,
    roots: Vec<PathBuf>,
    progress: HashMap<PathBuf, Progress>,
}

struct WatchActor {
    roots: Vec<PathBuf>,
    inbox: Sender<Message>,
    output: tokio::sync::mpsc::UnboundedSender<Observed>,
    desired: Arc<DesiredWatch>,
    active: Option<ActiveWatch>,
}

impl WatchActor {
    fn discard_stale_watch(&mut self) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| !self.desired.matches(true, active.generation))
        {
            self.active = None;
        }
    }

    fn set_enabled(&mut self, enabled: bool, generation: u64) -> anyhow::Result<()> {
        self.discard_stale_watch();
        if !self.desired.matches(enabled, generation) {
            return Ok(());
        }
        if enabled
            && self
                .active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
        {
            return Ok(());
        }

        // Drop both subscriptions and offsets before handling any more events.
        // While disabled, the actor only waits on its inbox and does no I/O.
        self.active = None;
        if !enabled {
            return Ok(());
        }

        use notify::{RecursiveMode, Watcher};

        let inbox = self.inbox.clone();
        let desired = self.desired.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            if desired.matches(true, generation) {
                // A callback racing with shutdown has nowhere left to deliver.
                let _ = inbox.send(Message::Notify { generation, event });
            }
        })
        .context("creating session file watcher")?;
        let mut progress = HashMap::new();
        let mut roots = Vec::new();
        let mut first_error = None;
        for root in &self.roots {
            if !self.desired.matches(true, generation) {
                return Ok(());
            }
            let mut registered = false;
            let install = (|| -> anyhow::Result<Option<HashMap<PathBuf, Progress>>> {
                if !root
                    .try_exists()
                    .with_context(|| format!("checking session root {}", root.display()))?
                {
                    return Ok(None); // The user may not have this CLI installed.
                }
                watcher
                    .watch(root, RecursiveMode::Recursive)
                    .with_context(|| format!("watching session root {}", root.display()))?;
                registered = true;
                // Callbacks only enqueue until this root has a complete EOF
                // baseline. A failed provider must not disable healthy ones.
                let mut root_progress = HashMap::new();
                if !seed_existing_progress_in(
                    std::slice::from_ref(root),
                    &mut root_progress,
                    || self.desired.matches(true, generation),
                )
                .with_context(|| format!("seeding session root {}", root.display()))?
                {
                    return Ok(None);
                }
                Ok(Some(root_progress))
            })();
            match install {
                Ok(Some(root_progress)) => {
                    roots.push(root.clone());
                    progress.extend(root_progress);
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        target: "session_watcher",
                        root = %root.display(),
                        error = %format!("{error:#}"),
                        "session root unavailable; other providers remain observed"
                    );
                    if registered {
                        if let Err(error) = watcher.unwatch(root) {
                            tracing::warn!(
                                target: "session_watcher",
                                root = %root.display(),
                                %error,
                                "could not unsubscribe unseeded root; its events will be ignored"
                            );
                        }
                    }
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if !self.desired.matches(true, generation) {
            return Ok(());
        }
        if roots.is_empty() {
            if let Some(error) = first_error {
                return Err(error);
            }
        }
        self.active = Some(ActiveWatch {
            generation,
            _watcher: watcher,
            roots,
            progress,
        });
        Ok(())
    }

    fn observe(&mut self, generation: u64, event: notify::Result<notify::Event>) -> bool {
        // Do not wait for SetEnabled behind a backlog of raw notifications:
        // the first dispatch after a toggle drops the superseded subscription.
        self.discard_stale_watch();
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.generation == generation)
        else {
            return true;
        };
        let event = match event {
            Ok(event) => event,
            Err(err) => {
                tracing::warn!(
                    target: "session_watcher",
                    generation,
                    error = %err,
                    "notify error"
                );
                return true;
            }
        };
        for path in event.paths {
            if !active.roots.iter().any(|root| path.starts_with(root)) {
                continue;
            }
            if !self.desired.matches(true, generation) {
                self.active = None;
                return true;
            }
            for emitted in process_change(&path, &mut active.progress) {
                if !self.desired.matches(true, generation) {
                    self.active = None;
                    return true;
                }
                if self
                    .output
                    .send(Observed {
                        generation,
                        emitted,
                    })
                    .is_err()
                {
                    return false;
                }
            }
        }
        true
    }

    fn run(mut self, messages: mpsc::Receiver<Message>) {
        for message in messages {
            match message {
                Message::SetEnabled {
                    enabled,
                    generation,
                } => {
                    if let Err(err) = self.set_enabled(enabled, generation) {
                        tracing::error!(
                            target: "session_watcher",
                            generation,
                            error = %format!("{err:#}"),
                            "session file observation could not be enabled; a later enable can retry"
                        );
                    }
                }
                Message::Notify { generation, event } => {
                    if !self.observe(generation, event) {
                        break;
                    }
                }
                Message::Shutdown => break,
            }
        }
    }
}

/// Start the event-driven fallback watcher actor on a dedicated blocking thread.
/// The Sessions preference controls observation, not explicit Resume bindings.
///
/// Off retains only the waiting actor, without notify subscriptions or progress.
/// Every enable seeds existing files to EOF, excluding activity while disabled.
/// There are no periodic sweeps or timeout-based polls; pane lifecycle events
/// remain responsible for ending fallback sessions.
pub(crate) fn start(
    tx: tokio::sync::mpsc::UnboundedSender<Observed>,
    enabled: bool,
    generation: u64,
) -> std::io::Result<WatchControl> {
    let (inbox, messages) = mpsc::channel();
    let desired = Arc::new(DesiredWatch::new(enabled, generation));
    let actor = WatchActor {
        roots: watched_roots(),
        inbox: inbox.clone(),
        output: tx,
        desired: desired.clone(),
        active: None,
    };
    // Queue startup through the same path as later transitions, including its
    // error reporting. No filesystem work runs on the caller's async thread.
    inbox
        .send(Message::SetEnabled {
            enabled,
            generation,
        })
        .map_err(|_| std::io::Error::other("session watcher inbox disconnected at startup"))?;
    std::thread::Builder::new()
        .name("session-watcher".into())
        .spawn(move || actor.run(messages))?;
    Ok(WatchControl { inbox, desired })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let root = std::env::current_dir()
                .unwrap()
                .join("target")
                .join("session-watcher-tests")
                .join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_actor(
        roots: Vec<PathBuf>,
    ) -> (
        WatchActor,
        mpsc::Receiver<Message>,
        tokio::sync::mpsc::UnboundedReceiver<Observed>,
    ) {
        let (inbox, messages) = mpsc::channel();
        let (output, observed) = tokio::sync::mpsc::unbounded_channel();
        (
            WatchActor {
                roots,
                inbox,
                output,
                desired: Arc::new(DesiredWatch::new(false, 0)),
                active: None,
            },
            messages,
            observed,
        )
    }

    fn changed(path: &Path) -> notify::Result<notify::Event> {
        Ok(notify::Event::new(notify::EventKind::Any).add_path(path.to_path_buf()))
    }

    fn append(path: &Path, bytes: &[u8]) {
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    fn transition(actor: &mut WatchActor, enabled: bool, generation: u64) -> anyhow::Result<()> {
        actor.desired.set(enabled, generation);
        actor.set_enabled(enabled, generation)
    }

    #[test]
    fn watch_actor_off_has_no_subscriptions_or_progress() {
        let dir = TestDir::new();
        // This is deliberately not a directory: trying to seed it would fail.
        let root = dir.0.join("session-state");
        std::fs::write(&root, b"not a directory").unwrap();
        let (mut actor, _messages, mut observed) = test_actor(vec![root.clone()]);

        transition(&mut actor, false, 0).unwrap();
        assert!(actor.active.is_none());
        assert!(actor.observe(0, changed(&root)));
        assert!(observed.try_recv().is_err());
    }

    #[test]
    fn control_toggle_cancels_backlog_before_commands_are_received() {
        for reenable in [false, true] {
            let dir = TestDir::new();
            let root = dir.0.join("session-state");
            let session = root.join("backlog");
            std::fs::create_dir_all(&session).unwrap();
            let path = session.join("events.jsonl");
            std::fs::write(&path, b"").unwrap();
            let (mut actor, messages, mut observed) = test_actor(vec![root]);
            transition(&mut actor, true, 1).unwrap();
            let control = WatchControl {
                inbox: actor.inbox.clone(),
                desired: actor.desired.clone(),
            };
            append(
                &path,
                b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n",
            );
            for _ in 0..128 {
                actor
                    .inbox
                    .send(Message::Notify {
                        generation: 1,
                        event: changed(&path),
                    })
                    .unwrap();
            }

            control.set_enabled(false, 2).unwrap();
            if reenable {
                control.set_enabled(true, 3).unwrap();
            }
            // Retain control (and its desired state) while the actor drains
            // notifications that precede the toggle commands in the inbox.
            actor.inbox.send(Message::Shutdown).unwrap();
            actor.run(messages);
            assert!(
                matches!(
                    observed.try_recv(),
                    Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
                ),
                "queued notifications must not read old bytes before Off or Off->On"
            );
        }
    }

    #[test]
    fn control_supersedes_queued_enable_before_installation() {
        let dir = TestDir::new();
        let root = dir.0.join("session-state");
        std::fs::write(&root, b"not a directory").unwrap();
        let (mut actor, _messages, _observed) = test_actor(vec![root]);
        let control = WatchControl {
            inbox: actor.inbox.clone(),
            desired: actor.desired.clone(),
        };

        control.set_enabled(true, 1).unwrap();
        control.set_enabled(false, 2).unwrap();
        // An attempted installation would fail on the invalid root; this old
        // command must instead return without any filesystem observation.
        actor.set_enabled(true, 1).unwrap();
        assert!(actor.active.is_none());
    }

    #[test]
    fn watch_actor_reenable_skips_disabled_history_and_stale_events() {
        let dir = TestDir::new();
        let root = dir.0.join("session-state");
        let session = root.join("existing");
        std::fs::create_dir_all(&session).unwrap();
        let path = session.join("events.jsonl");
        let record = b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n";
        std::fs::write(&path, record).unwrap();
        let (mut actor, _messages, mut observed) = test_actor(vec![root.clone()]);

        transition(&mut actor, true, 1).unwrap();
        assert!(actor.observe(1, changed(&path)));
        assert!(observed.try_recv().is_err(), "startup history is skipped");
        append(&path, record);
        assert!(actor.observe(1, changed(&path)));
        assert_eq!(observed.try_recv().unwrap().generation, 1);

        transition(&mut actor, false, 2).unwrap();
        assert!(actor.active.is_none(), "Off drops watcher and progress");
        append(&path, record);
        let created_off = root.join("created-off").join("events.jsonl");
        std::fs::create_dir_all(created_off.parent().unwrap()).unwrap();
        std::fs::write(&created_off, record).unwrap();
        assert!(actor.observe(1, changed(&path)));
        assert!(actor.observe(1, changed(&created_off)));
        assert!(observed.try_recv().is_err(), "Off ignores queued callbacks");

        transition(&mut actor, true, 3).unwrap();
        assert!(actor.observe(3, changed(&path)));
        assert!(actor.observe(3, changed(&created_off)));
        assert!(
            observed.try_recv().is_err(),
            "reenable skips appended history and files created while Off"
        );

        let seeded_offset = actor.active.as_ref().unwrap().progress[&path].offset;
        append(&path, record);
        assert!(actor.observe(1, changed(&path)));
        assert_eq!(
            actor.active.as_ref().unwrap().progress[&path].offset,
            seeded_offset,
            "stale callbacks must not read or advance progress"
        );
        assert!(observed.try_recv().is_err());
        assert!(actor.observe(3, changed(&path)));
        let fresh = observed.try_recv().unwrap();
        assert_eq!(fresh.generation, 3);
        assert_eq!(fresh.emitted.key, "existing");
        assert!(matches!(
            fresh.emitted.event,
            SessionEvent::ToolStarting { .. }
        ));
        assert!(observed.try_recv().is_err(), "only fresh data is emitted");

        append(&created_off, record);
        assert!(actor.observe(3, changed(&created_off)));
        let fresh = observed.try_recv().unwrap();
        assert_eq!(fresh.generation, 3);
        assert_eq!(fresh.emitted.key, "created-off");
        assert!(observed.try_recv().is_err());
    }

    #[test]
    fn watch_actor_duplicate_enable_does_not_discard_new_activity() {
        let dir = TestDir::new();
        let root = dir.0.join("session-state");
        let session = root.join("duplicate-enable");
        std::fs::create_dir_all(&session).unwrap();
        let path = session.join("events.jsonl");
        std::fs::write(&path, b"").unwrap();
        let (mut actor, _messages, mut observed) = test_actor(vec![root]);

        transition(&mut actor, true, 1).unwrap();
        append(
            &path,
            b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n",
        );
        transition(&mut actor, true, 1).unwrap();
        assert!(actor.observe(1, changed(&path)));
        assert_eq!(observed.try_recv().unwrap().generation, 1);
        assert!(observed.try_recv().is_err());
    }

    #[test]
    fn watch_actor_failed_install_stays_off_and_can_retry() {
        let dir = TestDir::new();
        let root = dir.0.join("session-state");
        std::fs::write(&root, b"not a directory").unwrap();
        let (mut actor, _messages, mut observed) = test_actor(vec![root.clone()]);

        assert!(transition(&mut actor, true, 1).is_err());
        assert!(actor.active.is_none(), "partial installation is dropped");
        assert!(actor.observe(1, changed(&root)));
        assert!(observed.try_recv().is_err());

        std::fs::remove_file(&root).unwrap();
        std::fs::create_dir_all(root.join("retry")).unwrap();
        let path = root.join("retry").join("events.jsonl");
        let record = b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n";
        std::fs::write(&path, record).unwrap();
        transition(&mut actor, true, 1).unwrap();
        assert!(actor.observe(1, changed(&path)));
        assert!(observed.try_recv().is_err(), "retry still seeds to EOF");
        append(&path, record);
        assert!(actor.observe(1, changed(&path)));
        assert_eq!(observed.try_recv().unwrap().generation, 1);
    }

    #[test]
    fn watch_actor_failed_provider_does_not_disable_healthy_roots_or_replay_history() {
        let dir = TestDir::new();
        let healthy = dir.0.join("healthy").join("session-state");
        let failing = dir.0.join("failing").join("session-state");
        std::fs::create_dir_all(healthy.join("session")).unwrap();
        std::fs::create_dir_all(failing.parent().unwrap()).unwrap();
        std::fs::write(&failing, b"not a directory").unwrap();
        let healthy_path = healthy.join("session").join("events.jsonl");
        let record = b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n";
        std::fs::write(&healthy_path, record).unwrap();
        let (mut actor, _messages, mut observed) = test_actor(vec![failing.clone(), healthy]);
        transition(&mut actor, true, 1).unwrap();
        assert_eq!(actor.active.as_ref().unwrap().roots.len(), 1);
        assert!(actor.observe(1, changed(&healthy_path)));
        assert!(observed.try_recv().is_err());
        append(&healthy_path, record);
        assert!(actor.observe(1, changed(&healthy_path)));
        assert_eq!(observed.try_recv().unwrap().emitted.key, "session");

        std::fs::remove_file(&failing).unwrap();
        let recovered = failing.join("recovered").join("events.jsonl");
        std::fs::create_dir_all(recovered.parent().unwrap()).unwrap();
        std::fs::write(&recovered, record).unwrap();
        assert!(actor.observe(1, changed(&recovered)));
        assert!(
            observed.try_recv().is_err(),
            "unseeded provider callbacks must be ignored"
        );
        transition(&mut actor, false, 2).unwrap();
        transition(&mut actor, true, 3).unwrap();
        assert_eq!(actor.active.as_ref().unwrap().roots.len(), 2);
        assert!(actor.observe(3, changed(&recovered)));
        assert!(
            observed.try_recv().is_err(),
            "recovered provider must seed old records"
        );
        append(&recovered, record);
        assert!(actor.observe(3, changed(&recovered)));
        assert_eq!(observed.try_recv().unwrap().emitted.key, "recovered");
    }

    #[test]
    fn control_drop_shuts_down_with_callback_sender_still_alive() {
        let (mut actor, messages, mut observed) = test_actor(Vec::new());
        transition(&mut actor, true, 1).unwrap();
        let control = WatchControl {
            inbox: actor.inbox.clone(),
            desired: actor.desired.clone(),
        };
        let callback_sender = actor.inbox.clone();
        let thread = std::thread::spawn(move || actor.run(messages));

        drop(control);
        thread.join().unwrap();
        assert!(matches!(
            observed.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ));
        drop(callback_sender);
    }

    #[test]
    fn control_reports_a_stopped_actor() {
        let (inbox, messages) = mpsc::channel();
        drop(messages);
        let control = WatchControl {
            inbox,
            desired: Arc::new(DesiredWatch::new(false, 0)),
        };
        assert!(control.set_enabled(true, 1).is_err());
    }

    #[test]
    fn read_appended_returns_only_new_bytes() {
        let dir = TestDir::new();
        let path = dir.0.join("a.jsonl");
        std::fs::write(&path, b"line1\n").unwrap();
        let (first, off1) = read_appended(&path, 0).unwrap();
        assert_eq!(first, "line1\n");
        assert_eq!(off1, 6);
        // Append more, read only the delta.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"line2\n")
            .unwrap();
        let (second, off2) = read_appended(&path, off1).unwrap();
        assert_eq!(second, "line2\n");
        assert_eq!(off2, 12);
    }

    #[test]
    fn process_change_emits_copilot_events_incrementally() {
        let root = TestDir::new();
        let dir = root.0.join("session-state").join("sess-9");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n",
        )
        .unwrap();
        let mut progress = HashMap::new();
        let first = process_change(&path, &mut progress);
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0].event, SessionEvent::ToolStarting { .. }));
        // No new bytes -> no duplicate events.
        let second = process_change(&path, &mut progress);
        assert!(second.is_empty());
    }

    #[test]
    fn process_change_does_not_lose_a_partial_line() {
        let root = TestDir::new();
        let dir = root.0.join("session-state").join("sess-partial");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        // One complete record + a half-written second record (no newline yet).
        std::fs::write(
            &path,
            b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n{\"type\":\"assistant.turn",
        )
        .unwrap();
        let mut progress = std::collections::HashMap::new();
        let first = process_change(&path, &mut progress);
        assert_eq!(first.len(), 1, "only the complete line should classify");
        assert!(matches!(first[0].event, SessionEvent::ToolStarting { .. }));
        // Complete the partial record (turn_end → ToolCompleted under the
        // turn-based Copilot model).
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"_end\",\"data\":{\"turnId\":\"0\"}}\n")
            .unwrap();
        let second = process_change(&path, &mut progress);
        assert_eq!(
            second.len(),
            1,
            "the completed record must now classify (not be lost)"
        );
        assert!(matches!(
            second[0].event,
            SessionEvent::ToolCompleted { .. }
        ));
    }

    #[test]
    fn seed_skips_preexisting_history_then_tracks_new_appends() {
        // A preexisting Codex rollout (history) must be seeded to EOF so it is
        // NOT replayed from offset 0 — the bug that flooded master with revive
        // broadcasts. New content appended after seeding is still tracked.
        let dir = TestDir::new();
        let root = &dir.0;
        let day = root.join("2026").join("06").join("10");
        std::fs::create_dir_all(&day).unwrap();
        let path =
            day.join("rollout-2026-06-10T00-00-00-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n\
              {\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\"}}\n",
        )
        .unwrap();

        let mut progress = HashMap::new();
        assert!(
            seed_existing_progress_in(std::slice::from_ref(root), &mut progress, || true).unwrap()
        );

        // History is skipped — no replay on the first change.
        let replay = process_change(&path, &mut progress);
        assert!(
            replay.is_empty(),
            "seeded historical file must not replay, got {:?}",
            replay
        );

        // A genuinely new appended record IS processed.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n")
            .unwrap();
        let fresh = process_change(&path, &mut progress);
        assert_eq!(fresh.len(), 1, "new appended record must be classified");
        assert!(matches!(fresh[0].event, SessionEvent::ToolCompleted { .. }));
    }

    #[test]
    fn seed_does_not_skip_files_created_after_start() {
        // A file absent at seed time (new session created after the watcher
        // started) is not seeded, so it's read in full on first sight.
        let dir = TestDir::new();
        let root = &dir.0;
        let mut progress = HashMap::new();
        assert!(
            seed_existing_progress_in(std::slice::from_ref(root), &mut progress, || true).unwrap()
        );

        let path =
            root.join("rollout-2026-06-10T00-00-00-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();
        let out = process_change(&path, &mut progress);
        assert_eq!(out.len(), 1, "a new file must be read from offset 0");
        assert!(matches!(out[0].event, SessionEvent::ToolStarting { .. }));
    }

    #[test]
    fn seed_skips_a_partial_record_written_before_enable() {
        let dir = TestDir::new();
        let session = dir.0.join("session-state").join("partial");
        std::fs::create_dir_all(&session).unwrap();
        let path = session.join("events.jsonl");
        let prefix = b"{\"type\":\"assistant.turn";
        std::fs::write(&path, prefix).unwrap();
        let mut progress = HashMap::new();

        assert!(
            seed_existing_progress_in(std::slice::from_ref(&dir.0), &mut progress, || true)
                .unwrap()
        );
        assert_eq!(progress[&path].offset, prefix.len() as u64);
        append(
            &path,
            b"_end\",\"data\":{\"turnId\":\"0\"}}\n\
              {\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n",
        );
        let fresh = process_change(&path, &mut progress);
        assert_eq!(
            fresh.len(),
            1,
            "the pre-enable partial record is not replayed"
        );
        assert!(matches!(fresh[0].event, SessionEvent::ToolStarting { .. }));
    }

    #[test]
    fn seed_reports_invalid_roots_but_allows_missing_roots() {
        let dir = TestDir::new();
        let root = dir.0.join("not-a-directory");
        let mut progress = HashMap::new();
        assert!(
            seed_existing_progress_in(std::slice::from_ref(&root), &mut progress, || true).unwrap()
        );
        std::fs::write(&root, b"not a directory").unwrap();
        assert!(seed_existing_progress_in(&[root], &mut progress, || true).is_err());
        assert!(progress.is_empty());
    }

    #[test]
    fn seed_cancellation_does_not_read_roots() {
        let dir = TestDir::new();
        let root = dir.0.join("not-a-directory");
        std::fs::write(&root, b"not a directory").unwrap();
        let mut progress = HashMap::new();

        assert!(!seed_existing_progress_in(&[root], &mut progress, || false).unwrap());
        assert!(progress.is_empty());
    }

    #[test]
    fn process_change_ignores_codex_subagent_rollout() {
        // Codex's multi_agent_v1/spawn_agent forks a child thread with its own
        // rollout (source.subagent) that inherits the parent's history — it must
        // never surface as its own (duplicate) row.
        let root = TestDir::new();
        let dir = root.0.join("2026").join("06").join("10");
        std::fs::create_dir_all(&dir).unwrap();
        let path =
            dir.join("rollout-2026-06-10T13-15-12-99999999-2222-3333-4444-555555555555.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"99999999-2222-3333-4444-555555555555\",\"forked_from_id\":\"p\",\"source\":{\"subagent\":{\"thread_spawn\":{\"depth\":1}}}}}\n\
              {\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();

        let mut progress = HashMap::new();
        let out = process_change(&path, &mut progress);
        assert!(
            out.is_empty(),
            "subagent rollout must emit nothing, got {:?}",
            out
        );

        // Even after the subagent does work, it stays ignored.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n")
            .unwrap();
        assert!(
            process_change(&path, &mut progress).is_empty(),
            "a file flagged as a subagent stays ignored on later reads"
        );
    }
}

// wta/src/agent_sessions.rs
//
// Runtime registry for tracking live and historical CLI agent sessions.
// Independent from `agent_registry.rs`, which is the static catalog of
// CLI profiles.
//
// Two GUIDs flow through this module — they are DIFFERENT things and must
// not be confused:
//
//   * `pane_session_id` (== $env:WT_SESSION in the agent's pane):
//       The Windows Terminal pane/connection GUID. Set by
//       ConptyConnection.cpp on every spawned shell. Used as the routing
//       key for `wtcli focus-pane -t <guid>` and for filtering "is this
//       event from our own pane?". Plain GUID, no braces.
//
//   * `key` (the AgentKey, derived from the CLI agent's own session id):
//       Claude's UUID under ~/.claude/projects/.../<uuid>.jsonl, Gemini's
//       `sessionId` field, Copilot's session-state folder name. This is
//       what `claude --resume <id>` consumes. Stable across pane
//       lifetimes — a resumed session keeps the same key but gets a
//       brand-new pane_session_id.
//
// `resolve_or_synthesize_key` synthesises a `pane:<guid>` key from
// pane_session_id only when the agent's own session id is unknown
// (e.g. Copilot CLI doesn't fire any hooks). Such a row cannot be
// resumed — it's only valid while the pane is live.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

pub type AgentKey = String;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CliSource {
    Claude,
    Copilot,
    Gemini,
    Unknown(String),
}

impl CliSource {
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("").to_ascii_lowercase().as_str() {
            "claude"  => Self::Claude,
            "copilot" => Self::Copilot,
            "gemini"  => Self::Gemini,
            ""        => Self::Unknown(String::new()),
            other     => Self::Unknown(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentStatus {
    Idle,
    Working,
    Attention,
    Error,
    Ended,
    Historical,
}

#[derive(Clone, Debug)]
pub struct AgentSession {
    pub key:               AgentKey,
    pub cli_source:        CliSource,
    pub pane_session_id:   Option<String>,    // Guid as text form
    pub window_id:         Option<u64>,
    pub tab_id:            Option<u32>,
    pub title:             String,
    pub cwd:               PathBuf,
    pub started_at:        SystemTime,
    pub last_activity_at:  SystemTime,
    pub status:            AgentStatus,
    pub last_error:        Option<String>,
    pub current_tool:      Option<String>,
    pub attention_reason:  Option<String>,
    pub log_path:          Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    SessionStarted   { key: AgentKey, cli_source: CliSource, pane_session_id: String, cwd: PathBuf, title: String },
    ToolStarting     { key: AgentKey, tool_name: String },
    ToolCompleted    { key: AgentKey },
    Notification     { key: AgentKey, message: String },
    SessionStopped   { key: AgentKey, reason: String },
    ConnectionFailed { pane_session_id: String, reason: String },
    PaneClosed       { pane_session_id: String },
    /// Optimistic transition: a resume command for this key was just dispatched.
    /// Bumps a Historical/Ended row to Idle so a rapid second Enter on the same
    /// row doesn't dispatch another `wtcli split-pane` and create a duplicate
    /// pane while we wait for the new pane's SessionStarted hook to arrive.
    /// pane_session_id stays None until the hook lands; activate_session's
    /// focus-pane branch is a no-op while pane_session_id is None.
    ResumeDispatched { key: AgentKey },
    /// Bind a freshly-spawned resume pane's GUID to its session row, BEFORE
    /// any SessionStarted hook fires. Sourced from the JSON output of
    /// `wtcli --json split-pane`. Necessary for CLIs without hooks (Gemini
    /// today, plus any future CLI we don't yet have a bridge for) so that
    /// when the user later closes the pane, our `connection_state: closed`
    /// → `PaneClosed` path can transition the row to Ended (empty status)
    /// instead of leaving it stuck at Idle indefinitely.
    ///
    /// Idempotent w.r.t. SessionStarted: if Claude/Copilot's hook fires
    /// after this event, SessionStarted re-binds the same pane GUID and
    /// produces the same end state. If the hook fires *before* this
    /// (Claude/Copilot's typical fast path), the row already has the
    /// pane GUID and this event is a no-op for the same key+pane.
    ResumePaneAssigned { key: AgentKey, pane_session_id: String },
}

/// Returns `true` for tool names that represent the agent soliciting input
/// from the user (a clarifying question or a forced-choice prompt) rather
/// than running an autonomous task. Such tools never auto-complete — they
/// block until the user answers — so the row should show ATTENTION, not
/// WORKING. Match list is case-insensitive.
///
/// Known matches (verified against actual hook payloads):
///   - Copilot CLI: `ask_user` (carries `tool_input.question` + `choices`)
/// Speculative aliases for other CLIs are included so the heuristic catches
/// the common variants without needing per-CLI plumbing.
pub fn is_user_input_tool(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(),
        "ask_user"
        | "askuser"
        | "ask-user"
        | "ask_question"
        | "askquestion"
        | "ask_for_clarification"
        | "request_input"
        | "request_user_input"
        | "user_input"
        | "prompt_user"
        | "clarification_request"
    )
}

#[derive(Default)]
pub struct AgentSessionRegistry {
    sessions:        HashMap<AgentKey, AgentSession>,
    active_by_pane:  HashMap<String, AgentKey>,   // pane Guid (text) -> AgentKey
    dirty:           bool,
}

impl AgentSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, ev: SessionEvent) {
        let now = SystemTime::now();
        // Pane GUIDs (`pane_session_id`) arrive in mixed case — hooks emit
        // lowercase (from the `WT_SESSION` env var), WT-native events emit
        // uppercase (canonical Windows GUID). Normalise to lowercase here
        // so `active_by_pane` lookups succeed regardless of source.
        let ev = match ev {
            SessionEvent::SessionStarted { key, cli_source, pane_session_id, cwd, title } =>
                SessionEvent::SessionStarted { key, cli_source, pane_session_id: pane_session_id.to_ascii_lowercase(), cwd, title },
            SessionEvent::ConnectionFailed { pane_session_id, reason } =>
                SessionEvent::ConnectionFailed { pane_session_id: pane_session_id.to_ascii_lowercase(), reason },
            SessionEvent::PaneClosed { pane_session_id } =>
                SessionEvent::PaneClosed { pane_session_id: pane_session_id.to_ascii_lowercase() },
            SessionEvent::ResumePaneAssigned { key, pane_session_id } =>
                SessionEvent::ResumePaneAssigned { key, pane_session_id: pane_session_id.to_ascii_lowercase() },
            other => other,
        };
        match ev {
            SessionEvent::SessionStarted { key, cli_source, pane_session_id, cwd, title } => {
                let entry = self.sessions.entry(key.clone()).or_insert_with(|| AgentSession {
                    key:               key.clone(),
                    cli_source:        cli_source.clone(),
                    pane_session_id:   None,
                    window_id:         None,
                    tab_id:            None,
                    title:             title.clone(),
                    cwd:               cwd.clone(),
                    started_at:        now,
                    last_activity_at:  now,
                    status:            AgentStatus::Idle,
                    last_error:        None,
                    current_tool:      None,
                    attention_reason:  None,
                    log_path:          None,
                });
                // If we're rebinding to a different pane, drop the old pane's mapping first.
                if let Some(old_pane) = entry.pane_session_id.take() {
                    if old_pane != pane_session_id {
                        self.active_by_pane.remove(&old_pane);
                        tracing::info!(
                            target: "agent_session_registry",
                            key = %key,
                            old_pane = %old_pane,
                            new_pane = %pane_session_id,
                            "SessionStarted rebinding pane_session_id",
                        );
                    }
                } else {
                    tracing::info!(
                        target: "agent_session_registry",
                        key = %key,
                        new_pane = %pane_session_id,
                        cli = ?cli_source,
                        "SessionStarted assigning pane_session_id (first bind)",
                    );
                }
                entry.cli_source       = cli_source;
                // Preserve an existing title (e.g. one loaded from disk by the
                // history loader) when the new event carries no replacement.
                // Live synth titles are sent only for genuinely new sessions
                // (route_agent_event_to_registry passes "" for resumed ones).
                if !title.is_empty() {
                    entry.title        = title;
                }
                entry.cwd              = cwd;
                entry.pane_session_id  = Some(pane_session_id.clone());
                entry.status           = AgentStatus::Idle;
                entry.last_error       = None;
                entry.attention_reason = None;
                entry.current_tool     = None;
                entry.last_activity_at = now;
                self.active_by_pane.insert(pane_session_id, key);
                self.dirty = true;
            }

            SessionEvent::ToolStarting { key, tool_name } => {
                if let Some(entry) = self.sessions.get_mut(&key) {
                    entry.status            = AgentStatus::Working;
                    entry.current_tool      = Some(tool_name);
                    entry.last_activity_at  = now;
                    self.dirty = true;
                }
            }

            SessionEvent::ToolCompleted { key } => {
                if let Some(entry) = self.sessions.get_mut(&key) {
                    // Demote to Idle when this completion resolves an active
                    // wait. Two cases produce Attention:
                    //   1. A user-input tool started (e.g. Copilot's `ask_user`)
                    //      — its matching ToolCompleted means the user replied.
                    //   2. A `Notification` event from a permission_prompt hook
                    //      escalated us mid-tool — the matching tool.completed
                    //      / tool.failed (approve / reject) resolves it.
                    // Both cases mean "next event clears Attention", so we
                    // demote on any ToolCompleted while in Attention. The Error
                    // state is separate (set by ConnectionFailed) and is NOT
                    // touched here, so transient-error UX still works.
                    let demotable = entry.status == AgentStatus::Working
                        || entry.status == AgentStatus::Attention;
                    if demotable {
                        entry.status            = AgentStatus::Idle;
                        entry.attention_reason  = None;
                    }
                    entry.current_tool      = None;
                    entry.last_activity_at  = now;
                    self.dirty = true;
                }
            }

            SessionEvent::Notification { key, message } => {
                if let Some(entry) = self.sessions.get_mut(&key) {
                    entry.status            = AgentStatus::Attention;
                    entry.attention_reason  = Some(message);
                    entry.last_activity_at  = now;
                    self.dirty = true;
                }
            }

            SessionEvent::SessionStopped { key, reason: _ } => {
                if let Some(entry) = self.sessions.get_mut(&key) {
                    entry.status        = AgentStatus::Ended;
                    if let Some(pane) = entry.pane_session_id.take() {
                        self.active_by_pane.remove(&pane);
                    }
                    entry.current_tool      = None;
                    entry.attention_reason  = None;
                    entry.last_activity_at  = now;
                    self.dirty = true;
                }
            }

            SessionEvent::PaneClosed { pane_session_id } => {
                if let Some(key) = self.active_by_pane.remove(&pane_session_id) {
                    if let Some(entry) = self.sessions.get_mut(&key) {
                        entry.status            = AgentStatus::Ended;
                        entry.pane_session_id   = None;
                        entry.current_tool      = None;
                        entry.attention_reason  = None;
                        entry.last_activity_at  = now;
                        self.dirty = true;
                    }
                }
            }

            SessionEvent::ConnectionFailed { pane_session_id, reason } => {
                if let Some(key) = self.active_by_pane.get(&pane_session_id).cloned() {
                    if let Some(entry) = self.sessions.get_mut(&key) {
                        entry.status            = AgentStatus::Error;
                        entry.last_error        = Some(reason);
                        entry.last_activity_at  = now;
                        self.dirty = true;
                    }
                }
            }

            SessionEvent::ResumeDispatched { key } => {
                if let Some(entry) = self.sessions.get_mut(&key) {
                    // Only flip when the row genuinely had no live pane to
                    // begin with. If a SessionStarted event won the race and
                    // already populated pane_session_id with the new pane's
                    // Guid, leave the row alone — the hook layer is the
                    // source of truth for live state.
                    if matches!(entry.status, AgentStatus::Historical | AgentStatus::Ended) {
                        entry.status            = AgentStatus::Idle;
                        entry.last_activity_at  = now;
                        self.dirty = true;
                    }
                }
            }

            SessionEvent::ResumePaneAssigned { key, pane_session_id } => {
                if let Some(entry) = self.sessions.get_mut(&key) {
                    // No-op fast path: pane already correctly bound (e.g. a
                    // SessionStarted hook beat the split-pane callback).
                    if entry.pane_session_id.as_deref() == Some(pane_session_id.as_str()) {
                        return;
                    }
                    // Drop a stale binding (rare: previous pane never closed
                    // cleanly). Always rebind to the new pane.
                    if let Some(old_pane) = entry.pane_session_id.take() {
                        if old_pane != pane_session_id {
                            self.active_by_pane.remove(&old_pane);
                            tracing::info!(
                                target: "agent_session_registry",
                                key = %key,
                                old_pane = %old_pane,
                                new_pane = %pane_session_id,
                                "ResumePaneAssigned rebinding pane_session_id",
                            );
                        }
                    } else {
                        tracing::info!(
                            target: "agent_session_registry",
                            key = %key,
                            new_pane = %pane_session_id,
                            "ResumePaneAssigned binding pane_session_id (no hook bridge)",
                        );
                    }
                    entry.pane_session_id  = Some(pane_session_id.clone());
                    entry.last_activity_at = now;
                    self.active_by_pane.insert(pane_session_id, key);
                    self.dirty = true;
                }
            }
        }
    }

    pub fn iter_sorted(&self) -> Vec<&AgentSession> {
        let mut v: Vec<_> = self.sessions.values().collect();
        v.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
        v
    }

    pub fn take_dirty(&mut self) -> bool {
        let d = self.dirty;
        self.dirty = false;
        d
    }

    /// Resolve the key for an incoming hook event, falling back to a
    /// pane-Guid-derived placeholder when no agent_session_id was provided.
    pub fn resolve_or_synthesize_key(
        &self,
        agent_session_id: &str,
        pane_session_id: &str,
    ) -> AgentKey {
        if !agent_session_id.is_empty() {
            return agent_session_id.to_string();
        }
        let pane_lc = pane_session_id.to_ascii_lowercase();
        if let Some(existing) = self.active_by_pane.get(&pane_lc) {
            return existing.clone();
        }
        format!("pane:{}", pane_lc)
    }

    pub fn has_session(&self, key: &AgentKey) -> bool {
        self.sessions.contains_key(key)
    }

    /// Returns true if the given pane GUID is currently bound to an agent
    /// CLI session (Copilot/Claude/Gemini/...). Used by the autofix path to
    /// suppress "command failed" classification when the failing process is
    /// actually one of our managed agent CLIs exiting — Ctrl+C in Gemini is
    /// not a user command failure that needs auto-fix.
    pub fn is_agent_pane(&self, pane_session_id: &str) -> bool {
        // Lowercase the lookup key — hooks emit lowercase pane GUIDs but
        // WT-native vt_sequence/connection_state events emit uppercase.
        // active_by_pane is keyed by lowercase via apply()'s normaliser.
        self.active_by_pane.contains_key(&pane_session_id.to_ascii_lowercase())
    }

    pub fn remove(&mut self, key: &AgentKey) {
        if let Some(s) = self.sessions.remove(key) {
            if let Some(pane) = s.pane_session_id {
                self.active_by_pane.remove(&pane);
            }
            self.dirty = true;
        }
    }

    /// Drop any synthetic `pane:<guid>` session bound to the given pane.
    /// Used when a real `agent.session.started` arrives to clean up the
    /// placeholder created by an earlier tool event with no agent_session_id.
    pub fn drop_synthetic_for_pane(&mut self, pane_session_id: &str) {
        let pane_lc = pane_session_id.to_ascii_lowercase();
        if let Some(key) = self.active_by_pane.get(&pane_lc).cloned() {
            if key.starts_with("pane:") {
                self.sessions.remove(&key);
                self.active_by_pane.remove(&pane_lc);
                self.dirty = true;
            }
        }
    }

    /// Insert historical entries loaded from disk, skipping any whose key
    /// is already present (the live registry wins). Idempotent — safe to
    /// call multiple times. Used at startup.
    pub fn merge_historical(&mut self, loaded: Vec<AgentSession>) {
        for s in loaded {
            if self.sessions.contains_key(&s.key) {
                continue;
            }
            self.sessions.insert(s.key.clone(), s);
        }
        self.dirty = true;
    }

    /// Replace `title` for `key` only if the current title looks synthetic
    /// (empty, or equal to the cwd's leaf folder name). Used when fresh
    /// disk data — e.g. an updated `workspace.yaml summary:` field that
    /// the CLI wrote *after* this session was first registered live —
    /// becomes available later. A non-synthetic title (e.g. one already
    /// loaded from disk by `merge_historical`) is left untouched, so
    /// repeated refreshes are idempotent and never clobber a real summary.
    /// Returns `true` iff the title was actually changed.
    pub fn upgrade_title_if_synthetic(&mut self, key: &str, candidate: &str) -> bool {
        if candidate.is_empty() { return false; }
        let Some(entry) = self.sessions.get_mut(key) else { return false; };
        let cwd_leaf = entry.cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_synthetic = entry.title.is_empty() || entry.title == cwd_leaf;
        if !is_synthetic { return false; }
        if entry.title == candidate { return false; }
        entry.title = candidate.to_string();
        self.dirty = true;
        true
    }

    /// Read-only access to the cli_source for a key. Used by callers that
    /// need to dispatch on CLI without taking ownership of the entry.
    pub fn cli_source_for(&self, key: &str) -> Option<CliSource> {
        self.sessions.get(key).map(|s| s.cli_source.clone())
    }

    /// Returns true iff the session's current title is "synthetic" — empty
    /// or equal to the cwd's leaf folder. Used to short-circuit expensive
    /// disk lookups when the title is already a real one (e.g. loaded from
    /// `workspace.yaml summary:` at startup).
    pub fn title_is_synthetic(&self, key: &str) -> bool {
        let Some(entry) = self.sessions.get(key) else { return false; };
        let cwd_leaf = entry.cwd.file_name().and_then(|n| n.to_str()).unwrap_or("");
        entry.title.is_empty() || entry.title == cwd_leaf
    }

    /// Populate the registry with synthetic data covering all 6 statuses.
    /// Triggered by the `WTA_DEMO_AGENTS=1` env var on App startup so the
    /// Agents view (F2) can be exercised without running any real CLI.
    ///
    /// Layout (sorted by last_activity_at desc, newest first):
    ///   1. copilot  WORKING    — currently running a tool
    ///   2. claude   ATTENTION  — needs user approval
    ///   3. gemini   IDLE       — sitting waiting for input
    ///   4. copilot  ERROR      — connection failed
    ///   5. claude   ENDED      — exited normally a moment ago
    ///   6. gemini   HISTORICAL — loaded from an old log (no live pane)
    pub fn populate_demo_data(&mut self) {
        use std::time::Duration;

        let now = SystemTime::now();
        let cwd = PathBuf::from("C:/GitRepo/agentic-terminal");

        // 1. Working — copilot running a tool right now
        self.apply(SessionEvent::SessionStarted {
            key:             "demo-copilot-working".to_string(),
            cli_source:      CliSource::Copilot,
            pane_session_id: "11111111-1111-1111-1111-111111111111".to_string(),
            cwd:             cwd.clone(),
            title:           "copilot — refactor agent_sessions".to_string(),
        });
        self.apply(SessionEvent::ToolStarting {
            key:       "demo-copilot-working".to_string(),
            tool_name: "shell".to_string(),
        });

        // 2. Attention — claude waiting for tool approval
        self.apply(SessionEvent::SessionStarted {
            key:             "demo-claude-attention".to_string(),
            cli_source:      CliSource::Claude,
            pane_session_id: "22222222-2222-2222-2222-222222222222".to_string(),
            cwd:             cwd.clone(),
            title:           "claude — write tests for registry".to_string(),
        });
        self.apply(SessionEvent::Notification {
            key:     "demo-claude-attention".to_string(),
            message: "Allow tool: write_file ./src/lib.rs?".to_string(),
        });

        // 3. Idle — gemini waiting for next prompt
        self.apply(SessionEvent::SessionStarted {
            key:             "demo-gemini-idle".to_string(),
            cli_source:      CliSource::Gemini,
            pane_session_id: "33333333-3333-3333-3333-333333333333".to_string(),
            cwd:             cwd.clone(),
            title:           "gemini — explain build system".to_string(),
        });

        // 4. Error — copilot lost network
        self.apply(SessionEvent::SessionStarted {
            key:             "demo-copilot-error".to_string(),
            cli_source:      CliSource::Copilot,
            pane_session_id: "44444444-4444-4444-4444-444444444444".to_string(),
            cwd:             cwd.clone(),
            title:           "copilot — fix CI failure".to_string(),
        });
        self.apply(SessionEvent::ConnectionFailed {
            pane_session_id: "44444444-4444-4444-4444-444444444444".to_string(),
            reason:          "API request failed: 503 Service Unavailable".to_string(),
        });

        // 5. Ended — claude finished cleanly a moment ago
        self.apply(SessionEvent::SessionStarted {
            key:             "demo-claude-ended".to_string(),
            cli_source:      CliSource::Claude,
            pane_session_id: "55555555-5555-5555-5555-555555555555".to_string(),
            cwd:             cwd.clone(),
            title:           "claude — review PR diff".to_string(),
        });
        self.apply(SessionEvent::SessionStopped {
            key:    "demo-claude-ended".to_string(),
            reason: "end_turn".to_string(),
        });

        // 6. Historical — loaded from old log, no live pane
        let two_hours_ago = now - Duration::from_secs(2 * 60 * 60);
        let key = "demo-gemini-historical".to_string();
        self.sessions.insert(key.clone(), AgentSession {
            key:               key,
            cli_source:        CliSource::Gemini,
            pane_session_id:   None,
            window_id:         None,
            tab_id:            None,
            title:             "gemini — earlier debug session".to_string(),
            cwd:               cwd.clone(),
            started_at:        two_hours_ago - Duration::from_secs(60 * 30),
            last_activity_at:  two_hours_ago,
            status:            AgentStatus::Historical,
            last_error:        None,
            current_tool:      None,
            attention_reason:  None,
            log_path:          Some(PathBuf::from("~/.gemini/logs/2026-05-03-1530.log")),
        });

        // Stagger last_activity_at so the order in the UI matches the
        // narrative (working newest, historical oldest).
        let stagger = |secs: u64| now - Duration::from_secs(secs);
        if let Some(s) = self.sessions.get_mut("demo-copilot-working")  { s.last_activity_at = stagger(2); }
        if let Some(s) = self.sessions.get_mut("demo-claude-attention") { s.last_activity_at = stagger(15); }
        if let Some(s) = self.sessions.get_mut("demo-gemini-idle")      { s.last_activity_at = stagger(45); }
        if let Some(s) = self.sessions.get_mut("demo-copilot-error")    { s.last_activity_at = stagger(120); }
        if let Some(s) = self.sessions.get_mut("demo-claude-ended")     { s.last_activity_at = stagger(300); }

        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn k(s: &str) -> AgentKey { s.to_string() }
    fn pane(s: &str) -> String { s.to_string() }

    #[test]
    fn session_started_creates_idle_entry_bound_to_pane() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid-1"),
            cli_source: CliSource::Claude,
            pane_session_id: pane("00000000-0000-0000-0000-000000000001"),
            cwd: PathBuf::from("/work/proj"),
            title: "claude — proj".to_string(),
        });

        let s = reg.sessions.get("sid-1").expect("session created");
        assert_eq!(s.status, AgentStatus::Idle);
        assert_eq!(s.cli_source, CliSource::Claude);
        assert_eq!(s.pane_session_id.as_deref(), Some("00000000-0000-0000-0000-000000000001"));
        assert_eq!(reg.active_by_pane.get("00000000-0000-0000-0000-000000000001"), Some(&k("sid-1")));
        assert!(reg.take_dirty());
    }

    #[test]
    fn tool_starting_transitions_idle_to_working() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::ToolStarting { key: k("s"), tool_name: "bash".into() });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Working);
        assert_eq!(s.current_tool.as_deref(), Some("bash"));
    }

    #[test]
    fn tool_completed_returns_working_to_idle() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::ToolStarting   { key: k("s"), tool_name: "bash".into() });
        reg.apply(SessionEvent::ToolCompleted  { key: k("s") });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Idle);
        assert!(s.current_tool.is_none());
    }

    #[test]
    fn tool_completed_demotes_attention_to_idle_but_not_error() {
        // Attention is a transient "awaiting user" state set by either a
        // user-input tool (e.g. ask_user) or a permission_prompt notification
        // arriving mid-tool. The matching ToolCompleted/ToolFailed/Stop event
        // means the user has answered (approve, reject, or supplied input),
        // so Attention must clear back to Idle. Error, by contrast, is a real
        // failure state set by ConnectionFailed and must persist until a new
        // SessionStarted/ToolStarting event resets it.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });

        // Case 1: Attention from a permission_prompt-style notification.
        reg.apply(SessionEvent::ToolStarting {
            key: k("s"), tool_name: "shell".into(),
        });
        reg.apply(SessionEvent::Notification {
            key: k("s"), message: "approve: rm -rf foo".into(),
        });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Attention);
        // User rejects → tool.failed → ToolCompleted. Should clear Attention.
        reg.apply(SessionEvent::ToolCompleted { key: k("s") });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Idle);
        assert!(s.attention_reason.is_none(), "attention_reason should be cleared");
        assert!(s.current_tool.is_none());

        // Case 2: Error must NOT be demoted by ToolCompleted.
        reg.sessions.get_mut("s").unwrap().status = AgentStatus::Error;
        reg.sessions.get_mut("s").unwrap().last_error = Some("API failed".into());
        reg.apply(SessionEvent::ToolCompleted { key: k("s") });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Error);
    }

    #[test]
    fn notification_sets_attention_with_reason() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::Notification {
            key: k("s"),
            message: "approve: rm -rf foo".into(),
        });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Attention);
        assert_eq!(s.attention_reason.as_deref(), Some("approve: rm -rf foo"));
    }

    #[test]
    fn session_stopped_marks_ended_and_unbinds_pane() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::SessionStopped { key: k("s"), reason: "user_exit".into() });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Ended);
        assert!(s.pane_session_id.is_none());
        assert!(reg.active_by_pane.is_empty());
    }

    /// Regression for the round-24 case-mismatch bug.
    /// Hooks emit pane GUIDs in lowercase (from `WT_SESSION` env var) but
    /// WT-native vt_sequence/connection_state events emit uppercase
    /// (canonical Windows GUID). Before the fix, `is_agent_pane` did a
    /// case-sensitive lookup so the osc:133;A demotion never fired on the
    /// uppercase pane GUID, leaving Claude/Gemini rows stuck at IDLE
    /// after the agent CLI exited but the pane stayed alive.
    #[test]
    fn is_agent_pane_is_case_insensitive_for_pane_guid() {
        let mut reg = AgentSessionRegistry::new();
        // SessionStarted from a hook bridge → lowercase pane GUID.
        reg.apply(SessionEvent::SessionStarted {
            key: k("g"), cli_source: CliSource::Gemini,
            pane_session_id: "4df493b4-c122-4ae9-96f5-5775c21b8cd8".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        // is_agent_pane queried with uppercase (as WT-native events emit) must hit.
        assert!(
            reg.is_agent_pane("4DF493B4-C122-4AE9-96F5-5775C21B8CD8"),
            "is_agent_pane must match regardless of pane GUID case"
        );
        assert!(
            reg.is_agent_pane("4df493b4-c122-4ae9-96f5-5775c21b8cd8"),
            "is_agent_pane must still match the original lowercase form"
        );
    }

    /// Counterpart: PaneClosed via uppercase GUID must demote the row that
    /// was bound via lowercase. This is the actual end-to-end path that
    /// fires on osc:133;A (FinalTerm prompt-start emitted by the shell
    /// after the agent CLI exits but the pane stays alive).
    #[test]
    fn pane_closed_with_uppercase_pane_guid_demotes_lowercase_bound_session() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("c"), cli_source: CliSource::Claude,
            pane_session_id: "abcd1234-aaaa-bbbb-cccc-ddddeeeeffff".into(),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        // Now an osc:133;A arrives with uppercase GUID — apply PaneClosed.
        reg.apply(SessionEvent::PaneClosed {
            pane_session_id: "ABCD1234-AAAA-BBBB-CCCC-DDDDEEEEFFFF".into(),
        });
        let s = reg.sessions.get("c").unwrap();
        assert_eq!(s.status, AgentStatus::Ended,
            "uppercase PaneClosed must demote the lowercase-bound session");
        assert!(s.pane_session_id.is_none());
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn pane_closed_marks_active_session_ended() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("p") });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Ended);
        assert!(s.pane_session_id.is_none());
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn pane_closed_for_unknown_pane_is_noop() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("ghost") });
        assert!(reg.sessions.is_empty());
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn resume_dispatched_promotes_ended_to_idle() {
        // After Enter on a Historical/Ended row, dispatch_resume applies
        // ResumeDispatched so a rapid second Enter does not spawn another
        // pane while the first resume is in flight (Gemini's hooks can
        // take a long time to fire).
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Gemini,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("p") });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Ended);

        reg.apply(SessionEvent::ResumeDispatched { key: k("s") });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Idle);
        assert!(s.pane_session_id.is_none(),
            "pane_session_id should still be None — only the new SessionStarted hook should set it");
    }

    #[test]
    fn resume_dispatched_does_not_clobber_live_row() {
        // If a live row (Working/Idle) somehow receives ResumeDispatched
        // (e.g. SessionStarted hook arrived before our optimistic apply),
        // we must NOT downgrade it.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Gemini,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::ToolStarting {
            key: k("s"), tool_name: "shell".into(),
        });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Working);

        reg.apply(SessionEvent::ResumeDispatched { key: k("s") });
        // Working row stays Working — hook is the source of truth for live state.
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Working);
    }

    #[test]
    fn resume_dispatched_for_unknown_key_is_noop() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::ResumeDispatched { key: k("ghost") });
        assert!(reg.sessions.is_empty());
    }

    #[test]
    fn resume_pane_assigned_binds_pane_so_pane_closed_demotes_row() {
        // The Gemini-without-hooks scenario: user presses Enter on a
        // Historical Gemini row, dispatch_resume fires ResumeDispatched
        // (Historical -> Idle), and `wtcli split-pane`'s callback delivers
        // the new pane GUID via ResumePaneAssigned. When the user later
        // closes the resumed pane, PaneClosed must demote the row to Ended
        // (empty status), matching Copilot/Claude behavior.
        let mut reg = AgentSessionRegistry::new();
        reg.merge_historical(vec![AgentSession {
            key:               k("g"),
            cli_source:        CliSource::Gemini,
            pane_session_id:   None,
            window_id:         None,
            tab_id:            None,
            title:             "t".into(),
            cwd:               PathBuf::from("/x"),
            started_at:        SystemTime::UNIX_EPOCH,
            last_activity_at:  SystemTime::UNIX_EPOCH,
            status:            AgentStatus::Historical,
            last_error:        None,
            current_tool:      None,
            attention_reason:  None,
            log_path:          None,
        }]);
        reg.apply(SessionEvent::ResumeDispatched { key: k("g") });
        assert_eq!(reg.sessions.get(&k("g")).unwrap().status, AgentStatus::Idle,
            "ResumeDispatched promotes Historical -> Idle");

        // Split-pane callback fires: bind the new pane.
        reg.apply(SessionEvent::ResumePaneAssigned {
            key: k("g"),
            pane_session_id: pane("new-pane"),
        });
        let s = reg.sessions.get(&k("g")).unwrap();
        assert_eq!(s.pane_session_id.as_deref(), Some(pane("new-pane").as_str()));
        assert_eq!(s.status, AgentStatus::Idle, "binding does not change status");
        assert_eq!(reg.active_by_pane.get(&pane("new-pane")).map(String::as_str), Some(k("g").as_str()),
            "pane must be in active_by_pane so PaneClosed can find it");

        // Now simulate user closing the pane.
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("new-pane") });
        let s = reg.sessions.get(&k("g")).unwrap();
        assert_eq!(s.status, AgentStatus::Ended,
            "PaneClosed must demote a bound row to Ended (empty status)");
        assert!(s.pane_session_id.is_none());
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn resume_pane_assigned_is_idempotent_when_pane_matches() {
        // SessionStarted hook may beat the split-pane callback for
        // Claude/Copilot. If pane_session_id already matches, the
        // ResumePaneAssigned event must be a no-op (no logged "rebinding"
        // and no spurious dirty flag).
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("c"), cli_source: CliSource::Claude,
            pane_session_id: pane("p1"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        let _ = reg.take_dirty();

        reg.apply(SessionEvent::ResumePaneAssigned {
            key: k("c"),
            pane_session_id: pane("p1"),
        });
        let s = reg.sessions.get(&k("c")).unwrap();
        assert_eq!(s.pane_session_id.as_deref(), Some(pane("p1").as_str()));
        assert!(!reg.take_dirty(), "no-op should not mark registry dirty");
    }

    #[test]
    fn resume_pane_assigned_rebinds_when_pane_differs() {
        // If for some reason the row has a stale pane GUID (previous resume
        // never closed cleanly), accept the new one and clean up the map.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("c"), cli_source: CliSource::Gemini,
            pane_session_id: pane("old"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::ResumePaneAssigned {
            key: k("c"),
            pane_session_id: pane("new"),
        });
        let s = reg.sessions.get(&k("c")).unwrap();
        assert_eq!(s.pane_session_id.as_deref(), Some(pane("new").as_str()));
        assert!(reg.active_by_pane.get(&pane("old")).is_none(),
            "old pane mapping must be cleaned up");
        assert_eq!(reg.active_by_pane.get(&pane("new")).map(String::as_str), Some(k("c").as_str()));
    }

    #[test]
    fn resume_pane_assigned_for_unknown_key_is_noop() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::ResumePaneAssigned {
            key: k("ghost"),
            pane_session_id: pane("p"),
        });
        assert!(reg.sessions.is_empty());
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn connection_failed_sets_error_with_reason() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::ConnectionFailed {
            pane_session_id: pane("p"),
            reason: "ECONNRESET".into(),
        });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Error);
        assert_eq!(s.last_error.as_deref(), Some("ECONNRESET"));
        assert!(s.pane_session_id.is_some(), "pane stays bound until PaneClosed");
    }

    #[test]
    fn fallback_resolves_missing_id_to_pane_keyed_placeholder() {
        let reg = AgentSessionRegistry::new();
        let pane_id = "00000000-0000-0000-0000-0000000000aa";
        let key = reg.resolve_or_synthesize_key("", pane_id);
        assert_eq!(key, format!("pane:{}", pane_id));
    }

    #[test]
    fn fallback_returns_existing_active_key_when_pane_already_known() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: "real".into(), cli_source: CliSource::Claude,
            pane_session_id: "p".into(), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        let key = reg.resolve_or_synthesize_key("", "p");
        assert_eq!(key, "real");
    }

    #[test]
    fn fallback_uses_provided_id_when_present() {
        let reg = AgentSessionRegistry::new();
        let key = reg.resolve_or_synthesize_key("explicit", "anything");
        assert_eq!(key, "explicit");
    }

    // ─── Issue #2: SessionStarted rebinding pane leak ────────────────────────

    #[test]
    fn session_started_rebinding_to_new_pane_drops_old_pane_mapping() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("old"), cwd: PathBuf::from("/x"), title: "t".into(),
        });
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("new"), cwd: PathBuf::from("/x"), title: "t".into(),
        });
        assert_eq!(reg.active_by_pane.get("new"), Some(&k("s")));
        assert!(reg.active_by_pane.get("old").is_none(), "old pane mapping must be dropped");

        // Closing the OLD pane must NOT mark the session ended.
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("old") });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Idle);
    }

    #[test]
    fn populate_demo_data_creates_one_session_per_status() {
        let mut reg = AgentSessionRegistry::new();
        reg.populate_demo_data();
        let sessions = reg.iter_sorted();
        assert_eq!(sessions.len(), 6, "demo data should yield exactly 6 sessions");

        // Verify each status appears exactly once.
        let statuses: Vec<AgentStatus> = sessions.iter().map(|s| s.status.clone()).collect();
        for st in [
            AgentStatus::Working,
            AgentStatus::Attention,
            AgentStatus::Idle,
            AgentStatus::Error,
            AgentStatus::Ended,
            AgentStatus::Historical,
        ] {
            assert_eq!(statuses.iter().filter(|s| **s == st).count(), 1, "expected exactly one {:?}", st);
        }

        // Working session must come first (most recent activity).
        assert_eq!(sessions[0].status, AgentStatus::Working);
        // Historical session must be last and have no live pane binding.
        assert_eq!(sessions[5].status, AgentStatus::Historical);
        assert!(sessions[5].pane_session_id.is_none());

        // Error session must carry the failure reason.
        let err = sessions.iter().find(|s| s.status == AgentStatus::Error).unwrap();
        assert!(err.last_error.is_some());

        // Attention session must carry an attention reason.
        let att = sessions.iter().find(|s| s.status == AgentStatus::Attention).unwrap();
        assert!(att.attention_reason.is_some());
    }

    #[test]
    fn merge_historical_inserts_only_new_keys() {
        let mut reg = AgentSessionRegistry::new();
        // Pre-existing live session.
        reg.apply(SessionEvent::SessionStarted {
            key:             "live-1".into(),
            cli_source:      CliSource::Copilot,
            pane_session_id: "p".into(),
            cwd:             PathBuf::from("/x"),
            title:           "live".into(),
        });

        let now = SystemTime::now();
        let mk_hist = |key: &str| AgentSession {
            key:               key.to_string(),
            cli_source:        CliSource::Claude,
            pane_session_id:   None,
            window_id:         None, tab_id: None,
            title:             format!("hist {}", key),
            cwd:               PathBuf::from("/y"),
            started_at:        now,
            last_activity_at:  now,
            status:            AgentStatus::Historical,
            last_error:        None,
            current_tool:      None,
            attention_reason:  None,
            log_path:          None,
        };

        // Loaded set tries to overwrite live-1 + add hist-1.
        reg.merge_historical(vec![
            mk_hist("live-1"),
            mk_hist("hist-1"),
        ]);

        // live-1 must remain Working/Idle (Copilot, with pane), NOT Historical.
        let live = reg.sessions.get("live-1").unwrap();
        assert_eq!(live.cli_source, CliSource::Copilot);
        assert_ne!(live.status, AgentStatus::Historical);
        assert!(live.pane_session_id.is_some());

        // hist-1 must be added as Historical.
        let hist = reg.sessions.get("hist-1").unwrap();
        assert_eq!(hist.status, AgentStatus::Historical);
    }

    #[test]
    fn upgrade_title_replaces_synthetic_cwd_basename() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key:             "s1".into(),
            cli_source:      CliSource::Copilot,
            pane_session_id: "p1".into(),
            cwd:             PathBuf::from("C:\\Users\\yuazha"),
            // Synthetic title: cwd's leaf folder name.
            title:           "yuazha".into(),
        });
        let _ = reg.take_dirty();

        assert!(reg.upgrade_title_if_synthetic("s1", "Check Current Weather"));
        assert_eq!(reg.sessions.get("s1").unwrap().title, "Check Current Weather");
        assert!(reg.take_dirty(), "upgrade should mark registry dirty");
    }

    #[test]
    fn upgrade_title_replaces_empty_title() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key:             "s1".into(),
            cli_source:      CliSource::Copilot,
            pane_session_id: "p1".into(),
            cwd:             PathBuf::from("C:\\Users\\yuazha"),
            title:           String::new(),
        });
        assert!(reg.upgrade_title_if_synthetic("s1", "Real Summary"));
        assert_eq!(reg.sessions.get("s1").unwrap().title, "Real Summary");
    }

    #[test]
    fn upgrade_title_keeps_real_title_intact() {
        let mut reg = AgentSessionRegistry::new();
        // Pre-load a session with a real (non-synthetic) title from disk.
        reg.merge_historical(vec![AgentSession {
            key:               "s1".into(),
            cli_source:        CliSource::Copilot,
            pane_session_id:   None, window_id: None, tab_id: None,
            title:             "Workspace Summary From Disk".into(),
            cwd:               PathBuf::from("C:\\Users\\yuazha"),
            started_at:        SystemTime::now(),
            last_activity_at:  SystemTime::now(),
            status:            AgentStatus::Historical,
            last_error: None, current_tool: None, attention_reason: None,
            log_path: None,
        }]);
        let _ = reg.take_dirty();

        assert!(!reg.upgrade_title_if_synthetic("s1", "yuazha"));
        assert_eq!(reg.sessions.get("s1").unwrap().title, "Workspace Summary From Disk");
        assert!(!reg.take_dirty(), "no-op upgrade must not mark dirty");
    }

    #[test]
    fn upgrade_title_ignores_empty_candidate_and_unknown_key() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: "s1".into(), cli_source: CliSource::Copilot,
            pane_session_id: "p".into(), cwd: PathBuf::from("/x"),
            title: "x".into(),
        });
        assert!(!reg.upgrade_title_if_synthetic("s1", ""));
        assert!(!reg.upgrade_title_if_synthetic("missing", "title"));
    }

    #[test]
    fn is_user_input_tool_recognises_known_aliases() {
        // Verified Copilot CLI alias.
        assert!(is_user_input_tool("ask_user"));
        // Speculative aliases (case-insensitive, hyphen/underscore variants).
        assert!(is_user_input_tool("Ask_User"));
        assert!(is_user_input_tool("ask-user"));
        assert!(is_user_input_tool("AskQuestion"));
        assert!(is_user_input_tool("request_input"));
        assert!(is_user_input_tool("user_input"));
        // Regular tools must not match.
        assert!(!is_user_input_tool("shell.run"));
        assert!(!is_user_input_tool("read_file"));
        assert!(!is_user_input_tool("bash"));
        assert!(!is_user_input_tool(""));
    }

    #[test]
    fn tool_completed_demotes_attention_when_current_tool_was_user_input() {
        // Models the Copilot ask_user flow: BeforeTool fires with
        // tool_name="ask_user" → registry sees it as Attention. When the
        // user answers, AfterTool fires → ToolCompleted should demote
        // Attention back to Idle (so the row stops nagging).
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        // Route would do these two: ToolStarting (records current_tool)
        // then Notification (escalates status to Attention).
        reg.apply(SessionEvent::ToolStarting {
            key: k("s"), tool_name: "ask_user".into(),
        });
        reg.apply(SessionEvent::Notification {
            key: k("s"), message: "Which folder?".into(),
        });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Attention);
        assert_eq!(reg.sessions.get("s").unwrap().current_tool.as_deref(), Some("ask_user"));

        // User answers → AfterTool → ToolCompleted.
        reg.apply(SessionEvent::ToolCompleted { key: k("s") });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Idle);
        assert!(s.current_tool.is_none());
        assert!(s.attention_reason.is_none());
    }
}

// tools/wta/src/agent_sessions.rs
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

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

pub type AgentKey = String;

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

    /// Map an `agent_registry` agent id (`"copilot"`, `"claude"`, `"gemini"`,
    /// ...) to the matching `CliSource` variant. Returns `None` for agents
    /// the session registry does not track (e.g. `"codex"`, `"unknown"`, or
    /// an empty string), which the session-management view treats as
    /// "no filter — show all rows".
    pub fn from_agent_id(agent_id: &str) -> Option<Self> {
        match agent_id.to_ascii_lowercase().as_str() {
            "copilot" => Some(Self::Copilot),
            "claude"  => Some(Self::Claude),
            "gemini"  => Some(Self::Gemini),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AgentStatus {
    Idle,
    Working,
    Attention,
    Error,
    Ended,
    Historical,
}

/// 2D session-state model — **activity** dimension.
///
/// Captured separately from [`LivenessState`] so the session-management
/// view (F2) can answer two orthogonal questions independently:
///
///   * "Is this row still alive?" → [`LivenessState`]
///   * "If alive, what's it doing?" → [`ActivityState`]
///
/// The legacy [`AgentStatus`] enum mashes both into one dimension. For
/// backwards compatibility the storage in [`AgentSession::status`] is
/// unchanged — these enums are derived via [`AgentSession::activity`]
/// and [`AgentSession::liveness`]. New consumers should prefer the
/// derived view so they don't have to think about which `AgentStatus`
/// variants imply liveness vs. activity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityState {
    /// Sitting waiting for input.
    Idle,
    /// Running an autonomous tool.
    Working,
    /// Awaiting a clarifying answer from the user (ask_user etc.).
    Attention,
    /// Connection-level failure surfaced via ConnectionFailed.
    Error,
}

/// 2D session-state model — **liveness** dimension. See [`ActivityState`]
/// docs for the rationale.
///
/// Class A (agent-pane managed by WTA) liveness is composite:
/// `Live` iff *both* (a) the helper's alive mirror contains the
/// session's pane GUID and (b) no local PaneClosed event has fired
/// (the local event acts as a tombstone so a slow `session_removed`
/// push from master doesn't leave the row stuck `Live` for the
/// race window between WT closing the pane and the helper noticing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LivenessState {
    /// Pane is alive and the session is reachable.
    Live,
    /// Session ended in a known way (pane closed, agent stopped, etc.).
    Ended,
    /// Reconstructed from on-disk history; no live pane.
    Historical,
}

/// Where this session was first created from, used purely as UX metadata
/// (e.g. a small badge on Historical rows so the user can tell which
/// sessions were started by Intelligent Terminal's agent pane).
///
/// Populated authoritatively by `agent_pane_origin`: WTA appends a record
/// to the on-disk index whenever it creates an ACP session for an agent
/// pane (i.e. `--owner-tab-id` was supplied), and `history_loader` joins
/// that index when reconstructing historical rows. Live rows default to
/// `Unknown` because the UI only surfaces this badge for ended/historical
/// sessions, where it is most useful.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionOrigin {
    /// Origin not recorded — either the session pre-dates the index, was
    /// started outside of WTA (user ran `copilot` by hand), or the index
    /// file was unavailable when we tried to look it up.
    #[default]
    Unknown,
    /// Created by WTA on behalf of an Intelligent Terminal agent pane.
    AgentPane,
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
    /// Provenance for this session — populated for historical rows from
    /// the agent-pane origin index. See [`SessionOrigin`].
    pub origin:            SessionOrigin,
}

impl AgentSession {
    /// Derive the [`ActivityState`] dimension from the legacy
    /// one-dimensional `status` field.
    ///
    /// For non-Live rows (`Ended`/`Historical`) this returns
    /// [`ActivityState::Idle`] — the caller should consult
    /// [`Self::liveness`] first and only read `activity` when
    /// liveness is `Live`.
    pub fn activity(&self) -> ActivityState {
        match self.status {
            AgentStatus::Working   => ActivityState::Working,
            AgentStatus::Attention => ActivityState::Attention,
            AgentStatus::Error     => ActivityState::Error,
            AgentStatus::Idle
            | AgentStatus::Ended
            | AgentStatus::Historical => ActivityState::Idle,
        }
    }

    /// Derive the [`LivenessState`] dimension from the legacy
    /// one-dimensional `status` field.
    pub fn liveness(&self) -> LivenessState {
        match self.status {
            AgentStatus::Idle
            | AgentStatus::Working
            | AgentStatus::Attention
            | AgentStatus::Error      => LivenessState::Live,
            AgentStatus::Ended        => LivenessState::Ended,
            AgentStatus::Historical   => LivenessState::Historical,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Union of pane GUIDs ever seen in any alive-mirror snapshot
    /// (lowercased). Used by [`Self::apply_alive_pane_snapshot`] to
    /// scope its "end disappeared rows" sweep to Class A sessions
    /// (rows whose pane was at some point reported by master) and
    /// avoid touching Class B rows (standalone CLI panes the helper
    /// never managed). Once a pane is added it is never removed —
    /// the snapshot diff cares about "in this snapshot vs. some
    /// earlier snapshot", not about the union.
    known_alive_panes: HashSet<String>,
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
                // Orphan handover: if some other key was bound to this pane,
                // that previous session has been replaced (e.g. the agent CLI
                // ended one session and immediately started another in the
                // same pane). Demote it to Ended so the registry doesn't
                // carry two rows both claiming ownership of the same pane.
                // Defensive: only clear the previous entry's binding if it
                // still points at this pane — guards against rare cases
                // where `active_by_pane` is out of sync with the entry.
                //
                // Skip the handover entirely when `pane_session_id` is
                // empty. An empty pane GUID is not a real pane — it
                // means the event source didn't know which pane the
                // session belonged to (e.g. hook bridge that lost the
                // `pane_id` field, or a future event source we haven't
                // taught about pane attribution). Treating empty as a
                // real pane causes every session-start with no pane to
                // collide on the empty-string key in `active_by_pane`,
                // demoting the previous session to Ended whenever a
                // new one arrives — the exact shape of the
                // "second session arrives, first loses its status"
                // bug after the merge that renamed `session_id` to
                // `pane_id` for connection_state/vt_sequence but
                // missed wtcli's BuildSendEventJson.
                let pane_known = !pane_session_id.is_empty();
                if pane_known {
                    if let Some(prev_key) = self.active_by_pane.get(&pane_session_id).cloned() {
                        if prev_key != key {
                            if let Some(prev) = self.sessions.get_mut(&prev_key) {
                                if prev.pane_session_id.as_deref() == Some(pane_session_id.as_str()) {
                                    prev.status            = AgentStatus::Ended;
                                    prev.pane_session_id   = None;
                                    prev.current_tool      = None;
                                    prev.attention_reason  = None;
                                    prev.last_activity_at  = now;
                                    tracing::info!(
                                        target: "agent_session_registry",
                                        prev_key = %prev_key,
                                        new_key = %key,
                                        pane = %pane_session_id,
                                        "SessionStarted demoting previous owner of reused pane",
                                    );
                                    // active_by_pane[pane] is overwritten below.
                                }
                            }
                        }
                    }
                }

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
                    origin:            SessionOrigin::default(),
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
                // Only record a pane binding for non-empty pane GUIDs.
                // Symmetric with the skip above — an empty pane id
                // would never be valid for `active_by_pane` lookups
                // (PaneClosed, focus-pane, ConnectionFailed all key by
                // real GUID), so leave the entry's `pane_session_id`
                // as `None` and skip the map insert.
                if pane_known {
                    entry.pane_session_id  = Some(pane_session_id.clone());
                } else {
                    entry.pane_session_id  = None;
                }
                entry.status           = AgentStatus::Idle;
                entry.last_error       = None;
                entry.attention_reason = None;
                entry.current_tool     = None;
                entry.last_activity_at = now;
                if pane_known {
                    self.active_by_pane.insert(pane_session_id, key);
                }
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

            SessionEvent::SessionStopped { key, reason } => {
                // `reason` distinguishes two very different end-of-session
                // events from the same CLI hook:
                //
                //   * `complete`   — the agent CLI finished the current
                //                    conversation but the CLI process is
                //                    still running (e.g. Copilot's "new
                //                    chat" / `/new`). The pane stays alive
                //                    and the user can continue using it,
                //                    so for an agent-pane row we keep it
                //                    Idle with its pane binding intact.
                //
                //   * `user_exit`  — the user typed `/exit` (or equivalent)
                //                    and the agent CLI is exiting. The
                //                    pane will close imminently. Going to
                //                    Ended right now avoids leaving the
                //                    row stuck at Idle in the (observed)
                //                    case where WT never emits a
                //                    `connection_state:closed` for the
                //                    pane — which would otherwise cause
                //                    Enter on the row to try to focus a
                //                    dead pane GUID and fail with
                //                    FocusPane 0x80004005.
                //
                //   * anything else — treat conservatively as a real
                //                     end-of-session (Ended). The
                //                     "keep Idle" branch is opt-in via
                //                     an explicit whitelist below so
                //                     unknown reasons don't accidentally
                //                     leave rows stuck Idle.
                //
                // Non-agent-pane sessions (origin defaulting to Unknown)
                // always take the original Ended path regardless of
                // reason, matching the legacy hook-bridge behavior.
                let reason_keeps_session_alive = matches!(
                    reason.as_str(),
                    "complete"
                );
                let pane_still_live = self
                    .sessions
                    .get(&key)
                    .and_then(|s| s.pane_session_id.as_deref())
                    .map(|p| self.active_by_pane.get(p) == Some(&key))
                    .unwrap_or(false);
                let is_agent_pane_session = self
                    .sessions
                    .get(&key)
                    .map(|s| s.origin == SessionOrigin::AgentPane)
                    .unwrap_or(false);
                let keep_idle = is_agent_pane_session
                    && pane_still_live
                    && reason_keeps_session_alive;
                if let Some(entry) = self.sessions.get_mut(&key) {
                    if keep_idle {
                        entry.status = AgentStatus::Idle;
                    } else {
                        entry.status = AgentStatus::Ended;
                        if let Some(pane) = entry.pane_session_id.take() {
                            self.active_by_pane.remove(&pane);
                        }
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
                // Same orphan-handover as SessionStarted: if another session
                // currently holds this pane, demote it first. In practice
                // ResumePaneAssigned binds a freshly-created pane, so this
                // is defensive, but it preserves the invariant that
                // `active_by_pane[p]` and `sessions[k].pane_session_id`
                // agree for every k that thinks it owns p.
                if let Some(prev_key) = self.active_by_pane.get(&pane_session_id).cloned() {
                    if prev_key != key {
                        if let Some(prev) = self.sessions.get_mut(&prev_key) {
                            if prev.pane_session_id.as_deref() == Some(pane_session_id.as_str()) {
                                prev.status            = AgentStatus::Ended;
                                prev.pane_session_id   = None;
                                prev.current_tool      = None;
                                prev.attention_reason  = None;
                                prev.last_activity_at  = now;
                            }
                        }
                    }
                }
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

    /// Like [`iter_sorted`], but keeps only rows whose `cli_source` matches
    /// the supplied filter. Passing `None` disables filtering and returns
    /// the same list as `iter_sorted`. Used by the session-management view
    /// so that, when the agent pane is running a known CLI (copilot /
    /// claude / gemini), the list only shows sessions for that CLI.
    pub fn iter_sorted_filtered(&self, filter: Option<&CliSource>) -> Vec<&AgentSession> {
        let sorted = self.iter_sorted();
        match filter {
            None => sorted,
            Some(want) => sorted
                .into_iter()
                .filter(|s| &s.cli_source == want)
                .collect(),
        }
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

    /// Read-only borrow of the [`AgentSession`] for `key`, or `None` if
    /// the key isn't tracked. Used by post-apply hooks in the routing
    /// layer that need to inspect the session's `cli_source` / `status`
    /// to decide whether to prune (e.g. dropping a "phantom" row whose
    /// on-disk artefact has no resumable content).
    pub fn get(&self, key: &AgentKey) -> Option<&AgentSession> {
        self.sessions.get(key)
    }

    /// Update the `origin` field on an existing session entry. No-op if
    /// `key` is not in the registry. Used by the routing layer to stamp
    /// `AgentPane` on live rows once the agent-pane origin index has
    /// confirmed the session was started by WTA on behalf of an agent
    /// pane. Kept as a focused setter (rather than a parameter on
    /// `SessionEvent::SessionStarted`) so we don't have to thread the
    /// flag through every test fixture and demo path that constructs
    /// SessionStarted events.
    pub fn set_origin(&mut self, key: &str, origin: SessionOrigin) {
        if let Some(entry) = self.sessions.get_mut(key) {
            if entry.origin != origin {
                entry.origin = origin;
                self.dirty = true;
            }
        }
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

    /// Look up the [`AgentKey`] currently bound to `pane_session_id`, if
    /// any. Returns `None` for panes that aren't tracked or that have
    /// already had their binding cleared (e.g. after `PaneClosed`).
    /// Callers that want to act on a key *just before* `PaneClosed`
    /// unbinds it must take this lookup before applying the event.
    pub fn key_for_pane(&self, pane_session_id: &str) -> Option<AgentKey> {
        self.active_by_pane
            .get(&pane_session_id.to_ascii_lowercase())
            .cloned()
    }

    /// Look up the [`SessionOrigin`] of whatever session is currently
    /// bound to `pane_session_id`, if any. Returns `None` for panes
    /// that aren't tracked.
    ///
    /// Used by the helper's OSC 133;A handler to distinguish between
    /// "agent running inside a shell pane (origin Unknown) — shell
    /// prompt-start really does mean the agent exited" and "agent
    /// pane (origin AgentPane) — there's no shell underneath, so any
    /// OSC 133;A is spurious (likely a focus/window-switch artifact
    /// emitted by WT itself) and must NOT trigger PaneClosed".
    pub fn origin_for_pane(&self, pane_session_id: &str) -> Option<SessionOrigin> {
        let key = self
            .active_by_pane
            .get(&pane_session_id.to_ascii_lowercase())?;
        self.sessions.get(key).map(|s| s.origin.clone())
    }

    pub fn remove(&mut self, key: &AgentKey) {
        if let Some(s) = self.sessions.remove(key) {
            if let Some(pane) = s.pane_session_id {
                self.active_by_pane.remove(&pane);
            }
            self.dirty = true;
        }
    }

    /// Reconcile this registry against the helper's alive-mirror snapshot
    /// of Class A (wta-managed agent-pane) sessions.
    ///
    /// `alive_panes` is the set of WT pane GUIDs (lowercase, no braces)
    /// that the master currently knows about — i.e. every pane that has
    /// an active helper holding an open ACP session. Any row whose
    /// `pane_session_id` was previously in some alive snapshot but is
    /// *not* in this one is transitioned to [`AgentStatus::Ended`].
    /// This is the second half of Class A's composite-liveness source:
    /// the local `PaneClosed` event handles the case where WT closes a
    /// pane before master notices, and this method handles the
    /// reverse — master tells us the helper exited (e.g. agent CLI
    /// crashed) before the pane has finished tearing down on our side.
    ///
    /// The sweep is intentionally scoped to panes the helper has *ever*
    /// reported as alive (tracked in `known_alive_panes`). Class B rows
    /// (standalone `copilot` panes the user started by hand) never
    /// appear in any alive snapshot, so they remain untouched by this
    /// method and continue to rely on `PaneClosed` for their `Ended`
    /// transition.
    ///
    /// Idempotent: re-applying the same snapshot is a no-op. Calling
    /// with an empty `alive_panes` set after a previous non-empty
    /// snapshot will end every Class A row.
    pub fn apply_alive_pane_snapshot(&mut self, alive_panes: HashSet<String>) {
        let now = SystemTime::now();
        // Normalise to lowercase to match the rest of the registry's
        // pane-GUID handling (see `apply()`'s normaliser).
        let alive_lc: HashSet<String> =
            alive_panes.into_iter().map(|p| p.to_ascii_lowercase()).collect();

        // Compute panes we used to know about that are now gone.
        let removed: Vec<String> = self
            .known_alive_panes
            .iter()
            .filter(|p| !alive_lc.contains(*p))
            .cloned()
            .collect();

        for pane in &removed {
            // Mirror PaneClosed's reducer: find the bound key, transition
            // to Ended, clear pane-side bookkeeping. Skip rows that have
            // already been ended (e.g. by a prior PaneClosed event) to
            // keep the second half of the composite source idempotent.
            if let Some(key) = self.active_by_pane.remove(pane) {
                if let Some(entry) = self.sessions.get_mut(&key) {
                    if entry.liveness() == LivenessState::Live {
                        entry.status            = AgentStatus::Ended;
                        entry.pane_session_id   = None;
                        entry.current_tool      = None;
                        entry.attention_reason  = None;
                        entry.last_activity_at  = now;
                        self.dirty = true;
                        tracing::info!(
                            target: "agent_session_registry",
                            key = %key,
                            pane = %pane,
                            "alive snapshot removed pane; row → Ended",
                        );
                    }
                }
            }
            // Stop tracking the pane once it's gone — if it comes back
            // (e.g. resume creates a new pane with a new GUID) we'll
            // pick it up again on the next snapshot.
            self.known_alive_panes.remove(pane);
        }

        // Union in any newly-seen panes from this snapshot. We track
        // the union (not just the current snapshot) so that a pane
        // missing from snapshot N+1 still triggers a removal even if
        // we never saw it in snapshot N+1's predecessor — but in
        // practice apply_alive_pane_snapshot is called for each
        // ExtNotification batch, so the union grows monotonically
        // and only ever shrinks via the `removed` loop above.
        for pane in &alive_lc {
            self.known_alive_panes.insert(pane.clone());
        }
    }

    /// Demote the row owned by `session_id` to `Ended` if it is currently
    /// alive. The incremental counterpart of [`apply_alive_pane_snapshot`]:
    /// where the snapshot path computes "panes that disappeared from the
    /// alive set", this path acts on a single explicit `session_removed`
    /// broadcast from master (i.e. the helper that owned `session_id` just
    /// disconnected or the agent CLI exited).
    ///
    /// Mirrors [`SessionEvent::PaneClosed`]'s reducer: clears the pane
    /// binding, transitions to [`AgentStatus::Ended`], and removes the
    /// pane from `active_by_pane`. The pane is **also** removed from
    /// `known_alive_panes` so a subsequent `apply_alive_pane_snapshot`
    /// won't try to re-end it.
    ///
    /// No-op when the row is `Historical` (it was loaded from disk; no
    /// pane to demote), `Ended` (already tombstoned by a local
    /// `PaneClosed` event), or absent (we never had a row for this sid).
    /// Idempotent.
    pub fn apply_master_session_ended(&mut self, session_id: &str) {
        let now = SystemTime::now();
        let Some(entry) = self.sessions.get_mut(session_id) else {
            return;
        };
        if entry.liveness() != LivenessState::Live {
            return;
        }
        let pane_to_clear = entry.pane_session_id.take();
        entry.status            = AgentStatus::Ended;
        entry.current_tool      = None;
        entry.attention_reason  = None;
        entry.last_activity_at  = now;
        self.dirty = true;
        if let Some(pane) = pane_to_clear {
            self.active_by_pane.remove(&pane);
            self.known_alive_panes.remove(&pane);
        }
        tracing::info!(
            target: "agent_session_registry",
            key = %session_id,
            "master session_removed broadcast demoted row → Ended",
        );
    }

    /// Join the helper's alive-session mirror into this registry — the
    /// "upgrade Historical to Live" half of Class A's composite source.
    ///
    /// Each `(session_id, pane_session_id)` tuple represents one entry
    /// from the master's [`SessionInfo`](crate::session_registry::SessionInfo)
    /// snapshot. For each tuple, if there's a row whose [`AgentKey`]
    /// equals `session_id` and whose [`liveness`](AgentSession::liveness)
    /// is `Historical` or `Ended`, upgrade it to `Live` (`AgentStatus::Idle`)
    /// and bind the pane.
    ///
    /// Motivation: at startup the on-disk history scan
    /// (`history_loader::load_all`) and the helper's `list_sessions`
    /// bootstrap can land in either order, and a WTA process attached
    /// to an existing master in another WT window may never see the
    /// originating `SessionStarted` hook event. Without this join, a
    /// session that is still alive in some pane would be shown as
    /// Historical in F2 and Enter would mis-route to "resume new"
    /// instead of "focus existing".
    ///
    /// Idempotent — re-applying with the same snapshot is a no-op
    /// because the second call sees `liveness == Live` and skips.
    /// Live rows (Idle/Working/Attention/Error) are never demoted by
    /// this method — `apply_alive_pane_snapshot` is the canonical
    /// disappearance path.
    ///
    /// The join is intentionally string-keyed (no `SessionId` import)
    /// to keep this module decoupled from `session_registry`. For
    /// `Class A` (agent-pane-managed) sessions, ACP `session_id`
    /// equals `AgentKey` for the CLIs that reuse their own session id
    /// (Claude). For CLIs whose ACP id diverges from the CLI's own id
    /// (Copilot may differ), the join simply misses — the row stays
    /// Historical, which is the same behaviour as today and degrades
    /// gracefully (Enter will start a new session).
    ///
    /// Tombstone safety: `Ended` rows reflect a local `PaneClosed`
    /// observation in this WTA process; we treat that as authoritative
    /// and refuse to resurrect them, because the alternative (a stale
    /// `session_added` broadcast arriving before master detects the
    /// helper disconnect) would silently resurrect a pane that's
    /// already gone, leaving the F2 row Live forever with no demotion
    /// path. Cross-WTA-process resume-after-disconnect is rare;
    /// preferring the safe direction.
    ///
    /// Live-without-pane rebind: `Live` rows that already have a pane
    /// bound are no-ops (`apply_alive_pane_snapshot` is the canonical
    /// disappearance path). `Live` rows with `pane_session_id == None`,
    /// however, are upgraded just enough to bind the pane carried in
    /// the snapshot — without touching `status` or any tool/attention
    /// state. This handles the cross-window resume race: in the tab
    /// that issued the F2 Enter on a Historical row,
    /// `dispatch_resume_in_agent_pane` fires `ResumeDispatched`, which
    /// optimistically flips the row to `Idle (Live)` so a rapid double
    /// press can't dispatch twice — but leaves `pane_session_id =
    /// None` because the resume runs in a freshly spawned tab whose
    /// helper hasn't issued a hook yet. When master finally broadcasts
    /// `session_added` with the new helper-pane's GUID, the gating
    /// helper (the one that pressed Enter) is no longer `Historical`,
    /// so without this rebind it would drop the broadcast on the floor
    /// and leave the row permanently `Live` without a pane — every
    /// subsequent F2 Enter on the same row would return `NotResumable
    /// { LiveWithoutPane }`.
    pub fn apply_alive_session_join<'a>(
        &mut self,
        alive: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    ) {
        let now = SystemTime::now();
        for (sid, pane_opt) in alive {
            let Some(entry) = self.sessions.get_mut(sid) else { continue };
            match entry.liveness() {
                LivenessState::Ended => {
                    // Tombstone — see method docstring.
                    continue;
                }
                LivenessState::Live => {
                    // Already live; only fill in a missing pane binding.
                    // A non-None binding is the local source of truth
                    // (set by a SessionStarted hook or ResumePaneAssigned).
                    if entry.pane_session_id.is_some() {
                        continue;
                    }
                    let Some(pane) = pane_opt else { continue };
                    let pane_lc = pane.to_ascii_lowercase();
                    entry.pane_session_id = Some(pane_lc.clone());
                    self.active_by_pane.insert(pane_lc.clone(), sid.to_string());
                    self.known_alive_panes.insert(pane_lc);
                    entry.last_activity_at = now;
                    self.dirty = true;
                    tracing::info!(
                        target: "agent_session_registry",
                        key = %sid,
                        pane = ?pane_opt,
                        "alive snapshot bound pane to Live-without-pane row",
                    );
                }
                LivenessState::Historical => {
                    entry.status            = AgentStatus::Idle;
                    entry.last_activity_at  = now;
                    entry.current_tool      = None;
                    entry.attention_reason  = None;
                    entry.last_error        = None;
                    if let Some(pane) = pane_opt {
                        let pane_lc = pane.to_ascii_lowercase();
                        // Drop any previous binding pointing elsewhere.
                        if let Some(old_pane) = entry.pane_session_id.take() {
                            if old_pane != pane_lc {
                                self.active_by_pane.remove(&old_pane);
                            }
                        }
                        entry.pane_session_id = Some(pane_lc.clone());
                        self.active_by_pane.insert(pane_lc.clone(), sid.to_string());
                        self.known_alive_panes.insert(pane_lc);
                    }
                    self.dirty = true;
                    tracing::info!(
                        target: "agent_session_registry",
                        key = %sid,
                        pane = ?pane_opt,
                        "alive snapshot upgraded Historical row → Live",
                    );
                }
            }
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
        // 5. Ended — claude finished cleanly a moment ago. Origin is the
        // default (Unknown), so SessionStopped takes the original
        // immediate-Ended path — no PaneClosed needed.
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
            origin:            SessionOrigin::default(),
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
    fn session_stopped_while_pane_alive_keeps_idle_and_binding() {
        // Under the agent-pane-friendly semantics, an `agent.session.end`
        // with reason="complete" (Copilot's signal for "user opened a
        // new chat in the same pane") leaves the pane alive. The row
        // must stay Idle with its pane binding intact so pressing Enter
        // focuses the still-live pane (instead of triggering a "resume
        // in new tab" path that would spawn a duplicate pane).
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.set_origin("s", SessionOrigin::AgentPane);
        reg.apply(SessionEvent::SessionStopped { key: k("s"), reason: "complete".into() });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Idle,
            "agent-pane session.end (reason=complete) on still-live pane must keep Idle");
        assert_eq!(s.pane_session_id.as_deref(), Some(pane("p").as_str()),
            "pane binding must be preserved so Enter focuses the live pane");
        assert!(reg.active_by_pane.contains_key(&pane("p")),
            "active_by_pane must still map the pane to this key");
    }

    #[test]
    fn session_stopped_with_user_exit_reason_goes_to_ended_even_for_agent_pane() {
        // When the user typed `/exit`, the CLI process is exiting and
        // the pane will close — possibly without `connection_state:closed`
        // ever being broadcast. Going to Ended immediately here avoids
        // a "stuck Idle" row whose pane binding points at a dead pane
        // GUID (`focus-pane` would later fail with 0x80004005).
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.set_origin("s", SessionOrigin::AgentPane);
        reg.apply(SessionEvent::SessionStopped { key: k("s"), reason: "user_exit".into() });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Ended,
            "agent-pane session.end (reason=user_exit) must go to Ended, not stay Idle");
        assert!(s.pane_session_id.is_none(),
            "pane binding must be released so Enter doesn't try to focus a dead pane GUID");
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn session_stopped_with_unknown_reason_goes_to_ended_for_agent_pane() {
        // Defensive default: anything we don't recognise as a known
        // "session-only end" reason demotes the row to Ended. Keeps
        // future agent-CLI behaviour from accidentally producing stuck
        // Idle rows when they emit a new `reason` value we haven't
        // whitelisted.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.set_origin("s", SessionOrigin::AgentPane);
        reg.apply(SessionEvent::SessionStopped { key: k("s"), reason: "some_new_reason".into() });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Ended);
    }

    #[test]
    fn session_stopped_for_non_agent_pane_session_goes_to_ended_immediately() {
        // Sessions that did not originate from an agent pane (e.g. the
        // user typed `copilot` themselves in a shell) keep the original
        // hook-bridge semantics: `agent.session.end` flips them straight
        // to Ended and releases the binding regardless of the reason.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        // origin stays Unknown (default). Even with the "complete"
        // reason that would otherwise keep an agent-pane row Idle, a
        // non-agent-pane row must still go straight to Ended.
        reg.apply(SessionEvent::SessionStopped { key: k("s"), reason: "complete".into() });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Ended);
        assert!(s.pane_session_id.is_none());
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn session_stopped_after_pane_closed_still_goes_to_ended() {
        // Order: PaneClosed first (binding cleared) → SessionStopped
        // arriving late finds no live pane and must fall through to
        // Ended without touching `active_by_pane`. Defensive against
        // hook events landing after the WT-native close. The keep-Idle
        // branch requires both agent-pane origin AND a live binding AND
        // a session-only reason; missing any one falls through to Ended.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.set_origin("s", SessionOrigin::AgentPane);
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("p") });
        reg.apply(SessionEvent::SessionStopped { key: k("s"), reason: "complete".into() });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Ended);
        assert!(s.pane_session_id.is_none());
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn session_stopped_then_pane_closed_demotes_to_ended() {
        // Forward order on an agent-pane session: SessionStopped with
        // a session-only reason keeps Idle (pane still alive), then
        // PaneClosed transitions to Ended.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.set_origin("s", SessionOrigin::AgentPane);
        reg.apply(SessionEvent::SessionStopped { key: k("s"), reason: "complete".into() });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Idle);
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("p") });
        let s = reg.sessions.get("s").unwrap();
        assert_eq!(s.status, AgentStatus::Ended);
        assert!(s.pane_session_id.is_none());
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn session_started_on_pane_held_by_another_session_demotes_previous() {
        // Copilot scenario: agent.session.end (reason=complete) +
        // agent.session.started fire back-to-back for the same pane
        // (user opened a new chat in the same agent pane). The old
        // session row stays Idle through the SessionStopped (because
        // it's agent-pane + reason="complete" + pane is live), and then
        // the new SessionStarted on the same pane must demote it to
        // Ended and take over the binding.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("old"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "old".into(),
        });
        reg.set_origin("old", SessionOrigin::AgentPane);
        reg.apply(SessionEvent::SessionStopped { key: k("old"), reason: "complete".into() });
        assert_eq!(reg.sessions.get("old").unwrap().status, AgentStatus::Idle,
            "agent-pane old row must stay Idle pending pane reuse");
        reg.apply(SessionEvent::SessionStarted {
            key: k("new"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "new".into(),
        });

        let old = reg.sessions.get("old").unwrap();
        assert_eq!(old.status, AgentStatus::Ended,
            "previous owner of a reused pane must be demoted to Ended");
        assert!(old.pane_session_id.is_none(),
            "previous owner must release its pane binding");

        let new = reg.sessions.get("new").unwrap();
        assert_eq!(new.status, AgentStatus::Idle);
        assert_eq!(new.pane_session_id.as_deref(), Some(pane("p").as_str()));
        assert_eq!(reg.active_by_pane.get(&pane("p")), Some(&k("new")));
    }

    #[test]
    fn session_started_with_empty_pane_does_not_demote_previous_empty_pane_session() {
        // Reproduces the user-reported bug introduced by the merge that
        // renamed `params["session_id"] → params["pane_id"]` in
        // `TerminalPage.cpp` but missed `wtcli/BuildSendEventJson` (which
        // still emits `session_id`). WTA's main.rs reads `params["pane_id"]`
        // and finds nothing for hook-bridge `agent_event` envelopes, so
        // every routed `SessionStarted` arrives with `pane_session_id = ""`.
        //
        // Pre-fix, the registry would index the empty-string key in
        // `active_by_pane`, so a second `SessionStarted` (different key,
        // also empty pane) triggered orphan handover and demoted the
        // first session to Ended. User-visible symptom: row 1's status
        // badge silently vanishes the moment row 2's first hook arrives.
        //
        // Post-fix, the registry must:
        //   1. Skip the orphan-handover demotion when `pane_session_id`
        //      is empty (no real pane to collide on).
        //   2. NOT insert an empty key into `active_by_pane`.
        //   3. Leave the entry's `pane_session_id` field as `None`
        //      (no fake binding).
        //
        // wtcli is also fixed in the same PR to emit `pane_id`, and
        // WTA's main.rs falls back to `session_id` for backward
        // compatibility — but this test guards the registry-level
        // invariant independent of those upstream sources.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("session-a"), cli_source: CliSource::Copilot,
            pane_session_id: String::new(),  // empty (the bug shape)
            cwd: PathBuf::from("/x"),
            title: "Implement Day Query Feature".into(),
        });
        reg.apply(SessionEvent::SessionStarted {
            key: k("session-b"), cli_source: CliSource::Copilot,
            pane_session_id: String::new(),  // empty again
            cwd: PathBuf::from("/x"),
            title: "ask me a question".into(),
        });

        let a = reg.sessions.get("session-a").expect("session-a row exists");
        assert_eq!(
            a.status, AgentStatus::Idle,
            "first session must NOT be demoted when a second empty-pane \
             SessionStarted arrives; got status {:?}",
            a.status
        );
        assert!(
            a.pane_session_id.is_none(),
            "no real pane was bound, so pane_session_id stays None",
        );

        let b = reg.sessions.get("session-b").expect("session-b row exists");
        assert_eq!(b.status, AgentStatus::Idle);
        assert!(b.pane_session_id.is_none());

        // The empty string must never have been inserted into the
        // pane→key map (it would collide for every empty-pane event).
        assert!(
            !reg.active_by_pane.contains_key(""),
            "empty pane GUID must not be indexed in active_by_pane",
        );
    }

    #[test]
    fn session_stopped_marks_ended_and_unbinds_pane_when_pane_already_dead() {
        // Same shape as the legacy `session_stopped_marks_ended_and_unbinds_pane`
        // test, but using the new "pane already closed" path. Note that
        // the legacy test's premise (SessionStopped alone implies Ended)
        // still holds for non-agent-pane sessions or for any reason
        // other than `complete`; this case exercises the agent-pane
        // path after PaneClosed already cleared the binding.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Copilot,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.set_origin("s", SessionOrigin::AgentPane);
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("p") });
        reg.apply(SessionEvent::SessionStopped { key: k("s"), reason: "complete".into() });
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
            origin:            SessionOrigin::default(),
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
        // Preexisting live session.
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
            origin:            SessionOrigin::default(),
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

    #[test]
    fn from_agent_id_maps_known_cli_ids() {
        assert_eq!(CliSource::from_agent_id("copilot"), Some(CliSource::Copilot));
        assert_eq!(CliSource::from_agent_id("claude"),  Some(CliSource::Claude));
        assert_eq!(CliSource::from_agent_id("gemini"),  Some(CliSource::Gemini));
        // Case-insensitive — `current_agent_id` is conventionally lowercase
        // but mixed-case must not silently drop the filter.
        assert_eq!(CliSource::from_agent_id("Copilot"), Some(CliSource::Copilot));
    }

    #[test]
    fn from_agent_id_returns_none_for_untracked_or_empty() {
        // Empty / unknown / codex are all "no filter" — the F2 view will
        // fall back to showing every row.
        assert_eq!(CliSource::from_agent_id(""),         None);
        assert_eq!(CliSource::from_agent_id("codex"),    None);
        assert_eq!(CliSource::from_agent_id("unknown"),  None);
        assert_eq!(CliSource::from_agent_id("bogus"),    None);
    }

    #[test]
    fn iter_sorted_filtered_keeps_only_matching_cli_source() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("cop"),
            cli_source: CliSource::Copilot,
            pane_session_id: pane("p-cop"),
            cwd: PathBuf::from("/x"),
            title: "copilot run".into(),
        });
        reg.apply(SessionEvent::SessionStarted {
            key: k("cla"),
            cli_source: CliSource::Claude,
            pane_session_id: pane("p-cla"),
            cwd: PathBuf::from("/x"),
            title: "claude run".into(),
        });
        reg.apply(SessionEvent::SessionStarted {
            key: k("gem"),
            cli_source: CliSource::Gemini,
            pane_session_id: pane("p-gem"),
            cwd: PathBuf::from("/x"),
            title: "gemini run".into(),
        });

        // No filter → all three rows in last_activity_at-desc order.
        let all: Vec<&str> = reg
            .iter_sorted_filtered(None)
            .iter()
            .map(|s| s.key.as_str())
            .collect();
        assert_eq!(all.len(), 3);

        // Copilot filter → only the copilot row.
        let cop: Vec<&str> = reg
            .iter_sorted_filtered(Some(&CliSource::Copilot))
            .iter()
            .map(|s| s.key.as_str())
            .collect();
        assert_eq!(cop, vec!["cop"]);

        // Claude filter → only the claude row.
        let cla: Vec<&str> = reg
            .iter_sorted_filtered(Some(&CliSource::Claude))
            .iter()
            .map(|s| s.key.as_str())
            .collect();
        assert_eq!(cla, vec!["cla"]);
    }

    #[test]
    fn iter_sorted_filtered_excludes_unknown_cli_source() {
        // A row with Unknown(_) cli_source — e.g. a malformed hook payload
        // — must not appear under any concrete filter. With no filter it
        // does appear, matching iter_sorted's behaviour.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("cop"),
            cli_source: CliSource::Copilot,
            pane_session_id: pane("p-cop"),
            cwd: PathBuf::from("/x"),
            title: "copilot".into(),
        });
        reg.apply(SessionEvent::SessionStarted {
            key: k("mystery"),
            cli_source: CliSource::Unknown("foo".into()),
            pane_session_id: pane("p-mystery"),
            cwd: PathBuf::from("/x"),
            title: "mystery".into(),
        });

        let cop: Vec<&str> = reg
            .iter_sorted_filtered(Some(&CliSource::Copilot))
            .iter()
            .map(|s| s.key.as_str())
            .collect();
        assert_eq!(cop, vec!["cop"]);

        let all_len = reg.iter_sorted_filtered(None).len();
        assert_eq!(all_len, 2);
    }

    // -------- B-8: liveness/activity 2D + alive-pane snapshot --------

    #[test]
    fn activity_and_liveness_derive_from_legacy_status() {
        // Sanity-check that the 2D derived view matches the existing
        // one-dimensional AgentStatus for every variant. Doubles as
        // documentation for the mapping.
        let make = |status: AgentStatus| AgentSession {
            key: "k".into(),
            cli_source: CliSource::Claude,
            pane_session_id: None,
            window_id: None,
            tab_id: None,
            title: "t".into(),
            cwd: PathBuf::from("/x"),
            started_at: SystemTime::UNIX_EPOCH,
            last_activity_at: SystemTime::UNIX_EPOCH,
            status,
            last_error: None,
            current_tool: None,
            attention_reason: None,
            log_path: None,
            origin: SessionOrigin::Unknown,
        };

        let cases = [
            (AgentStatus::Idle,       ActivityState::Idle,      LivenessState::Live),
            (AgentStatus::Working,    ActivityState::Working,   LivenessState::Live),
            (AgentStatus::Attention,  ActivityState::Attention, LivenessState::Live),
            (AgentStatus::Error,      ActivityState::Error,     LivenessState::Live),
            (AgentStatus::Ended,      ActivityState::Idle,      LivenessState::Ended),
            (AgentStatus::Historical, ActivityState::Idle,      LivenessState::Historical),
        ];
        for (st, want_act, want_live) in cases {
            let s = make(st.clone());
            assert_eq!(s.activity(), want_act, "activity mismatch for {:?}", st);
            assert_eq!(s.liveness(), want_live, "liveness mismatch for {:?}", st);
        }
    }

    #[test]
    fn apply_alive_pane_snapshot_ends_disappeared_session() {
        // Class A row: pane appeared in snapshot, then disappeared from
        // the next snapshot without a PaneClosed event ever firing. The
        // registry must transition the row to Ended on its own — this
        // is the "agent CLI crashed and the helper exited before WT
        // noticed the pane was dead" race.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid-1"),
            cli_source: CliSource::Claude,
            pane_session_id: pane("pane-aaa"),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });

        // First snapshot says the pane is alive → row stays Live.
        reg.apply_alive_pane_snapshot(HashSet::from(["pane-aaa".into()]));
        let s = reg.sessions.get("sid-1").unwrap();
        assert_eq!(s.liveness(), LivenessState::Live);
        assert_eq!(s.status, AgentStatus::Idle);
        let _ = reg.take_dirty();

        // Second snapshot omits the pane → row → Ended.
        reg.apply_alive_pane_snapshot(HashSet::new());
        let s = reg.sessions.get("sid-1").unwrap();
        assert_eq!(s.liveness(), LivenessState::Ended);
        assert_eq!(s.status, AgentStatus::Ended);
        assert!(s.pane_session_id.is_none(), "pane binding cleared");
        assert!(reg.active_by_pane.get("pane-aaa").is_none(),
                "active_by_pane unbound after row → Ended");
        assert!(reg.take_dirty(), "row transition must flag the registry dirty");
    }

    #[test]
    fn apply_alive_pane_snapshot_is_noop_for_never_seen_panes() {
        // Class B row: the user ran `copilot` in a plain pane that the
        // helper never opened an ACP session for, so the pane GUID
        // never enters any alive snapshot. The row must NOT be
        // demoted to Ended just because it's missing from a snapshot —
        // its lifecycle is still owned by PaneClosed/hooks.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("standalone"),
            cli_source: CliSource::Copilot,
            pane_session_id: pane("pane-bbb"),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });

        // Several snapshots arrive, none mentioning our pane — Class A
        // for some *other* pane, irrelevant to us.
        reg.apply_alive_pane_snapshot(HashSet::from(["pane-other".into()]));
        reg.apply_alive_pane_snapshot(HashSet::from(["pane-other".into()]));
        reg.apply_alive_pane_snapshot(HashSet::new());

        let s = reg.sessions.get("standalone").unwrap();
        assert_eq!(s.liveness(), LivenessState::Live,
                   "standalone (Class B) row must not be ended by snapshots that never \
                    contained its pane");
        assert_eq!(s.status, AgentStatus::Idle);
        assert_eq!(s.pane_session_id.as_deref(), Some("pane-bbb"));
    }

    #[test]
    fn apply_alive_pane_snapshot_is_idempotent_after_pane_closed() {
        // Composite source: if a local PaneClosed event fired first,
        // the row is already Ended. A subsequent alive snapshot that
        // also omits the pane must NOT bump last_activity_at or
        // produce a second tracing entry — the second branch of the
        // composite is supposed to be a no-op once PaneClosed wins.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid"),
            cli_source: CliSource::Claude,
            pane_session_id: pane("pane-x"),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply_alive_pane_snapshot(HashSet::from(["pane-x".into()]));
        let _ = reg.take_dirty();

        // PaneClosed wins the race.
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("pane-x") });
        assert_eq!(reg.sessions.get("sid").unwrap().status, AgentStatus::Ended);
        let before = reg.sessions.get("sid").unwrap().last_activity_at;
        let _ = reg.take_dirty();
        // Ensure the next branch can detect "did anything change" via timestamp.
        std::thread::sleep(std::time::Duration::from_millis(2));

        // Snapshot omits the pane too — should be a no-op now.
        reg.apply_alive_pane_snapshot(HashSet::new());
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.status, AgentStatus::Ended);
        assert_eq!(s.last_activity_at, before,
                   "second branch of composite source must not retouch \
                    a row already ended by PaneClosed");
        assert!(!reg.take_dirty(), "no second dirty bump");
    }

    #[test]
    fn apply_alive_pane_snapshot_is_idempotent_when_replayed() {
        // Calling the same snapshot twice must not flag the registry
        // dirty the second time — the F2 view re-applies snapshots
        // every time master pushes a session_added/removed batch.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        let _ = reg.take_dirty();

        reg.apply_alive_pane_snapshot(HashSet::from(["p".into()]));
        assert!(!reg.take_dirty(),
                "first snapshot that just confirms an existing live row must not be dirty");
        reg.apply_alive_pane_snapshot(HashSet::from(["p".into()]));
        assert!(!reg.take_dirty(), "replayed snapshot is a no-op");
    }

    #[test]
    fn apply_alive_pane_snapshot_normalises_pane_guid_case() {
        // Snapshots arrive from master with whatever case master stored —
        // helpers report lowercase from WT_SESSION but in-memory ACP
        // sessions may have come from WT-native events (uppercase) on
        // the master side. The reducer normalises GUIDs to lowercase,
        // so the snapshot input must too.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid"),
            cli_source: CliSource::Claude,
            pane_session_id: pane("aaa-BBB-CCC"),
            cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        // Confirm the reducer lowercased the binding.
        assert!(reg.active_by_pane.contains_key("aaa-bbb-ccc"));

        // Snapshot reports the same pane in mixed case — should still
        // count as "alive".
        reg.apply_alive_pane_snapshot(HashSet::from(["AAA-bbb-CCC".into()]));
        assert_eq!(reg.sessions.get("sid").unwrap().liveness(), LivenessState::Live);

        // Drop it — also in mixed case absence form (empty snapshot).
        reg.apply_alive_pane_snapshot(HashSet::new());
        assert_eq!(reg.sessions.get("sid").unwrap().liveness(), LivenessState::Ended);
    }

    // -------- B-9: history × alive-mirror join --------

    fn make_historical(key: &str) -> AgentSession {
        AgentSession {
            key: key.into(),
            cli_source: CliSource::Claude,
            pane_session_id: None,
            window_id: None,
            tab_id: None,
            title: "t".into(),
            cwd: PathBuf::from("/x"),
            started_at: SystemTime::UNIX_EPOCH,
            last_activity_at: SystemTime::UNIX_EPOCH,
            status: AgentStatus::Historical,
            last_error: None,
            current_tool: None,
            attention_reason: None,
            log_path: None,
            origin: SessionOrigin::AgentPane,
        }
    }

    #[test]
    fn apply_alive_session_join_upgrades_historical_to_live_and_binds_pane() {
        // The scenario the join is meant to fix: history scan loaded a
        // row as Historical (it pre-dates this WTA process), but the
        // master's alive snapshot says the session is still running in
        // some pane. The join must upgrade the row to Live (Idle) and
        // bind the pane so a subsequent F2 Enter routes to "focus".
        let mut reg = AgentSessionRegistry::new();
        reg.merge_historical(vec![make_historical("sid-hist")]);
        assert_eq!(reg.sessions.get("sid-hist").unwrap().liveness(),
                   LivenessState::Historical);

        reg.apply_alive_session_join([("sid-hist", Some("pane-XYZ"))]);

        let s = reg.sessions.get("sid-hist").unwrap();
        assert_eq!(s.liveness(), LivenessState::Live);
        assert_eq!(s.status, AgentStatus::Idle);
        assert_eq!(s.pane_session_id.as_deref(), Some("pane-xyz"),
                   "pane GUID is bound and lowercased");
        assert_eq!(reg.active_by_pane.get("pane-xyz").map(|k| k.as_str()),
                   Some("sid-hist"));
        assert!(reg.take_dirty());
    }

    #[test]
    fn apply_alive_session_join_is_noop_for_already_live_rows() {
        // If the alive snapshot replays a session that we already know
        // about via SessionStarted, the join must NOT clobber tool /
        // attention state by demoting back to Idle. Live wins.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid"), cli_source: CliSource::Claude,
            pane_session_id: pane("pane-orig"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::ToolStarting { key: k("sid"), tool_name: "bash".into() });
        let _ = reg.take_dirty();

        reg.apply_alive_session_join([("sid", Some("pane-different"))]);
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.status, AgentStatus::Working, "tool state preserved");
        assert_eq!(s.pane_session_id.as_deref(), Some("pane-orig"),
                   "pane binding not overwritten");
        assert!(!reg.take_dirty(), "no-op must not flag dirty");
    }

    #[test]
    fn apply_alive_session_join_does_not_resurrect_locally_ended_rows() {
        // Local PaneClosed tombstones win over (potentially stale)
        // alive broadcasts — see apply_alive_session_join's tombstone
        // safety comment. Rationale: if a stale `session_added` from
        // master arrives after PaneClosed has already ended the row,
        // resurrecting it would leave it Live with no demotion path
        // (the pane is genuinely gone in this process).
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid"), cli_source: CliSource::Claude,
            pane_session_id: pane("pane-old"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("pane-old") });
        assert_eq!(reg.sessions.get("sid").unwrap().status, AgentStatus::Ended);

        reg.apply_alive_session_join([("sid", Some("pane-new"))]);
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.liveness(), LivenessState::Ended, "Ended must stay Ended");
        // Pane bindings should not be re-established for a tombstoned row.
        assert!(reg.active_by_pane.get("pane-new").is_none());
        assert!(reg.active_by_pane.get("pane-old").is_none());
    }

    #[test]
    fn apply_alive_session_join_binds_pane_to_live_without_pane_row() {
        // Regression for the cross-window F2-Enter focus bug:
        // `dispatch_resume_in_agent_pane` fires `ResumeDispatched`,
        // which optimistically promotes a Historical row to `Idle (Live)`
        // *without* binding a pane (the resume runs in a freshly spawned
        // sibling tab; the gating helper never sees a SessionStarted
        // hook). When master finally broadcasts `session_added` with
        // the new helper-pane's GUID, the gating helper's row is no
        // longer Historical — but it must still adopt the pane binding,
        // otherwise the row stays Live-without-pane forever and every
        // subsequent F2 Enter on the same row returns
        // `NotResumable { LiveWithoutPane }` ("Cannot focus session …:
        // it appears live but no pane GUID is bound yet").
        let mut reg = AgentSessionRegistry::new();
        reg.merge_historical(vec![make_historical("sid")]);
        // Simulate the optimistic flip done by `dispatch_resume_in_agent_pane`.
        reg.apply(SessionEvent::ResumeDispatched { key: k("sid") });
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.liveness(), LivenessState::Live);
        assert!(s.pane_session_id.is_none(),
            "ResumeDispatched leaves pane_session_id None on purpose");
        let _ = reg.take_dirty();

        // Master's broadcast lands with the new helper-pane's GUID.
        reg.apply_alive_session_join([("sid", Some("pane-new"))]);
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.status, AgentStatus::Idle, "status preserved");
        assert_eq!(s.pane_session_id.as_deref(), Some("pane-new"),
            "broadcast binds the new pane so cross-window Focus can resolve");
        assert_eq!(
            reg.active_by_pane.get("pane-new").map(String::as_str),
            Some(k("sid").as_str()),
            "active_by_pane mirrors the binding",
        );
        assert!(reg.known_alive_panes.contains("pane-new"));
        assert!(reg.take_dirty(), "bind flagged dirty for snapshot");
    }

    #[test]
    fn apply_alive_session_join_does_not_overwrite_existing_pane_on_live_row() {
        // The Live-without-pane rebind must NOT overwrite a Live row
        // that already has a pane bound (e.g. by a local SessionStarted
        // hook). Local hooks are the source of truth for live state.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid"), cli_source: CliSource::Claude,
            pane_session_id: pane("pane-local"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        let _ = reg.take_dirty();

        reg.apply_alive_session_join([("sid", Some("pane-other"))]);
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.pane_session_id.as_deref(), Some("pane-local"),
            "existing local pane binding wins over broadcast");
        assert!(!reg.take_dirty(), "no-op must not flag dirty");
        assert!(reg.active_by_pane.get("pane-other").is_none(),
            "broadcast's pane must not leak into active_by_pane");
    }

    #[test]
    fn apply_alive_session_join_ignores_unknown_sids() {
        // SessionInfo for a sid we don't have in the registry → no-op.
        // We don't fabricate rows from alive snapshots; SessionStarted
        // is still the canonical source for new rows.
        let mut reg = AgentSessionRegistry::new();
        reg.apply_alive_session_join([("never-seen", Some("pane-x"))]);
        assert!(reg.sessions.is_empty());
        assert!(reg.active_by_pane.is_empty());
        assert!(!reg.take_dirty());
    }

    #[test]
    fn apply_alive_session_join_without_pane_only_upgrades_status() {
        // Some SessionInfo entries have pane_session_id == None (e.g.
        // legacy sessions replayed before _meta.wta carried a pane id).
        // The join must still upgrade Historical → Live; no pane is
        // bound, and active_by_pane is untouched.
        let mut reg = AgentSessionRegistry::new();
        reg.merge_historical(vec![make_historical("sid")]);

        reg.apply_alive_session_join([("sid", None)]);
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.liveness(), LivenessState::Live);
        assert_eq!(s.status, AgentStatus::Idle);
        assert!(s.pane_session_id.is_none(), "no pane binding without a pane id");
        assert!(reg.active_by_pane.is_empty());
    }

    #[test]
    fn apply_alive_session_join_then_pane_snapshot_round_trip() {
        // Bookend test: history loads Historical → join upgrades to Live
        // with pane bound → later pane-snapshot drops it → row Ended.
        // Verifies B-8's apply_alive_pane_snapshot interoperates with
        // the join (the bound pane lands in known_alive_panes).
        let mut reg = AgentSessionRegistry::new();
        reg.merge_historical(vec![make_historical("sid")]);
        reg.apply_alive_session_join([("sid", Some("pane-1"))]);
        assert_eq!(reg.sessions.get("sid").unwrap().liveness(), LivenessState::Live);

        // Master initially confirms the pane is alive.
        reg.apply_alive_pane_snapshot(HashSet::from(["pane-1".into()]));
        assert_eq!(reg.sessions.get("sid").unwrap().liveness(), LivenessState::Live);

        // Then it disappears from a later snapshot.
        reg.apply_alive_pane_snapshot(HashSet::new());
        assert_eq!(reg.sessions.get("sid").unwrap().liveness(), LivenessState::Ended);
    }

    #[test]
    fn apply_master_session_ended_demotes_live_row_to_ended() {
        // Mirrors PaneClosed: Live row → Ended with pane binding cleared.
        // Driven by master's `intellterm.wta/session_removed` broadcast
        // when a helper exits — without this path the agent_sessions
        // reducer never sees the disappearance and the F2 row stays
        // stuck on Live.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        // Seed known_alive_panes so we can assert it's also pruned.
        reg.apply_alive_pane_snapshot(HashSet::from(["p".into()]));
        assert_eq!(reg.sessions.get("sid").unwrap().liveness(), LivenessState::Live);
        assert!(reg.known_alive_panes.contains("p"));

        reg.apply_master_session_ended("sid");
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.liveness(), LivenessState::Ended);
        assert!(s.pane_session_id.is_none());
        assert!(reg.active_by_pane.get("p").is_none(),
            "active_by_pane must be cleared so future pane events don't hit a stale binding");
        assert!(!reg.known_alive_panes.contains("p"),
            "known_alive_panes must be pruned so a subsequent pane snapshot doesn't try to re-end");
        assert!(reg.take_dirty());
    }

    #[test]
    fn apply_master_session_ended_is_noop_for_historical_row() {
        // Historical rows have never been Live in this process; the
        // master removed broadcast carries no useful information for
        // them (they never had a pane binding to clear). Must be a
        // pure no-op so we don't surprise the disk loader.
        let mut reg = AgentSessionRegistry::new();
        reg.merge_historical(vec![make_historical("sid")]);
        reg.take_dirty();

        reg.apply_master_session_ended("sid");
        let s = reg.sessions.get("sid").unwrap();
        assert_eq!(s.liveness(), LivenessState::Historical);
        assert!(!reg.take_dirty(), "no-op must not dirty the registry");
    }

    #[test]
    fn apply_master_session_ended_is_noop_for_already_ended_row() {
        // Two paths can end a row: local PaneClosed and master
        // session_removed. Whichever wins the race, the other must
        // be a no-op so we don't double-fire `dirty` or accidentally
        // re-clear a binding the user has since re-established via
        // resume.
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("sid"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        reg.apply(SessionEvent::PaneClosed { pane_session_id: pane("p") });
        assert_eq!(reg.sessions.get("sid").unwrap().liveness(), LivenessState::Ended);
        reg.take_dirty();

        reg.apply_master_session_ended("sid");
        assert!(!reg.take_dirty());
    }

    #[test]
    fn apply_master_session_ended_is_noop_for_unknown_sid() {
        // Master may broadcast `session_removed` for sessions we never
        // saw (e.g. created in another WT window's WTA process). Must
        // not fabricate a tombstone row.
        let mut reg = AgentSessionRegistry::new();
        reg.apply_master_session_ended("never-seen");
        assert!(reg.sessions.is_empty());
        assert!(!reg.take_dirty());
    }
}

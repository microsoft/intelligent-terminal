// wta/src/agent_sessions.rs
//
// Runtime registry for tracking live and historical CLI agent sessions.
// Independent from `agent_registry.rs`, which is the static catalog of
// CLI profiles.

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
                    }
                }
                entry.cli_source       = cli_source;
                entry.title            = title;
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
                    if entry.status == AgentStatus::Working {
                        entry.status        = AgentStatus::Idle;
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
        if let Some(existing) = self.active_by_pane.get(pane_session_id) {
            return existing.clone();
        }
        format!("pane:{}", pane_session_id)
    }

    pub fn has_session(&self, key: &AgentKey) -> bool {
        self.sessions.contains_key(key)
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
        if let Some(key) = self.active_by_pane.get(pane_session_id).cloned() {
            if key.starts_with("pane:") {
                self.sessions.remove(&key);
                self.active_by_pane.remove(pane_session_id);
                self.dirty = true;
            }
        }
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
    fn tool_completed_does_not_demote_attention_or_error() {
        let mut reg = AgentSessionRegistry::new();
        reg.apply(SessionEvent::SessionStarted {
            key: k("s"), cli_source: CliSource::Claude,
            pane_session_id: pane("p"), cwd: PathBuf::from("/x"),
            title: "t".into(),
        });
        // simulate Notification arriving before tool completes:
        reg.sessions.get_mut("s").unwrap().status = AgentStatus::Attention;
        reg.apply(SessionEvent::ToolCompleted { key: k("s") });
        assert_eq!(reg.sessions.get("s").unwrap().status, AgentStatus::Attention);
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
}

//! Typed agent-failure taxonomy.
//!
//! Every failure of the `tab → helper → master → agent CLI` stack collapses
//! into a single [`AgentFailure`] value, classified at the helper boundary —
//! the one place that can see the typed ACP error, the transport signal, and
//! the startup-handshake outcome. The App handler then drives recovery off the
//! *type*, never off substring-matching the message text.
//!
//! See `doc/specs/agent-failure-handling.md` for the design and the per-class
//! recovery policy.

use agent_client_protocol as acp;
use std::fmt;

/// Which step of the startup handshake failed. Surfaced to the user and the
/// log so a connect failure is self-explanatory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandshakeStage {
    PipeConnect,
    Initialize,
    NewSession,
    LoadSession,
}

impl HandshakeStage {
    /// Stable label for structured logging.
    pub fn label(self) -> &'static str {
        match self {
            HandshakeStage::PipeConnect => "pipe_connect",
            HandshakeStage::Initialize => "initialize",
            HandshakeStage::NewSession => "new_session",
            HandshakeStage::LoadSession => "load_session",
        }
    }
}

/// A typed classification of an agent-stack failure.
///
/// The discriminant — not the message text — decides the recovery path. The
/// `message` / `detail` payloads are carried only for display and logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentFailure {
    /// The agent needs sign-in (`ErrorCode::AuthRequired` / -32000, or the
    /// transitional non-compliant-auth shim). Routes to the sign-in screen.
    AuthRequired { message: String },

    /// The helper↔master pipe ended (master died, or an agent-CLI death
    /// cascaded into a master shutdown). A *signal*, never an ACP error.
    /// Recovery today is manual `/restart`; Phase 3 adds auto-reconnect.
    TransportLost,

    /// The connection never *established*: pipe-connect / `initialize` /
    /// `session/new` / `session/load` failed or timed out at startup.
    HandshakeFailed { stage: HandshakeStage, detail: String },

    /// A referenced resource is gone (`ErrorCode::ResourceNotFound` / -32002):
    /// e.g. `session/load` of an expired session. The session survives; the
    /// user can start fresh.
    ResourceGone { message: String },

    /// An agent-returned protocol error that does *not* kill the session
    /// (`InvalidParams` / `InvalidRequest` / `MethodNotFound` / `ParseError` /
    /// `InternalError` / any other code). The turn ends; the session stays.
    Protocol { code: i32, message: String },

    /// User-initiated cancel surfaced as an error (`RequestCancelled` /
    /// -32800). Not a failure — the turn just ends, nothing is shown.
    Cancelled,
}

impl AgentFailure {
    /// Classify a typed ACP JSON-RPC error by its `code`.
    ///
    /// Matching is numeric so it is independent of which `unstable_*` Cargo
    /// features gate the corresponding `ErrorCode` variant (e.g.
    /// `RequestCancelled` is behind `unstable_cancel_request`, which we do not
    /// enable — without numeric matching it would not even be nameable here).
    pub fn from_acp_error(e: &acp::Error) -> Self {
        let code: i32 = e.code.into();
        match code {
            -32000 => AgentFailure::AuthRequired {
                message: e.message.clone(),
            },
            -32002 => AgentFailure::ResourceGone {
                message: e.message.clone(),
            },
            -32800 => AgentFailure::Cancelled,
            other => AgentFailure::Protocol {
                code: other,
                message: e.message.clone(),
            },
        }
    }

    /// True if this failure should route the user to the sign-in screen.
    pub fn is_auth(&self) -> bool {
        matches!(self, AgentFailure::AuthRequired { .. })
    }

    /// True if the user-initiated cancel — the handler shows nothing for it.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, AgentFailure::Cancelled)
    }

    /// Short, stable class label for structured logging (`target=failure`).
    pub fn class(&self) -> &'static str {
        match self {
            AgentFailure::AuthRequired { .. } => "auth_required",
            AgentFailure::TransportLost => "transport_lost",
            AgentFailure::HandshakeFailed { .. } => "handshake_failed",
            AgentFailure::ResourceGone { .. } => "resource_gone",
            AgentFailure::Protocol { .. } => "protocol",
            AgentFailure::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for AgentFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentFailure::AuthRequired { message } => {
                write!(f, "authentication required: {message}")
            }
            AgentFailure::TransportLost => f.write_str("connection to the agent was lost"),
            AgentFailure::HandshakeFailed { stage, detail } => {
                write!(f, "handshake failed at {}: {detail}", stage.label())
            }
            AgentFailure::ResourceGone { message } => write!(f, "resource not found: {message}"),
            AgentFailure::Protocol { code, message } => {
                write!(f, "protocol error [{code}]: {message}")
            }
            AgentFailure::Cancelled => f.write_str("request cancelled"),
        }
    }
}

// Implementing `Error` lets us attach a typed `AgentFailure` to an
// `anyhow::Error` (`anyhow::Error::new(failure)`) so the auth/resource class
// survives a `?`-bubbled handshake error that is otherwise collapsed to a
// string before it reaches `main.rs`. The receiver recovers it with
// `downcast_ref::<AgentFailure>()`.
impl std::error::Error for AgentFailure {}

/// Recover a typed [`AgentFailure`] from an `anyhow::Error` that bubbled out of
/// the startup handshake. Prefers a `downcast` to an `AgentFailure` attached at
/// the original `acp::Error` site (so an auth error survives the `?`-collapse
/// into `anyhow`); falls back to the transitional auth-string shim, then to a
/// generic [`AgentFailure::HandshakeFailed`] tagged with `stage`.
pub fn classify_anyhow(e: &anyhow::Error, stage: HandshakeStage) -> AgentFailure {
    if let Some(failure) = e.downcast_ref::<AgentFailure>() {
        return failure.clone();
    }
    let detail = format!("{e:#}");
    if message_looks_like_auth(&detail) {
        tracing::warn!(
            target: "failure",
            non_compliant_auth = true,
            "auth recovered via string fallback (no typed AuthRequired)"
        );
        return AgentFailure::AuthRequired { message: detail };
    }
    AgentFailure::HandshakeFailed { stage, detail }
}

/// Narrow, **transitional** substring check used *only* where a typed
/// `acp::Error` is unavailable — i.e. a handshake error that was already
/// collapsed into an `anyhow::Error` string before classification. The typed
/// path ([`AgentFailure::from_acp_error`]) is always preferred; this is the
/// fallback for non-compliant agents that mislabel auth, and is logged as
/// `non_compliant_auth=true` when it fires. Removable once all agents return
/// `ErrorCode::AuthRequired`.
pub fn message_looks_like_auth(msg: &str) -> bool {
    let l = msg.to_lowercase();
    l.contains("authentication required")
        || l.contains("not logged in")
        || l.contains("unauthorized")
        || l.contains("401")
        || l.contains("apikey is missing")
        || l.contains("api key")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol as acp;

    #[test]
    fn auth_code_classifies_as_auth_required() {
        let f = AgentFailure::from_acp_error(&acp::Error::auth_required());
        assert!(f.is_auth());
        assert_eq!(f.class(), "auth_required");
    }

    #[test]
    fn resource_not_found_classifies_as_resource_gone() {
        let e = acp::Error::new(-32002, "session gone");
        assert!(matches!(
            AgentFailure::from_acp_error(&e),
            AgentFailure::ResourceGone { .. }
        ));
    }

    #[test]
    fn invalid_params_is_protocol_not_auth() {
        let f = AgentFailure::from_acp_error(&acp::Error::invalid_params());
        assert!(!f.is_auth());
        assert!(matches!(f, AgentFailure::Protocol { code: -32602, .. }));
    }

    #[test]
    fn cancelled_code_classifies_as_cancelled() {
        let e = acp::Error::new(-32800, "cancelled");
        assert!(AgentFailure::from_acp_error(&e).is_cancelled());
    }

    #[test]
    fn string_fallback_matches_auth_phrases_only() {
        assert!(message_looks_like_auth(
            "new_session over master pipe failed: authentication required"
        ));
        assert!(message_looks_like_auth("HTTP 401 Unauthorized"));
        assert!(!message_looks_like_auth("new_session timed out after 30s"));
    }

    #[test]
    fn failure_survives_anyhow_downcast() {
        let err = anyhow::Error::new(AgentFailure::AuthRequired {
            message: "nope".into(),
        })
        .context("new_session over master pipe failed");
        let recovered = err.downcast_ref::<AgentFailure>().cloned();
        assert_eq!(
            recovered,
            Some(AgentFailure::AuthRequired {
                message: "nope".into()
            })
        );
    }
}

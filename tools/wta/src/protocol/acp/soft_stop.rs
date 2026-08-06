//! Soft-stop classification — the *outcome* axis, distinct from failure.
//!
//! An ACP `session/prompt` can succeed at the transport/RPC level (it returns
//! `Ok(PromptResponse)`, never an `acp::Error`) yet still end the turn for a
//! reason the user should be told about: the model hit its output limit, the
//! agent exhausted its self-directed request budget, or it refused the prompt.
//!
//! These are **not** [`crate::protocol::acp::failure::AgentFailure`] — the
//! connection is healthy and the session stays `Connected`. We model them on a
//! separate axis so the failure handler never has to special-case "this looks
//! like a failure but isn't one". A soft stop only appends an informational
//! line to the chat; it changes no connection state.
//!
//! `StopReason::EndTurn` (normal completion) and `StopReason::Cancelled`
//! (user-initiated cancel, already handled by the cancel path) are *not* soft
//! stops and classify to `None`.

use agent_client_protocol as acp;

/// A turn that completed successfully at the protocol level but ended for a
/// reason worth surfacing to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoftStopReason {
    /// The model reached its maximum output-token budget; the reply is
    /// truncated (`StopReason::MaxTokens`).
    MaxTokens,
    /// The agent hit the cap on self-directed requests it may make between
    /// user turns (`StopReason::MaxTurnRequests`).
    MaxTurnRequests,
    /// The agent declined to continue (`StopReason::Refusal`). The spec
    /// explicitly says this should be reflected in the UI.
    Refusal,
}

impl SoftStopReason {
    /// Classify a successful turn's [`acp::schema::v1::StopReason`]. Returns `None` for the
    /// outcomes that need no notice: `EndTurn` (normal) and `Cancelled`
    /// (already surfaced by the cancel path).
    pub fn from_stop_reason(reason: acp::schema::v1::StopReason) -> Option<Self> {
        match reason {
            acp::schema::v1::StopReason::MaxTokens => Some(SoftStopReason::MaxTokens),
            acp::schema::v1::StopReason::MaxTurnRequests => Some(SoftStopReason::MaxTurnRequests),
            acp::schema::v1::StopReason::Refusal => Some(SoftStopReason::Refusal),
            // EndTurn / Cancelled and any future variant: not a soft stop.
            _ => None,
        }
    }

    /// Short, stable class label for structured logging (`target=soft_stop`).
    pub fn class(self) -> &'static str {
        match self {
            SoftStopReason::MaxTokens => "max_tokens",
            SoftStopReason::MaxTurnRequests => "max_turn_requests",
            SoftStopReason::Refusal => "refusal",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol as acp;

    #[test]
    fn end_turn_is_not_a_soft_stop() {
        assert_eq!(SoftStopReason::from_stop_reason(acp::schema::v1::StopReason::EndTurn), None);
    }

    #[test]
    fn cancelled_is_not_a_soft_stop() {
        assert_eq!(
            SoftStopReason::from_stop_reason(acp::schema::v1::StopReason::Cancelled),
            None
        );
    }

    #[test]
    fn limit_and_refusal_reasons_classify() {
        assert_eq!(
            SoftStopReason::from_stop_reason(acp::schema::v1::StopReason::MaxTokens),
            Some(SoftStopReason::MaxTokens)
        );
        assert_eq!(
            SoftStopReason::from_stop_reason(acp::schema::v1::StopReason::MaxTurnRequests),
            Some(SoftStopReason::MaxTurnRequests)
        );
        assert_eq!(
            SoftStopReason::from_stop_reason(acp::schema::v1::StopReason::Refusal),
            Some(SoftStopReason::Refusal)
        );
    }

    #[test]
    fn class_labels_are_stable() {
        assert_eq!(SoftStopReason::MaxTokens.class(), "max_tokens");
        assert_eq!(SoftStopReason::MaxTurnRequests.class(), "max_turn_requests");
        assert_eq!(SoftStopReason::Refusal.class(), "refusal");
    }
}

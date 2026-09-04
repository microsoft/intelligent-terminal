//! Explicit per-tab turn lifecycle.
//!
//! Source of truth for "is the agent doing something for us right now, and
//! if so what?". Replaces the ~10 scattered boolean / Option fields that
//! previously encoded the same state implicitly. See
//! `doc/specs/turn-state-refactor.md`.
//!
//! This module is pure data + small pure helpers. All transitions and side
//! effects live on `App` methods in `app.rs`.

use crate::coordinator::RecommendationSet;
use crate::protocol::acp::client::PromptSubmission;
use crate::turn_context::TurnContext;

/// Per-tab turn state.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnState {
    /// No turn in flight. Input box accepts new prompts.
    Idle,
    /// Accepted while the selected agent is still connecting. This is visible
    /// in the transcript but has not crossed the ACP boundary.
    Queued(QueuedPrompt),
    /// Prompt sent over ACP; awaiting first chunk.
    Submitted(SubmittedPrompt),
    /// The Agent started producing events for this prompt. Visible content
    /// lives in the tab's ordered active transcript, not in lifecycle state.
    Streaming { prompt: SubmittedPrompt },
    /// Outcome surfaced to UI (card visible, chat turn committed, or empty).
    /// `end_pending` is true until `AgentMessageEnd` arrives — the UI gate
    /// stays held during that window to align with ACP single-flight.
    Surfaced {
        prompt: SubmittedPrompt,
        outcome: TurnOutcome,
        end_pending: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueuedPrompt {
    pub prompt: SubmittedPrompt,
    pub submission: PromptSubmission,
    pub queued_at: std::time::Instant,
    /// Immutable agent binding this input was accepted for.
    pub binding_generation: u64,
    /// Set while the ACP client's tab-to-session map is being rekeyed.
    pub rekey_pending: bool,
}

impl Default for TurnState {
    fn default() -> Self {
        TurnState::Idle
    }
}

/// Identifying info for the prompt that opened the current turn.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmittedPrompt {
    pub id: u64,
    pub text: String,
    pub submitted_at_unix_s: f64,
    pub context: TurnContext,
    pub autofix: Option<AutofixContext>,
}

/// Extra context attached to autofix-initiated turns.
#[derive(Debug, Clone, PartialEq)]
pub struct AutofixContext {
    /// `App.autofix_generation` at submit time. Compared against current
    /// generation on every chunk / end event; mismatch means a newer autofix
    /// (or an Esc cancel) has invalidated this turn — drop the response.
    pub generation: u64,
}

/// What the assistant produced for the user this turn.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// Recommendation card is visible. Unified across autofix Fix and
    /// planner-mode task suggestions.
    Recommendation(RecommendationSet),
    /// The user acted on a recommendation before the Agent finished. The
    /// card is hidden, but its history summary is retained until the active
    /// transcript can be committed at the real turn boundary.
    ResolvedRecommendation {
        summary: String,
        trailing_marker: String,
    },
    /// Prose / explain text has been committed to `completed_turns`.
    ChatTurn,
    /// No visible response (cancelled, or model returned nothing parseable).
    Empty,
}

/// Distinguishes thought-stream chunks from message chunks at the App layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    Thought,
    Message,
}

impl TurnState {
    pub fn is_idle(&self) -> bool {
        matches!(self, TurnState::Idle)
    }

    pub fn is_streaming(&self) -> bool {
        matches!(self, TurnState::Streaming { .. })
    }

    /// True iff a new prompt submission is allowed right now.
    ///
    /// Idle and `Surfaced { end_pending: false }` accept new prompts. All
    /// other states are "busy" — the UI shows a "wait" toast and ACP would
    /// reject the prompt anyway.
    pub fn accepts_new_prompt(&self) -> bool {
        match self {
            TurnState::Idle => true,
            TurnState::Queued(_) => false,
            TurnState::Surfaced { end_pending, .. } => !*end_pending,
            _ => false,
        }
    }

    /// True while the agent owes us output for the current prompt — i.e.
    /// the turn is `Submitted`, `Streaming`, or `Surfaced{end_pending:true}`.
    /// Used by event handlers (ToolCall, Plan, Permission) to drop chunks
    /// that arrive after a cancel.
    pub fn is_in_flight(&self) -> bool {
        match self {
            TurnState::Submitted(_) | TurnState::Streaming { .. } => true,
            TurnState::Surfaced {
                end_pending: true, ..
            } => true,
            _ => false,
        }
    }

    /// True while an Agent request can still be attributed to this turn.
    ///
    /// A surfaced turn may have released its UI busy gate before a follow-up
    /// permission or clarification request reaches the App event loop. The
    /// request itself proves the Agent is still waiting; only `Idle` means
    /// there is no turn left to service.
    pub fn can_service_agent_request(&self) -> bool {
        matches!(
            self,
            TurnState::Submitted(_) | TurnState::Streaming { .. } | TurnState::Surfaced { .. }
        )
    }

    /// The surfaced recommendation set, if the outcome is a card.
    pub fn recommendations(&self) -> Option<&RecommendationSet> {
        match self {
            TurnState::Surfaced {
                outcome: TurnOutcome::Recommendation(rec),
                ..
            } => Some(rec),
            _ => None,
        }
    }

    /// Prompt info for the in-flight or just-surfaced turn.
    pub fn prompt(&self) -> Option<&SubmittedPrompt> {
        match self {
            TurnState::Idle => None,
            TurnState::Queued(queued) => Some(&queued.prompt),
            TurnState::Submitted(p) => Some(p),
            TurnState::Streaming { prompt } => Some(prompt),
            TurnState::Surfaced { prompt, .. } => Some(prompt),
        }
    }

    /// Mutable prompt info for the in-flight or just-surfaced turn. Used to
    /// late-bind the host-resolved turn context (see
    /// `App::apply_prompt_target_resolved`).
    pub fn prompt_mut(&mut self) -> Option<&mut SubmittedPrompt> {
        match self {
            TurnState::Idle => None,
            TurnState::Queued(queued) => Some(&mut queued.prompt),
            TurnState::Submitted(p) => Some(p),
            TurnState::Streaming { prompt } => Some(prompt),
            TurnState::Surfaced { prompt, .. } => Some(prompt),
        }
    }

    /// Autofix generation snapshot for the current turn, if any.
    pub fn autofix_generation(&self) -> Option<u64> {
        self.prompt()
            .and_then(|p| p.autofix.as_ref())
            .map(|a| a.generation)
    }

    /// Whether the current turn is an autofix turn.
    pub fn is_autofix(&self) -> bool {
        self.prompt().map(|p| p.autofix.is_some()).unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::RecommendationSet;

    fn prompt() -> SubmittedPrompt {
        SubmittedPrompt {
            id: 1,
            text: "hello".into(),
            submitted_at_unix_s: 0.0,
            context: TurnContext::default(),
            autofix: None,
        }
    }

    fn autofix_prompt(gen: u64) -> SubmittedPrompt {
        SubmittedPrompt {
            id: 2,
            text: "autofix".into(),
            submitted_at_unix_s: 0.0,
            context: TurnContext::with_target_pane("pane-1"),
            autofix: Some(AutofixContext { generation: gen }),
        }
    }

    fn empty_rec_set() -> RecommendationSet {
        RecommendationSet {
            recommended_choice: None,
            choices: vec![],
        }
    }

    #[test]
    fn default_is_idle() {
        assert!(TurnState::default().is_idle());
    }

    #[test]
    fn idle_state_predicates() {
        let s = TurnState::Idle;
        assert!(s.is_idle());
        assert!(!s.is_streaming());
        assert!(s.accepts_new_prompt());
        assert!(s.recommendations().is_none());
        assert!(s.prompt().is_none());
        assert!(s.autofix_generation().is_none());
        assert!(!s.is_autofix());
        assert!(!s.is_in_flight());
    }

    #[test]
    fn submitted_state_predicates() {
        let s = TurnState::Submitted(prompt());
        assert!(!s.is_idle());
        assert!(!s.is_streaming());
        assert!(!s.accepts_new_prompt());
        assert!(s.recommendations().is_none());
        assert!(s.prompt().is_some());
        assert!(s.is_in_flight());
    }

    #[test]
    fn queued_state_is_busy_but_not_acp_in_flight() {
        let state = TurnState::Queued(QueuedPrompt {
            prompt: prompt(),
            submission: PromptSubmission::new("hello".to_string(), None),
            queued_at: std::time::Instant::now(),
            binding_generation: 1,
            rekey_pending: false,
        });

        assert!(!state.accepts_new_prompt());
        assert!(!state.is_in_flight());
        assert!(!state.can_service_agent_request());
        assert!(state.prompt().is_some());
    }

    #[test]
    fn streaming_state_predicates() {
        let s = TurnState::Streaming { prompt: prompt() };
        assert!(!s.is_idle());
        assert!(s.is_streaming());
        assert!(!s.accepts_new_prompt());
        assert!(s.recommendations().is_none());
        assert!(s.prompt().is_some());
        assert!(s.is_in_flight());
    }

    #[test]
    fn surfaced_end_pending_blocks_new_prompts() {
        let s = TurnState::Surfaced {
            prompt: prompt(),
            outcome: TurnOutcome::ChatTurn,
            end_pending: true,
        };
        assert!(!s.accepts_new_prompt());
        // end_pending=true means AgentMessageEnd hasn't arrived yet, so the UI
        // gate remains held even though visible output may already be present.
        assert!(s.is_in_flight());
    }

    #[test]
    fn surfaced_end_done_is_not_in_flight() {
        let s = TurnState::Surfaced {
            prompt: prompt(),
            outcome: TurnOutcome::ChatTurn,
            end_pending: false,
        };
        assert!(s.accepts_new_prompt());
        assert!(!s.is_in_flight());
    }

    #[test]
    fn surfaced_end_done_accepts_new_prompts() {
        let s = TurnState::Surfaced {
            prompt: prompt(),
            outcome: TurnOutcome::ChatTurn,
            end_pending: false,
        };
        assert!(s.accepts_new_prompt());
        assert!(!s.is_in_flight());
    }

    #[test]
    fn surfaced_recommendation_exposes_set() {
        let s = TurnState::Surfaced {
            prompt: prompt(),
            outcome: TurnOutcome::Recommendation(empty_rec_set()),
            end_pending: true,
        };
        assert!(s.recommendations().is_some());
    }

    #[test]
    fn surfaced_chat_has_no_recommendation() {
        let s = TurnState::Surfaced {
            prompt: prompt(),
            outcome: TurnOutcome::ChatTurn,
            end_pending: false,
        };
        assert!(s.recommendations().is_none());
    }

    #[test]
    fn autofix_generation_propagates() {
        let s = TurnState::Submitted(autofix_prompt(42));
        assert_eq!(s.autofix_generation(), Some(42));
        assert!(s.is_autofix());

        let s = TurnState::Streaming {
            prompt: autofix_prompt(7),
        };
        assert_eq!(s.autofix_generation(), Some(7));

        let s = TurnState::Surfaced {
            prompt: autofix_prompt(99),
            outcome: TurnOutcome::Empty,
            end_pending: false,
        };
        assert_eq!(s.autofix_generation(), Some(99));
    }

    #[test]
    fn non_autofix_turn_has_no_generation() {
        let s = TurnState::Submitted(prompt());
        assert_eq!(s.autofix_generation(), None);
        assert!(!s.is_autofix());
    }

    #[test]
    fn idle_has_no_autofix_generation() {
        assert_eq!(TurnState::Idle.autofix_generation(), None);
    }

    #[test]
    fn surfaced_turn_can_service_late_agent_request() {
        let state = TurnState::Surfaced {
            prompt: prompt(),
            outcome: TurnOutcome::ChatTurn,
            end_pending: false,
        };

        assert!(!state.is_in_flight());
        assert!(state.can_service_agent_request());
        assert!(!TurnState::Idle.can_service_agent_request());
    }
}

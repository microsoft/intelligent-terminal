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

/// Per-tab turn state.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnState {
    /// No turn in flight. Input box accepts new prompts.
    Idle,
    /// Prompt sent over ACP; awaiting first chunk.
    Submitted(SubmittedPrompt),
    /// Receiving streamed chunks. `buf` is the accumulated assistant text.
    Streaming {
        prompt: SubmittedPrompt,
        buf: String,
    },
    /// Outcome surfaced to UI (card visible, chat turn committed, or empty).
    /// `end_pending` is true until `AgentMessageEnd` arrives — the UI gate
    /// stays held during that window to align with ACP single-flight.
    Surfaced {
        prompt: SubmittedPrompt,
        outcome: TurnOutcome,
        end_pending: bool,
    },
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
    pub autofix: Option<AutofixContext>,
}

/// Extra context attached to autofix-initiated turns.
#[derive(Debug, Clone, PartialEq)]
pub struct AutofixContext {
    /// Pane that produced the failing command.
    pub target_pane_id: String,
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
            TurnState::Surfaced { end_pending: true, .. } => true,
            _ => false,
        }
    }

    /// Streaming buffer, if any.
    pub fn buffer(&self) -> Option<&str> {
        match self {
            TurnState::Streaming { buf, .. } => Some(buf.as_str()),
            _ => None,
        }
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
            TurnState::Submitted(p) => Some(p),
            TurnState::Streaming { prompt, .. } => Some(prompt),
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
        self.prompt()
            .map(|p| p.autofix.is_some())
            .unwrap_or(false)
    }

    /// Spinner label, if the state should drive a busy indicator.
    /// `Submitted` / `Streaming` show the spinner; `Surfaced` and `Idle`
    /// do not.
    pub fn spinner_label(&self) -> Option<&'static str> {
        match self {
            TurnState::Submitted(_) => Some("Thinking..."),
            TurnState::Streaming { .. } => Some("Thinking..."),
            _ => None,
        }
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
            autofix: None,
        }
    }

    fn autofix_prompt(gen: u64) -> SubmittedPrompt {
        SubmittedPrompt {
            id: 2,
            text: "autofix".into(),
            submitted_at_unix_s: 0.0,
            autofix: Some(AutofixContext {
                target_pane_id: "pane-1".into(),
                generation: gen,
            }),
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
        assert!(s.buffer().is_none());
        assert!(s.recommendations().is_none());
        assert!(s.prompt().is_none());
        assert!(s.autofix_generation().is_none());
        assert!(!s.is_autofix());
        assert!(s.spinner_label().is_none());
        assert!(!s.is_in_flight());
    }

    #[test]
    fn submitted_state_predicates() {
        let s = TurnState::Submitted(prompt());
        assert!(!s.is_idle());
        assert!(!s.is_streaming());
        assert!(!s.accepts_new_prompt());
        assert!(s.buffer().is_none());
        assert!(s.recommendations().is_none());
        assert!(s.prompt().is_some());
        assert!(s.spinner_label().is_some());
        assert!(s.is_in_flight());
    }

    #[test]
    fn streaming_state_predicates() {
        let s = TurnState::Streaming {
            prompt: prompt(),
            buf: "partial".into(),
        };
        assert!(!s.is_idle());
        assert!(s.is_streaming());
        assert!(!s.accepts_new_prompt());
        assert_eq!(s.buffer(), Some("partial"));
        assert!(s.recommendations().is_none());
        assert!(s.prompt().is_some());
        assert!(s.spinner_label().is_some());
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
        assert!(s.spinner_label().is_none());
        assert!(s.is_in_flight());
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
            buf: String::new(),
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
}

//! `App`'s turn-state transition methods, split out of the large `app.rs`
//! file. Declared as a regular (non-test) child module of `app` via `#[path]`
//! so it can reach `App`'s private fields and helper methods just like the
//! rest of `app.rs` does.

use super::tab_state::PendingTerminalActionProposal;
use super::*;

enum DirectProposalEvaluation {
    Presented,
    Duplicate(String),
    Stale(String),
    Rejected(crate::agent_tools::action_proposal::schema::ProposalError),
    Unavailable(String),
}

// ─────────────────────────────────────────────────────────────────────────
// TurnState transition methods
//
// Source of truth for the per-turn lifecycle (see
// `doc/specs/turn-state-refactor.md`). Every event handler — chunk arrival,
// end-of-turn, Enter on a card, Esc / Ctrl+C cancel, Auto error handling trigger — goes
// through one of these methods.
// ─────────────────────────────────────────────────────────────────────────

impl App {
    /// Transition `tab.turn` into `Submitted` for a new prompt and perform
    /// the side effects: clear stale in-flight chat state (messages, tool
    /// calls, permission, scroll), push the user bubble, log
    /// `prompt_received`. Caller is responsible for actually dispatching the
    /// prompt over ACP (so this method stays free of async / channel
    /// concerns).
    pub fn turn_submit_prompt(&mut self, session_id: &str, prompt: SubmittedPrompt) {
        let tab_key = self.tab_for_session(session_id);
        self.turn_submit_prompt_for_tab(&tab_key, prompt);
    }

    /// Identical to `turn_submit_prompt` but takes the target tab's id
    /// directly, bypassing the `session_id → tab_id` lookup. Used by the
    /// Auto error handling path so a failure in a background tab installs the turn on
    /// that tab even when its ACP session hasn't been created yet (the ACP
    /// layer lazy-creates one when the prompt is dispatched).
    pub fn turn_submit_prompt_for_tab(&mut self, tab_id: &str, prompt: SubmittedPrompt) {
        prompt_timing_log(
            prompt.id,
            prompt.submitted_at_unix_s,
            "prompt_received",
            &format!(
                "auto_error_handling={} text_chars={}",
                prompt.auto_error_handling.is_some(),
                prompt.text.chars().count()
            ),
        );
        let is_auto_error_handling = prompt.auto_error_handling.is_some();
        let user_text = prompt.text.clone();
        let tab = self.tab_mut(tab_id);
        // Per Decision #3, every Idle→Submitted transition explicitly clears
        // these orthogonal fields rather than relying on side effects from a
        // grab-bag helper.
        tab.messages.clear();
        tab.clear_streaming_thought();
        // Dropping any in-flight responders signals Cancelled back to
        // the agent — appropriate when the user starts a new turn.
        tab.permission.clear();
        tab.user_input.clear();
        tab.chat_scroll.reset();
        tab.selection_visible_pending = false;
        // Any leftover card from the previous turn's
        // `Surfaced{end_pending:false}` is dismissed by the new submit.
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.rec_scroll.reset();
        tab.pending_terminal_action_proposal = None;
        tab.active_direct_proposal_id = None;
        // Auto error handling prompts are synthesized by the system; they don't render
        // as a User bubble (the user already sees the error line in the
        // failing pane).
        if !is_auto_error_handling {
            tab.messages.push(ChatMessage::User(user_text));
        }
        tab.scroll_to_bottom();
        tab.activity_frame = 0;
        tab.timing_note = None;
        tab.turn = TurnState::Submitted(prompt);
        // NOTE: `has_meaningful_conversation` is deliberately NOT set here.
        // It gates `resumable_session_id`, i.e. the session id Terminal writes
        // into the saved layout, and a prompt that has only been submitted is
        // not yet proof that the agent has taken it — the caller still has to
        // dispatch it over ACP. A save landing in that window would record a
        // freshly created `session/new` id the agent never wrote to disk, and
        // the restore would fail with "Resource not found". The flag is set
        // instead on the first sign of agent activity for the turn
        // (`turn_observe_chunk` / `turn_close`).

        // Submitting a new prompt dismisses any prior leftover card (the
        // `selected_recommendation = 0` + turn reset above). If the helper
        // had pinned the chip onto that card's pane, release it now so the
        // chip falls back to source-of-agent while the new turn is in
        // flight. Note: this only matters for the new-turn case; the
        // freshly-submitted Auto error handling path overrides chip via the eventual
        // `turn_surface_*` callback once recommendations arrive.
        let owned_tab = tab_id.to_string();
        self.recompute_chip_override(&owned_tab);
        self.project_tab_state(&owned_tab);
    }

    /// Observe a streamed chunk. Lifecycle state records only whether output
    /// started; visible text appends directly to the ordered transcript.
    pub fn turn_observe_chunk(&mut self, session_id: &str, kind: ChunkKind, text: &str) -> bool {
        // Stale Auto error handling check: if the chunk belongs to an Auto error handling turn whose
        // generation no longer matches the tab's counter, drop it.
        let tab = self.session_tab_mut(session_id);
        let current_gen = tab.auto_error_handling.generation;
        if let Some(gen) = tab.turn.auto_error_handling_generation() {
            if gen != current_gen {
                tracing::debug!(
                    target: "auto_error_handling",
                    inflight_gen = gen,
                    current_gen,
                    "dropping stale Auto error handling chunk",
                );
                return false;
            }
        }

        // A chunk for this turn is proof the agent has taken the prompt, and
        // therefore that it has written the session the chunk belongs to. That
        // is the point from which the id is safe to persist — see the note in
        // `turn_submit_prompt_for_tab`.
        tab.has_meaningful_conversation = true;

        match (&tab.turn, kind) {
            (TurnState::Submitted(_), _) => {
                let TurnState::Submitted(prompt) =
                    std::mem::replace(&mut tab.turn, TurnState::Idle)
                else {
                    unreachable!();
                };
                tab.turn = TurnState::Streaming { prompt };
                tab.reveal_chars = 0;
                if kind == ChunkKind::Message {
                    tab.append_agent_chunk(text);
                    true
                } else {
                    tab.append_thought_chunk(text);
                    !text.is_empty()
                }
            }
            (TurnState::Streaming { .. }, ChunkKind::Message) => {
                if tab.streaming_thought_text().is_some() {
                    tab.clear_streaming_thought();
                }
                tab.append_agent_chunk(text);
                true
            }
            (TurnState::Streaming { .. }, ChunkKind::Thought) => {
                tab.append_thought_chunk(text);
                !text.is_empty()
            }
            // A direct proposal may complete before the agent emits its final
            // message chunks. Keep those chunks visible alongside the card.
            (TurnState::Surfaced { .. }, ChunkKind::Message)
                if tab.active_direct_proposal_id.is_some() =>
            {
                tab.append_agent_chunk(text);
                true
            }
            (
                TurnState::Surfaced {
                    outcome: TurnOutcome::ResolvedRecommendation { .. },
                    end_pending: true,
                    ..
                },
                ChunkKind::Message,
            ) => {
                tab.append_agent_chunk(text);
                true
            }
            (TurnState::Surfaced { .. }, _) => false,
            // Chunks while Idle: shouldn't happen; defensive drop.
            (TurnState::Idle, _) => false,
        }
    }

    fn validate_and_stage_terminal_action_proposal(
        &mut self,
        session_id: &str,
        prompt_id: u64,
        active_target: Option<&str>,
        payload: &str,
        proposal_id: &str,
        source: crate::agent_tools::action_proposal::pipe::ProposalPayloadSource,
    ) -> DirectProposalEvaluation {
        use crate::agent_tools::action_proposal::schema::{
            build_recommendation_set, parse_mcp_action_payload, parse_proposal_payload,
        };

        if !self.session_to_tab.contains_key(session_id) {
            return DirectProposalEvaluation::Unavailable(
                "session is not bound to this helper".to_string(),
            );
        }

        match &self.session_tab(session_id).turn {
            TurnState::Surfaced { .. } => {
                return DirectProposalEvaluation::Duplicate(
                    "a card is already showing for this turn".to_string(),
                );
            }
            TurnState::Idle => {
                return DirectProposalEvaluation::Stale(
                    "no turn is in flight for this session".to_string(),
                );
            }
            TurnState::Submitted(_) | TurnState::Streaming { .. } => {}
        }

        if self
            .session_tab(session_id)
            .turn
            .prompt()
            .map(|prompt| prompt.id)
            != Some(prompt_id)
        {
            return DirectProposalEvaluation::Stale(
                "proposal belongs to an earlier prompt".to_string(),
            );
        }

        let is_auto_error_handling = self.session_tab(session_id).turn.is_auto_error_handling();
        if is_auto_error_handling {
            let turn_generation = self
                .session_tab(session_id)
                .turn
                .auto_error_handling_generation();
            let current_generation = self.session_tab(session_id).auto_error_handling.generation;
            if turn_generation != Some(current_generation) {
                return DirectProposalEvaluation::Stale(
                    "Auto error handling turn was superseded".to_string(),
                );
            }
        }

        let wire = match source {
            crate::agent_tools::action_proposal::pipe::ProposalPayloadSource::Cli => {
                parse_proposal_payload(payload.as_bytes())
            }
            crate::agent_tools::action_proposal::pipe::ProposalPayloadSource::Mcp(tool) => {
                parse_mcp_action_payload(tool, payload.as_bytes(), is_auto_error_handling)
            }
        };
        let wire = match wire {
            Ok(wire) => wire,
            Err(error) => return DirectProposalEvaluation::Rejected(error),
        };
        let configured_delegate_id = self
            .delegate_agents
            .as_ref()
            .and_then(|shared| shared.lock().ok())
            .and_then(|guard| guard.first().map(|runtime| runtime.id.clone()));
        let recommendations = match build_recommendation_set(
            &wire,
            is_auto_error_handling,
            configured_delegate_id.as_deref(),
            active_target,
            self.pane_id.as_deref(),
        ) {
            Ok(set) => set,
            Err(error) => return DirectProposalEvaluation::Rejected(error),
        };

        self.session_tab_mut(session_id)
            .pending_terminal_action_proposal = Some(PendingTerminalActionProposal {
            proposal_id: proposal_id.to_string(),
            session_id: session_id.to_string(),
            prompt_id,
            is_auto_error_handling,
            recommendations,
        });
        DirectProposalEvaluation::Presented
    }

    pub(super) fn evaluate_direct_terminal_action_proposal(
        &mut self,
        context: &crate::agent_tools::action_proposal::channel::ValidationContext,
        payload: &str,
        source: crate::agent_tools::action_proposal::pipe::ProposalPayloadSource,
    ) -> crate::agent_tools::action_proposal::pipe::ProposalValidationDecision {
        use crate::agent_tools::action_proposal::channel::ProposalValidationStatus;
        use crate::agent_tools::action_proposal::schema::ProposalError;

        let binding = &context.binding;
        match self.validate_and_stage_terminal_action_proposal(
            &binding.session_id,
            binding.prompt_id,
            binding.active_target.as_deref(),
            payload,
            &context.proposal_id,
            source,
        ) {
            DirectProposalEvaluation::Presented => {
                crate::agent_tools::action_proposal::pipe::ProposalValidationDecision::accepted()
            }
            DirectProposalEvaluation::Duplicate(reason) => {
                crate::agent_tools::action_proposal::pipe::ProposalValidationDecision {
                    status: ProposalValidationStatus::AlreadyConsumed,
                    reason: Some(reason),
                    retryable: false,
                }
            }
            DirectProposalEvaluation::Stale(reason) => {
                crate::agent_tools::action_proposal::pipe::ProposalValidationDecision {
                    status: ProposalValidationStatus::Stale,
                    reason: Some(reason),
                    retryable: false,
                }
            }
            DirectProposalEvaluation::Rejected(error) => {
                let (status, retryable) = match &error {
                    ProposalError::TooLarge { .. }
                    | ProposalError::Malformed(_)
                    | ProposalError::UnsupportedSchemaVersion(_) => {
                        (ProposalValidationStatus::InvalidSchema, true)
                    }
                    ProposalError::PolicyViolation(_) => {
                        (ProposalValidationStatus::Rejected, false)
                    }
                };
                crate::agent_tools::action_proposal::pipe::ProposalValidationDecision {
                    status,
                    reason: Some(error.reason()),
                    retryable,
                }
            }
            DirectProposalEvaluation::Unavailable(reason) => {
                crate::agent_tools::action_proposal::pipe::ProposalValidationDecision {
                    status: ProposalValidationStatus::Unavailable,
                    reason: Some(reason),
                    retryable: false,
                }
            }
        }
    }

    pub(super) fn commit_terminal_action_proposal(&mut self, proposal_id: &str) -> bool {
        let pending = self.tab_sessions.values_mut().find_map(|tab| {
            let matches = tab
                .pending_terminal_action_proposal
                .as_ref()
                .is_some_and(|pending| pending.proposal_id == proposal_id);
            matches
                .then(|| tab.pending_terminal_action_proposal.take())
                .flatten()
        });
        let Some(pending) = pending else {
            return false;
        };
        match &self.session_tab(&pending.session_id).turn {
            TurnState::Submitted(prompt) | TurnState::Streaming { prompt, .. }
                if prompt.id == pending.prompt_id => {}
            _ => return false,
        }
        if pending.is_auto_error_handling {
            self.turn_surface_fix(
                &pending.session_id,
                pending.recommendations,
                "direct_proposal_fix",
            );
        } else {
            self.turn_surface_recommendation(
                &pending.session_id,
                pending.recommendations,
                "direct_proposal",
            );
        }
        self.session_tab_mut(&pending.session_id)
            .active_direct_proposal_id = Some(proposal_id.to_string());
        true
    }

    pub(super) fn invalidate_terminal_action_proposal(
        &mut self,
        proposal_id: &str,
        session_id: &str,
    ) {
        let tab = self.session_tab_mut(session_id);
        if tab
            .pending_terminal_action_proposal
            .as_ref()
            .is_some_and(|pending| pending.proposal_id == proposal_id)
        {
            tab.pending_terminal_action_proposal = None;
        }
        if tab.active_direct_proposal_id.as_deref() == Some(proposal_id) {
            self.turn_cancel(session_id);
        }
    }

    /// Close the in-flight turn on `AgentMessageEnd`. Dispatches across
    /// four termination paths:
    ///
    /// 1. Stale Auto error handling discard (newer trigger or Esc cancelled this turn).
    /// 2. A direct proposal already surfaced — release the UI gate.
    /// 3. `Submitted` with no chunks — model returned nothing.
    /// 4. `Streaming` with a buffer — commit it as assistant text.
    pub fn turn_close(&mut self, session_id: &str) {
        // (1) Stale Auto error handling discard.
        let current_gen = self.session_tab(session_id).auto_error_handling.generation;
        if let Some(gen) = self
            .session_tab(session_id)
            .turn
            .auto_error_handling_generation()
        {
            if gen != current_gen {
                tracing::info!(
                    target: "auto_error_handling",
                    inflight_gen = gen,
                    current_gen,
                    "discarding stale Auto error handling turn at close",
                );
                self.turn_clear_agent_activity(session_id);
                let tab = self.session_tab_mut(session_id);
                tab.messages.clear();
                tab.reveal_chars = 0;
                tab.turn = TurnState::Idle;
                return;
            }
        }

        // Reaching a turn boundary means the agent processed the prompt, so
        // its session is on disk even if the turn produced no visible chunks
        // (a tool-only turn, say). See `turn_submit_prompt_for_tab`. Gated on
        // there actually being a turn: a stray end-of-turn against an idle tab
        // is no evidence of a conversation.
        {
            let tab = self.session_tab_mut(session_id);
            if !matches!(tab.turn, TurnState::Idle) {
                tab.has_meaningful_conversation = true;
            }
        }

        // (2) A direct proposal already surfaced. Keep its transcript active
        // until this real turn boundary so late tool updates and prose still
        // target the same ordered source of truth.
        let surfaced_commit = match &self.session_tab(session_id).turn {
            TurnState::Surfaced {
                outcome: TurnOutcome::Recommendation(recommendations),
                end_pending: true,
                ..
            } => Some((format_recommendations_for_chat(recommendations), None, true)),
            TurnState::Surfaced {
                outcome:
                    TurnOutcome::ResolvedRecommendation {
                        summary,
                        trailing_marker,
                    },
                end_pending: true,
                ..
            } => Some((summary.clone(), Some(trailing_marker.clone()), false)),
            TurnState::Surfaced {
                end_pending: true, ..
            } => {
                self.turn_release_end_pending_logged(session_id, "via=surfaced+end");
                self.turn_clear_agent_activity(session_id);
                return;
            }
            _ => None,
        };
        if let Some((summary, trailing_marker, keep_card)) = surfaced_commit {
            self.turn_commit_recommendation_history(session_id, summary, trailing_marker);
            if !keep_card {
                if let TurnState::Surfaced { outcome, .. } =
                    &mut self.session_tab_mut(session_id).turn
                {
                    *outcome = TurnOutcome::Empty;
                }
            }
            self.turn_release_end_pending_logged(session_id, "via=direct+end");
            self.turn_clear_agent_activity(session_id);
            return;
        }

        // (3) Submitted, no chunks. For Auto error handling this would leave the bar
        //     stuck in Pending; clear it explicitly.
        let is_auto_error_handling = match &self.session_tab(session_id).turn {
            TurnState::Streaming { prompt } => prompt.auto_error_handling.is_some(),
            TurnState::Submitted(_) => {
                self.turn_close_no_chunks(session_id);
                return;
            }
            // Idle / already-surfaced+end_done — nothing to do.
            _ => return,
        };

        // (4) Typed action cards arrive only through the direct proposal
        // channel. Streamed assistant content is always prose.
        if is_auto_error_handling {
            self.turn_close_finalize_auto_error_handling_text(session_id);
        } else {
            self.turn_close_finalize_chat(session_id);
        }
        self.turn_clear_agent_activity(session_id);
    }

    /// Path (3): close a turn that received `AgentMessageEnd` with no
    /// streamed content. Emits `auto_error_handling_state_cleared` if it was an
    /// Auto error handling turn so the bottom bar doesn't stick in Pending.
    fn turn_close_no_chunks(&mut self, session_id: &str) {
        let target_tab = self.tab_for_session(session_id);
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        // Empty `target_pane_id` (manual `/fix`) is not a real pane — filter
        // it out so an empty-response turn doesn't emit a bottom-bar event.
        let auto_error_handling_pane = prompt
            .auto_error_handling
            .as_ref()
            .and(prompt.context.target_pane_id().map(str::to_string));
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending: true,
        };
        if auto_error_handling_pane.is_some() {
            self.emit_auto_error_handling_state_cleared(&target_tab);
            let auto_error_handling = &mut self.session_tab_mut(session_id).auto_error_handling;
            auto_error_handling.pane_id = None;
            auto_error_handling.started_at = None;
        }
        self.turn_release_end_pending(session_id);
        self.turn_clear_agent_activity(session_id);
    }

    fn turn_close_finalize_auto_error_handling_text(&mut self, session_id: &str) {
        if !self
            .session_tab(session_id)
            .active_agent_text()
            .trim()
            .is_empty()
        {
            self.turn_surface_explain(session_id, "auto_error_handling_text");
            self.turn_release_end_pending(session_id);
            return;
        }

        let target_tab = self.tab_for_session(session_id);
        let pane_id = self
            .session_tab(session_id)
            .auto_error_handling
            .pane_id
            .clone();
        if pane_id.is_some() {
            self.emit_auto_error_handling_state_cleared(&target_tab);
        }
        let auto_error_handling = &mut self.session_tab_mut(session_id).auto_error_handling;
        auto_error_handling.pane_id = None;
        auto_error_handling.started_at = None;
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let details = tab.take_current_turn_details();
        if !details.is_empty() {
            tab.completed_turns.push(CompletedTurn {
                prompt: t!("chat.auto_error_handling_prompt_label").into_owned(),
                details,
                expanded: true,
                trailing_marker: None,
            });
        }
        tab.scroll_to_bottom();
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending: false,
        };
    }

    fn turn_close_finalize_chat(&mut self, session_id: &str) {
        let response_chars = self
            .session_tab(session_id)
            .active_agent_text()
            .chars()
            .count();
        self.log_selection_phase_for(
            session_id,
            "assistant_text",
            &format!("response_chars={response_chars}"),
        );
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let details = tab.take_current_turn_details();
        tab.completed_turns.push(CompletedTurn {
            prompt: prompt.text.clone(),
            details,
            expanded: true,
            trailing_marker: None,
        });
        tab.scroll_to_bottom();
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::ChatTurn,
            end_pending: true,
        };
        self.turn_release_end_pending(session_id);
    }

    fn turn_commit_recommendation_history(
        &mut self,
        session_id: &str,
        summary: String,
        trailing_marker: Option<String>,
    ) {
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let prompt_label = if prompt.auto_error_handling.is_some() {
            t!("chat.auto_error_handling_prompt_label").into_owned()
        } else {
            prompt.text
        };
        let mut details = tab.take_current_turn_details();
        details.push(ChatMessage::Agent(summary));
        tab.completed_turns.push(CompletedTurn {
            prompt: prompt_label,
            details,
            expanded: true,
            trailing_marker,
        });
        tab.scroll_to_bottom();
    }

    /// Variant of `turn_release_end_pending` with a custom `via=` log tag
    /// for the eager-surface path. `turn_release_end_pending` uses
    /// `via=end_only`; `via=eager+end` lets `prompt_timing` consumers
    /// distinguish.
    fn turn_release_end_pending_logged(&mut self, session_id: &str, via: &str) {
        let tab = self.session_tab_mut(session_id);
        if let TurnState::Surfaced {
            end_pending,
            prompt,
            ..
        } = &mut tab.turn
        {
            if *end_pending {
                *end_pending = false;
                let prompt_id = prompt.id;
                let submitted_at = prompt.submitted_at_unix_s;
                prompt_timing_log(prompt_id, submitted_at, "prompt_complete", via);
            }
        }
    }

    /// Helper called at every turn-close path. Clears the animation phase and
    /// first-visible-activity latch.
    fn turn_clear_agent_activity(&mut self, session_id: &str) {
        let tab = self.session_tab_mut(session_id);
        tab.activity_frame = 0;
        tab.clear_streaming_thought();
    }

    /// User pressed Enter while a card was visible — dispatch the selected
    /// choice to the coordinator and transition to `Surfaced { Empty, .. }`
    /// while preserving the ACP single-flight gate.
    pub fn turn_execute_card(&mut self, session_id: &str) {
        let Some(choice) = self.selected_recommendation_choice().cloned() else {
            return;
        };
        let tab = self.session_tab(session_id);
        let TurnState::Surfaced {
            outcome: TurnOutcome::Recommendation(recommendations),
            ..
        } = &tab.turn
        else {
            return;
        };
        let summary = format_recommendations_for_chat(recommendations);
        // Snapshot the title before `choice` is moved into ChoiceExecution,
        // so we can stamp the chat history with an "executed" marker after
        // dispatch.
        let executed_title = choice.title.clone();
        let direct_proposal_id = self
            .session_tab(session_id)
            .active_direct_proposal_id
            .clone();
        let insert_only =
            self.session_tab(session_id).selected_button == 1 && self.is_send_choice(&choice);
        let target_tab = self.tab_for_session(session_id);
        let context = self
            .session_tab(session_id)
            .turn
            .prompt()
            .map(|prompt| prompt.context.clone())
            .unwrap_or_default();
        let confirmation_claim = if let Some(proposal_id) = direct_proposal_id.as_deref() {
            let Some(claim) = self.proposal_channels.claim_confirmation(proposal_id) else {
                self.turn_cancel(session_id);
                return;
            };
            Some(claim)
        } else {
            None
        };
        let dispatched = self
            .recommendation_tx
            .send(crate::coordinator::ChoiceExecution {
                choice,
                insert_only,
                context,
            })
            .is_ok();
        if let Some(claim) = confirmation_claim {
            let status = if dispatched {
                crate::agent_tools::action_proposal::channel::ProposalFinalStatus::Confirmed
            } else {
                crate::agent_tools::action_proposal::channel::ProposalFinalStatus::Unavailable
            };
            self.proposal_channels.finalize_confirmation(claim, status);
            if !dispatched {
                self.turn_cancel(session_id);
                return;
            }
        }
        if self
            .session_tab(session_id)
            .turn
            .prompt()
            .and_then(|p| p.auto_error_handling.as_ref())
            .is_some()
        {
            self.emit_auto_error_handling_state_cleared(&target_tab);
        }
        let auto_error_handling = &mut self.session_tab_mut(session_id).auto_error_handling;
        auto_error_handling.pane_id = None;
        auto_error_handling.started_at = None;
        let tab = self.session_tab_mut(session_id);
        let TurnState::Surfaced {
            prompt,
            end_pending,
            ..
        } = std::mem::replace(&mut tab.turn, TurnState::Idle)
        else {
            unreachable!()
        };
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.active_direct_proposal_id = None;
        tab.rec_scroll.reset();
        let marker = t!("chat.turn_executed", title = &executed_title).into_owned();
        let outcome = if end_pending {
            TurnOutcome::ResolvedRecommendation {
                summary,
                trailing_marker: marker,
            }
        } else {
            // AgentMessageEnd already committed this turn while the card was
            // visible, so only annotate that existing history entry.
            if let Some((index, last)) = tab.completed_turns.iter_mut().enumerate().next_back() {
                last.trailing_marker = Some(marker);
                tab.invalidate_completed_turn_height(index);
            }
            TurnOutcome::Empty
        };
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome,
            end_pending,
        };

        // Exiting Surfaced{Recommendation} — release any chip override the
        // card had pinned. The C++ side falls back to source-of-agent.
        let target_tab = self.tab_for_session(session_id);
        self.recompute_chip_override(&target_tab);
    }

    /// User pressed Esc — cancel the in-flight turn. Bumps
    /// `auto_error_handling_generation` so any chunks that arrive after this point are
    /// dropped by the stale-check in `turn_observe_chunk`.
    pub fn turn_cancel(&mut self, session_id: &str) {
        let direct_proposal_id = self
            .session_tab(session_id)
            .active_direct_proposal_id
            .clone();
        let target_tab = self.tab_for_session(session_id);
        let pane_id = {
            let tab = self.session_tab_mut(session_id);
            tab.auto_error_handling.generation = tab.auto_error_handling.generation.wrapping_add(1);
            tab.turn
                .prompt()
                .and_then(|prompt| {
                    prompt
                        .auto_error_handling
                        .as_ref()
                        .and(prompt.context.target_pane_id().map(str::to_string))
                })
                .or_else(|| tab.auto_error_handling.pane_id.clone())
        };
        if pane_id.is_some() {
            self.emit_auto_error_handling_state_cleared(&target_tab);
        }
        let tab = self.session_tab_mut(session_id);
        tab.auto_error_handling.started_at = None;
        let canceled_marker = t!("chat.turn_canceled").into_owned();
        // Three paths into cancel:
        //   - Submitted / Streaming → commit a fresh completed_turn (prompt +
        //     whatever streamed + canceled marker) so the user always sees
        //     that this turn happened and that they cancelled it.
        //   - Surfaced{Recommendation}: commit now if AgentMessageEnd is still
        //     pending; otherwise annotate the history committed at turn end.
        //   - Other states (Idle / Surfaced{Empty / ChatTurn}) → no-op.
        let new_turn_data: Option<(String, Option<String>, String)> = match &tab.turn {
            TurnState::Submitted(prompt) => {
                let label = match prompt.auto_error_handling.as_ref() {
                    Some(_) => t!("chat.auto_error_handling_prompt_label").into_owned(),
                    None => prompt.text.clone(),
                };
                Some((label, None, canceled_marker.clone()))
            }
            TurnState::Streaming { prompt } => {
                let label = match prompt.auto_error_handling.as_ref() {
                    Some(_) => t!("chat.auto_error_handling_prompt_label").into_owned(),
                    None => prompt.text.clone(),
                };
                Some((label, None, canceled_marker.clone()))
            }
            TurnState::Surfaced {
                prompt,
                outcome: TurnOutcome::Recommendation(recommendations),
                end_pending: true,
            } => {
                let label = match prompt.auto_error_handling.as_ref() {
                    Some(_) => t!("chat.auto_error_handling_prompt_label").into_owned(),
                    None => prompt.text.clone(),
                };
                Some((
                    label,
                    Some(format_recommendations_for_chat(recommendations)),
                    canceled_marker.clone(),
                ))
            }
            TurnState::Surfaced {
                prompt,
                outcome:
                    TurnOutcome::ResolvedRecommendation {
                        summary,
                        trailing_marker,
                    },
                end_pending: true,
            } => {
                let label = match prompt.auto_error_handling.as_ref() {
                    Some(_) => t!("chat.auto_error_handling_prompt_label").into_owned(),
                    None => prompt.text.clone(),
                };
                Some((label, Some(summary.clone()), trailing_marker.clone()))
            }
            _ => None,
        };
        let annotate_card = matches!(
            &tab.turn,
            TurnState::Surfaced {
                outcome: TurnOutcome::Recommendation(_),
                end_pending: false,
                ..
            }
        );
        if let Some((prompt_label, summary, trailing_marker)) = new_turn_data {
            let mut details = tab.take_current_turn_details();
            if let Some(summary) = summary {
                details.push(ChatMessage::Agent(summary));
            }
            tab.completed_turns.push(CompletedTurn {
                prompt: prompt_label,
                details,
                expanded: true,
                trailing_marker: Some(trailing_marker),
            });
            tab.scroll_to_bottom();
        } else if annotate_card {
            if let Some((index, last)) = tab.completed_turns.iter_mut().enumerate().next_back() {
                last.trailing_marker = Some(canceled_marker);
                tab.invalidate_completed_turn_height(index);
            }
        }
        tab.auto_error_handling.pane_id = None;
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.rec_scroll.reset();
        tab.activity_frame = 0;
        tab.clear_streaming_thought();
        tab.user_input.clear();
        tab.turn = TurnState::Idle;
        tab.pending_terminal_action_proposal = None;
        tab.active_direct_proposal_id = None;
        if let Some(proposal_id) = direct_proposal_id.as_deref() {
            self.proposal_channels.resolve_final(
                proposal_id,
                crate::agent_tools::action_proposal::channel::ProposalFinalStatus::Cancelled,
            );
        }

        // Esc on a Send card or in-flight Auto error handling exits the chip-override
        // state; release whatever the helper had pinned. C++ falls back to
        // source-of-agent driven rendering.
        self.recompute_chip_override(&target_tab);
    }

    // ── Internal surface helpers (shared between eager and end-of-turn). ──

    /// Surface a planner-mode recommendation card.
    fn turn_surface_recommendation(
        &mut self,
        session_id: &str,
        recommendations: RecommendationSet,
        phase_name: &str,
    ) {
        let rec_idx = recommended_choice_index(&recommendations);
        let choice_count = recommendations.choices.len();
        let recommended_choice = recommendations.recommended_choice;
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "choice_count={} recommended_choice={:?}",
                choice_count, recommended_choice
            ),
        );
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        tab.scroll_to_bottom();
        tab.selected_recommendation = rec_idx;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.rec_scroll.reset();
        tab.selection_visible_pending = true;
        tab.clear_completed_turn_selection();
        tab.activity_frame = 0;
        tab.clear_streaming_thought();
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Recommendation(recommendations),
            end_pending: true,
        };

        // Entering Surfaced{Recommendation} with a Send card selected is
        // the typing→card transition; ask C++ to pin the chip onto that
        // card's target pane (or release it when the selected card has no
        // Send action).
        let target_tab = self.tab_for_session(session_id);
        self.recompute_chip_override(&target_tab);
    }

    /// Surface an Auto error handling fix as an actionable result card.
    fn turn_surface_fix(
        &mut self,
        session_id: &str,
        recommendations: RecommendationSet,
        phase_name: &str,
    ) {
        // Defensive: only Auto error handling turns surface a fix card here.
        let prompt = self.session_tab(session_id).turn.prompt();
        let Some(prompt) = prompt.filter(|prompt| prompt.auto_error_handling.is_some()) else {
            return;
        };
        // A manual `/fix` may have no concrete failing pane. Still surface the
        // card below, but skip the
        // bottom-bar / result-pane side effects — they key off a real
        // failing pane (the Review pill, the Ctrl+Alt+. hotkey target).
        let bar_pane = prompt.context.target_pane_id().map(str::to_string);
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "pane={bar_pane:?} title={:?}",
                recommendations.choices.first().map(|c| &c.title)
            ),
        );
        let target_tab = self.tab_for_session(session_id);
        // Analysis produced a fix recommendation. Record it as a result
        // pending review and surface the bar accordingly (Review when the
        // pane is closed, Idle when it's already open). The recommendation
        // card still lives in the turn below so the user can act on it
        // inside the pane — Auto error handling no longer auto-executes.
        if let Some(pane_id) = bar_pane.as_ref() {
            {
                let auto_error_handling = &mut self.tab_mut(&target_tab).auto_error_handling;
                auto_error_handling.result_pane_id = Some(pane_id.clone());
                auto_error_handling.pane_id = None;
                auto_error_handling.started_at = None;
            }
            self.update_auto_error_handling_review_state(&target_tab, pane_id);
        }
        let rec_idx = recommended_choice_index(&recommendations);
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        tab.scroll_to_bottom();
        tab.selected_recommendation = rec_idx;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.selection_visible_pending = true;
        tab.activity_frame = 0;
        tab.clear_streaming_thought();
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Recommendation(recommendations),
            end_pending: true,
        };

        // Same handoff as `turn_surface_recommendation`: a fresh Send card
        // is now selectable, pin the chip onto its target pane.
        let target_tab = self.tab_for_session(session_id);
        self.recompute_chip_override(&target_tab);
    }

    /// Surface an Auto error handling Explain answer as a chat turn and
    /// bottom-bar Review indicator.
    fn turn_surface_explain(&mut self, session_id: &str, phase_name: &str) {
        // Defensive: only Auto error handling turns surface an explain answer here.
        let prompt = self.session_tab(session_id).turn.prompt();
        let Some(prompt) = prompt.filter(|prompt| prompt.auto_error_handling.is_some()) else {
            return;
        };
        // A manual `/fix` may have no concrete failing pane: surface the
        // explanation, but skip the bottom-bar /
        // result-pane side effects below.
        let bar_pane = prompt.context.target_pane_id().map(str::to_string);
        let response_chars = self
            .session_tab(session_id)
            .active_agent_text()
            .chars()
            .count();
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!("pane={bar_pane:?} chars={response_chars}"),
        );

        let turn_prompt_label = t!("chat.auto_error_handling_prompt_label").into_owned();
        {
            let tab = self.session_tab_mut(session_id);
            let details = tab.take_current_turn_details();
            // Auto-expand the auto-diagnosed-error turn: when the user
            // opens the Review result they came here specifically to
            // read the explanation, so showing the collapsed preview
            // would force a second click.
            tab.completed_turns.push(CompletedTurn {
                prompt: turn_prompt_label,
                details,
                expanded: true,
                trailing_marker: None,
            });
            tab.scroll_to_bottom();
        }

        let target_tab = self.tab_for_session(session_id);
        // Explanation lives in the chat above; mark the tab as having a
        // result pending review and surface the bar (Review when the pane
        // is closed, Idle when already open).
        if let Some(pane_id) = bar_pane.as_ref() {
            {
                let tab = self.session_tab_mut(session_id);
                tab.auto_error_handling.result_pane_id = Some(pane_id.clone());
                tab.auto_error_handling.pane_id = None;
                tab.auto_error_handling.started_at = None;
            }
            self.update_auto_error_handling_review_state(&target_tab, pane_id);
        }

        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.rec_scroll.reset();
        tab.activity_frame = 0;
        tab.clear_streaming_thought();
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::ChatTurn,
            end_pending: true,
        };
    }

    /// Flip `end_pending=false` after a final-path surface. Mirrors the
    /// `prompt_complete` log used by the eager path.
    fn turn_release_end_pending(&mut self, session_id: &str) {
        let tab = self.session_tab_mut(session_id);
        if let TurnState::Surfaced {
            end_pending,
            prompt,
            ..
        } = &mut tab.turn
        {
            if *end_pending {
                *end_pending = false;
                let prompt_id = prompt.id;
                let submitted_at = prompt.submitted_at_unix_s;
                prompt_timing_log(prompt_id, submitted_at, "prompt_complete", "via=end_only");
            }
        }
    }
}

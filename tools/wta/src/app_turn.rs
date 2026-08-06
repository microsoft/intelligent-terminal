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
// end-of-turn, Enter on a card, Esc / Ctrl+C cancel, autofix trigger — goes
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
    /// autofix path so a failure in a background tab installs the turn on
    /// that tab even when its ACP session hasn't been created yet (the ACP
    /// layer lazy-creates one when the prompt is dispatched).
    pub fn turn_submit_prompt_for_tab(&mut self, tab_id: &str, prompt: SubmittedPrompt) {
        prompt_timing_log(
            prompt.id,
            prompt.submitted_at_unix_s,
            "prompt_received",
            &format!(
                "autofix={} text_chars={}",
                prompt.autofix.is_some(),
                prompt.text.chars().count()
            ),
        );
        let is_autofix = prompt.autofix.is_some();
        let user_text = prompt.text.clone();
        let tab = self.tab_mut(tab_id);
        // Per Decision #3, every Idle→Submitted transition explicitly clears
        // these orthogonal fields rather than relying on side effects from a
        // grab-bag helper.
        tab.messages.clear();
        tab.tool_calls.clear();
        // Dropping any in-flight responders signals Cancelled back to
        // the agent — appropriate when the user starts a new turn.
        tab.permission.clear();
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
        // Autofix prompts are synthesized by the system; they don't render
        // as a User bubble (the user already sees the error line in the
        // failing pane).
        if !is_autofix {
            tab.messages.push(ChatMessage::User(user_text));
        }
        tab.scroll_to_bottom();
        tab.activity_frame = 0;
        tab.timing_note = None;
        tab.turn = TurnState::Submitted(prompt);

        // Submitting a new prompt dismisses any prior leftover card (the
        // `selected_recommendation = 0` + turn reset above). If the helper
        // had pinned the chip onto that card's pane, release it now so the
        // chip falls back to source-of-agent while the new turn is in
        // flight. Note: this only matters for the new-turn case; the
        // freshly-submitted autofix path overrides chip via the eventual
        // `turn_surface_*` callback once recommendations arrive.
        let owned_tab = tab_id.to_string();
        self.recompute_chip_override(&owned_tab);
    }

    /// Observe a streamed chunk. Thought chunks only advance the state
    /// (Submitted→Streaming with empty buffer); message chunks append to the
    /// streaming buffer. Returns true if the buffer changed (so the caller
    /// can decide whether to attempt an eager surface).
    pub fn turn_observe_chunk(&mut self, session_id: &str, kind: ChunkKind, text: &str) -> bool {
        // Stale-autofix check: if the chunk belongs to an autofix turn whose
        // generation no longer matches the tab's counter, drop it.
        let tab = self.session_tab_mut(session_id);
        let current_gen = tab.autofix.generation;
        if let Some(gen) = tab.turn.autofix_generation() {
            if gen != current_gen {
                tracing::debug!(
                    target: "autofix",
                    inflight_gen = gen,
                    current_gen,
                    "dropping stale autofix chunk",
                );
                return false;
            }
        }

        match (&mut tab.turn, kind) {
            // First message chunk: transition Submitted → Streaming.
            (TurnState::Submitted(_), ChunkKind::Message) => {
                let TurnState::Submitted(prompt) =
                    std::mem::replace(&mut tab.turn, TurnState::Idle)
                else {
                    unreachable!();
                };
                tab.turn = TurnState::Streaming {
                    prompt,
                    buf: text.to_string(),
                };
                // New turn: restart the typewriter reveal from the top.
                tab.reveal_chars = 0;
                true
            }
            // Thought chunk while Submitted: enter Streaming with empty buf.
            (TurnState::Submitted(_), ChunkKind::Thought) => {
                let TurnState::Submitted(prompt) =
                    std::mem::replace(&mut tab.turn, TurnState::Idle)
                else {
                    unreachable!();
                };
                tab.turn = TurnState::Streaming {
                    prompt,
                    buf: String::new(),
                };
                tab.reveal_chars = 0;
                false
            }
            // Streaming → Streaming, append message chunks only.
            (TurnState::Streaming { buf, .. }, ChunkKind::Message) => {
                buf.push_str(text);
                true
            }
            // Thought chunks during Streaming: no buffer change.
            (TurnState::Streaming { .. }, ChunkKind::Thought) => false,
            // A direct proposal may complete before the agent emits its final
            // message chunks. Keep those chunks visible alongside the card.
            (TurnState::Surfaced { .. }, ChunkKind::Message)
                if tab.active_direct_proposal_id.is_some() =>
            {
                match tab.messages.last_mut() {
                    Some(ChatMessage::Agent(existing)) => existing.push_str(text),
                    _ => tab.messages.push(ChatMessage::Agent(text.to_string())),
                }
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
            build_recommendation_set, parse_mcp_proposal_payload, parse_proposal_payload,
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

        let is_autofix = self.session_tab(session_id).turn.is_autofix();
        if is_autofix {
            let turn_generation = self.session_tab(session_id).turn.autofix_generation();
            let current_generation = self.session_tab(session_id).autofix.generation;
            if turn_generation != Some(current_generation) {
                return DirectProposalEvaluation::Stale("autofix turn was superseded".to_string());
            }
        }

        let wire = match source {
            crate::agent_tools::action_proposal::pipe::ProposalPayloadSource::Cli => {
                parse_proposal_payload(payload.as_bytes())
            }
            crate::agent_tools::action_proposal::pipe::ProposalPayloadSource::Mcp => {
                parse_mcp_proposal_payload(payload.as_bytes(), is_autofix)
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
            is_autofix,
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
            is_autofix,
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
        if pending.is_autofix {
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
    /// 1. Stale-autofix discard (newer trigger or Esc cancelled this turn).
    /// 2. A direct proposal already surfaced — release the UI gate.
    /// 3. `Submitted` with no chunks — model returned nothing.
    /// 4. `Streaming` with a buffer — commit it as assistant text.
    pub fn turn_close(&mut self, session_id: &str) {
        // (1) Stale-autofix discard.
        let current_gen = self.session_tab(session_id).autofix.generation;
        if let Some(gen) = self.session_tab(session_id).turn.autofix_generation() {
            if gen != current_gen {
                tracing::info!(
                    target: "autofix",
                    inflight_gen = gen,
                    current_gen,
                    "discarding stale autofix turn at close",
                );
                self.turn_clear_agent_activity(session_id);
                self.session_tab_mut(session_id).turn = TurnState::Idle;
                return;
            }
        }

        // (2) A direct proposal already surfaced.
        if let TurnState::Surfaced {
            end_pending: true, ..
        } = &self.session_tab(session_id).turn
        {
            self.turn_commit_trailing_direct_proposal_details(session_id);
            self.turn_release_end_pending_logged(session_id, "via=direct+end");
            self.turn_clear_agent_activity(session_id);
            return;
        }

        // (3) Submitted, no chunks. For autofix this would leave the bar
        //     stuck in Pending; clear it explicitly.
        let (buf, is_autofix) = match &self.session_tab(session_id).turn {
            TurnState::Streaming { buf, prompt } => (buf.clone(), prompt.autofix.is_some()),
            TurnState::Submitted(_) => {
                self.turn_close_no_chunks(session_id);
                return;
            }
            // Idle / already-surfaced+end_done — nothing to do.
            _ => return,
        };

        // (4) Typed action cards arrive only through the direct proposal
        // channel. Streamed assistant content is always prose.
        if is_autofix {
            self.turn_close_finalize_autofix_text(session_id, &buf);
        } else {
            self.turn_close_finalize_chat(session_id, buf);
        }
        self.turn_clear_agent_activity(session_id);
    }

    /// Path (3): close a turn that received `AgentMessageEnd` with no
    /// streamed content. Emits `autofix_state_cleared` if it was an
    /// autofix turn so the bottom bar doesn't stick in Pending.
    fn turn_close_no_chunks(&mut self, session_id: &str) {
        let target_tab = self.tab_for_session(session_id);
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        // Empty `target_pane_id` (manual `/fix`) is not a real pane — filter
        // it out so an empty-response turn doesn't emit a bottom-bar event.
        let autofix_pane = prompt
            .autofix
            .as_ref()
            .and(prompt.context.target_pane_id().map(str::to_string));
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending: true,
        };
        if autofix_pane.is_some() {
            self.emit_autofix_state_cleared(&target_tab);
            let autofix = &mut self.session_tab_mut(session_id).autofix;
            autofix.pane_id = None;
            autofix.armed_at = None;
        }
        self.turn_release_end_pending(session_id);
        self.turn_clear_agent_activity(session_id);
    }

    fn turn_close_finalize_autofix_text(&mut self, session_id: &str, buf: &str) {
        if !buf.trim().is_empty() {
            self.turn_surface_explain(session_id, String::new(), buf.to_string(), "autofix_text");
                self.turn_release_end_pending(session_id);
            return;
            }

                let target_tab = self.tab_for_session(session_id);
                let pane_id = self.session_tab(session_id).autofix.pane_id.clone();
                if pane_id.is_some() {
                    self.emit_autofix_state_cleared(&target_tab);
                }
                let autofix = &mut self.session_tab_mut(session_id).autofix;
                autofix.pane_id = None;
                autofix.armed_at = None;
                let tab = self.session_tab_mut(session_id);
                let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let details = tab.current_turn_details();
                if !details.is_empty() {
                    tab.completed_turns.push(CompletedTurn {
                        prompt: t!("chat.autofix_prompt_label").into_owned(),
                        details,
                        expanded: true,
                        trailing_marker: None,
                    });
                }
                tab.messages.clear();
                tab.tool_calls.clear();
                tab.scroll_to_bottom();
                tab.turn = TurnState::Surfaced {
                    prompt,
                    outcome: TurnOutcome::Empty,
                    end_pending: false,
                };
            }

    fn turn_close_finalize_chat(&mut self, session_id: &str, buf: String) {
                self.log_selection_phase_for(
                    session_id,
            "assistant_text",
            &format!("response_chars={}", buf.chars().count()),
                );
                let tab = self.session_tab_mut(session_id);
                let prompt = tab.turn.prompt().cloned().expect("prompt set");
                let mut details = tab.current_turn_details();
                details.push(ChatMessage::Agent(buf));
                tab.completed_turns.push(CompletedTurn {
                    prompt: prompt.text.clone(),
                    details,
                    expanded: true,
                    trailing_marker: None,
                });
                tab.messages.clear();
                tab.tool_calls.clear();
                tab.scroll_to_bottom();
                tab.turn = TurnState::Surfaced {
                    prompt,
                    outcome: TurnOutcome::ChatTurn,
                    end_pending: true,
                };
                self.turn_release_end_pending(session_id);
            }

    fn turn_commit_trailing_direct_proposal_details(&mut self, session_id: &str) {
        let tab = self.session_tab_mut(session_id);
        let trailing = tab.current_turn_details();
        if let Some(completed) = tab.completed_turns.last_mut() {
            completed.details.extend(trailing);
        }
        tab.messages.clear();
        tab.tool_calls.clear();
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
            outcome: TurnOutcome::Recommendation(_),
            ..
        } = &tab.turn
        else {
            return;
        };
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
            .and_then(|p| p.autofix.as_ref())
            .is_some()
        {
            self.emit_autofix_state_cleared(&target_tab);
        }
        let autofix = &mut self.session_tab_mut(session_id).autofix;
        autofix.pane_id = None;
        autofix.armed_at = None;
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
        // Stamp the matching completed_turn (pushed during surface) with an
        // "executed" marker so chat history reflects the user's choice.
        if let Some(last) = tab.completed_turns.last_mut() {
            let marker = t!("chat.turn_executed", title = &executed_title).into_owned();
            last.trailing_marker = Some(marker);
        }
        // commit pending turn (in case eager surface staged one).
        tab.turn = TurnState::Surfaced {
            prompt,
            outcome: TurnOutcome::Empty,
            end_pending,
        };

        // Exiting Surfaced{Recommendation} — release any chip override the
        // card had pinned. The C++ side falls back to source-of-agent.
        let target_tab = self.tab_for_session(session_id);
        self.recompute_chip_override(&target_tab);
    }

    /// User pressed Esc — cancel the in-flight turn. Bumps
    /// `autofix_generation` so any chunks that arrive after this point are
    /// dropped by the stale-check in `turn_observe_chunk`.
    pub fn turn_cancel(&mut self, session_id: &str) {
        let direct_proposal_id = self
            .session_tab(session_id)
            .active_direct_proposal_id
            .clone();
        let target_tab = self.tab_for_session(session_id);
        let pane_id = {
            let tab = self.session_tab_mut(session_id);
            tab.autofix.generation = tab.autofix.generation.wrapping_add(1);
            tab.turn
                .prompt()
                .and_then(|prompt| {
                    prompt
                        .autofix
                        .as_ref()
                        .and(prompt.context.target_pane_id().map(str::to_string))
                })
                .or_else(|| tab.autofix.pane_id.clone())
        };
        if pane_id.is_some() {
            self.emit_autofix_state_cleared(&target_tab);
        }
        let tab = self.session_tab_mut(session_id);
        tab.autofix.armed_at = None;
        let canceled_marker = t!("chat.turn_canceled").into_owned();
        // Three paths into cancel:
        //   - Submitted / Streaming → commit a fresh completed_turn (prompt +
        //     whatever streamed + canceled marker) so the user always sees
        //     that this turn happened and that they cancelled it.
        //   - Surfaced{Recommendation}: turn_surface_* already pushed a
        //     completed_turn; just append the canceled marker to its details.
        //   - Other states (Idle / Surfaced{Empty / ChatTurn}) → no-op.
        let new_turn_data: Option<(String, Option<String>)> = match &tab.turn {
            TurnState::Submitted(prompt) => {
                let label = match prompt.autofix.as_ref() {
                    Some(_) => t!("chat.autofix_prompt_label").into_owned(),
                    None => prompt.text.clone(),
                };
                Some((label, None))
            }
            TurnState::Streaming { prompt, buf } => {
                let label = match prompt.autofix.as_ref() {
                    Some(_) => t!("chat.autofix_prompt_label").into_owned(),
                    None => prompt.text.clone(),
                };
                let visible = ui::chat::user_visible_stream_text(buf).map(|c| c.into_owned());
                Some((label, visible))
            }
            _ => None,
        };
        let annotate_card = matches!(
            &tab.turn,
            TurnState::Surfaced {
                outcome: TurnOutcome::Recommendation(_),
                ..
            }
        );
        if let Some((prompt_label, visible)) = new_turn_data {
            let mut details = tab.current_turn_details();
            if let Some(v) = visible {
                details.push(ChatMessage::Agent(v));
            }
            tab.completed_turns.push(CompletedTurn {
                prompt: prompt_label,
                details,
                expanded: true,
                trailing_marker: Some(canceled_marker),
            });
            tab.messages.clear();
            tab.tool_calls.clear();
            tab.scroll_to_bottom();
        } else if annotate_card {
            if let Some(last) = tab.completed_turns.last_mut() {
                last.trailing_marker = Some(canceled_marker);
            }
        }
        tab.autofix.pane_id = None;
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.rec_scroll.reset();
        tab.activity_frame = 0;
        tab.turn = TurnState::Idle;
        tab.pending_terminal_action_proposal = None;
        tab.active_direct_proposal_id = None;
        if let Some(proposal_id) = direct_proposal_id.as_deref() {
            self.proposal_channels.resolve_final(
                proposal_id,
                crate::agent_tools::action_proposal::channel::ProposalFinalStatus::Cancelled,
            );
        }

        // Esc on a Send card or in-flight autofix exits the chip-override
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
        let summary = format_recommendations_for_chat(&recommendations);
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
        let mut details = tab.current_turn_details();
        details.push(ChatMessage::Agent(summary));
        tab.completed_turns.push(CompletedTurn {
            prompt: prompt.text.clone(),
            details,
            expanded: true,
            trailing_marker: None,
        });
        tab.messages.clear();
        tab.tool_calls.clear();
        tab.scroll_to_bottom();
        tab.selected_recommendation = rec_idx;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.rec_scroll.reset();
        tab.selection_visible_pending = true;
        tab.selected_completed_turn_idx = None;
        tab.activity_frame = 0;
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

    /// Surface an autofix Fix recommendation as an Armed card.
    fn turn_surface_fix(
        &mut self,
        session_id: &str,
        recommendations: RecommendationSet,
        phase_name: &str,
    ) {
        // Defensive: only autofix turns surface a fix card here.
        let prompt = self.session_tab(session_id).turn.prompt();
        let Some(prompt) = prompt.filter(|prompt| prompt.autofix.is_some()) else {
            return;
        };
        // A manual `/fix` may have no concrete failing pane. Still surface the
        // card below, but skip the
        // bottom-bar / suggested-pane side effects — they key off a real
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
        // inside the pane — autofix no longer auto-executes.
        if let Some(pane_id) = bar_pane.as_ref() {
            {
                let autofix = &mut self.tab_mut(&target_tab).autofix;
                autofix.suggested_pane_id = Some(pane_id.clone());
                autofix.pane_id = None;
                autofix.armed_at = None;
            }
            self.emit_autofix_state_result(&target_tab, pane_id);
        }
        let rec_idx = recommended_choice_index(&recommendations);
        let summary = format_recommendations_for_chat(&recommendations);
        let turn_prompt_label = t!("chat.autofix_prompt_label").into_owned();
        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        let mut details = tab.current_turn_details();
        details.push(ChatMessage::Agent(summary));
        tab.completed_turns.push(CompletedTurn {
            prompt: turn_prompt_label,
            details,
            expanded: true,
            trailing_marker: None,
        });
        tab.messages.clear();
        tab.tool_calls.clear();
        tab.scroll_to_bottom();
        tab.selected_recommendation = rec_idx;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.selection_visible_pending = true;
        tab.activity_frame = 0;
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

    /// Surface an autofix Explain answer as a chat turn + bottom-bar
    /// Suggested indicator.
    fn turn_surface_explain(
        &mut self,
        session_id: &str,
        title: String,
        explanation: String,
        phase_name: &str,
    ) {
        // Defensive: only autofix turns surface an explain answer here.
        let prompt = self.session_tab(session_id).turn.prompt();
        let Some(prompt) = prompt.filter(|prompt| prompt.autofix.is_some()) else {
            return;
        };
        // A manual `/fix` may have no concrete failing pane: surface the
        // explanation, but skip the bottom-bar /
        // suggested-pane side effects below.
        let bar_pane = prompt.context.target_pane_id().map(str::to_string);
        self.log_selection_phase_for(
            session_id,
            phase_name,
            &format!(
                "pane={bar_pane:?} title={title:?} chars={}",
                explanation.chars().count()
            ),
        );

        let turn_prompt_label = t!("chat.autofix_prompt_label").into_owned();
        {
            let tab = self.session_tab_mut(session_id);
            let mut details = tab.current_turn_details();
            details.push(ChatMessage::Agent(explanation));
            // Auto-expand the auto-diagnosed-error turn: when the user
            // clicks the Suggested pill they came here specifically to
            // read the explanation, so showing the collapsed preview
            // would force a second click.
            tab.completed_turns.push(CompletedTurn {
                prompt: turn_prompt_label,
                details,
                expanded: true,
                trailing_marker: None,
            });
            tab.messages.clear();
            tab.tool_calls.clear();
            tab.scroll_to_bottom();
        }

        let target_tab = self.tab_for_session(session_id);
        // Explanation lives in the chat above; mark the tab as having a
        // result pending review and surface the bar (Review when the pane
        // is closed, Idle when already open).
        if let Some(pane_id) = bar_pane.as_ref() {
            {
                let tab = self.session_tab_mut(session_id);
                tab.autofix.suggested_pane_id = Some(pane_id.clone());
                tab.autofix.pane_id = None;
                tab.autofix.armed_at = None;
            }
            self.emit_autofix_state_result(&target_tab, pane_id);
        }

        let tab = self.session_tab_mut(session_id);
        let prompt = tab.turn.prompt().cloned().expect("prompt set");
        tab.selected_recommendation = 0;
        tab.selected_button = 0;
        tab.recommendation_focus = RecommendationFocus::Button;
        tab.rec_scroll.reset();
        tab.activity_frame = 0;
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

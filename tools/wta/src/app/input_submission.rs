use super::*;
use crate::protocol::acp::client::AutofixTextKind;

#[derive(Debug, Clone)]
pub(super) struct AutofixInputMetadata {
    pub(super) text_kind: AutofixTextKind,
    pub(super) context: AutofixContext,
    pub(super) arm_trigger_echo: bool,
}

/// ACP-bound input prepared before IDs and timestamps are allocated.
#[derive(Debug, Clone)]
pub(super) struct InputEnvelope {
    pub(super) text: String,
    pub(super) display_text: String,
    pub(super) images: Vec<crate::clipboard_image::PastedImage>,
    pub(super) pane_context: PaneContext,
    pub(super) turn_context: TurnContext,
    pub(super) agent_command: bool,
    pub(super) autofix: Option<AutofixInputMetadata>,
    pub(super) is_byok: bool,
    pub(super) agent_id: String,
}

impl InputEnvelope {
    pub(super) fn autofix_target_pane(&self) -> Option<&str> {
        self.autofix
            .as_ref()
            .and(self.turn_context.target_pane_id())
    }

    pub(super) fn autofix_generation(&self) -> Option<u64> {
        self.autofix
            .as_ref()
            .map(|metadata| metadata.context.generation)
    }
}

impl App {
    pub(super) fn reserve_autofix_generation(&self, tab_id: &str) -> u64 {
        self.tab_sessions
            .get(tab_id)
            .map(|tab| {
                tab.pending_inputs
                    .iter()
                    .rev()
                    .find_map(InputEnvelope::autofix_generation)
                    .unwrap_or(tab.autofix.generation)
            })
            .unwrap_or_default()
            .wrapping_add(1)
    }

    /// Apply downstream behavior only after the generic gate selects the input.
    pub(super) fn dispatch_input(&mut self, tab_id: &str, envelope: InputEnvelope, queued: bool) {
        let InputEnvelope {
            text,
            display_text,
            images,
            mut pane_context,
            turn_context,
            agent_command,
            autofix,
            is_byok,
            agent_id,
        } = envelope;
        pane_context.tab_id = Some(tab_id.to_string());
        let failure_autofix = autofix
            .as_ref()
            .filter(|metadata| metadata.text_kind == AutofixTextKind::FailureSummary)
            .and_then(|_| {
                turn_context
                    .target_pane_id()
                    .map(|pane| (text.clone(), pane.to_string()))
            });

        if let Some(metadata) = autofix.as_ref() {
            let target_pane = turn_context.target_pane_id().map(str::to_string);
            let tab = self.tab_mut(tab_id);
            tab.autofix.generation = metadata.context.generation;
            tab.autofix.suggested_pane_id = None;
            if metadata.text_kind == AutofixTextKind::FailureSummary {
                tab.autofix.pane_id = target_pane.clone();
                tab.autofix.armed_at = Some(std::time::Instant::now());
                if metadata.arm_trigger_echo && !queued {
                    tab.autofix.trigger_echo_pane = target_pane;
                }
            }
        }

        let mut prompt = match autofix.as_ref().map(|metadata| metadata.text_kind) {
            Some(AutofixTextKind::UserRequest) => {
                PromptSubmission::new_autofix(text, Some(pane_context))
            }
            Some(AutofixTextKind::FailureSummary) => {
                PromptSubmission::new_autofix_failure(text, Some(pane_context))
            }
            None if agent_command => PromptSubmission::new_agent_command(text, Some(pane_context)),
            None => PromptSubmission::new(text, Some(pane_context)),
        };
        prompt = prompt
            .with_images(images)
            .with_byok(is_byok)
            .with_agent_id(agent_id);
        if autofix.is_none() {
            prompt_timing_log(
                prompt.id,
                prompt.submitted_at_unix_s,
                "ui_submit",
                &format!("preview={:?}", prompt.preview()),
            );
        }
        let submitted = SubmittedPrompt {
            id: prompt.id,
            text: display_text,
            submitted_at_unix_s: prompt.submitted_at_unix_s,
            context: turn_context,
            autofix: autofix.map(|metadata| metadata.context),
        };
        // Auto-drained queued prompts should not steal a manual reading position.
        // Restore the pre-submit scroll offset after the generic submit reset so
        // chat anchoring can keep the same surviving history row in view.
        let preserved_reading = queued.then(|| {
            self.tab_sessions.get(tab_id).and_then(|tab| {
                (tab.chat_scroll.offset > 0)
                    .then(|| (tab.chat_scroll.offset, tab.chat_reading_position))
            })
        });
        self.turn_submit_prompt_for_tab_with_cancellation(
            tab_id,
            submitted,
            prompt.cancellation_token(),
        );
        if let Some((offset, reading_position)) = preserved_reading.flatten() {
            let tab = self.tab_mut(tab_id);
            tab.chat_scroll.offset = offset;
            tab.chat_reading_position = reading_position;
        }
        let _ = self.prompt_tx.send(prompt);

        if let Some((summary, pane_id)) = failure_autofix {
            self.emit_autofix_state_pending(tab_id, &pane_id, &summary);
        }
    }
}

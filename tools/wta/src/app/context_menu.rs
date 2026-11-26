use super::*;
use crossterm::event::{MouseEvent, MouseEventKind};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const SNAPSHOT_LIFETIME: Duration = Duration::from_secs(60);

#[cfg(test)]
#[path = "context_menu_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct AgentContextMenu {
    epoch: Option<String>,
    sequence: u64,
    snapshot: Option<SelectionSnapshot>,
}

struct SelectionSnapshot {
    token: String,
    sequence: u64,
    text: Option<String>,
    input: bool,
    copied: bool,
    tab: String,
    session: Option<String>,
    mode: AppMode,
    view: View,
    created: Instant,
}

impl App {
    pub(super) fn enable_context_menu_bridge(&mut self, params: &Value) {
        if params.get("context_menu_bridge").and_then(Value::as_bool) != Some(true)
            || self.agent_paste_target_tab(params).is_none()
        {
            return;
        }

        let epoch = self
            .agent_context_menu
            .epoch
            .get_or_insert_with(|| uuid::Uuid::new_v4().to_string())
            .clone();
        self.publish_context_menu(json!({ "action": "ready", "epoch": epoch }));
    }

    fn publish_context_menu(&self, mut params: Value) {
        let (Some(window), Some(tab), Some(pane)) =
            (&self.window_id, &self.owner_tab_id, &self.pane_id)
        else {
            tracing::warn!(target: "agent_context_menu", "missing helper owner identity");
            return;
        };
        params["window_id"] = json!(window);
        params["tab_id"] = json!(tab);
        params["pane_id"] = json!(pane);
        crate::wt_protocol_events::send(
            json!({ "type": "event", "method": "agent_context_menu", "params": params })
                .to_string(),
        );
    }

    pub(super) fn invalidate_context_menu(&mut self) {
        self.agent_context_menu.snapshot = None;
    }

    pub(super) fn request_agent_context_menu(&mut self, mouse: MouseEvent) -> bool {
        let Some(epoch) = self.agent_context_menu.epoch.clone() else {
            return false;
        };
        self.cancel_completed_turn_click();
        self.agent_context_menu.sequence += 1;
        let sequence = self.agent_context_menu.sequence;
        // VT RightDown follows the drag in the same input stream; a host-side
        // selection query could overtake that drag over the protocol channel.
        let input = self.chat_input_has_edit_focus()
            && self.current_tab().input_all_selected
            && !self.current_tab().input.is_empty();
        let text = if input {
            Some(self.current_tab().input.clone())
        } else {
            self.text_selection.selected_text()
        };
        let token = uuid::Uuid::new_v4().to_string();
        let has_selection = text.is_some();
        self.agent_context_menu.snapshot = Some(SelectionSnapshot {
            token: token.clone(),
            sequence,
            text,
            input,
            copied: self.text_selection.was_copied(),
            tab: self.active_tab_key().to_string(),
            session: self.current_tab().session_id.clone(),
            mode: self.mode,
            view: self.current_tab().current_view,
            created: Instant::now(),
        });
        self.publish_context_menu(json!({
            "action": "show", "epoch": epoch, "sequence": sequence, "token": token,
            "has_selection": has_selection, "column": mouse.column, "row": mouse.row,
        }));
        true
    }

    pub(super) fn handle_agent_context_menu_action(&mut self, params: &Value) {
        self.handle_agent_context_menu_action_with(params, crate::win32::copy_text_to_clipboard);
    }

    pub(super) fn handle_agent_context_menu_action_with(
        &mut self,
        params: &Value,
        copy: impl FnOnce(&str) -> std::io::Result<()>,
    ) {
        if self.agent_paste_target_tab(params).is_none()
            || params.get("epoch").and_then(Value::as_str)
                != self.agent_context_menu.epoch.as_deref()
        {
            tracing::debug!(target: "agent_context_menu", "ignoring foreign menu action");
            return;
        }
        let valid = self
            .agent_context_menu
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                params.get("sequence").and_then(Value::as_u64) == Some(snapshot.sequence)
                    && params.get("token").and_then(Value::as_str) == Some(&snapshot.token)
                    && snapshot.created.elapsed() < SNAPSHOT_LIFETIME
                    && self.current_tab().pane_open
                    && self.active_tab_key() == snapshot.tab
                    && self.current_tab().session_id == snapshot.session
                    && self.mode == snapshot.mode
                    && self.current_tab().current_view == snapshot.view
            });
        if !valid {
            tracing::warn!(target: "agent_context_menu", "rejecting expired or stale menu action");
            return;
        }
        let action = params.get("action").and_then(Value::as_str).unwrap_or("");
        if !matches!(action, "copy" | "direct" | "dismiss") {
            tracing::warn!(target: "agent_context_menu", action, "invalid menu action");
            return;
        }
        let Some(snapshot) = self.agent_context_menu.snapshot.take() else {
            return;
        };
        if action == "dismiss" {
            return;
        }
        let copy_on_select = self.current_tab().copy_on_select;
        let should_copy = snapshot.text.is_some()
            && (action == "copy" || snapshot.input || !copy_on_select || !snapshot.copied);
        let mut success = action != "copy" || snapshot.text.is_some();
        if !success {
            tracing::warn!(target: "agent_context_menu", "Copy requested without selected text");
        }
        if should_copy {
            if let Some(text) = &snapshot.text {
                if snapshot.input {
                    self.current_tab_mut().break_input_undo_group();
                    self.close_pane_armed_at = None;
                }
                match copy(text) {
                    Ok(()) => {
                        self.close_pane_armed_at = None;
                        if !snapshot.input {
                            self.text_selection.clear();
                        }
                        self.transient_hint = Some((
                            t!("system.selection_copied").into_owned(),
                            Instant::now() + SELECTION_COPIED_HINT_WINDOW,
                        ));
                    }
                    Err(error) => {
                        success = false;
                        self.transient_hint = None;
                        tracing::warn!(target: "clipboard", %error, "Agent menu Copy failed");
                    }
                }
            }
        }
        if success && action == "direct" && (copy_on_select || !should_copy) {
            if copy_on_select {
                self.text_selection.clear();
            }
            if let Some(request) = self.default_paste_request_for_current_tab() {
                self.current_tab_mut().clear_completed_turn_selection();
                crate::wt_protocol_events::send(request);
            } else {
                tracing::debug!(target: "agent_paste", "Agent menu Default Paste request was gated");
            }
        }
        self.publish_context_menu(json!({
            "action": "complete", "epoch": self.agent_context_menu.epoch,
            "sequence": snapshot.sequence, "token": snapshot.token, "success": success,
        }));
    }

    pub(super) fn context_menu_input_invalidates(&self, event: &AppEvent) -> bool {
        match event {
            AppEvent::Key(_) | AppEvent::Resize(_, _) => true,
            AppEvent::Mouse(mouse) => !matches!(
                mouse.kind,
                MouseEventKind::Moved | MouseEventKind::Down(crossterm::event::MouseButton::Right)
            ),
            AppEvent::WtEvent { method, params, .. } => {
                if params
                    .get("window_id")
                    .and_then(Value::as_str)
                    .zip(self.window_id.as_deref())
                    .is_some_and(|(target, owner)| target != owner)
                    || (method == "set_agent_state"
                        && params
                            .get("tab_id")
                            .and_then(Value::as_str)
                            .zip(self.owner_tab_id.as_deref())
                            .is_some_and(|(target, owner)| target != owner))
                {
                    return false;
                }
                matches!(
                    method.as_str(),
                    "tab_changed" | "tab_renamed" | "load_session"
                ) || (method == "set_agent_state"
                    && (params.get("pane_open").and_then(Value::as_bool) == Some(false)
                        || params
                            .get("view")
                            .and_then(Value::as_str)
                            .is_some_and(|view| view != "chat")))
            }
            _ => false,
        }
    }
}

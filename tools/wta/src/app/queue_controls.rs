use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueControl {
    Recall,
    SendRemaining,
    Discard,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter(app: &mut App, text: &str) {
        app.current_tab_mut().replace_input(text.into());
        app.handle_event(AppEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
    }

    fn queued_app(paused: bool) -> (App, mpsc::UnboundedReceiver<PromptSubmission>) {
        let mut app = crate::app::tests::test_app();
        app.state = ConnectionState::Connected;
        app.show_welcome_hint = false;
        let rx = app.test_prompt_rx.take().unwrap();
        app.current_tab_mut().config_pending_id = Some("hold".into());
        enter(&mut app, "first waiting");
        enter(&mut app, "last waiting");
        if paused {
            app.current_tab_mut().pause_pending_prompts();
        }
        app.current_tab_mut().config_pending_id = None;
        (app, rx)
    }

    fn key(app: &mut App, code: KeyCode) {
        app.handle_event(AppEvent::Key(KeyEvent::new(code, KeyModifiers::ALT)));
    }

    fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16) {
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }));
    }

    fn hit(app: &mut App, action: QueueControl) -> QueueControlHit {
        crate::app::tests::render_to_buffer(app, 110, 22);
        app.queue_control_hits
            .iter()
            .find(|hit| hit.action == action)
            .unwrap()
            .clone()
    }

    #[test]
    fn alt_r_recalls_last_request_without_resending_the_original() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = queued_app(true);
        key(&mut app, KeyCode::Char('r'));
        assert_eq!(app.current_tab().input, "last waiting");
        assert_eq!(app.pending_input_previews().count(), 1);
        assert!(rx.try_recv().is_err());
        key(&mut app, KeyCode::Char('R'));
        assert_eq!(app.current_tab().input, "last waiting");
        assert_eq!(app.pending_input_previews().count(), 1);
        assert!(
            matches!(app.current_tab().messages.last(), Some(ChatMessage::Notice { text, .. })
            if text == t!("queue.draft_busy").as_ref())
        );
    }

    #[test]
    fn alt_s_resumes_and_alt_d_discards_without_cancelling_active_turn() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = queued_app(true);
        key(&mut app, KeyCode::Char('s'));
        let active = rx.try_recv().unwrap();
        assert_eq!(active.text, "first waiting");
        assert!(!app.pending_queue_paused());
        assert!(rx.try_recv().is_err());
        app.current_tab_mut().replace_input("keep draft".into());
        key(&mut app, KeyCode::Char('d'));
        assert!(app.pending_input_previews().next().is_none());
        assert_eq!(app.current_tab().turn.prompt_id(), Some(active.id));
        assert!(!active.cancellation_token().is_cancelled());
        assert_eq!(app.current_tab().input, "keep draft");
    }

    #[test]
    fn queue_buttons_require_matching_mouse_press_and_release() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = queued_app(true);
        let button = hit(&mut app, QueueControl::SendRemaining);
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            button.area.x,
            button.area.y,
        );
        assert!(rx.try_recv().is_err());
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            button.area.x,
            button.area.y,
        );
        assert!(rx.try_recv().is_err());
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            button.area.x,
            button.area.y,
        );
        assert_eq!(rx.try_recv().unwrap().text, "first waiting");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queue_clicks_do_not_survive_drag_resize_or_queue_replacement() {
        let _locale = crate::test_support::lock_locale();
        for change in ["drag", "resize", "queue", "tab", "modal", "focus"] {
            let (mut app, mut rx) = queued_app(true);
            let button = hit(&mut app, QueueControl::Discard);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                button.area.x,
                button.area.y,
            );
            match change {
                "drag" => mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    button.area.x,
                    button.area.y,
                ),
                "resize" => app.handle_event(AppEvent::Resize(80, 18)),
                "queue" => enter(&mut app, "new request"),
                "tab" => {
                    app.tab_mut("other-tab");
                    app.tab_id = Some("other-tab".into());
                }
                "modal" => app.help_overlay_visible = true,
                _ => app.handle_event(AppEvent::FocusChanged(false)),
            }
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                button.area.x,
                button.area.y,
            );
            let original = &app.tab_sessions[DEFAULT_TAB_ID];
            assert!(original.prompt_queue.entries.len() >= 2, "{change}");
            assert!(rx.try_recv().is_err());
        }
    }

    #[test]
    fn paused_queue_controls_respect_connection_and_modal_gates() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = queued_app(true);
        app.state = ConnectionState::Failed("offline".into());
        assert!(!hit(&mut app, QueueControl::SendRemaining).enabled);
        key(&mut app, KeyCode::Char('s'));
        assert!(app.pending_queue_paused());
        assert!(rx.try_recv().is_err());
        app.help_overlay_visible = true;
        key(&mut app, KeyCode::Char('d'));
        key(&mut app, KeyCode::Char('r'));
        assert_eq!(app.pending_input_previews().count(), 2);
        assert!(app.current_tab().input.is_empty());
        assert!(hit(&mut app, QueueControl::Discard).enabled == false);
    }

    #[test]
    fn controls_are_contextual_and_discard_is_available_while_running() {
        let _locale = crate::test_support::lock_locale();
        let mut idle = crate::app::tests::test_app();
        let text = crate::app::tests::render_to_text(&mut idle, 90, 9);
        assert!(idle.queue_control_hits.is_empty());
        assert!(!text.contains("Alt+R") && !text.contains("Alt+S") && !text.contains("Alt+D"));

        let (mut app, mut rx) = queued_app(false);
        app.dispatch_prompt_queues();
        let active = rx.try_recv().unwrap();
        app.current_tab_mut().replace_input("keep draft".into());
        let button = hit(&mut app, QueueControl::Discard);
        assert!(button.enabled);
        assert!(app
            .queue_control_hits
            .iter()
            .any(|hit| hit.action == QueueControl::Recall));
        assert!(!app
            .queue_control_hits
            .iter()
            .any(|hit| hit.action == QueueControl::SendRemaining));
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            button.area.x,
            button.area.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            button.area.x,
            button.area.y,
        );
        assert!(app.pending_input_previews().next().is_none());
        assert_eq!(app.current_tab().input, "keep draft");
        assert_eq!(app.current_tab().turn.prompt_id(), Some(active.id));
        assert!(!active.cancellation_token().is_cancelled());
        assert!(rx.try_recv().is_err());
        crate::app::tests::render_to_buffer(&mut app, 90, 9);
        assert!(app.queue_control_hits.is_empty());
    }

    #[test]
    fn compact_queue_keeps_all_controls_before_previews() {
        // Isolate the extended locale matrix from legacy render tests that do
        // not all acquire the shared locale lock.
        const CHILD: &str = "WTA_QUEUE_LOCALE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "app::queue_controls::tests::compact_queue_keeps_all_controls_before_previews",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .expect("run isolated queue locale matrix");
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let _locale = crate::test_support::lock_locale();
        for locale in ["en-US", "zh-CN", "de-DE"] {
            rust_i18n::set_locale(locale);
            for width in [24, 48, 90] {
                for height in [6, 7, 9] {
                    let (mut app, mut rx) = queued_app(true);
                    app.current_tab_mut().replace_input("draft\n".repeat(10));
                    let text = crate::app::tests::render_to_text(&mut app, width, height);
                    for action in [
                        QueueControl::SendRemaining,
                        QueueControl::Recall,
                        QueueControl::Discard,
                    ] {
                        assert!(
                            app.queue_control_hits
                                .iter()
                                .any(|hit| hit.action == action),
                            "{locale} {width}x{height}: {action:?}\n{text}"
                        );
                    }
                    assert!(app.input_dialog_area.unwrap().height >= 3);
                    assert!(rx.try_recv().is_err());
                }
            }
        }
    }

    #[test]
    fn paused_queue_explains_automatic_sending_and_compact_buttons_are_clickable() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = queued_app(true);
        let text = crate::app::tests::render_to_text(&mut app, 90, 9);
        assert!(
            text.contains(t!("queue.paused_header", count = 2).as_ref()),
            "{text}"
        );
        let emergency = crate::app::tests::render_to_text(&mut app, 90, 5);
        assert!(
            emergency.contains(t!("queue.paused_header", count = 2).as_ref()),
            "{emergency}"
        );
        assert_eq!(app.queue_control_hits.len(), 3, "{emergency}");
        crate::app::tests::render_to_buffer(&mut app, 24, 6);
        let button = app
            .queue_control_hits
            .iter()
            .find(|hit| hit.action == QueueControl::SendRemaining)
            .unwrap()
            .clone();
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            button.area.x,
            button.area.y,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            button.area.x,
            button.area.y,
        );
        assert_eq!(rx.try_recv().unwrap().text, "first waiting");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn queue_controls_fit_narrow_layouts_and_clear_when_hidden() {
        let _locale = crate::test_support::lock_locale();
        for width in [1, 12, 24, 48, 100] {
            for height in [3, 7, 14, 24] {
                let (mut app, _) = queued_app(true);
                let buffer = crate::app::tests::render_to_buffer(&mut app, width, height);
                for hit in &app.queue_control_hits {
                    assert!(hit.area.right() <= buffer.area.right());
                    assert!(hit.area.bottom() <= buffer.area.bottom());
                    assert!(hit.area.bottom() <= app.input_dialog_area.unwrap().y);
                }
                app.current_tab_mut().current_view = View::Agents;
                crate::app::tests::render_to_buffer(&mut app, width, height);
                assert!(app.queue_control_hits.is_empty());
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueControlHit {
    pub area: Rect,
    pub action: QueueControl,
    pub enabled: bool,
    pub tab_id: String,
    pub requests: Vec<u64>,
}

impl App {
    pub(crate) fn register_queue_control(
        &mut self,
        area: Rect,
        action: QueueControl,
        enabled: bool,
    ) {
        self.queue_control_hits.push(QueueControlHit {
            area,
            action,
            enabled,
            tab_id: self.active_tab_key().to_owned(),
            requests: self
                .current_tab()
                .prompt_queue
                .entries
                .iter()
                .map(|entry| entry.submission.id)
                .collect(),
        });
    }

    pub(crate) fn pending_queue_controls_available(&self) -> bool {
        self.mode == AppMode::Chat
            && self.current_tab().current_view == View::Chat
            && !self.help_overlay_visible
            && !self.command_popup_visible()
            && self.model_popup_state().is_none()
            && self.config_popup_state().is_none()
            && self.agent_popup_state().is_none()
            && self.current_tab().permission.is_empty()
            && self.current_tab().user_input.is_empty()
            && !self.current_tab().paste_pending
            && self
                .owner_tab_id
                .as_deref()
                .is_none_or(|owner| owner == self.active_tab_key())
    }

    fn invoke_queue_control(&mut self, action: QueueControl) {
        match action {
            QueueControl::Recall => self.recall_last_pending_input(),
            QueueControl::SendRemaining => self.resume_pending_inputs(),
            QueueControl::Discard => self.discard_pending_inputs(),
        }
    }

    pub(super) fn handle_pending_queue_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers != KeyModifiers::ALT {
            return false;
        }
        let action = match key.code {
            KeyCode::Char('r' | 'R') => QueueControl::Recall,
            KeyCode::Char('s' | 'S') => QueueControl::SendRemaining,
            KeyCode::Char('d' | 'D') => QueueControl::Discard,
            _ => return false,
        };
        if self.pending_queue_controls_available() {
            self.invoke_queue_control(action);
        }
        true
    }

    pub(super) fn handle_pending_queue_mouse(&mut self, mouse: MouseEvent) -> bool {
        let available = self.pending_queue_controls_available();
        let hit = available
            .then(|| {
                self.queue_control_hits.iter().find(|hit| {
                    hit.area.contains(Position::new(mouse.column, mouse.row))
                        && hit.tab_id == self.active_tab_key()
                        && hit.requests.iter().copied().eq(self
                            .current_tab()
                            .prompt_queue
                            .entries
                            .iter()
                            .map(|entry| entry.submission.id))
                })
            })
            .flatten()
            .cloned();
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.pressed_queue_control = None;
                if let Some(hit) = hit {
                    self.cancel_completed_turn_click();
                    self.text_selection.clear();
                    self.pressed_queue_control = Some((hit, false));
                    return true;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((_, dragged)) = self.pressed_queue_control.as_mut() {
                    *dragged = true;
                    return true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some((pressed, dragged)) = self.pressed_queue_control.take() {
                    if !dragged && pressed.enabled && hit.as_ref() == Some(&pressed) {
                        self.invoke_queue_control(pressed.action);
                    }
                    return true;
                }
            }
            _ => {}
        }
        false
    }
}

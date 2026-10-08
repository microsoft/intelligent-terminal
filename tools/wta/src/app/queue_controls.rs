use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueControl {
    Recall,
    SendRemaining,
    Discard,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn queued_app(paused: bool) -> (App, mpsc::UnboundedReceiver<PromptSubmission>) {
        let mut app = crate::app::tests::test_app();
        app.state = ConnectionState::Connected;
        app.show_welcome_hint = false;
        let rx = app.test_prompt_rx.take().unwrap();
        app.current_tab_mut().config_pending_id = Some("hold".into());
        for text in ["first waiting", "last waiting"] {
            app.current_tab_mut().replace_input(text.into());
            app.handle_event(AppEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )));
        }
        if paused {
            app.current_tab_mut().pause_pending_prompts();
            app.current_tab_mut().config_pending_id = None;
        }
        (app, rx)
    }

    #[test]
    fn queue_shortcuts_are_not_exposed_in_count_only_ui() {
        let _locale = crate::test_support::lock_locale();
        for paused in [false, true] {
            let (mut app, mut rx) = queued_app(paused);
            for key in ['r', 'R', 's', 'S', 'd', 'D'] {
                app.handle_event(AppEvent::Key(KeyEvent::new(
                    KeyCode::Char(key),
                    KeyModifiers::ALT,
                )));
                assert_eq!(app.pending_input_count(), 2);
                assert_eq!(app.pending_queue_paused(), paused);
                assert!(app.current_tab().input.is_empty());
                assert!(rx.try_recv().is_err());
            }
        }
    }

    #[test]
    fn queue_count_is_one_dim_noninteractive_row_in_running_and_paused_states() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        for paused in [false, true] {
            for width in [24, 48, 90] {
                for height in [5, 6, 9, 22] {
                    let (mut app, mut rx) = queued_app(paused);
                    let buffer = crate::app::tests::render_to_buffer(&mut app, width, height);
                    let y = app.input_dialog_area.unwrap().y - 1;
                    let label = "2 messages queued";
                    let row: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                    assert_eq!(row.trim(), label);
                    for x in 1..=label.len() as u16 {
                        assert_eq!(buffer[(x, y)].fg, ratatui::style::Color::DarkGray);
                        assert!(!buffer[(x, y)]
                            .modifier
                            .contains(ratatui::style::Modifier::BOLD));
                    }
                    assert!(app.queue_control_hits.is_empty());
                    for kind in [
                        MouseEventKind::Down(MouseButton::Left),
                        MouseEventKind::Up(MouseButton::Left),
                    ] {
                        app.handle_event(AppEvent::Mouse(MouseEvent {
                            kind,
                            column: 1,
                            row: y,
                            modifiers: KeyModifiers::NONE,
                        }));
                    }
                    assert_eq!(app.pending_input_count(), 2);
                    assert!(rx.try_recv().is_err());
                }
            }
        }
    }

    #[test]
    fn queue_count_tracks_admission_and_dispatch_in_the_owning_tab() {
        let _locale = crate::test_support::lock_locale();
        rust_i18n::set_locale("en-US");
        let mut app = crate::app::tests::test_app();
        app.state = ConnectionState::Connected;
        app.show_welcome_hint = false;
        app.current_tab_mut().session_id = Some(DEFAULT_TAB_ID.into());
        app.session_to_tab
            .insert(DEFAULT_TAB_ID.into(), DEFAULT_TAB_ID.into());
        let mut rx = app.test_prompt_rx.take().unwrap();
        for (text, count) in [("active", 0), ("waiting one", 1), ("waiting two", 2)] {
            app.current_tab_mut().replace_input(text.into());
            app.handle_event(AppEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )));
            assert_eq!(app.pending_input_count(), count);
            assert_count_row(&mut app, count);
        }
        assert_eq!(rx.try_recv().unwrap().text, "active");
        assert!(rx.try_recv().is_err());
        app.tab_id = Some("other-tab".into());
        app.tab_mut("other-tab");
        assert_count_row(&mut app, 0);
        app.tab_id = None;
        assert_count_row(&mut app, 2);
        for (text, count) in [("waiting one", 1), ("waiting two", 0)] {
            app.handle_event(AppEvent::AgentMessageEnd {
                session_id: DEFAULT_TAB_ID.into(),
            });
            assert_eq!(rx.try_recv().unwrap().text, text);
            assert!(rx.try_recv().is_err());
            assert_eq!(app.pending_input_count(), count);
            assert_count_row(&mut app, count);
        }
    }

    fn assert_count_row(app: &mut App, count: usize) {
        let text = crate::app::tests::render_to_text(app, 90, 22);
        let labels: Vec<_> = text
            .lines()
            .filter(|line| line.contains("queued"))
            .collect();
        let input_y = app.input_dialog_area.unwrap().y as usize;
        if count == 0 {
            assert!(!text.contains("message queued"));
            assert!(!text.contains("messages queued"));
        } else {
            let expected = if count == 1 {
                "1 message queued".into()
            } else {
                format!("{count} messages queued")
            };
            assert_eq!(text.lines().nth(input_y - 1).unwrap().trim(), expected);
            assert_eq!(
                labels
                    .iter()
                    .filter(|line| line.contains(&expected))
                    .count(),
                1
            );
        }
        assert!(app.queue_control_hits.is_empty());
    }

    #[test]
    fn retained_queue_actions_still_work_without_public_entry_points() {
        let _locale = crate::test_support::lock_locale();
        let (mut app, mut rx) = queued_app(true);
        app.invoke_queue_control(QueueControl::Recall);
        assert_eq!(app.current_tab().input, "last waiting");
        assert_eq!(app.pending_input_count(), 1);
        app.invoke_queue_control(QueueControl::SendRemaining);
        let active = rx.try_recv().unwrap();
        assert_eq!(active.text, "first waiting");
        assert!(!active.cancellation_token().is_cancelled());
        app.current_tab_mut().replace_input("third waiting".into());
        app.enqueue_input(None);
        app.invoke_queue_control(QueueControl::Discard);
        assert_eq!(app.pending_input_count(), 0);
        assert!(!active.cancellation_token().is_cancelled());
    }
}

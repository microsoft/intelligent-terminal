use super::*;
use crossterm::event::{KeyModifiers, MouseButton};
use ratatui::{buffer::Buffer, layout::Rect, style::Style};

fn app() -> App {
    let mut app = crate::app::tests::test_app();
    app.window_id = Some("1".into());
    app.owner_tab_id = Some("tab-a".into());
    app.tab_id = Some("tab-a".into());
    app.pane_id = Some("pane-a".into());
    app.current_tab_mut().pane_open = true;
    app.enable_context_menu_bridge(&json!({
        "window_id": "1", "tab_id": "tab-a", "pane_id": "pane-a",
        "context_menu_bridge": true,
    }));
    app
}

fn mouse(kind: MouseEventKind, column: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }
}

fn open(app: &mut App, action: &str) -> Value {
    app.handle_event(AppEvent::Mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        8,
    )));
    let mut request = crate::wt_protocol_events::take_test_published_events()
        .iter()
        .map(|event| serde_json::from_str::<Value>(event).unwrap())
        .find(|event| event["params"]["action"] == "show")
        .unwrap()["params"]
        .clone();
    request["action"] = json!(action);
    request
}

#[test]
fn right_click_context_menu_freezes_partial_unicode_output_across_redraws() {
    for copy_on_select in [false, true] {
        let _events = crate::wt_protocol_events::capture_test_published_events();
        let mut app = app();
        app.current_tab_mut().copy_on_select = copy_on_select;
        let mut frame = Buffer::empty(Rect::new(0, 0, 30, 1));
        frame.set_string(0, 0, "prefix \u{03bb}\u{4e2d} suffix", Style::default());
        app.text_selection.snapshot_and_render(&mut frame);
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            let column = if kind == MouseEventKind::Down(MouseButton::Left) {
                7
            } else {
                9
            };
            // Avoid the separate live-clipboard copyOnSelect side effect in this test.
            app.text_selection.handle_mouse(mouse(kind, column));
        }
        assert_eq!(
            app.text_selection.selected_text().as_deref(),
            Some("\u{03bb}\u{4e2d}")
        );
        app.last_completed_turn_click = Some(CompletedTurnClickRecord {
            tab_id: app.active_tab_key().into(),
            column: 8,
            row: 0,
            turn_index: 0,
            previous_selected_index: None,
            previous_selection_pending: false,
            previous_expanded: false,
        });
        let action = open(&mut app, "copy");
        assert!(
            app.last_completed_turn_click.is_none(),
            "opening the menu must cancel the preceding completed-turn gesture"
        );
        frame.set_string(0, 0, "a different streaming reply", Style::default());
        app.text_selection.snapshot_and_render(&mut frame);
        app.handle_event(AppEvent::FocusChanged(false));
        let mut copied = None;
        app.handle_agent_context_menu_action_with(&action, |text| {
            copied = Some(text.to_owned());
            Ok(())
        });
        assert_eq!(copied.as_deref(), Some("\u{03bb}\u{4e2d}"));
        assert!(app.text_selection.selected_text().is_none());
    }
}

#[test]
fn right_click_context_menu_direct_mode_preserves_input_copy_and_default_paste() {
    for copy_on_select in [false, true] {
        let _events = crate::wt_protocol_events::capture_test_published_events();
        let mut app = app();
        app.current_tab_mut().copy_on_select = copy_on_select;
        app.current_tab_mut().input = "selected draft".into();
        app.current_tab_mut().select_all_input();
        let action = open(&mut app, "direct");
        app.handle_agent_context_menu_action_with(&action, |text| {
            assert_eq!(text, "selected draft");
            Ok(())
        });
        let events: Vec<Value> = crate::wt_protocol_events::take_test_published_events()
            .iter()
            .map(|event| serde_json::from_str(event).unwrap())
            .collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| event["method"] == "request_default_paste")
                .count(),
            usize::from(copy_on_select)
        );
    }
}

#[test]
fn right_click_context_menu_rejects_expired_or_replaced_context_without_side_effects() {
    for case in ["expired", "session", "hidden", "window", "epoch", "token"] {
        let _events = crate::wt_protocol_events::capture_test_published_events();
        let mut app = app();
        app.current_tab_mut().input = "private draft".into();
        app.current_tab_mut().select_all_input();
        let mut action = open(&mut app, "direct");
        match case {
            "expired" => {
                app.agent_context_menu.snapshot.as_mut().unwrap().created =
                    Instant::now() - SNAPSHOT_LIFETIME - Duration::from_secs(1);
            }
            "session" => app.current_tab_mut().session_id = Some("replacement-session".into()),
            "hidden" => app.current_tab_mut().pane_open = false,
            "window" => action["window_id"] = json!("another-window"),
            "epoch" => action["epoch"] = json!("previous-helper"),
            "token" => action["token"] = json!("previous-menu"),
            _ => unreachable!(),
        }
        let mut copied = false;
        app.handle_agent_context_menu_action_with(&action, |_| {
            copied = true;
            Ok(())
        });
        assert!(!copied, "{case} must not copy");
        let events: Vec<Value> = crate::wt_protocol_events::take_test_published_events()
            .iter()
            .map(|event| serde_json::from_str(event).unwrap())
            .collect();
        assert!(
            !events
                .iter()
                .any(|event| event["method"] == "request_default_paste"),
            "{case} must not fall through to Paste"
        );
    }
}

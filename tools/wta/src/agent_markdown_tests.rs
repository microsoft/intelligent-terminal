use super::*;
use crate::app_contracts::PlanEntry;
use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};
use serde_json::json;
use std::time::{Duration, Instant};

fn replying_app(source: &str) -> App {
    let mut app = super::tests::test_app();
    app.state = ConnectionState::Connected;
    super::tests::submit_test_prompt(&mut app, "format this reply");
    app.handle_event(AppEvent::AgentMessageChunk {
        session_id: DEFAULT_TAB_ID.into(),
        text: source.into(),
    });
    app.current_tab_mut().reveal_chars = usize::MAX;
    app
}

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui::render(frame, app);
            app.text_selection.snapshot_and_render(frame.buffer_mut());
        })
        .unwrap();
    terminal.backend().buffer().clone()
}

fn plain(buffer: &Buffer) -> String {
    buffer
        .content
        .chunks(usize::from(buffer.area.width).max(1))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_for_render(app: &mut App, ready: impl Fn(&str) -> bool) -> Buffer {
    wait_for_state(app, |_, text| ready(text))
}

fn wait_for_state(app: &mut App, ready: impl Fn(&App, &str) -> bool) -> Buffer {
    wait_for_state_at(app, 90, 30, ready)
}

fn wait_for_state_at(
    app: &mut App,
    width: u16,
    height: u16,
    ready: impl Fn(&App, &str) -> bool,
) -> Buffer {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let buffer = render(app, width, height);
        let text = plain(&buffer);
        if ready(app, &text) {
            return buffer;
        }
        assert!(
            Instant::now() < deadline,
            "completed reply did not switch from raw to Markdown:\n{text}"
        );
        std::thread::yield_now();
    }
}

fn stats(app: &App) -> super::agent_markdown::WorkStats {
    app.current_tab().agent_markdown.borrow().stats()
}

fn select_text(app: &mut App, source: &str) {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use unicode_width::UnicodeWidthStr;

    let text = plain(&render(app, 90, 30));
    let (row, column) = text
        .lines()
        .enumerate()
        .find_map(|(row, text)| {
            text.find(source)
                .map(|byte| (row as u16, text[..byte].width() as u16))
        })
        .expect("selected source must be visible");
    for (kind, offset) in [
        (MouseEventKind::Down(MouseButton::Left), 0),
        (
            MouseEventKind::Drag(MouseButton::Left),
            source.width() as u16 - 1,
        ),
        (
            MouseEventKind::Up(MouseButton::Left),
            source.width() as u16 - 1,
        ),
    ] {
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind,
            column: column + offset,
            row,
            modifiers: KeyModifiers::NONE,
        }));
        render(app, 90, 30);
    }
    assert_eq!(app.text_selection.selected_text().as_deref(), Some(source));
}

fn clear_selection(app: &mut App) {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let input = app.input_dialog_area.unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        app.handle_event(AppEvent::Mouse(MouseEvent {
            kind,
            column: input.x + 2,
            row: input.y + 1,
            modifiers: KeyModifiers::NONE,
        }));
    }
    assert!(app.text_selection.selected_text().is_none());
}

fn held_thought_app() -> (App, String, Vec<String>) {
    let mut app = super::tests::test_app();
    app.state = ConnectionState::Connected;
    super::tests::submit_test_prompt(&mut app, "thinking");
    app.current_tab_mut()
        .messages
        .push(ChatMessage::Error("obsolete".into()));
    app.current_tab_mut().append_thought_chunk(
        &(0..500)
            .map(|index| format!("THINK_{index:03} 界e\u{301}\r\n"))
            .collect::<String>(),
    );
    render(&mut app, 90, 30);
    app.current_tab_mut().chat_scroll.by(130);
    let text = plain(&render(&mut app, 90, 30));
    let selected = text
        .lines()
        .find_map(|line| {
            line.find("THINK_")
                .map(|byte| line[byte..byte + 9].to_owned())
        })
        .unwrap();
    select_text(&mut app, &selected);
    assert!(app.current_tab().holds_markdown_selection());
    let rows = plain(&render(&mut app, 90, 30))
        .lines()
        .take(5)
        .map(str::to_owned)
        .collect();
    (app, selected, rows)
}

#[test]
fn agent_markdown_held_anchor_tracks_utf8_thought_retention() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, selected, rows) = held_thought_app();
    for chunk in ["界", "e\u{301}\r\n", "more 界\r\n"] {
        let before = app.current_tab().markdown_reading_position().unwrap();
        let (id, byte) = before.thought_source.unwrap();
        let cut_at = app
            .current_tab()
            .streaming_thought_text()
            .unwrap()
            .char_indices()
            .nth(chunk.chars().count())
            .unwrap()
            .0;
        app.current_tab_mut().append_thought_chunk(chunk);
        assert_eq!(
            app.current_tab()
                .markdown_reading_position()
                .unwrap()
                .thought_source,
            Some((id, byte.saturating_sub(cut_at)))
        );
        for _ in 0..2 {
            let after = plain(&render(&mut app, 90, 30));
            assert_eq!(after.lines().take(5).collect::<Vec<_>>(), rows);
            assert_eq!(
                app.text_selection.selected_text().as_deref(),
                Some(selected.as_str())
            );
            assert!(app.current_tab().holds_markdown_selection());
        }
    }
}

#[test]
fn agent_markdown_held_anchor_tracks_retained_message_indices() {
    let _locale = crate::test_support::lock_locale();
    let (mut app, selected, rows) = held_thought_app();
    let before = app.current_tab().markdown_reading_position().unwrap();
    app.current_tab_mut()
        .retain_current_messages(|message| !matches!(message, ChatMessage::Error(_)));
    let after = app.current_tab().markdown_reading_position().unwrap();
    assert_eq!(
        after.message_index,
        before.message_index.map(|index| index - 1)
    );
    assert_eq!(after.thought_source, before.thought_source);
    assert_eq!(after.row_offset, before.row_offset);
    for _ in 0..2 {
        let rendered = plain(&render(&mut app, 90, 30));
        assert_eq!(rendered.lines().take(5).collect::<Vec<_>>(), rows);
        assert_eq!(
            app.text_selection.selected_text().as_deref(),
            Some(selected.as_str())
        );
    }
    app.current_tab_mut()
        .retain_current_messages(|message| !matches!(message, ChatMessage::Thought { .. }));
    let fallback = app.current_tab().markdown_reading_position().unwrap();
    assert_eq!(fallback.message_index, Some(0));
    assert_eq!(fallback.row_offset, 0);
    assert!(fallback.thought_source.is_none());
    assert!(app.current_tab().holds_markdown_selection());
    app.current_tab_mut().retain_current_messages(|_| false);
    assert!(app.current_tab().markdown_reading_position().is_none());
    assert!(app.current_tab().holds_markdown_selection());
}

#[test]
fn agent_markdown_streams_raw_and_formats_completed_reply() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**FINISHED_BOLD**";
    let mut app = replying_app(source);
    assert!(plain(&render(&mut app, 90, 30)).contains(source));

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    assert_eq!(
        app.current_tab().completed_turns[0].details,
        [ChatMessage::Agent(source.into())]
    );
    let completed = wait_for_render(&mut app, |text| {
        text.contains("FINISHED_BOLD") && !text.contains(source)
    });
    assert!(completed
        .content
        .iter()
        .any(|cell| { cell.symbol() == "F" && cell.modifier.contains(Modifier::BOLD) }));
    for _ in 0..3 {
        assert_eq!(plain(&render(&mut app, 90, 30)), plain(&completed));
    }
    assert!(serde_json::to_string(&app.current_tab().completed_turns)
        .unwrap()
        .contains(source));
}

#[test]
fn agent_markdown_formats_failed_partial_response() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**FAILED_PARTIAL**";
    let mut app = replying_app(source);
    assert!(plain(&render(&mut app, 90, 30)).contains(source));
    app.handle_event(AppEvent::PromptError {
        tab_id: DEFAULT_TAB_ID.into(),
        prompt_id: 42,
        message: "fixture failure".into(),
    });
    assert!(app.current_tab().turn.is_idle());
    let completed = wait_for_render(&mut app, |text| {
        text.contains("FAILED_PARTIAL") && !text.contains(source)
    });
    assert!(plain(&completed).contains("fixture failure"));
    assert!(app
        .current_tab()
        .messages
        .iter()
        .any(|message| matches!(message, ChatMessage::Agent(text) if text == source)));
}

#[test]
fn agent_markdown_formats_canceled_partial_response() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**CANCELED_PARTIAL**";
    let mut app = replying_app(source);
    app.turn_cancel(DEFAULT_TAB_ID);
    app.handle_event(AppEvent::AgentMessageChunk {
        session_id: DEFAULT_TAB_ID.into(),
        text: "**LATE_CANCELED_TEXT**".into(),
    });
    wait_for_render(&mut app, |text| {
        text.contains("CANCELED_PARTIAL") && !text.contains(source)
    });
    assert_eq!(
        app.current_tab().completed_turns[0].details,
        [ChatMessage::Agent(source.into())]
    );
}

#[test]
fn agent_markdown_agent_transport_error_preserves_received_partial_source() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**DISCONNECTED_PARTIAL**";
    let mut app = replying_app(source);
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: "unbound-session".into(),
    });
    assert!(plain(&render(&mut app, 90, 30)).contains(source));
    assert_eq!(stats(&app), Default::default());
    app.handle_event(AppEvent::AgentError {
        session_id: Some(DEFAULT_TAB_ID.into()),
        failure: crate::protocol::acp::failure::AgentFailure::Protocol {
            code: -32603,
            message: "fixture transport closed".into(),
        },
        message: "fixture transport closed".into(),
    });
    assert!(app.current_tab().turn.is_idle());
    let buffer = wait_for_render(&mut app, |text| {
        text.contains("DISCONNECTED_PARTIAL") && !text.contains(source)
    });
    assert!(plain(&buffer).contains("fixture transport closed"));
    assert!(app
        .current_tab()
        .messages
        .iter()
        .any(|message| { matches!(message, ChatMessage::Agent(text) if text == source) }));
}

#[test]
fn agent_markdown_raw_mode_never_submits_rendering_work() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**RAW_COMPLETION**";
    let mut app = replying_app(source);
    app.render_agent_markdown = false;
    assert!(plain(&render(&mut app, 90, 30)).contains(source));
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    for _ in 0..4 {
        assert!(plain(&render(&mut app, 90, 30)).contains(source));
    }
    assert_eq!(stats(&app), Default::default());
}

#[test]
fn agent_markdown_restores_history_with_heading_tasks_and_code_fences() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "# Heading\r\n- [x] **DONE**\r\n\r\n```rust\r\nlet value = 1;\r\n```\r\n";
    let original = vec![CompletedTurn {
        prompt: "**USER_LITERAL**".into(),
        details: vec![ChatMessage::Agent(source.into())],
        expanded: true,
        trailing_marker: None,
    }];
    let stored = serde_json::to_string(&original).unwrap();
    let mut app = super::tests::test_app();
    app.state = ConnectionState::Connected;
    app.current_tab_mut().completed_turns = serde_json::from_str(&stored).unwrap();
    let buffer = wait_for_render(&mut app, |text| {
        text.contains("Heading") && !text.contains("# Heading") && !text.contains("**DONE**")
    });
    let text = plain(&buffer);
    assert!(text.contains("**USER_LITERAL**"), "{text}");
    assert!(text.contains("DONE"), "{text}");
    assert!(text.contains("```rust"), "{text}");
    assert!(text.contains("let value = 1;"), "{text}");
    assert_eq!(text.matches("```").count(), 2, "{text}");
    assert!(!text.contains('\r'), "{text:?}");
    assert_eq!(
        serde_json::to_string(&app.current_tab().completed_turns).unwrap(),
        stored
    );
}

#[test]
fn agent_markdown_keeps_live_segments_and_non_agent_text_literal() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let mut app = super::tests::test_app();
    app.state = ConnectionState::Connected;
    super::tests::submit_test_prompt(&mut app, "**USER_LITERAL**");
    app.handle_event(AppEvent::AgentMessageChunk {
        session_id: DEFAULT_TAB_ID.into(),
        text: "**FIRST_SEGMENT**".into(),
    });
    app.handle_event(AppEvent::Plan {
        session_id: DEFAULT_TAB_ID.into(),
        entries: vec![PlanEntry {
            content: "**PLAN_LITERAL**".into(),
            status: PlanEntryStatus::InProgress,
        }],
    });
    app.handle_event(AppEvent::AgentMessageChunk {
        session_id: DEFAULT_TAB_ID.into(),
        text: "**LAST_SEGMENT**".into(),
    });
    app.current_tab_mut().reveal_chars = usize::MAX;
    let active = plain(&render(&mut app, 90, 30));
    for literal in [
        "**USER_LITERAL**",
        "**FIRST_SEGMENT**",
        "**PLAN_LITERAL**",
        "**LAST_SEGMENT**",
    ] {
        assert!(active.contains(literal), "{active}");
    }
    assert_eq!(stats(&app), Default::default());
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    let completed = plain(&wait_for_render(&mut app, |text| {
        text.contains("FIRST_SEGMENT")
            && text.contains("LAST_SEGMENT")
            && !text.contains("**FIRST_SEGMENT**")
            && !text.contains("**LAST_SEGMENT**")
    }));
    assert!(completed.contains("**USER_LITERAL**"), "{completed}");
    assert!(completed.contains("**PLAN_LITERAL**"), "{completed}");
    assert!(completed.find("FIRST_SEGMENT") < completed.find("PLAN_LITERAL"));
    assert!(completed.find("PLAN_LITERAL") < completed.find("LAST_SEGMENT"));
    assert_eq!(stats(&app).submitted_jobs, 1);
    assert_eq!(stats(&app).completed_batch_calls, 2);
}

#[test]
fn agent_markdown_height_reads_and_unchanged_frames_do_not_render_again() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**MEASURE_ONCE**";
    let mut app = replying_app(source);
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    for _ in 0..4 {
        crate::ui::chat::estimated_block_height(&app, 88, 30);
    }
    assert_eq!(stats(&app), Default::default());
    wait_for_render(&mut app, |text| {
        text.contains("MEASURE_ONCE") && !text.contains(source)
    });
    let work = stats(&app);
    assert_eq!(work.submitted_jobs, 1);
    assert_eq!(work.completed_batch_calls, 1);
    assert_eq!(work.submitted_source_bytes, source.len() as u64);
    for _ in 0..4 {
        crate::ui::chat::estimated_block_height(&app, 88, 30);
        render(&mut app, 90, 30);
    }
    assert_eq!(stats(&app), work);
}

#[test]
fn agent_markdown_redraw_does_not_rewrap_offscreen_published_rows() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**VIEWPORT_BODY**\n\n".repeat(256) + "**VIEWPORT_END**";
    let mut app = replying_app(&source);
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    let ready = wait_for_render(&mut app, |text| {
        text.contains("VIEWPORT_END") && !text.contains("**VIEWPORT_END**")
    });
    assert!(
        crate::ui::chat::chat_paragraph_source_lines() <= usize::from(ready.area.height),
        "the draw widget received {} source rows for a {}-row viewport",
        crate::ui::chat::chat_paragraph_source_lines(),
        ready.area.height
    );
    let before = stats(&app);
    assert_eq!(plain(&render(&mut app, 90, 30)), plain(&ready));
    assert_eq!(stats(&app), before);
}

#[test]
fn agent_markdown_clear_rejects_a_result_for_reused_history_indices() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let mut app = replying_app("**OLD_RESPONSE**");
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    render(&mut app, 90, 30);
    app.current_tab_mut().clear_completed_turns();
    super::tests::submit_test_prompt(&mut app, "new response");
    app.handle_event(AppEvent::AgentMessageChunk {
        session_id: DEFAULT_TAB_ID.into(),
        text: "**NEW_RESPONSE**".into(),
    });
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    let text = plain(&wait_for_render(&mut app, |text| {
        text.contains("NEW_RESPONSE") && !text.contains("**NEW_RESPONSE**")
    }));
    assert!(!text.contains("OLD_RESPONSE"), "{text}");
    assert_eq!(app.current_tab().completed_turns.len(), 1);
}

#[test]
fn agent_markdown_preserves_selected_raw_rows_without_blocking_the_next_prompt() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let selected = "**SELECTED_RAW**";
    let source = format!("{}{selected}\n{}", "line\n".repeat(35), "tail\n".repeat(4));
    let mut app = replying_app(&source);
    let tab = app.current_tab_mut();
    tab.input = "queued next".into();
    tab.cursor_pos = tab.input.len();
    tab.refresh_command_popup();
    app.handle_event(AppEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    assert_eq!(app.pending_input_count(), 1);
    assert!(app.test_prompt_rx.as_mut().unwrap().try_recv().is_err());
    select_text(&mut app, selected);

    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    assert_eq!(app.current_tab().completed_turns.len(), 1);
    assert_eq!(
        app.test_prompt_rx
            .as_mut()
            .unwrap()
            .try_recv()
            .unwrap()
            .text,
        "queued next"
    );
    assert_eq!(app.current_tab().turn.prompt().unwrap().text, "queued next");
    let held = plain(&render(&mut app, 90, 30));
    assert_eq!(
        app.text_selection.selected_text().as_deref(),
        Some(selected),
        "{held}"
    );
    wait_for_state(&mut app, |app, text| {
        stats(app).completed_batch_calls == 1 && text.contains(selected)
    });
    assert_eq!(
        app.text_selection.selected_text().as_deref(),
        Some(selected)
    );

    clear_selection(&mut app);
    let published = plain(&render(&mut app, 90, 30));
    assert!(published.contains("SELECTED_RAW"), "{published}");
    assert!(!published.contains(selected), "{published}");
    assert_eq!(stats(&app).completed_batch_calls, 1);
}

#[test]
fn agent_markdown_mode_changes_preserve_selection_until_the_first_safe_frame() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "# Heading\n\n**MODE_SELECTION**";
    for initially_enabled in [true, false] {
        let mut app = replying_app(source);
        app.render_agent_markdown = initially_enabled;
        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: DEFAULT_TAB_ID.into(),
        });
        if initially_enabled {
            wait_for_render(&mut app, |text| {
                text.contains("MODE_SELECTION") && !text.contains("**MODE_SELECTION**")
            });
        }
        let selected = if initially_enabled {
            "MODE_SELECTION"
        } else {
            "**MODE_SELECTION**"
        };
        select_text(&mut app, selected);
        let before = stats(&app);

        app.render_agent_markdown = !initially_enabled;
        let held = plain(&render(&mut app, 90, 30));
        assert_eq!(
            app.text_selection.selected_text().as_deref(),
            Some(selected),
            "mode change moved selected text before clear:\n{held}"
        );
        if initially_enabled {
            for _ in 0..3 {
                render(&mut app, 90, 30);
            }
            assert_eq!(stats(&app), before, "disabled mode submitted new work");
        } else {
            wait_for_state(&mut app, |app, _| stats(app).completed_batch_calls == 1);
        }
        assert_eq!(
            app.text_selection.selected_text().as_deref(),
            Some(selected)
        );

        clear_selection(&mut app);
        let published = plain(&render(&mut app, 90, 30));
        assert_eq!(
            published.contains("**MODE_SELECTION**"),
            initially_enabled,
            "{published}"
        );
        assert_eq!(
            app.current_tab().completed_turns[0].details,
            [ChatMessage::Agent(source.into())]
        );
    }
}

#[test]
fn agent_markdown_does_not_render_turns_outside_the_viewport() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let mut app = super::tests::test_app();
    app.state = ConnectionState::Connected;
    for index in 0..25 {
        app.current_tab_mut().completed_turns.push(CompletedTurn {
            prompt: format!("PROMPT_{index:02}"),
            details: vec![ChatMessage::Agent(format!("**BODY_{index:02}**"))],
            expanded: true,
            trailing_marker: None,
        });
    }
    let buffer = wait_for_state(&mut app, |app, _| {
        stats(app).completed_batch_calls > 0
            && !app.current_tab().agent_markdown.borrow().has_pending()
    });
    let text = plain(&buffer);
    let mut visible = 0;
    for index in 0..25 {
        if text.contains(&format!("BODY_{index:02}"))
            || text.contains(&format!("PROMPT_{index:02}"))
        {
            visible += 1;
        } else {
            assert!(
                app.current_tab()
                    .agent_markdown
                    .borrow()
                    .rows(index, 0, 88)
                    .is_none(),
                "offscreen turn {index} was rendered:\n{text}"
            );
        }
    }
    assert_eq!(stats(&app).completed_batch_calls, visible);
}

#[test]
fn agent_markdown_eviction_invalidates_height_and_reexpansion_reuses_visible_output() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**EVICTED_REPLY**\nparagraph continues";
    let mut app = replying_app(source);
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    wait_for_render(&mut app, |text| {
        text.contains("EVICTED_REPLY") && !text.contains("**EVICTED_REPLY**")
    });
    let warm = stats(&app);
    app.current_tab_mut().select_completed_turn(0);
    app.current_tab_mut().toggle_selected_completed_turn();
    render(&mut app, 90, 30);
    app.current_tab_mut().toggle_selected_completed_turn();
    let expanded = plain(&render(&mut app, 90, 30));
    assert!(!expanded.contains("**EVICTED_REPLY**"));
    assert_eq!(stats(&app), warm);

    for index in 0..5 {
        app.current_tab_mut().completed_turns.push(CompletedTurn {
            prompt: format!("newer {index}"),
            details: vec![ChatMessage::Agent("new paragraph\n\n".repeat(40))],
            expanded: true,
            trailing_marker: None,
        });
    }
    app.current_tab_mut().scroll_to_bottom();
    render(&mut app, 90, 30);
    assert!(app
        .current_tab()
        .agent_markdown
        .borrow()
        .rows(0, 0, 88)
        .is_none());
    assert!(app
        .current_tab()
        .cached_completed_turn_height(0, 88)
        .is_none());
    let before_read = stats(&app);
    crate::ui::chat::estimated_block_height(&app, 88, 30);
    assert_eq!(
        stats(&app),
        before_read,
        "height queries must not rebuild evicted Markdown"
    );

    app.current_tab_mut().select_completed_turn(0);
    wait_for_render(&mut app, |text| {
        text.contains("EVICTED_REPLY") && !text.contains("**EVICTED_REPLY**")
    });
    assert_eq!(
        app.current_tab().completed_turns[0].details,
        [ChatMessage::Agent(source.into())]
    );
}

#[test]
fn agent_markdown_resize_clears_selection_and_rebuilds_only_the_new_width() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**RESIZED_REPLY** with enough text to wrap across several narrow pane rows.";
    let mut app = replying_app(source);
    select_text(&mut app, "**RESIZED_REPLY**");
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    wait_for_state(&mut app, |app, _| stats(app).completed_batch_calls == 1);
    assert_eq!(
        app.text_selection.selected_text().as_deref(),
        Some("**RESIZED_REPLY**")
    );

    app.handle_event(AppEvent::Resize(30, 30));
    assert!(app.text_selection.selected_text().is_none());
    let buffer = wait_for_state_at(&mut app, 30, 30, |app, text| {
        stats(app).completed_batch_calls == 2
            && text.contains("RESIZED_REPLY")
            && !text.contains("**RESIZED_REPLY**")
    });
    let text = plain(&buffer);
    assert!(text.contains("rows."), "{text}");
    let work = stats(&app);
    assert_eq!(plain(&render(&mut app, 30, 30)), text);
    assert_eq!(stats(&app), work);
}

#[test]
fn agent_markdown_helper_accepts_the_raw_startup_switch() {
    use clap::Parser;

    let cli = crate::Cli::try_parse_from([
        "wta",
        "--connect-master",
        "fixture-pipe",
        "--no-agent-markdown",
    ])
    .expect("the helper must accept the host's raw display preference");
    assert!(crate::helper_config(cli).no_agent_markdown);
    let default = crate::Cli::try_parse_from(["wta", "--connect-master", "fixture-pipe"]).unwrap();
    assert!(!crate::helper_config(default).no_agent_markdown);
}

#[test]
fn agent_markdown_runtime_preference_is_optional_scoped_and_preserves_the_session() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let source = "**LIVE_PREFERENCE**";
    let mut app = replying_app(source);
    app.owner_tab_id = Some(DEFAULT_TAB_ID.into());
    app.window_id = Some("markdown-window".into());
    app.handle_event(AppEvent::AgentMessageEnd {
        session_id: DEFAULT_TAB_ID.into(),
    });
    wait_for_render(&mut app, |text| {
        text.contains("LIVE_PREFERENCE") && !text.contains(source)
    });
    let session = app.current_tab().session_id.clone();
    assert!(matches!(
        app.agent_reconnect_state,
        AgentReconnectState::Idle
    ));
    let stored = serde_json::to_string(&app.current_tab().completed_turns).unwrap();

    for (params, expected) in [
        (
            json!({"render_agent_markdown": false, "window_id": "other-window"}),
            true,
        ),
        (
            json!({"render_agent_markdown": false, "tab_id": "other-tab"}),
            true,
        ),
        (json!({"render_agent_markdown": false}), false),
        (json!({"sessions_in_sidebar": false}), false),
        (json!({"render_agent_markdown": "true"}), false),
        (json!({"render_agent_markdown": true}), true),
    ] {
        app.handle_event(AppEvent::WtEvent {
            method: "agent_config_changed".into(),
            pane_id: String::new(),
            tab_id: None,
            params,
        });
        assert_eq!(app.render_agent_markdown, expected);
        if expected {
            wait_for_render(&mut app, |text| {
                text.contains("LIVE_PREFERENCE") && !text.contains(source)
            });
        } else {
            assert!(plain(&render(&mut app, 90, 30)).contains(source));
        }
        assert_eq!(app.current_tab().session_id, session);
        assert!(matches!(
            app.agent_reconnect_state,
            AgentReconnectState::Idle
        ));
        assert!(!app.pending_acp_start);
        assert!(!app.should_quit);
        assert_eq!(
            serde_json::to_string(&app.current_tab().completed_turns).unwrap(),
            stored
        );
    }
}

#[test]
#[ignore = "Run representative timing measurements in isolation; no latency budget is implied"]
fn agent_markdown_measure_existing_workloads() {
    let _locale = crate::test_support::lock_locale();
    rust_i18n::set_locale("en-US");
    let block = "## Completed response\n\nA paragraph with **strong**, *emphasis*, `code`, and Unicode 中 e\u{301} 👩\u{200d}💻.\n\n| Key | Value |\n| --- | --- |\n| item | complete |\n\n";
    for blocks in [256, 4096] {
        let source = block.repeat(blocks) + "**WORKLOAD_END**";
        let started = Instant::now();
        let mut app = replying_app(&source);
        app.handle_event(AppEvent::AgentMessageEnd {
            session_id: DEFAULT_TAB_ID.into(),
        });
        wait_for_state(&mut app, |app, text| {
            stats(app).completed_batch_calls == 1
                && text.contains("WORKLOAD_END")
                && !text.contains("**WORKLOAD_END**")
        });
        let ready_ms = started.elapsed().as_secs_f64() * 1000.0;
        let work = stats(&app);
        let mut frame_ms = Vec::new();
        for _ in 0..4 {
            let started = Instant::now();
            std::hint::black_box(render(&mut app, 90, 30));
            frame_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        assert_eq!(
            stats(&app),
            work,
            "unchanged frames must not submit new package work"
        );
        assert_eq!(work.submitted_jobs, 1);
        assert_eq!(work.submitted_source_bytes, source.len() as u64);
        let cache = app.current_tab().agent_markdown.borrow();
        let rows = cache.rows(0, 0, 88).unwrap();
        let display_bytes: usize = rows
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.len())
            .sum();
        println!(
            "{}",
            json!({
                "workload_blocks": blocks,
                "source_bytes": source.len(),
                "cached_rows": rows.len(),
                "display_utf8_bytes": display_bytes,
                "completed_batch_calls": work.completed_batch_calls,
                "submitted_jobs": work.submitted_jobs,
                "ready_ms": ready_ms,
                "test_backend_frame_ms": frame_ms,
                "frame_width": 90,
                "frame_height": 30
            })
        );
    }
}

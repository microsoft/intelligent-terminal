//! Behavior tests for the agent-pane slash-command system, split out of the
//! large `app.rs` / `commands.rs` test modules so all of it lives in one
//! place: the pure `commands::classify` mapping and the `App` dispatch path.
//!
//! This is a child module of `app` (declared with `#[path]` in app.rs), not
//! of the crate root, so it can reach `App`'s private dispatch methods —
//! exactly like the inline `app::tests` module it borrows `test_app` from.

use super::tests::test_app;
use super::*;

/// Dispatch a zero-arg slash command by name through the real
/// `handle_slash_command` path, the way the Enter handler does.
fn run_slash(app: &mut App, name: &str) {
    let spec = commands::lookup(name).expect("name is a registered command");
    app.handle_slash_command(ParsedCommand {
        kind: spec.kind,
        spec,
        rest: String::new(),
    });
}

// ---- commands::classify — the pure input → intent mapping ----

#[test]
fn classify_known_command() {
    match commands::classify("/stop") {
        ParseOutcome::Command(c) => assert_eq!(c.kind, CommandKind::Stop),
        other => panic!("expected Command, got {other:?}"),
    }
    match commands::classify("/help me please") {
        ParseOutcome::Command(c) => {
            assert_eq!(c.kind, CommandKind::Help);
            assert_eq!(c.rest, "me please");
        }
        other => panic!("expected Command, got {other:?}"),
    }
}

#[test]
fn classify_unknown_keeps_attempted_token() {
    // Token carries its leading `/`, and trailing args are dropped from it.
    assert_eq!(
        commands::classify("/nope"),
        ParseOutcome::Unknown("/nope".to_string())
    );
    assert_eq!(
        commands::classify("/nope foo bar"),
        ParseOutcome::Unknown("/nope".to_string())
    );
    // Leading whitespace is trimmed before the token is taken.
    assert_eq!(
        commands::classify("   /missing"),
        ParseOutcome::Unknown("/missing".to_string())
    );
}

#[test]
fn classify_not_a_command() {
    assert_eq!(commands::classify("hello"), ParseOutcome::NotCommand);
    // `//literal` escape is a prompt, not an unknown-command warning.
    assert_eq!(commands::classify("//etc/hosts"), ParseOutcome::NotCommand);
    // Bare slash / whitespace-only slash have no token to name.
    assert_eq!(commands::classify("/"), ParseOutcome::NotCommand);
    assert_eq!(commands::classify("/  "), ParseOutcome::NotCommand);
    // A `/` in the middle of a prompt is not an attempt.
    assert_eq!(commands::classify("run cmd /flag"), ParseOutcome::NotCommand);
}

// ---- App dispatch — state effects via handle_slash_command ----

#[test]
fn slash_help_toggles_overlay() {
    let mut app = test_app();
    assert!(!app.help_overlay_visible);
    run_slash(&mut app, "help");
    assert!(app.help_overlay_visible);
    run_slash(&mut app, "help");
    assert!(!app.help_overlay_visible);
}

#[test]
fn slash_clear_wipes_active_tab_history() {
    let mut app = test_app();
    app.current_tab_mut()
        .messages
        .push(ChatMessage::System("stale".into()));
    app.current_tab_mut().selected_completed_turn_idx = Some(0);

    run_slash(&mut app, "clear");

    assert!(app.current_tab().messages.is_empty());
    assert_eq!(app.current_tab().selected_completed_turn_idx, None);
}

#[test]
fn slash_stop_when_idle_notes_nothing_to_stop() {
    let mut app = test_app();
    // Fresh tab: turn is Idle, so /stop only emits the advisory message.
    assert!(!app.current_tab().turn.is_in_flight());

    run_slash(&mut app, "stop");

    assert_eq!(app.current_tab().messages.len(), 1);
    assert!(matches!(
        app.current_tab().messages.last(),
        Some(ChatMessage::System(_))
    ));
}

#[test]
fn slash_new_when_idle_resets_session() {
    let mut app = test_app();
    app.current_tab_mut().session_id = Some("sid-1".into());
    app.current_tab_mut()
        .messages
        .push(ChatMessage::System("stale".into()));

    run_slash(&mut app, "new");

    assert_eq!(app.current_tab().session_id, None);
    assert!(app.current_tab().messages.is_empty());
}

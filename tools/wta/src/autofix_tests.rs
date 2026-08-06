//! Autofix-trigger reducer tests, split out of the large `app.rs` test module
//! so the per-tab autofix gating logic lives in one place. Declared as a child
//! of `app` (via `#[path]` in app.rs) so it can reach `App`'s private
//! `maybe_trigger_autofix` / `trigger_autofix_inner` dispatch and the
//! `pub(super)` `TabAutofixState` fields.
//!
//! These cover the gating decisions that have no UI and are pure per-tab state
//! transitions:
//!
//!   * cold-start drop (`state != Connected`),
//!   * missing-`tab_id` drop,
//!   * suggest-mode (auto-suggest off) surfaces a Detected pill but submits no
//!     LLM turn,
//!   * busy single-flight: same-pane re-trigger re-emits without resubmitting,
//!     different-pane re-trigger is dropped.
//!
//! The osc:133 echo-gate / dismiss lifecycle and the agent-pane suppression
//! edge cases are covered by the sibling tests in `app::tests`.

use super::tests::test_app;
use super::*;

/// Build an Actionable command-failure notification for `pane` owned by `tab`.
fn failure_notification(pane: &str, tab: Option<&str>) -> WtNotification {
    WtNotification {
        severity: WtEventSeverity::Actionable,
        pane_id: pane.to_string(),
        tab_id: tab.map(|t| t.to_string()),
        summary: "Command failed (exit 1)".to_string(),
        acknowledged: false,
        age_ticks: 0,
    }
}

/// Cold start: a failure that lands before the helper's ACP session reaches
/// `Connected` must be dropped outright — no pill, no arm, no submit. This is
/// the `trigger_autofix_inner` `state != Connected` early-return that the
/// release checklist calls out as "cold-start behavior is acceptable".
#[test]
fn cold_start_drops_autofix_when_not_connected() {
    let mut app = test_app();
    app.state = ConnectionState::Connecting("Initializing ACP...".to_string());
    app.autofix_enabled = true;

    app.maybe_trigger_autofix(&failure_notification("pane-cold", Some("tab-cold")));

    assert!(
        app.tab_sessions
            .values()
            .all(|t| t.autofix.pane_id.is_none()),
        "a failure before Connected must not arm autofix on any tab"
    );
    assert!(
        app.tab_sessions.values().all(|t| t.turn.is_idle()),
        "a failure before Connected must not submit an autofix turn"
    );
}

/// A notification with no `tab_id` (older WT build, or an event with no tab
/// context) must be dropped with a warning rather than landing the fix in
/// whatever tab happens to be focused. No tab is armed and no turn is queued.
#[test]
fn missing_tab_id_drops_autofix() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;

    app.maybe_trigger_autofix(&failure_notification("pane-no-tab", None));

    assert!(
        app.tab_sessions
            .values()
            .all(|t| t.autofix.pane_id.is_none()),
        "a notification without tab_id must not arm autofix"
    );
    assert!(
        app.tab_sessions.values().all(|t| t.turn.is_idle()),
        "a notification without tab_id must not submit an autofix turn"
    );
}

/// Auto-suggest off: a detected failure surfaces the Detected pill so the user
/// can opt in, but the LLM is NOT called — no turn is submitted and the
/// failing pane is not armed for execution (only the bar snapshot changes).
#[test]
fn suggestion_off_emits_detected_without_submitting_turn() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = false; // auto-suggest off → suggest-mode
    let tab = "tab-suggest-off";

    app.maybe_trigger_autofix(&failure_notification("pane-suggest", Some(tab)));

    assert!(
        matches!(
            app.tab_mut(tab).autofix.bar_snapshot,
            AutofixBarSnapshot::Detected { .. }
        ),
        "auto-suggest off must surface the Detected pill"
    );
    assert!(
        app.tab_mut(tab).autofix.pane_id.is_none(),
        "auto-suggest off must not arm the pane for an LLM fix"
    );
    assert!(
        app.tab_mut(tab).turn.is_idle(),
        "auto-suggest off must not submit an autofix turn (no LLM call)"
    );
}

/// Single-flight, same pane: re-triggering autofix for the *same* failing pane
/// while a turn is already in flight must re-emit the bar state only — it must
/// not bump the generation or submit a second turn (the agent is already
/// working on it).
#[test]
fn busy_same_pane_reemit_does_not_resubmit() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;
    let tab = "tab-busy-same";
    let pane = "pane-busy-same";

    // First trigger arms the pane and submits a turn.
    app.maybe_trigger_autofix(&failure_notification(pane, Some(tab)));
    assert_eq!(
        app.tab_mut(tab).autofix.pane_id.as_deref(),
        Some(pane),
        "first trigger must arm the failing pane"
    );
    assert!(
        !app.tab_mut(tab).turn.is_idle(),
        "first trigger must submit an autofix turn"
    );
    let gen_after_first = app.tab_mut(tab).autofix.generation;

    // Same pane, still busy: re-emit only — no generation bump, no resubmit.
    app.maybe_trigger_autofix(&failure_notification(pane, Some(tab)));
    assert_eq!(
        app.tab_mut(tab).autofix.generation, gen_after_first,
        "same-pane re-trigger while busy must not bump the generation (no resubmit)"
    );
    assert_eq!(
        app.tab_mut(tab).autofix.pane_id.as_deref(),
        Some(pane),
        "same-pane re-trigger must keep the original pane armed"
    );
}

/// Single-flight, different pane: a failure in a *different* pane while the
/// tab already has an autofix turn in flight is dropped — the originally armed
/// pane stays armed and the new pane is not adopted.
#[test]
fn busy_different_pane_is_dropped() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;
    let tab = "tab-busy-diff";
    let pane_a = "pane-busy-a";
    let pane_b = "pane-busy-b";

    app.maybe_trigger_autofix(&failure_notification(pane_a, Some(tab)));
    assert_eq!(
        app.tab_mut(tab).autofix.pane_id.as_deref(),
        Some(pane_a),
        "first trigger must arm pane A"
    );
    let gen_after_first = app.tab_mut(tab).autofix.generation;

    // Different pane while A's turn is in flight → dropped.
    app.maybe_trigger_autofix(&failure_notification(pane_b, Some(tab)));
    assert_eq!(
        app.tab_mut(tab).autofix.pane_id.as_deref(),
        Some(pane_a),
        "different-pane re-trigger while busy must not steal the armed pane"
    );
    assert_eq!(
        app.tab_mut(tab).autofix.generation, gen_after_first,
        "different-pane re-trigger while busy must not submit a new turn"
    );
}

/// End-to-end negative: a *successful* command (osc:133;D;0) routed through the
/// real `handle_event` dispatcher must classify as silent and never arm
/// autofix. This is the "successful commands ignored" half of the detection
/// contract — `classify_wt_event`'s exit-code split is unit-tested separately,
/// this asserts the dispatcher honors it.
#[test]
fn success_exit_code_does_not_arm_autofix() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;
    let pane = "abcdef00-1111-2222-3333-444444444444";

    app.handle_event(AppEvent::WtEvent {
        method: "vt_sequence".to_string(),
        pane_id: pane.to_string(),
        tab_id: Some("tab-success".to_string()),
        params: serde_json::json!({
            "session_id": pane,
            "sequence": "osc:133;D;0",
        }),
    });

    assert!(
        app.tab_sessions
            .values()
            .all(|t| t.autofix.pane_id.is_none()),
        "a successful command (exit 0) must not arm autofix"
    );
    assert!(
        app.tab_sessions.values().all(|t| t.turn.is_idle()),
        "a successful command (exit 0) must not submit an autofix turn"
    );
}

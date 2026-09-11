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

#[test]
fn autofix_dismissal_does_not_report_unverified_fix_resolution() {
    // Keep the retired schema/emitter absent as well as preserving the
    // dismissal lifecycle exercised below. There is no production event sink
    // to observe now that the unsupported success event has been removed.
    assert!(!include_str!("telemetry.rs").contains("\"ErrorFixResolved\""));
    assert!(!include_str!("telemetry.rs").contains("fn log_error_fix_resolved("));
    assert!(!include_str!("app_events.rs").contains("log_error_fix_resolved("));

    let pane = "pane-autofix";
    let tab = "tab-autofix";
    for (case, events, remains_pending) in [
        ("trigger echo", vec![(pane, "osc:133;A")], true),
        (
            "fresh prompt without exit zero",
            vec![(pane, "osc:133;A"), (pane, "osc:133;A")],
            false,
        ),
        (
            "unrelated pane exit zero",
            vec![("other-pane", "osc:133;D;0")],
            true,
        ),
        (
            "same pane exit zero without fix execution",
            vec![(pane, "osc:133;D;0")],
            false,
        ),
    ] {
        let mut app = test_app();
        app.state = ConnectionState::Connected;
        app.autofix_enabled = true;
        let vt_event = |pane: &str, sequence: &str| AppEvent::WtEvent {
            method: "vt_sequence".to_string(),
            pane_id: pane.to_string(),
            tab_id: Some(tab.to_string()),
            params: serde_json::json!({ "sequence": sequence }),
        };
        app.handle_event(vt_event(pane, "osc:133;D;1"));
        assert!(app.tab_mut(tab).autofix.armed_at.is_some(), "{case}");
        for (event_pane, sequence) in events {
            app.handle_event(vt_event(event_pane, sequence));
        }

        assert_eq!(
            app.tab_mut(tab).autofix.pane_id.is_some(),
            remains_pending,
            "{case}: preserve the existing UI dismissal behavior"
        );
        assert_eq!(
            app.tab_mut(tab).autofix.armed_at.is_some(),
            remains_pending,
            "{case}: clear analysis timing only when dismissed"
        );
        assert_eq!(
            matches!(
                app.tab_mut(tab).autofix.bar_snapshot,
                AutofixBarSnapshot::Pending { .. }
            ),
            remains_pending,
            "{case}"
        );
    }
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

fn detected_helper(tab: &str, pane: &str) -> App {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = false;
    app.owner_tab_id = Some(tab.to_string());
    app.tab_id = Some(tab.to_string());
    app.maybe_trigger_autofix(&failure_notification(pane, Some(tab)));
    assert!(matches!(
        app.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Detected { .. }
    ));
    app
}

fn detected_action(pane: &str, tab: Option<&str>) -> AppEvent {
    AppEvent::WtEvent {
        method: "autofix_execute_from_detected".to_string(),
        pane_id: pane.to_string(),
        tab_id: tab.map(str::to_string),
        params: serde_json::json!({}),
    }
}

#[test]
fn detected_action_only_submits_on_the_target_helper() {
    let mut a = detected_helper("tab-a", "pane-a");
    let mut b = detected_helper("tab-b", "pane-b");
    for app in [&mut a, &mut b] {
        app.handle_event(detected_action("pane-b", Some("tab-b")));
    }
    assert!(a.current_tab().turn.is_idle());
    assert!(matches!(
        a.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Detected { .. }
    ));
    assert!(!b.current_tab().turn.is_idle());
    assert_eq!(b.current_tab().autofix.pane_id.as_deref(), Some("pane-b"));
    let generation = b.current_tab().autofix.generation;
    b.handle_event(detected_action("pane-b", Some("tab-b")));
    assert_eq!(b.current_tab().autofix.generation, generation);
}

#[test]
fn detected_action_rejects_missing_stale_and_cross_tab_targets() {
    for (pane, tab) in [
        ("", Some("tab-a")),
        ("", None),
        ("pane-old-split", Some("tab-a")),
        ("pane-a", Some("tab-b")),
        ("pane-a", Some("")),
    ] {
        let mut app = detected_helper("tab-a", "pane-a");
        app.handle_event(detected_action(pane, tab));
        assert!(app.current_tab().turn.is_idle(), "{pane:?} {tab:?}");
        assert!(matches!(
            app.current_tab().autofix.bar_snapshot,
            AutofixBarSnapshot::Detected { .. }
        ));
    }
}

#[test]
fn legacy_detected_action_without_tab_still_requires_matching_pane() {
    let mut a = detected_helper("tab-a", "pane-a");
    let mut b = detected_helper("tab-b", "pane-b");
    a.handle_event(detected_action("pane-b", None));
    b.handle_event(detected_action("pane-b", None));
    assert!(a.current_tab().turn.is_idle());
    assert!(!b.current_tab().turn.is_idle());
}

#[test]
fn detected_action_does_not_replay_after_source_pane_closes() {
    let mut app = detected_helper("tab-a", "pane-a");
    app.handle_event(closed_event("pane-a", "tab-a"));
    app.handle_event(detected_action("pane-a", Some("tab-a")));
    assert!(app.current_tab().turn.is_idle());
    assert!(matches!(
        app.current_tab().autofix.bar_snapshot,
        AutofixBarSnapshot::Idle
    ));
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
        app.tab_mut(tab).autofix.generation,
        gen_after_first,
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
        app.tab_mut(tab).autofix.generation,
        gen_after_first,
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

fn closed_event(pane: &str, tab: &str) -> AppEvent {
    AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: pane.to_string(),
        tab_id: Some(tab.to_string()),
        params: serde_json::json!({
            "session_id": pane,
            "state": "closed",
        }),
    }
}

fn closed_event_without_tab(pane: &str) -> AppEvent {
    AppEvent::WtEvent {
        method: "connection_state".to_string(),
        pane_id: pane.to_string(),
        tab_id: None,
        params: serde_json::json!({
            "session_id": pane,
            "state": "closed",
        }),
    }
}

#[test]
fn closing_source_pane_clears_detected_autofix() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = false;
    let pane = "pane-detected";
    let tab = "tab-detected";

    app.maybe_trigger_autofix(&failure_notification(pane, Some(tab)));
    app.handle_event(closed_event(pane, tab));

    assert!(matches!(
        app.tab_mut(tab).autofix.bar_snapshot,
        AutofixBarSnapshot::Idle
    ));
    assert!(app.tab_mut(tab).autofix.trigger_echo_pane.is_none());
}

#[test]
fn closing_source_pane_cancels_pending_autofix() {
    let mut app = test_app();
    app.state = ConnectionState::Connected;
    app.autofix_enabled = true;
    let pane = "pane-pending";
    let tab = "tab-pending";

    app.maybe_trigger_autofix(&failure_notification(pane, Some(tab)));
    let generation = app.tab_mut(tab).autofix.generation;
    // UI-initiated pane close currently races tab lookup in C++ and commonly
    // arrives without tab_id. The pane ID is globally unique and must still
    // resolve the owning tab's autofix state.
    app.handle_event(closed_event_without_tab(pane));

    let tab = app.tab_mut(tab);
    assert!(tab.turn.is_cancelling());
    assert!(tab.autofix.pane_id.is_none());
    assert!(matches!(tab.autofix.bar_snapshot, AutofixBarSnapshot::Idle));
    assert_eq!(tab.autofix.generation, generation.wrapping_add(1));
}

#[test]
fn closing_source_pane_clears_review_but_unrelated_close_does_not() {
    let mut app = test_app();
    let pane = "pane-review";
    let tab = "tab-review";
    {
        let tab = app.tab_mut(tab);
        tab.autofix.suggested_pane_id = Some(pane.to_string());
        tab.autofix.bar_snapshot = AutofixBarSnapshot::Review {
            pane_id: pane.to_string(),
            hotkey_hint: "Ctrl+Alt+.".to_string(),
        };
    }

    app.handle_event(closed_event("other-pane", tab));
    assert!(matches!(
        app.tab_mut(tab).autofix.bar_snapshot,
        AutofixBarSnapshot::Review { .. }
    ));

    app.handle_event(closed_event(pane, tab));
    let tab = app.tab_mut(tab);
    assert!(tab.autofix.suggested_pane_id.is_none());
    assert!(matches!(tab.autofix.bar_snapshot, AutofixBarSnapshot::Idle));
}

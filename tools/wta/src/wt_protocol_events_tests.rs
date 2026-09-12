use super::*;
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn status() -> Value {
    json!({
        "type": "event",
        "method": "agent_status",
        "params": {
            "tab_id": "tab-1",
            "agent_id": "copilot",
            "name": "Copilot",
            "version": "1",
            "model": "model-1",
            "backend": "",
            "agent_source": "host",
            "state": "connecting",
            "available_models": [],
            "current_model_id": "model-1",
            "host_catalog_ready": false,
        },
    })
}

fn queued(payload: &str) -> QueuedEvent {
    // Explicitly older than the attempt cutoff, independent of clock resolution.
    queued_at(payload, Instant::now() - Duration::from_secs(1))
}

fn queued_at(payload: &str, enqueued_at: Instant) -> QueuedEvent {
    (
        payload.to_owned(),
        crate::startup_timing::StartupTiming::new("wt_publish_queue"),
        enqueued_at,
    )
}

fn run_queue(payloads: &[String], outcomes: &[bool]) -> Vec<String> {
    let (sender, receiver) = mpsc::channel();
    for payload in payloads {
        sender.send(queued(payload)).unwrap();
    }
    drop(sender);
    let mut attempts = Vec::new();
    let mut publish = |payload: &str| {
        let result = outcomes.get(attempts.len()).copied().unwrap_or(true);
        attempts.push(payload.to_owned());
        result
    };
    let mut pending = None;
    while let Some(event) = pending.take().or_else(|| receiver.recv().ok()) {
        pending = publish_pending_run(event, &receiver, &mut publish);
    }
    attempts
}

#[test]
fn pending_identical_statuses_reduce_publish_attempts() {
    for state in ["connecting", "connected", "failed", "disconnected"] {
        let mut snapshot = status();
        snapshot["params"]["state"] = json!(state);
        let payload = snapshot.to_string();
        assert_eq!(
            run_queue(&vec![payload.clone(); 10], &[]),
            [payload],
            "{state}"
        );
    }
}

#[test]
fn changed_status_fields_are_preserved_in_order() {
    let original = status().to_string();
    for (field, value) in [
        ("tab_id", json!("tab-2")),
        ("window_id", json!("window-2")),
        ("agent_id", json!("claude")),
        ("name", json!("Claude")),
        ("version", json!("2")),
        ("model", json!("model-2")),
        ("backend", json!("WSL: Ubuntu")),
        ("agent_source", json!("wsl")),
        ("state", json!("connected")),
        (
            "available_models",
            json!([{"id": "model-2", "name": "Model 2", "description": "new"}]),
        ),
        ("current_model_id", json!("model-2")),
        ("host_catalog_ready", json!(true)),
        ("selected_agent", json!("claude")),
        ("future_field", json!("changed")),
    ] {
        let mut changed = status();
        changed["params"][field] = value;
        let changed = changed.to_string();
        let payloads = [original.clone(), changed, original.clone()];
        assert_eq!(run_queue(&payloads, &[]), payloads, "{field}");
    }
}

#[test]
fn payload_equality_is_exact_not_normalized_json() {
    let payloads = [
        status().to_string(),
        serde_json::to_string_pretty(&status()).unwrap(),
    ];
    assert_eq!(run_queue(&payloads, &[]), payloads);

    let mut changed = status();
    changed["request_id"] = json!("another-envelope");
    let payloads = [status().to_string(), changed.to_string()];
    assert_eq!(run_queue(&payloads, &[]), payloads);
}

#[test]
fn failed_publish_retries_pending_duplicate() {
    let payload = status().to_string();
    assert_eq!(
        run_queue(&vec![payload.clone(); 3], &[false, true]),
        [payload.clone(), payload.clone()]
    );
    assert_eq!(
        run_queue(&vec![payload.clone(); 3], &[false, false, false]),
        vec![payload; 3]
    );
}

#[test]
fn non_status_and_lifecycle_events_are_ordering_barriers() {
    let payload = status().to_string();
    for barrier in [
        restart_agent_stack_event_with_id("restart-1"),
        json!({"type": "event", "method": "pane_agent_session_changed", "params": {
            "agent": "copilot", "agent_session_id": "session-1", "pane_id": "pane-1"
        }})
        .to_string(),
        json!({"type": "event", "method": "agent_state_changed", "params": {
            "tab_id": "tab-1", "pane_open": true
        }})
        .to_string(),
    ] {
        let payloads = [
            payload.clone(),
            payload.clone(),
            barrier.clone(),
            barrier.clone(),
            payload.clone(),
            payload.clone(),
        ];
        assert_eq!(
            run_queue(&payloads, &[]),
            [payload.clone(), barrier.clone(), barrier, payload.clone()]
        );
    }
}

#[test]
fn later_identical_status_after_queue_drains_is_published_again() {
    let payload = status().to_string();
    let (sender, receiver) = mpsc::channel();
    let mut attempts = Vec::new();
    let mut publish = |payload: &str| {
        attempts.push(payload.to_owned());
        true
    };
    for _ in 0..2 {
        sender.send(queued(&payload)).unwrap();
        assert!(publish_pending_run(receiver.recv().unwrap(), &receiver, &mut publish).is_none());
    }
    assert_eq!(attempts, [payload.clone(), payload]);
}

#[test]
fn connected_status_after_listener_ready_reannounces_runtime_config() {
    let mut snapshot = status();
    snapshot["params"]["state"] = json!("connected");
    let payload = snapshot.to_string();
    let (sender, receiver) = mpsc::channel();
    let mut attempts = Vec::new();
    let mut publish = |payload: &str| {
        attempts.push(payload.to_owned());
        true
    };

    // ACP can connect before the WT event subscription becomes ready. Even
    // successful publication then cannot guarantee receipt of host config.
    sender.send(queued(&payload)).unwrap();
    sender.send(queued(&payload)).unwrap();
    assert!(publish_pending_run(receiver.recv().unwrap(), &receiver, &mut publish).is_none());
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    // The later wt_listener_ready AppEvent republishes this unchanged Connected
    // snapshot so the host resends runtime config to the now-ready listener.
    sender.send(queued(&payload)).unwrap();
    assert!(publish_pending_run(receiver.recv().unwrap(), &receiver, &mut publish).is_none());
    assert_eq!(attempts, [payload.clone(), payload]);
}

#[test]
fn connected_listener_ready_during_successful_publish_is_published_again() {
    let mut snapshot = status();
    snapshot["params"]["state"] = json!("connected");
    let payload = snapshot.to_string();
    let (sender, receiver) = mpsc::channel();
    sender.send(queued(&payload)).unwrap();
    sender.send(queued(&payload)).unwrap();
    let mut attempts = Vec::new();
    let mut publish = |published: &str| {
        attempts.push(published.to_owned());
        if attempts.len() == 1 {
            // Model wt_listener_ready after COM delivery but before wtcli exit:
            // the host response to the original status missed the listener.
            sender.send(queued_at(published, Instant::now())).unwrap();
        }
        true
    };
    let pending = publish_pending_run(receiver.recv().unwrap(), &receiver, &mut publish)
        .expect("in-flight reannouncement must remain pending");
    assert!(matches!(
        receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    assert!(publish_pending_run(pending, &receiver, &mut publish).is_none());
    assert_eq!(attempts, [payload.clone(), payload]);
}

#[test]
fn selected_agent_status_is_never_coalesced_even_if_null() {
    for selection in [json!("copilot"), Value::Null, json!("")] {
        let mut snapshot = status();
        snapshot["params"]["selected_agent"] = selection;
        let payloads = vec![snapshot.to_string(); 3];
        assert_eq!(run_queue(&payloads, &[]), payloads);
    }
}

#[test]
fn unscoped_or_unrecognized_status_is_never_coalesced() {
    let mut snapshots = vec![
        json!({"type": "event", "method": "agent_status", "params": null}),
        json!({"method": "agent_status", "params": {"tab_id": "tab-1", "state": "connecting"}}),
    ];
    for tab in [Value::Null, json!(""), json!(123)] {
        let mut snapshot = status();
        snapshot["params"]["tab_id"] = tab;
        snapshots.push(snapshot);
    }
    let mut missing_tab = status();
    missing_tab["params"]
        .as_object_mut()
        .unwrap()
        .remove("tab_id");
    snapshots.push(missing_tab);
    for state in [Value::Null, json!(""), json!("future-state")] {
        let mut snapshot = status();
        snapshot["params"]["state"] = state;
        snapshots.push(snapshot);
    }
    for snapshot in snapshots {
        let payloads = vec![snapshot.to_string(); 2];
        assert_eq!(run_queue(&payloads, &[]), payloads);
    }
    let payloads = vec!["not json".to_owned(); 2];
    assert_eq!(run_queue(&payloads, &[]), payloads);
}

#[test]
fn duplicate_scan_is_bounded_and_preserves_following_barrier() {
    let payload = status().to_string();
    let barrier = restart_agent_stack_event_with_id("after-burst");
    let (sender, receiver) = mpsc::channel();
    for _ in 0..MAX_COALESCED_PER_PUBLISH + 2 {
        sender.send(queued(&payload)).unwrap();
    }
    sender.send(queued(&barrier)).unwrap();
    drop(sender);
    assert!(publish_pending_run(receiver.recv().unwrap(), &receiver, &mut |_| true).is_none());
    assert_eq!(receiver.recv().unwrap().0, payload);
    assert_eq!(receiver.recv().unwrap().0, barrier);
}

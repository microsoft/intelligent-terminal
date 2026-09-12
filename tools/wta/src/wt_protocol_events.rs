/// Publish raw JSON events to Windows Terminal in submission order.
pub fn send(json_payload: String) {
    let timing = crate::startup_timing::StartupTiming::new("wt_publish_queue");
    let _ = publisher_sender().send((json_payload, timing, std::time::Instant::now()));
}

pub(crate) fn resumed_pane_binding_event(
    agent_id: &str,
    session_id: &str,
    pane_id: &str,
    location: &crate::agent_sessions::SessionLocation,
) -> Option<String> {
    // The native binding map currently rebuilds host resume invocations.
    // Do not turn an existing WSL launch into a host command on persistence.
    if location.is_wsl() {
        return None;
    }
    Some(
        serde_json::json!({
            "type": "event",
            "method": "pane_agent_session_changed",
            "params": {
                "agent": agent_id,
                "agent_session_id": session_id,
                "pane_id": pane_id,
            },
        })
        .to_string(),
    )
}

pub(crate) fn restart_agent_stack_event() -> String {
    restart_agent_stack_event_with_id(&uuid::Uuid::new_v4().to_string())
}

pub(crate) fn restart_agent_stack_event_with_id(request_id: &str) -> String {
    serde_json::json!({
        "type": "event",
        "method": "restart_agent_stack",
        "params": {
            "request_id": request_id,
        },
    })
    .to_string()
}

type QueuedEvent = (
    String,
    crate::startup_timing::StartupTiming,
    std::time::Instant,
);

fn publisher_sender() -> &'static std::sync::mpsc::Sender<QueuedEvent> {
    static SENDER: std::sync::OnceLock<std::sync::mpsc::Sender<QueuedEvent>> =
        std::sync::OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<QueuedEvent>();
        std::thread::Builder::new()
            .name("wt-event-publisher".into())
            .spawn(move || {
                let mut pending = None;
                while let Some(event) = pending.take().or_else(|| rx.recv().ok()) {
                    pending = publish_pending_run(event, &rx, &mut publish_blocking);
                }
            })
            .expect("spawn wt-event-publisher thread");
        tx
    })
}

// Bound each scan even if producers continuously enqueue identical statuses.
const MAX_COALESCED_PER_PUBLISH: usize = 64;

fn is_coalescible_status(payload: &str) -> bool {
    let Ok(event) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    if event.get("type").and_then(serde_json::Value::as_str) != Some("event")
        || event.get("method").and_then(serde_json::Value::as_str) != Some("agent_status")
    {
        return false;
    }
    let Some(params) = event.get("params").and_then(serde_json::Value::as_object) else {
        return false;
    };
    // Unscoped broadcasts and one-shot selection persistence are not snapshots.
    !params.contains_key("selected_agent")
        && params
            .get("tab_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|tab| !tab.is_empty())
        && matches!(
            params.get("state").and_then(serde_json::Value::as_str),
            Some("connecting" | "connected" | "failed" | "disconnected")
        )
}

fn publish_pending_run(
    (payload, mut timing, _): QueuedEvent,
    receiver: &std::sync::mpsc::Receiver<QueuedEvent>,
    publish: &mut impl FnMut(&str) -> bool,
) -> Option<QueuedEvent> {
    timing.mark("dequeued");
    let publish_started = std::time::Instant::now();
    let published = publish(&payload);
    timing.mark("publish_finished");
    if !published {
        timing.mark("publish_failed");
        return None;
    }
    if !is_coalescible_status(&payload) {
        return None;
    }

    let mut pending = None;
    let mut coalesced = 0;
    for _ in 0..MAX_COALESCED_PER_PUBLISH {
        let Ok(event) = receiver.try_recv() else {
            break;
        };
        // A listener-ready reannouncement can arrive after COM delivery but
        // before wtcli exits. Only snapshots queued strictly before this
        // attempt may be suppressed; newer arrivals are ordering barriers too.
        // Compare the entire wire payload, not selected UI fields.
        if event.2 >= publish_started || event.0 != payload {
            pending = Some(event);
            break;
        }
        let (_, mut timing, _) = event;
        timing.mark("dequeued");
        timing.mark("coalesced");
        coalesced += 1;
    }
    if coalesced > 0 {
        tracing::debug!(
            target: "wt_protocol",
            coalesced,
            payload_bytes = payload.len(),
            "coalesced pending agent_status publications"
        );
    }
    // Never remember the last published snapshot beyond this pending run:
    // later identical statuses can re-announce readiness or retry host config.
    pending
}

fn publish_command(exe: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(exe);
    command.arg("publish").arg("--stdin");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::piped());
    command
}

#[derive(Debug)]
enum PublishError {
    Spawn(std::io::Error),
    MissingStdin,
    Write(std::io::Error),
    Wait(std::io::Error),
}

fn execute_publish(
    command: &mut std::process::Command,
    json_payload: &[u8],
) -> Result<std::process::ExitStatus, PublishError> {
    use std::io::Write;

    let mut startup = crate::startup_timing::StartupTiming::new("wt_publish_process");
    let mut child = command.spawn().map_err(PublishError::Spawn)?;
    startup.mark("spawn");
    let write_result = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(json_payload).map_err(PublishError::Write),
        None => Err(PublishError::MissingStdin),
    };
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }

    startup.mark("stdin_written");
    let result = child.wait().map_err(PublishError::Wait);
    startup.mark("process_exit");
    result
}

fn publish_blocking(json_payload: &str) -> bool {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|directory| directory.join("wtcli.exe")))
        .filter(|path| path.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("wtcli.exe"));
    let payload_bytes = json_payload.len();
    let event_method_cache = std::sync::OnceLock::new();
    let event_method = || {
        event_method_cache.get_or_init(|| {
            serde_json::from_str::<serde_json::Value>(json_payload)
                .ok()
                .and_then(|event| event.get("method")?.as_str().map(str::to_owned))
                .unwrap_or_else(|| "<unknown>".to_owned())
        })
    };
    let mut command = publish_command(&exe);
    match execute_publish(&mut command, json_payload.as_bytes()) {
        Ok(status) if !status.success() => {
            tracing::warn!(
                target: "wt_protocol",
                ?status,
                payload_bytes,
                event_method = event_method(),
                "wtcli publish failed"
            );
        }
        Err(PublishError::Spawn(error)) => {
            tracing::warn!(
                target: "wt_protocol",
                %error,
                payload_bytes,
                event_method = event_method(),
                "failed to start wtcli publish"
            );
        }
        Err(PublishError::MissingStdin) => {
            tracing::warn!(
                target: "wt_protocol",
                payload_bytes,
                event_method = event_method(),
                "wtcli publish stdin was not piped"
            );
        }
        Err(PublishError::Write(error)) => {
            tracing::warn!(
                target: "wt_protocol",
                %error,
                payload_bytes,
                event_method = event_method(),
                "failed writing wtcli publish payload"
            );
        }
        Err(PublishError::Wait(error)) => {
            tracing::warn!(
                target: "wt_protocol",
                %error,
                payload_bytes,
                event_method = event_method(),
                "failed waiting for wtcli publish"
            );
        }
        Ok(_) => return true,
    }
    false
}

#[cfg(test)]
#[path = "wt_protocol_events_tests.rs"]
mod queue_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn resumed_pane_binding_uses_explicit_session_and_created_pane_identity() {
        for agent in ["copilot", "claude", "codex", "gemini", "opencode"] {
            let event: serde_json::Value = serde_json::from_str(
                &super::resumed_pane_binding_event(
                    agent,
                    "known-session",
                    "new-pane",
                    &crate::agent_sessions::SessionLocation::Host,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                event,
                serde_json::json!({
                    "type": "event",
                    "method": "pane_agent_session_changed",
                    "params": {
                        "agent": agent,
                        "agent_session_id": "known-session",
                        "pane_id": "new-pane",
                    },
                })
            );
        }
    }

    #[test]
    fn resumed_pane_binding_does_not_rewrite_wsl_resumes_as_host_commands() {
        assert!(super::resumed_pane_binding_event(
            "copilot",
            "known-session",
            "new-pane",
            &crate::agent_sessions::SessionLocation::Wsl {
                distro: "Ubuntu".into(),
            },
        )
        .is_none());
    }

    #[test]
    fn restart_event_has_unique_shared_request_id() {
        let first: serde_json::Value =
            serde_json::from_str(&super::restart_agent_stack_event()).unwrap();
        let second: serde_json::Value =
            serde_json::from_str(&super::restart_agent_stack_event()).unwrap();

        let first_request_id = first["params"]["request_id"].as_str().unwrap();
        let second_request_id = second["params"]["request_id"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(first_request_id).is_ok());
        assert!(uuid::Uuid::parse_str(second_request_id).is_ok());
        assert_ne!(first_request_id, second_request_id);
    }

    #[test]
    fn restart_event_preserves_supplied_request_id() {
        let event: serde_json::Value =
            serde_json::from_str(&super::restart_agent_stack_event_with_id("auth-recovery-1"))
                .unwrap();

        assert_eq!(event["params"]["request_id"], "auth-recovery-1");
    }

    #[test]
    fn publish_command_selects_stdin_transport() {
        let command = super::publish_command(std::path::Path::new("wtcli.exe"));
        let arguments: Vec<_> = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();

        assert_eq!(arguments, ["publish", "--stdin"]);
    }

    #[cfg(windows)]
    #[test]
    fn execute_publish_writes_and_closes_large_stdin_payload() {
        let capture_path = std::env::current_dir().unwrap().join(format!(
            "wta-publish-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$source = [Console]::OpenStandardInput(); \
                 $destination = [IO.File]::Create($env:WTA_TEST_CAPTURE); \
                 try { $source.CopyTo($destination) } finally { $destination.Dispose() }",
            ])
            .env("WTA_TEST_CAPTURE", &capture_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let payload = format!(
            r#"{{"type":"event","method":"agent_status","params":{{"body":"{}"}}}}"#,
            "x".repeat(128 * 1024)
        );

        let status = super::execute_publish(&mut command, payload.as_bytes())
            .expect("fake wtcli process must accept the payload and observe EOF");
        let captured = std::fs::read(&capture_path).expect("fake wtcli must capture stdin");
        let _ = std::fs::remove_file(&capture_path);

        assert!(status.success());
        assert_eq!(captured, payload.as_bytes());
    }
}

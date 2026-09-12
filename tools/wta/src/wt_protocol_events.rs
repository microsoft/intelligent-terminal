/// Publish raw JSON events to Windows Terminal in submission order.
pub fn send(json_payload: String) {
    send_with_callback(json_payload, None);
}

type PublishCallback = Box<dyn FnOnce(anyhow::Result<()>) + Send + 'static>;
const PUBLISH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const PUBLISH_REAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResumeOutcome {
    AgentTabPublished,
    PaneCreated {
        pane_session_id: String,
        binding_error: Option<String>,
    },
}

impl From<Option<String>> for ResumeOutcome {
    fn from(pane: Option<String>) -> Self {
        match pane {
            Some(pane_session_id) => Self::PaneCreated {
                pane_session_id,
                binding_error: None,
            },
            None => Self::AgentTabPublished,
        }
    }
}

type ResumeCallback = Box<dyn FnOnce(anyhow::Result<ResumeOutcome>) + Send + 'static>;

pub(crate) fn complete_resumed_pane_creation(
    result: anyhow::Result<String>,
    agent_id: &str,
    session_id: &str,
    location: &crate::agent_sessions::SessionLocation,
    on_complete: ResumeCallback,
) {
    match result {
        Err(error) => on_complete(Err(error)),
        Ok(pane_session_id) => {
            if let Some(binding) =
                resumed_pane_binding_event(agent_id, session_id, &pane_session_id, location)
            {
                publish_resume_binding(binding, pane_session_id, true, on_complete);
            } else {
                on_complete(Ok(Some(pane_session_id).into()));
            }
        }
    }
}

fn publish_resume_binding(
    binding: String,
    pane_session_id: String,
    retry: bool,
    on_complete: ResumeCallback,
) {
    send_with_callback(
        binding.clone(),
        Some(Box::new(move |result| {
            if let Err(error) = &result {
                if retry {
                    tracing::warn!(target: "wt_protocol", %pane_session_id, error = %format!("{error:#}"), "retrying resumed pane persistence binding");
                    // Only this idempotent publication is retried, never pane creation.
                    publish_resume_binding(binding, pane_session_id, false, on_complete);
                    return;
                }
            }
            on_complete(Ok(ResumeOutcome::PaneCreated {
                pane_session_id,
                binding_error: result.err().map(|error| format!("{error:#}")),
            }));
        })),
    );
}

/// Report transport completion, not just acceptance into the publisher queue.
pub fn send_with_callback(json_payload: String, on_complete: Option<PublishCallback>) {
    #[cfg(test)]
    {
        if on_complete.is_some()
            && TEST_DEFERRED_PUBLICATIONS.with(|queue| queue.borrow().is_some())
        {
            TEST_DEFERRED_PUBLICATIONS.with(|queue| {
                queue.borrow_mut().as_mut().unwrap().push_back(PublishJob {
                    json_payload,
                    on_complete,
                });
            });
            return;
        }
        TEST_PUBLISHED_EVENTS.with(|capture| {
            if let Some(events) = capture.borrow_mut().as_mut() {
                if events.len() == TEST_PUBLISHED_EVENT_LIMIT {
                    events.pop_front();
                }
                events.push_back(json_payload);
            }
        });
        if let Some(callback) = on_complete {
            callback(Ok(()));
        }
    }

    #[cfg(not(test))]
    {
        let job = PublishJob {
            json_payload,
            on_complete,
        };
        match publisher_sender() {
            Ok(sender) => {
                if let Err(error) = sender.send(job) {
                    error
                        .0
                        .complete(Err(anyhow::anyhow!("WT event publisher stopped")));
                }
            }
            Err(error) => job.complete(Err(error)),
        }
    }
}

struct PublishJob {
    json_payload: String,
    on_complete: Option<PublishCallback>,
}

impl PublishJob {
    fn complete(self, result: anyhow::Result<()>) {
        if let Err(error) = &result {
            let method = serde_json::from_str::<serde_json::Value>(&self.json_payload)
                .ok()
                .and_then(|event| event.get("method")?.as_str().map(str::to_owned));
            tracing::warn!(
                target: "wt_protocol",
                event_method = ?method,
                error = %format!("{error:#}"),
                "wtcli publish failed"
            );
        }
        if let Some(callback) = self.on_complete {
            callback(result);
        }
    }
}

#[cfg(test)]
thread_local! {
    static TEST_PUBLISHED_EVENTS:
        std::cell::RefCell<Option<std::collections::VecDeque<String>>> =
        const { std::cell::RefCell::new(None) };
    static TEST_DEFERRED_PUBLICATIONS:
        std::cell::RefCell<Option<std::collections::VecDeque<PublishJob>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct TestDeferredPublications;

#[cfg(test)]
impl Drop for TestDeferredPublications {
    fn drop(&mut self) {
        TEST_DEFERRED_PUBLICATIONS.with(|queue| *queue.borrow_mut() = None);
    }
}

#[cfg(test)]
impl TestDeferredPublications {
    pub(crate) fn pending_count(&self) -> usize {
        TEST_DEFERRED_PUBLICATIONS.with(|queue| queue.borrow().as_ref().unwrap().len())
    }

    pub(crate) fn complete_next(&self, result: anyhow::Result<()>) -> String {
        let job = TEST_DEFERRED_PUBLICATIONS
            .with(|queue| queue.borrow_mut().as_mut().unwrap().pop_front().unwrap());
        let payload = job.json_payload.clone();
        job.complete(result);
        payload
    }
}

#[cfg(test)]
pub(crate) fn defer_test_publications() -> TestDeferredPublications {
    TEST_DEFERRED_PUBLICATIONS.with(|queue| {
        assert!(queue
            .borrow_mut()
            .replace(std::collections::VecDeque::new())
            .is_none());
    });
    TestDeferredPublications
}

#[cfg(test)]
const TEST_PUBLISHED_EVENT_LIMIT: usize = 64;

#[cfg(test)]
pub(crate) struct TestPublishedEventCapture;

#[cfg(test)]
impl Drop for TestPublishedEventCapture {
    fn drop(&mut self) {
        TEST_PUBLISHED_EVENTS.with(|capture| *capture.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn capture_test_published_events() -> TestPublishedEventCapture {
    TEST_PUBLISHED_EVENTS.with(|capture| {
        let previous = capture
            .borrow_mut()
            .replace(std::collections::VecDeque::new());
        assert!(previous.is_none(), "test event capture cannot be nested");
    });
    TestPublishedEventCapture
}

#[cfg(test)]
pub(crate) fn take_test_published_events() -> Vec<String> {
    TEST_PUBLISHED_EVENTS.with(|capture| {
        capture
            .borrow_mut()
            .as_mut()
            .map(|events| events.drain(..).collect())
            .unwrap_or_default()
    })
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

pub(crate) fn agent_availability_changed_event(agent_id: &str, tab_id: Option<&str>) -> String {
    serde_json::json!({
        "type": "event",
        "method": "agent_availability_changed",
        "params": {
            "agent_id": agent_id,
            "tab_id": tab_id,
        },
    })
    .to_string()
}

#[cfg(not(test))]
fn publisher_sender() -> anyhow::Result<&'static std::sync::mpsc::Sender<PublishJob>> {
    static SENDER: std::sync::OnceLock<std::io::Result<std::sync::mpsc::Sender<PublishJob>>> =
        std::sync::OnceLock::new();
    SENDER
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<PublishJob>();
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            std::thread::Builder::new()
                .name("wt-event-publisher".into())
                .spawn(move || {
                    run_publisher(
                        rx,
                        &runtime,
                        |_| resolved_publish_command(),
                        PUBLISH_TIMEOUT,
                    );
                })?;
            Ok(tx)
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!("failed to start WT event publisher: {error}"))
}

fn run_publisher(
    rx: std::sync::mpsc::Receiver<PublishJob>,
    runtime: &tokio::runtime::Runtime,
    mut command_for: impl FnMut(&str) -> tokio::process::Command,
    timeout: std::time::Duration,
) {
    while let Ok(job) = rx.recv() {
        let result = runtime.block_on(execute_publish(
            &mut command_for(&job.json_payload),
            job.json_payload.as_bytes(),
            timeout,
        ));
        job.complete(result);
    }
}

fn publish_command(exe: &std::path::Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(exe);
    command.arg("publish").arg("--stdin");
    #[cfg(windows)]
    {
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdin(std::process::Stdio::piped());
    command
}

async fn execute_publish(
    command: &mut tokio::process::Command,
    json_payload: &[u8],
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let mut child = command
        .kill_on_drop(true)
        .spawn()
        .context("failed to start wtcli publish")?;
    finish_publish(&mut child, json_payload, timeout).await
}

async fn finish_publish(
    child: &mut tokio::process::Child,
    json_payload: &[u8],
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    use anyhow::Context;
    use tokio::io::AsyncWriteExt;

    let stdin = child.stdin.take();
    // Both a child that never reads stdin and a blocked SendEvent share this
    // deadline. Cancellation drops the pipe before kill/reap begins.
    let completed = tokio::time::timeout(timeout, async {
        let mut stdin = stdin.context("wtcli publish stdin was not piped")?;
        stdin
            .write_all(json_payload)
            .await
            .context("failed writing wtcli publish payload")?;
        stdin
            .flush()
            .await
            .context("failed flushing wtcli publish payload")?;
        drop(stdin);
        child
            .wait()
            .await
            .context("failed waiting for wtcli publish")
    })
    .await;
    let mut failure = match completed {
        Ok(Ok(status)) if status.success() => return Ok(()),
        Ok(Ok(status)) => anyhow::bail!("wtcli publish failed ({status})"),
        Ok(Err(error)) => error,
        Err(_) => anyhow::anyhow!("wtcli publish timed out after {timeout:?}"),
    };
    if let Err(error) = child.start_kill() {
        failure = failure.context(format!("wtcli publish kill failed: {error}"));
    }
    match tokio::time::timeout(PUBLISH_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => failure = failure.context(format!("wtcli publish reap failed: {error}")),
        Err(_) => {
            failure = failure.context(format!(
                "wtcli publish reap timed out after {PUBLISH_REAP_TIMEOUT:?}"
            ))
        }
    }
    Err(failure)
}

#[cfg(not(test))]
fn resolved_publish_command() -> tokio::process::Command {
    let exe = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|directory| directory.join("wtcli.exe")))
        .filter(|path| path.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("wtcli.exe"));
    publish_command(&exe)
}

#[cfg(test)]
mod tests {
    fn blocked_publish_command() -> tokio::process::Command {
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Threading.Thread]::Sleep(60000)",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        command
    }

    #[tokio::test]
    async fn publish_timeout_bounds_stdin_and_wait_and_reaps_child() {
        for payload in [vec![b'x'], vec![b'x'; 1024 * 1024]] {
            let mut child = blocked_publish_command().spawn().unwrap();
            let error =
                super::finish_publish(&mut child, &payload, std::time::Duration::from_millis(100))
                    .await
                    .unwrap_err();
            assert!(
                format!("{error:#}").contains("wtcli publish timed out"),
                "{error:#}"
            );
            assert!(
                child.try_wait().unwrap().is_some(),
                "timed-out child must be reaped"
            );
        }
    }

    #[test]
    fn publish_timeout_completes_once_and_queue_keeps_submission_order() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (results, received) = std::sync::mpsc::channel();
        for index in 0..3 {
            let results = results.clone();
            tx.send(super::PublishJob {
                json_payload: index.to_string(),
                on_complete: Some(Box::new(move |result| {
                    results
                        .send((index, result.map_err(|error| format!("{error:#}"))))
                        .unwrap();
                })),
            })
            .unwrap();
        }
        drop(tx);
        drop(results);
        let worker = std::thread::spawn(move || {
            super::run_publisher(
                rx,
                &runtime,
                |payload| {
                    if payload == "0" {
                        blocked_publish_command()
                    } else {
                        let mut command = tokio::process::Command::new("cmd.exe");
                        command
                            .args(["/D", "/C", "set /p value= & exit /b 0"])
                            .stdin(std::process::Stdio::piped());
                        command
                    }
                },
                std::time::Duration::from_secs(1),
            );
        });
        for expected in 0..3 {
            let (index, result) = received
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(index, expected);
            if index == 0 {
                assert!(result.unwrap_err().contains("wtcli publish timed out"));
            } else {
                assert!(result.is_ok(), "{result:?}");
            }
        }
        worker.join().unwrap();
        assert!(matches!(
            received.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
    }

    #[tokio::test]
    async fn resume_publish_reports_transport_failures_to_callback() {
        use std::process::Stdio;
        use tokio::process::Command;

        let mut missing = Command::new(format!("missing-wtcli-{}.exe", uuid::Uuid::new_v4()));
        missing.stdin(Stdio::piped());
        let mut exited = Command::new("cmd.exe");
        exited.args(["/D", "/C", "exit /b 1"]).stdin(Stdio::piped());
        let mut no_stdin = Command::new("cmd.exe");
        no_stdin
            .args(["/D", "/C", "exit /b 0"])
            .stdin(Stdio::null());
        let mut closed_stdin = Command::new("cmd.exe");
        closed_stdin
            .args(["/D", "/C", "exit /b 0"])
            .stdin(Stdio::piped());

        for (mut command, payload, expected) in [
            (missing, Vec::new(), &["failed to start wtcli publish"][..]),
            (exited, Vec::new(), &["wtcli publish failed"][..]),
            (no_stdin, Vec::new(), &["stdin was not piped"][..]),
            (
                closed_stdin,
                vec![b'x'; 1024 * 1024],
                // Buffered pipe I/O may observe closure during write or flush.
                &[
                    "failed writing wtcli publish",
                    "failed flushing wtcli publish",
                ][..],
            ),
        ] {
            let result =
                super::execute_publish(&mut command, &payload, super::PUBLISH_TIMEOUT).await;
            let (tx, rx) = std::sync::mpsc::channel();
            super::PublishJob {
                json_payload: r#"{"method":"resume_in_new_agent_tab"}"#.into(),
                on_complete: Some(Box::new(move |result| tx.send(result).unwrap())),
            }
            .complete(result);
            let error = rx.recv().unwrap().unwrap_err();
            let detail = format!("{error:#}");
            assert!(
                expected.iter().any(|message| detail.contains(message)),
                "{detail}"
            );
        }
    }

    #[test]
    fn resume_publish_reports_success_to_callback() {
        let (tx, rx) = std::sync::mpsc::channel();
        super::PublishJob {
            json_payload: r#"{"method":"resume_in_new_agent_tab"}"#.into(),
            on_complete: Some(Box::new(move |result| tx.send(result).unwrap())),
        }
        .complete(Ok(()));
        assert!(rx.recv().unwrap().is_ok());
    }

    #[test]
    fn test_event_capture_is_opt_in() {
        super::take_test_published_events();
        super::send(r#"{"type":"event","method":"uncaptured"}"#.to_string());

        assert!(
            super::take_test_published_events().is_empty(),
            "events sent outside an explicit capture scope must not leak into later tests"
        );

        {
            let _capture = super::capture_test_published_events();
            super::send(r#"{"type":"event","method":"captured"}"#.to_string());
            assert_eq!(super::take_test_published_events().len(), 1);

            for index in 0..=super::TEST_PUBLISHED_EVENT_LIMIT {
                super::send(format!(r#"{{"type":"event","index":{index}}}"#));
            }
            let bounded = super::take_test_published_events();
            assert_eq!(bounded.len(), super::TEST_PUBLISHED_EVENT_LIMIT);
            assert!(bounded.first().unwrap().contains(r#""index":1"#));
            assert!(bounded.last().unwrap().contains(r#""index":64"#));
        }

        super::send(r#"{"type":"event","method":"after-scope"}"#.to_string());
        assert!(super::take_test_published_events().is_empty());
    }

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
    fn availability_event_routes_to_the_installing_tab() {
        let event: serde_json::Value = serde_json::from_str(
            &super::agent_availability_changed_event("copilot", Some("tab-a")),
        )
        .unwrap();

        assert_eq!(event["method"], "agent_availability_changed");
        assert_eq!(event["params"]["agent_id"], "copilot");
        assert_eq!(event["params"]["tab_id"], "tab-a");
    }

    #[test]
    fn publish_command_selects_stdin_transport() {
        let command = super::publish_command(std::path::Path::new("wtcli.exe"));
        let arguments: Vec<_> = command
            .as_std()
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect();

        assert_eq!(arguments, ["publish", "--stdin"]);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn execute_publish_writes_and_closes_large_stdin_payload() {
        let capture_path = std::env::temp_dir().join(format!(
            "wta-publish-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let mut command = tokio::process::Command::new("powershell.exe");
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

        super::execute_publish(&mut command, payload.as_bytes(), super::PUBLISH_TIMEOUT)
            .await
            .expect("fake wtcli process must accept the payload and observe EOF");
        let captured = std::fs::read(&capture_path).expect("fake wtcli must capture stdin");
        let _ = std::fs::remove_file(&capture_path);

        assert_eq!(captured, payload.as_bytes());
    }
}

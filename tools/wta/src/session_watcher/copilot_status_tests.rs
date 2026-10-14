use super::*;
use std::os::windows::fs::OpenOptionsExt;

struct Fixture {
    root: PathBuf,
    directory: PathBuf,
    row: SessionInfo,
    hold: Option<std::fs::File>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("scratch")
            .join(format!("copilot-status-{}", uuid::Uuid::new_v4()));
        let sid = uuid::Uuid::new_v4().to_string();
        let directory = root.join(&sid);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("inuse.42.lock"), b"42\n").unwrap();
        std::fs::write(directory.join("inuse.42.hold"), b"").unwrap();
        let hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(directory.join("inuse.42.hold"))
            .unwrap();
        let mut row = SessionInfo::new(
            agent_client_protocol::schema::v1::SessionId::new(sid),
            PathBuf::new(),
        );
        row.provider_id = Some("copilot".into());
        row.cli_source = Some(CliSource::Copilot);
        row.location = SessionLocation::Host;
        row.status = Some(AgentStatus::Historical);
        Self {
            root,
            directory,
            row,
            hold: Some(hold),
        }
    }

    fn write(&self, text: &str) {
        std::fs::write(self.directory.join("events.jsonl"), text).unwrap();
    }

    fn read(&self) -> Option<AgentStatus> {
        status(
            &self.row,
            &self.root,
            &|pid| {
                (pid == 42).then_some(Process {
                    created: SystemTime::UNIX_EPOCH,
                    image: PathBuf::from("C:\\native\\copilot.exe"),
                })
            },
            Instant::now() + Duration::from_secs(2),
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.hold.take();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn external_in_use_does_not_read_transcripts_or_infer_activity() {
    let fixture = Fixture::new();
    let original = fixture.row.clone();
    assert_eq!(
        fixture.read(),
        Some(AgentStatus::InUse),
        "no transcript is required"
    );
    for transcript in [
        "{\"type\":\"assistant.turn_start\"}\n",
        "{\"type\":\"assistant.turn_end\"}\n",
        "{\"type\":\"permission.requested\"}\n",
        "malformed or incomplete",
    ] {
        fixture.write(transcript);
        assert_eq!(fixture.read(), Some(AgentStatus::InUse));
    }
    let _unreadable_transcript = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fixture.directory.join("events.jsonl"))
        .unwrap();
    assert_eq!(fixture.read(), Some(AgentStatus::InUse));
    assert_eq!(
        fixture.row, original,
        "the registry input remains historical and unbound"
    );
    assert!(fixture.row.pane_session_id.is_none());
    assert!(fixture.row.owner_window_id.is_none());
    let mut response = fixture.row.clone();
    response.status = fixture.read();
    let json = serde_json::to_value(response).unwrap();
    assert_eq!(json["status"], "InUse");
    assert!(json["pane_session_id"].is_null());
    assert!(json.get("owner_window_id").is_none());
}

#[test]
fn malformed_dead_reused_wrong_executable_markers_are_not_live() {
    let created = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
    let valid = || {
        Some(Process {
            created,
            image: PathBuf::from("C:\\native\\copilot.exe"),
        })
    };
    assert!(marker_process("inuse.42.lock", b"42\n", created, &|_| valid()).is_some());
    for (name, bytes) in [
        ("inuse.42.lock", b"43".as_slice()),
        ("inuse.42.lock", b"invalid".as_slice()),
        ("inuse.invalid.lock", b"42".as_slice()),
        ("inuse.0.lock", b"0".as_slice()),
    ] {
        assert!(marker_process(name, bytes, created, &|_| valid()).is_none());
    }
    assert!(marker_process("inuse.42.lock", b"42", created, &|_| None).is_none());
    assert!(marker_process("inuse.42.lock", b"42", SystemTime::UNIX_EPOCH, &|_| valid()).is_none());
    assert!(
        marker_process("inuse.42.lock", b"42", created, &|_| Some(Process {
            created,
            image: PathBuf::from("C:\\node.exe")
        }))
        .is_none()
    );
}

#[test]
fn released_session_lease_cannot_promote_old_sid_even_with_live_pid() {
    let mut fixture = Fixture::new();
    assert_eq!(fixture.read(), Some(AgentStatus::InUse));
    fixture.hold.take();
    assert!(
        fixture.read().is_none(),
        "stale lock/hold filenames do not prove an active lease"
    );
    std::fs::remove_file(fixture.directory.join("inuse.42.hold")).unwrap();
    assert!(fixture.read().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn blocking_probe_admission_survives_timeout_and_waiter_drop() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    for timeout in [false, true] {
        let gate = Arc::new(tokio::sync::Semaphore::new(1));
        let (started, started_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let mut waiter = Box::pin(run_probe(gate.clone(), move || {
            started.send(()).unwrap();
            release_rx.recv().unwrap();
        }));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(waiter.as_mut().poll(&mut context), Poll::Pending));
        started_rx.await.unwrap();
        if timeout {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(3)).await;
            assert_eq!(waiter.await, Err(ProbeError::Timeout));
            tokio::time::resume();
        } else {
            drop(waiter);
        }
        assert_eq!(
            run_probe(gate.clone(), || panic!("busy probe must not be spawned")).await,
            Err::<(), _>(ProbeError::Busy)
        );
        release.send(()).unwrap();
        let permit = gate.clone().acquire_owned().await.unwrap();
        drop(permit);
        assert_eq!(run_probe(gate.clone(), || 42).await, Ok(42));
    }
}

#[test]
fn activation_probe_distinguishes_absence_release_timeout_and_errors() {
    let mut fixture = Fixture::new();
    let lookup = |_: u32| {
        Ok(Some(Process {
            created: SystemTime::UNIX_EPOCH,
            image: PathBuf::from("C:\\native\\copilot.exe"),
        }))
    };
    let read = |fixture: &Fixture| {
        probe_status(
            &fixture.row,
            &fixture.root,
            &lookup,
            Instant::now() + Duration::from_secs(2),
        )
    };
    assert_eq!(read(&fixture), Ok(Some(AgentStatus::InUse)));
    assert_eq!(
        probe_status(&fixture.row, &fixture.root, &lookup, Instant::now()),
        Err(ProbeError::Timeout)
    );
    assert_eq!(
        probe_status(
            &fixture.row,
            &fixture.root,
            &|_| Err(ProbeError::Unavailable),
            Instant::now() + Duration::from_secs(2),
        ),
        Err(ProbeError::Unavailable)
    );
    fixture.hold.take();
    assert_eq!(read(&fixture), Ok(None));
    std::fs::remove_file(fixture.directory.join("inuse.42.lock")).unwrap();
    assert_eq!(read(&fixture), Ok(None));
}

#[test]
fn live_it_registration_keeps_every_detailed_status() {
    let mut fixture = Fixture::new();
    for activity in [
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::Attention,
        AgentStatus::Error,
    ] {
        fixture.row.status = Some(activity.clone());
        fixture.write("{\"type\":\"assistant.turn_start\"}\n");
        assert!(
            fixture.read().is_none(),
            "live IT registration is not enriched"
        );
        assert_eq!(fixture.row.status, Some(activity));
    }
}

#[test]
fn history_rows_are_not_live_it_registrations() {
    let mut fixture = Fixture::new();
    for history in [AgentStatus::Historical, AgentStatus::Ended] {
        fixture.row.status = Some(history.clone());
        assert_eq!(fixture.read(), Some(AgentStatus::InUse));
        assert_eq!(fixture.row.status, Some(history));
    }
}

#[test]
fn qualified_scope_is_preserved() {
    let mut fixture = Fixture::new();
    fixture.write("{\"type\":\"assistant.turn_start\"}\n");
    let original = fixture.row.clone();
    for variant in 0..6 {
        fixture.row = original.clone();
        match variant {
            0 => fixture.row.provider_id = Some("claude".into()),
            1 => {
                fixture.row.location = SessionLocation::Wsl {
                    distro: "Ubuntu".into(),
                }
            }
            2 => fixture.row.session_universe = Some("other-home".into()),
            3 => fixture.row.origin = Some(SessionOrigin::AgentPane),
            4 => fixture.row.status = Some(AgentStatus::Attention),
            _ => fixture.row.session_id = agent_client_protocol::schema::v1::SessionId::new(".."),
        }
        assert!(fixture.read().is_none());
    }
}

#[test]
fn missing_dead_ambiguous_or_expired_evidence_is_not_in_use() {
    let fixture = Fixture::new();
    assert!(status(
        &fixture.row,
        &fixture.root,
        &|_| None,
        Instant::now() + Duration::from_secs(2),
    )
    .is_none());
    let process = |pid| {
        Some(Process {
            created: SystemTime::UNIX_EPOCH,
            image: PathBuf::from("C:\\native\\copilot.exe"),
        })
        .filter(|_| matches!(pid, 42 | 43))
    };
    assert!(status(&fixture.row, &fixture.root, &process, Instant::now()).is_none());
    std::fs::write(fixture.directory.join("inuse.43.lock"), b"43\n").unwrap();
    std::fs::write(fixture.directory.join("inuse.43.hold"), b"").unwrap();
    let _other_owner = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(fixture.directory.join("inuse.43.hold"))
        .unwrap();
    assert!(status(
        &fixture.row,
        &fixture.root,
        &process,
        Instant::now() + Duration::from_secs(2),
    )
    .is_none());
    std::fs::remove_file(fixture.directory.join("inuse.42.lock")).unwrap();
    assert!(fixture.read().is_none());
}

#[test]
#[ignore = "explicit read-only current-session evidence; requires WTA_COPILOT_STATUS_PROBE_SID"]
fn current_native_session_read_only_probe() {
    let sid = std::env::var("WTA_COPILOT_STATUS_PROBE_SID").unwrap();
    let mut row = SessionInfo::new(
        agent_client_protocol::schema::v1::SessionId::new(sid),
        PathBuf::new(),
    );
    row.provider_id = Some("copilot".into());
    row.cli_source = Some(CliSource::Copilot);
    row.location = SessionLocation::Host;
    row.status = Some(AgentStatus::Historical);
    let result = status(
        &row,
        &session_root().unwrap(),
        &|pid| native_process(pid).ok().flatten(),
        Instant::now() + Duration::from_secs(2),
    );
    println!("validated_native_provider_status={result:?}");
    assert_eq!(result, Some(AgentStatus::InUse));
    assert_eq!(row.status, Some(AgentStatus::Historical));
    assert!(row.pane_session_id.is_none());
}

use super::*;
use std::io::Write;
use std::os::windows::fs::OpenOptionsExt;

struct Fixture {
    root: PathBuf,
    directory: PathBuf,
    row: SessionInfo,
    hold: Option<std::fs::File>,
    cache: Cache,
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
            cache: Cache::default(),
        }
    }

    fn write(&self, text: &str) {
        std::fs::write(self.directory.join("events.jsonl"), text).unwrap();
    }

    fn read(&mut self) -> Option<AgentStatus> {
        status(
            &self.row,
            &self.root,
            &mut self.cache,
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
fn working_idle_cached_append_and_response_copy_preserve_identity() {
    let mut fixture = Fixture::new();
    fixture.write("{\"type\":\"assistant.turn_start\"}\n");
    let original = fixture.row.clone();
    assert_eq!(fixture.read(), Some(AgentStatus::Working));
    let offset = fixture.cache.0.values().next().unwrap().offset;
    assert_eq!(fixture.read(), Some(AgentStatus::Working));
    assert_eq!(fixture.cache.0.values().next().unwrap().offset, offset);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(fixture.directory.join("events.jsonl"))
        .unwrap();
    writeln!(file, "{{\"type\":\"tool.execution_complete\"}}").unwrap();
    assert_eq!(fixture.read(), Some(AgentStatus::Working));
    writeln!(file, "{{\"type\":\"assistant.turn_end\"}}").unwrap();
    assert_eq!(fixture.read(), Some(AgentStatus::Idle));
    assert_eq!(
        fixture.row, original,
        "the registry input remains historical and unbound"
    );
    assert!(fixture.row.pane_session_id.is_none());
    assert!(fixture.row.owner_window_id.is_none());
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
    fixture.write("{\"type\":\"assistant.turn_start\"}\n");
    assert_eq!(fixture.read(), Some(AgentStatus::Working));
    fixture.hold.take();
    assert!(
        fixture.read().is_none(),
        "stale lock/hold filenames do not prove an active lease"
    );
    assert!(
        fixture.cache.0.is_empty(),
        "released leases invalidate cached activity"
    );
    std::fs::remove_file(fixture.directory.join("inuse.42.hold")).unwrap();
    assert!(fixture.read().is_none());
}

#[test]
fn qualified_scope_live_hook_status_and_unknown_phase_are_preserved() {
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
    fixture.row = original;
    fixture.write("{\"type\":\"tool.execution_complete\"}\n");
    assert!(fixture.read().is_none());
    fixture.write("{\"type\":\"assistant.turn_start\"}\n{\"type\":");
    assert!(fixture.read().is_none());
    fixture.write("{\"type\":\"assistant.turn_start\"}\nmalformed\n");
    assert!(fixture.read().is_none());
}

#[test]
fn bounded_bootstrap_reads_long_log_suffix_and_requires_complete_records() {
    let mut fixture = Fixture::new();
    let mut bytes = vec![b'x'; MAX_TAIL as usize + 256];
    bytes.extend_from_slice(b"\n{\"type\":\"assistant.turn_start\"}\n");
    std::fs::write(fixture.directory.join("events.jsonl"), bytes).unwrap();
    assert_eq!(fixture.read(), Some(AgentStatus::Working));
    assert!(fixture.cache.0.values().next().unwrap().offset > MAX_TAIL);
    fixture.write("{\"type\":\"assistant.turn_start\"}\n");
    assert_eq!(
        fixture.read(),
        Some(AgentStatus::Working),
        "truncation resets the tail"
    );
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
        &mut Cache::default(),
        &native_process,
        Instant::now() + Duration::from_secs(2),
    );
    println!("validated_native_provider_phase={result:?}");
    assert!(result.is_some());
    assert_eq!(row.status, Some(AgentStatus::Historical));
    assert!(row.pane_session_id.is_none());
}

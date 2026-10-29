#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!(
                "could not remove diagnostic fixture {}: {error}",
                self.0.display()
            );
        }
    }
}

const AGENT: &str = "pwsh.exe -NoLogo -NoProfile -NonInteractive -Command &$env:IT_DIAGNOSTIC_FIXTURE -LogPath $env:IT_DIAGNOSTIC_LOG -StartupControlPath $env:IT_DIAGNOSTIC_CONTROL";

fn run_probe(root: &Path, scratch: &Path, verb: &str, retry: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wta"));
    command
        .args([verb, "--agent", AGENT])
        .current_dir(scratch)
        .env("LOCALAPPDATA", scratch)
        .env("APPDATA", scratch)
        .env(
            "IT_DIAGNOSTIC_FIXTURE",
            root.join("test\\e2e\\fixtures\\Mock-AcpChatAgent.ps1"),
        )
        .env("IT_DIAGNOSTIC_LOG", scratch.join("requests.log"))
        .env("IT_DIAGNOSTIC_CONTROL", scratch.join("startup.json"));
    if retry {
        command.arg("--retry-startup");
    }
    command.output().expect("run diagnostic CLI")
}

fn starts(scratch: &Path) -> usize {
    std::fs::read_to_string(scratch.join("requests.log"))
        .expect("read real fixture requests")
        .lines()
        .filter(|line| line.contains("|start|"))
        .count()
}

#[test]
fn diagnostic_probes_share_exhaustion_retry_and_cleanup_quarantine() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let scratch = Scratch(
        root.join("Generated Files")
            .join(format!("diagnostic-startup-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir_all(&scratch.0).unwrap();
    let control = scratch.0.join("startup.json");
    std::fs::write(&control, r#"{"mode":"fail"}"#).unwrap();
    for verb in [
        "probe-sessions",
        "probe-host-sessions",
        "probe-sessions",
        "probe-host-sessions",
    ] {
        let output = run_probe(&root, &scratch.0, verb, false);
        assert!(!output.status.success(), "{verb}: {output:?}");
    }
    assert_eq!(
        starts(&scratch.0),
        3,
        "diagnostics share the startup ceiling"
    );

    std::fs::write(&control, r#"{"mode":"ready"}"#).unwrap();
    for verb in ["probe-sessions", "probe-host-sessions"] {
        assert!(!run_probe(&root, &scratch.0, verb, false).status.success());
        assert_eq!(starts(&scratch.0), 3, "exhausted targets must not launch");
    }
    let recovered = run_probe(&root, &scratch.0, "probe-sessions", true);
    assert!(recovered.status.success(), "{recovered:?}");
    let result: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(result["list_sessions_ok"], false);
    assert_eq!(result["sessions"], serde_json::json!([]));
    assert_eq!(starts(&scratch.0), 4);
    let host = run_probe(&root, &scratch.0, "probe-host-sessions", false);
    assert!(host.status.success(), "{host:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&host.stdout).unwrap(),
        serde_json::json!([])
    );
    assert_eq!(starts(&scratch.0), 5);

    std::fs::write(&control, r#"{"mode":"fail"}"#).unwrap();
    assert!(!run_probe(&root, &scratch.0, "probe-sessions", false)
        .status
        .success());
    assert_eq!(starts(&scratch.0), 6);
    let builds = std::fs::read_dir(scratch.0.join("IntelligentTerminal\\acp-startup"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(builds.len(), 1);
    let ledgers = std::fs::read_dir(&builds[0])
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(
        ledgers.len(),
        1,
        "both diagnostics must use the same target"
    );
    let mut saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&ledgers[0]).unwrap()).unwrap();
    saved["cleanup_failed"] = true.into();
    saved["last_failure"]["phase"] = "cleanup".into();
    std::fs::write(&ledgers[0], serde_json::to_vec(&saved).unwrap()).unwrap();

    for verb in ["probe-sessions", "probe-host-sessions"] {
        for retry in [false, true] {
            let output = run_probe(&root, &scratch.0, verb, retry);
            assert!(!output.status.success(), "{verb} retry={retry}: {output:?}");
            assert_eq!(
                starts(&scratch.0),
                6,
                "quarantine must prevent every launch"
            );
        }
    }
    let requests = std::fs::read_to_string(scratch.0.join("requests.log")).unwrap();
    assert!(!requests.contains("|method|session/new"));
    assert!(!requests.contains("|method|session/prompt"));
    assert!(!requests.contains("|method|authenticate"));
}

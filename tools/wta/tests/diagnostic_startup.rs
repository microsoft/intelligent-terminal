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

fn fixture() -> (PathBuf, Scratch) {
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
    (root, scratch)
}

#[test]
fn diagnostic_probes_do_not_persist_startup_failures() {
    let (root, scratch) = fixture();
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
        4,
        "each diagnostic owns its startup attempt"
    );
    assert!(!scratch.0.join("IntelligentTerminal\\acp-startup").exists());

    std::fs::write(&control, r#"{"mode":"ready"}"#).unwrap();
    for verb in ["probe-sessions", "probe-host-sessions"] {
        let output = run_probe(&root, &scratch.0, verb, false);
        assert!(output.status.success(), "{verb}: {output:?}");
    }
    assert_eq!(starts(&scratch.0), 6);
    let recovered = run_probe(&root, &scratch.0, "probe-sessions", true);
    assert!(recovered.status.success(), "{recovered:?}");
    let result: serde_json::Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(result["list_sessions_ok"], false);
    assert_eq!(result["sessions"], serde_json::json!([]));
    assert_eq!(starts(&scratch.0), 7);
    let host = run_probe(&root, &scratch.0, "probe-host-sessions", false);
    assert!(host.status.success(), "{host:?}");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&host.stdout).unwrap(),
        serde_json::json!([])
    );
    assert_eq!(starts(&scratch.0), 8);
    assert!(!scratch.0.join("IntelligentTerminal\\acp-startup").exists());
    let requests = std::fs::read_to_string(scratch.0.join("requests.log")).unwrap();
    assert!(!requests.contains("|method|session/new"));
    assert!(!requests.contains("|method|session/prompt"));
    assert!(!requests.contains("|method|authenticate"));
}

#[test]
fn model_probe_failure_budget_is_per_invocation() {
    let (root, scratch) = fixture();
    let control = scratch.0.join("startup.json");
    std::fs::write(&control, r#"{"mode":"fail"}"#).unwrap();
    for invocation in 1..=2 {
        let output = run_probe(&root, &scratch.0, "probe-models", false);
        assert!(!output.status.success(), "{output:?}");
        let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(failure["startup_failure"]["attempts"], 3);
        assert_eq!(starts(&scratch.0), invocation * 3);
    }
    std::fs::write(&control, r#"{"mode":"ready"}"#).unwrap();
    let output = run_probe(&root, &scratch.0, "probe-models", false);
    assert!(output.status.success(), "{output:?}");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        result["available_models"][0]["id"],
        "lifecycle-fixture-model"
    );
    assert_eq!(starts(&scratch.0), 7);
    assert!(!scratch.0.join("IntelligentTerminal\\acp-startup").exists());
}

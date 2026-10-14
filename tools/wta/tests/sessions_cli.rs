#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn sessions_list_missing_master_preserves_machine_readable_failure_contracts() {
    let id = uuid::Uuid::new_v4();
    let scratch = Scratch(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("sessions-cli-{id}")),
    );
    std::fs::create_dir_all(&scratch.0).expect("create isolated CLI runtime directory");
    let pipe = format!(r"\\.\pipe\wta-sessions-cli-missing-{id}");

    for (include_status, expected_stdout) in [
        (
            true,
            b"{\"error\":{\"kind\":\"master_unavailable\"}}\n".as_slice(),
        ),
        (false, b"".as_slice()),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wta"));
        command
            .args(["sessions", "list", "--json", "--master", &pipe])
            .current_dir(&scratch.0)
            // Keep runtime-path resolution and log housekeeping away from user data.
            .env("LOCALAPPDATA", &scratch.0)
            .env("APPDATA", &scratch.0);
        if include_status {
            command.arg("--include-status");
        }
        let output = command.output().expect("run built wta sessions CLI");

        assert!(
            output.status.code().is_some_and(|code| code != 0),
            "include_status={include_status}: {output:?}"
        );
        assert_eq!(
            output.stdout, expected_stdout,
            "include_status={include_status}: {output:?}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("wta-master not running"),
            "include_status={include_status}: {output:?}"
        );
    }
}

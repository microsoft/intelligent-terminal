#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

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

async fn run_isolated_cli(scratch: &Scratch, args: &[&str]) -> std::process::Output {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_wta"));
    command
        .args(args)
        .current_dir(&scratch.0)
        .env("LOCALAPPDATA", &scratch.0)
        .env("APPDATA", &scratch.0)
        .env("WTA_LOG", "cli_pipe=debug")
        .kill_on_drop(true);
    tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .expect("session CLI exceeded its deadline")
        .expect("run built session CLI")
}

async fn wait_for_missing_pipe_retry(scratch: &Scratch) {
    let logs = scratch.0.join("IntelligentTerminal").join("logs");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(mut files) = tokio::fs::read_dir(&logs).await {
                while let Some(file) = files.next_entry().await.unwrap() {
                    if let Ok(contents) = tokio::fs::read_to_string(file.path()).await {
                        if contents.contains("named pipe temporarily unavailable; retrying") {
                            return;
                        }
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("built CLI did not report its missing-pipe attempt");
}

async fn serve_mock_master(
    mut server: NamedPipeServer,
    pipe: String,
    owner: &'static str,
    connections: usize,
) -> Vec<String> {
    let mut methods = Vec::new();
    for connection in 0..connections {
        tokio::time::timeout(Duration::from_secs(10), server.connect())
            .await
            .expect("mock master did not receive its bounded CLI connection")
            .unwrap();
        let mut reader = BufReader::new(&mut server);
        loop {
            let mut line = String::new();
            assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
            let request: Value = serde_json::from_str(&line).unwrap();
            if request.get("id").is_none() {
                continue;
            }
            let method = request["method"].as_str().unwrap();
            let result = if method == "initialize" {
                json!({"protocolVersion": 1, "agentCapabilities": {}, "authMethods": []})
            } else if method.ends_with("/sessions/list") {
                json!({
                    "sessions": [{"session_id": owner, "cwd": "C:\\fixture", "title": owner}],
                    "history_status": "ready"
                })
            } else {
                assert!(
                    method.ends_with("/session/activate")
                        || method.ends_with("/session/activation_status"),
                    "{method}"
                );
                assert_eq!(request["params"]["window_id"], 42);
                assert_eq!(request["params"]["activation_id"], "original-activation");
                json!({
                    "activation_id": request["params"]["activation_id"],
                    "state": "complete", "action": "focus", "accepted": true, "detail": owner
                })
            };
            let response = json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
            reader
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
            if method != "initialize" {
                methods.push(method.to_string());
                let mut trailing = String::new();
                let closed =
                    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut trailing))
                        .await
                        .expect("session client did not close its connection");
                match closed {
                    Ok(0) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
                    other => panic!("unexpected data after session response: {other:?}"),
                }
                break;
            }
        }
        drop(reader);
        server.disconnect().unwrap();
        if connection + 1 < connections {
            server = ServerOptions::new().create(&pipe).unwrap();
        }
    }
    methods
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_owner_fails_but_a_later_owned_snapshot_can_succeed() {
    let id = uuid::Uuid::new_v4();
    let scratch = Scratch(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("sessions-later-owner-{id}")),
    );
    std::fs::create_dir_all(&scratch.0).unwrap();
    let unavailable = run_isolated_cli(
        &scratch,
        &[
            "sessions",
            "list",
            "--master",
            "",
            "--json",
            "--include-status",
        ],
    )
    .await;
    assert!(!unavailable.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&unavailable.stdout).unwrap()["error"]["kind"],
        "master_unavailable"
    );

    let pipe = format!(r"\\.\pipe\wta-master-later-{id}");
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe)
        .unwrap();
    let args = [
        "sessions",
        "list",
        "--master",
        &pipe,
        "--json",
        "--include-status",
    ];
    let (available, methods) = tokio::join!(
        run_isolated_cli(&scratch, &args),
        serve_mock_master(server, pipe.clone(), "later-owner", 1),
    );
    assert!(available.status.success(), "{available:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&available.stdout).unwrap()["sessions"][0]["session_id"],
        "later-owner"
    );
    assert_eq!(methods.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_snapshot_waits_for_creation_and_same_endpoint_restart() {
    let id = uuid::Uuid::new_v4();
    let pipe = format!(r"\\.\pipe\wta-master-readiness-{id}");
    for owner in ["first-generation", "replacement-generation"] {
        let scratch = Scratch(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!("sessions-readiness-{id}-{owner}")),
        );
        std::fs::create_dir_all(&scratch.0).unwrap();
        // Neither the first listener nor its replacement exists when this request starts.
        assert_eq!(
            tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&pipe)
                .unwrap_err()
                .raw_os_error(),
            Some(2)
        );
        let args = [
            "sessions",
            "list",
            "--master",
            &pipe,
            "--json",
            "--include-status",
        ];
        let (snapshot, methods) = tokio::join!(run_isolated_cli(&scratch, &args), async {
            // Observe an actual failed open in the built CLI before creating the listener.
            wait_for_missing_pipe_retry(&scratch).await;
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe)
                .unwrap();
            serve_mock_master(server, pipe.clone(), owner, 1).await
        },);
        assert!(snapshot.status.success(), "{snapshot:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&snapshot.stdout).unwrap()["sessions"][0]["session_id"],
            owner
        );
        assert_eq!(methods.len(), 1);
        // serve_mock_master has dropped its listener before the next generation starts.
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_activation_and_receipts_keep_their_owner_after_another_master_exits() {
    let id = uuid::Uuid::new_v4();
    let scratch = Scratch(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("sessions-owner-{id}")),
    );
    let state = scratch.0.join("IntelligentTerminal");
    std::fs::create_dir_all(&state).unwrap();
    let normal_pipe = format!(r"\\.\pipe\wta-master-normal-{id}");
    let admin_pipe = format!(r"\\.\pipe\wta-master-admin-{id}");
    let normal_server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&normal_pipe)
        .unwrap();
    let admin_server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&admin_pipe)
        .unwrap();
    let normal = tokio::spawn(serve_mock_master(
        normal_server,
        normal_pipe.clone(),
        "normal",
        3,
    ));
    let admin = tokio::spawn(serve_mock_master(
        admin_server,
        admin_pipe.clone(),
        "admin",
        1,
    ));

    // Reproduce the address left by another instance, without touching real package state.
    std::fs::write(state.join("master-pipe.txt"), &admin_pipe).unwrap();
    let admin_list = run_isolated_cli(
        &scratch,
        &[
            "sessions",
            "list",
            "--master",
            &admin_pipe,
            "--json",
            "--include-status",
        ],
    )
    .await;
    assert!(admin_list.status.success(), "{admin_list:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&admin_list.stdout).unwrap()["sessions"][0]["session_id"],
        "admin"
    );
    let admin_methods = tokio::time::timeout(Duration::from_secs(5), admin)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(admin_methods.len(), 1);

    let normal_list = run_isolated_cli(
        &scratch,
        &[
            "sessions",
            "list",
            "--master",
            &normal_pipe,
            "--json",
            "--include-status",
        ],
    )
    .await;
    assert!(normal_list.status.success(), "{normal_list:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&normal_list.stdout).unwrap()["sessions"][0]["session_id"],
        "normal"
    );

    let mut activation_args = vec![
        "sessions",
        "activate",
        "--master",
        &normal_pipe,
        "--session-id",
        "normal",
        "--provider",
        "copilot",
        "--location",
        "host",
        "--window-id",
        "42",
        "--activation-id",
        "original-activation",
        "--json",
    ];
    for status_only in [false, true] {
        if status_only {
            activation_args.push("--status-only");
        }
        let result = run_isolated_cli(&scratch, &activation_args).await;
        assert!(result.status.success(), "{result:?}");
        let receipt: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(receipt["detail"], "normal");
        assert_eq!(receipt["activation_id"], "original-activation");
        assert_eq!(receipt["accepted"], true);
    }
    let methods = tokio::time::timeout(Duration::from_secs(5), normal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(methods.len(), 3);
    assert!(methods[1].ends_with("/session/activate"));
    assert!(methods[2].ends_with("/session/activation_status"));

    let unscoped = run_isolated_cli(&scratch, &["sessions", "list", "--json"]).await;
    assert!(!unscoped.status.success());
    assert!(String::from_utf8_lossy(&unscoped.stderr).contains("--master"));
}

use super::*;
use clap::Parser;
use std::io::{Read, Write};
use std::process::Command;

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn profile_cli_contract_requires_id_and_delimits_extra_args() {
    use crate::cli::args::{Cli, Command};
    let cli = Cli::try_parse_from([
        "wta",
        "launch-agent",
        "--agent-id",
        "claude",
        "--model",
        "sonnet",
        "--permission-mode",
        "plan",
        "--",
        "--resume",
        "opaque session",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Some(Command::LaunchAgent { agent_id, model, permission_mode, additional_args })
            if agent_id == "claude"
                && model.as_deref() == Some("sonnet")
                && permission_mode.as_deref() == Some("plan")
                && additional_args == ["--resume", "opaque session"]
    ));
    assert!(Cli::try_parse_from(["wta", "launch-agent"]).is_err());
    assert!(
        Cli::try_parse_from(["wta", "launch-agent", "--agent-id", "codex", "--model"]).is_err()
    );
    assert!(matches!(
        Cli::try_parse_from(["wta", "probe-profile-agents"])
            .unwrap()
            .command,
        Some(Command::ProbeProfileAgents)
    ));
}

#[test]
fn defaults_leave_native_settings_untouched_and_models_are_atomic() {
    for profile in KNOWN_AGENTS {
        assert!(launch_args(profile.id, None, None, &[]).unwrap().is_empty());
        assert!(launch_args(profile.id, Some(""), Some(""), &[])
            .unwrap()
            .is_empty());
        assert_eq!(
            launch_args(profile.id, Some("provider/model with spaces"), None, &[]).unwrap(),
            ["--model", "provider/model with spaces"]
        );
        assert!(launch_args(profile.id, Some("--allow-all"), None, &[]).is_err());
    }
    for id in ["", "unknown", "custom:copilot", "Copilot", "claude.exe"] {
        assert!(launch_args(id, None, None, &[]).is_err());
    }
}

#[test]
fn permission_modes_are_provider_native_not_universal() {
    for (id, mode, expected) in [
        ("copilot", "allow-all-tools", vec!["--allow-all-tools"]),
        ("copilot", "allow-all", vec!["--allow-all"]),
        ("claude", "plan", vec!["--permission-mode", "plan"]),
        ("claude", "manual", vec!["--permission-mode", "manual"]),
        (
            "claude",
            "acceptEdits",
            vec!["--permission-mode", "acceptEdits"],
        ),
        ("claude", "auto", vec!["--permission-mode", "auto"]),
        ("claude", "dontAsk", vec!["--permission-mode", "dontAsk"]),
        (
            "claude",
            "bypassPermissions",
            vec!["--permission-mode", "bypassPermissions"],
        ),
        (
            "codex",
            "untrusted",
            vec!["--ask-for-approval", "untrusted"],
        ),
        (
            "codex",
            "on-request",
            vec!["--ask-for-approval", "on-request"],
        ),
        ("codex", "never", vec!["--ask-for-approval", "never"]),
        ("gemini", "default", vec!["--approval-mode", "default"]),
        ("gemini", "auto_edit", vec!["--approval-mode", "auto_edit"]),
        ("gemini", "yolo", vec!["--approval-mode", "yolo"]),
        ("gemini", "plan", vec!["--approval-mode", "plan"]),
        ("opencode", "auto", vec!["--auto"]),
    ] {
        assert_eq!(permission_args(id, mode).unwrap(), expected);
    }
    for profile in KNOWN_AGENTS {
        assert!(permission_args(profile.id, "readOnly").is_err());
        assert!(permission_args(profile.id, "invented-mode").is_err());
    }
    assert!(permission_args("codex", "read-only").is_err());
    assert!(permission_args("opencode", "plan").is_err());
    assert!(permission_args("claude", "default").is_err());
}

#[test]
fn additional_arguments_cannot_replace_settings_or_launch_other_modes() {
    for profile in KNOWN_AGENTS {
        for args in [
            vec!["--model", "other"],
            vec!["--model=other"],
            vec!["-mother"],
            vec!["--permission-mode=auto"],
            vec!["--approval-mode", "yolo"],
            vec!["--allow-all"],
            vec!["--auto"],
            vec!["--yolo"],
            vec!["--config", "approval_policy=never"],
            vec!["-capproval_policy=never"],
            vec!["--settings", "{}"],
            vec!["--profile", "unsafe"],
            vec!["--dangerously-skip-permissions"],
            vec!["--dangerously-bypass-approvals-and-sandbox"],
            vec!["--acp"],
            vec!["login"],
            vec!["plugin", "install"],
            vec!["--", "--model", "other"],
            vec!["--help=--model"],
            vec!["--unreviewed-option"],
        ] {
            assert!(
                launch_args(profile.id, Some("chosen"), None, &strings(&args)).is_err(),
                "{} {args:?}",
                profile.id
            );
        }
        assert_eq!(
            launch_args(profile.id, None, None, &strings(&["--version"])).unwrap(),
            ["--version"]
        );
    }
    assert!(validate_extra_args("claude", &strings(&["--resume", "session id"])).is_ok());
    assert!(validate_extra_args("claude", &strings(&["--resume=--model"])).is_err());
    assert!(validate_extra_args("claude", &strings(&["--add-dir"])).is_err());
    assert!(validate_extra_args("claude", &strings(&["--add-dir", "--model"])).is_err());
}

#[test]
fn native_resolution_prefers_exe_and_accepts_claude_cmd_without_npx() {
    let path = OsStr::new(r"C:\early;C:\late");
    let claude = profile("claude").unwrap();
    assert_eq!(
        find_in_path(claude, path, |p| Ok([
            Path::new(r"C:\early\claude.cmd"),
            Path::new(r"C:\late\claude.exe")
        ]
        .contains(&p)))
        .unwrap(),
        Some(PathBuf::from(r"C:\late\claude.exe"))
    );
    assert_eq!(
        find_in_path(claude, path, |p| Ok(p == Path::new(r"C:\early\claude.cmd"))).unwrap(),
        Some(PathBuf::from(r"C:\early\claude.cmd"))
    );
    assert!(find_in_path(claude, OsStr::new(""), |_| Ok(true))
        .unwrap()
        .is_none());
    assert!(find_in_path(claude, path, |p| Ok(
        p.extension() == Some(OsStr::new("ps1"))
    ))
    .unwrap()
    .is_none());
    assert!(find_in_path(claude, path, |_| Err(
        std::io::ErrorKind::PermissionDenied.into()
    ))
    .is_err());
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("native-profile-tests-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn discovery_reports_only_native_availability_and_allowed_ids() {
    let scratch = Scratch::new();
    std::fs::write(scratch.0.join("claude.cmd"), "").unwrap();
    std::fs::write(scratch.0.join("copilot.exe"), "").unwrap();
    let path = scratch.0.as_os_str();
    let all = discovery(path, &policy::Policy::default()).unwrap();
    assert_eq!(
        all,
        serde_json::json!({"agents": [
            {"id": "copilot", "display_name": "GitHub Copilot"},
            {"id": "claude", "display_name": "Claude"}
        ]})
    );
    let filtered = policy::Policy {
        allowed_agents: Some(vec!["CLAUDE".to_owned()]),
        automatic_approval_blocked: false,
    };
    assert_eq!(
        discovery(path, &filtered).unwrap(),
        serde_json::json!({"agents": [{"id": "claude", "display_name": "Claude"}]})
    );
    assert_eq!(
        discovery(OsStr::new(""), &policy::Policy::default()).unwrap(),
        serde_json::json!({"agents": []})
    );
}

#[test]
fn unsafe_batch_expansion_is_rejected_but_native_argv_is_not_shell_parsed() {
    for value in ["%PATH%", "!PATH!", "a\r\nb", "a\"b", "a^b"] {
        assert!(child_command(Path::new(r"C:\agent.cmd"), &strings(&[value])).is_err());
        assert!(child_command(Path::new(r"C:\agent.exe"), &strings(&[value])).is_ok());
    }
    assert!(child_command(Path::new(r"C:\%bad%\agent.cmd"), &[]).is_err());
    assert!(child_command(Path::new(r"C:\agent.cmd"), &strings(&["a & b", "日本語"])).is_ok());
}

const FIXTURE: &str = "cli::native_agent::tests::process_fixture";

fn fixture_args() -> Vec<String> {
    strings(&["--exact", FIXTURE, "--nocapture"])
}

/// Invoked only in child test processes; no native agent/network/auth is used.
#[test]
fn process_fixture() {
    let Ok(mode) = std::env::var("WTA_NATIVE_TEST_MODE") else {
        return;
    };
    if mode == "wrapper" || mode == "control-wrapper" {
        install_console_handler().unwrap();
        install_descendant_job().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let code = runtime.block_on(async {
            let mut command =
                child_command(&std::env::current_exe().unwrap(), &fixture_args()).unwrap();
            command.env(
                "WTA_NATIVE_TEST_MODE",
                if mode == "wrapper" {
                    "child"
                } else {
                    "control-child"
                },
            );
            command
                .spawn()
                .unwrap()
                .wait()
                .await
                .unwrap()
                .code()
                .unwrap()
        });
        crate::logging::shutdown_flush();
        std::process::exit(code);
    }
    if mode == "control-child" {
        use std::sync::atomic::{AtomicBool, Ordering};
        use windows_sys::Win32::System::Console::{
            GenerateConsoleCtrlEvent, SetConsoleCtrlHandler, CTRL_C_EVENT,
        };
        static RECEIVED: AtomicBool = AtomicBool::new(false);
        unsafe extern "system" fn handler(event: u32) -> i32 {
            if event == CTRL_C_EVENT {
                RECEIVED.store(true, Ordering::SeqCst);
                return 1;
            }
            0
        }
        // This fixture runs in a NEW console, never the test runner's console.
        // Its event reaches only the fixture wrapper and this owned child.
        unsafe {
            assert_ne!(SetConsoleCtrlHandler(Some(handler), 1), 0);
            assert_ne!(GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0), 0);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !RECEIVED.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            RECEIVED.load(Ordering::SeqCst),
            "child must not inherit Ctrl+C-ignore"
        );
        println!("NATIVE_CONTROL_RECEIVED");
        crate::logging::shutdown_flush();
        std::process::exit(37);
    }
    if mode == "tree-wrapper" {
        use std::io::BufRead;
        install_descendant_job().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(fixture_args())
            .env("WTA_NATIVE_TEST_MODE", "tree-leaf")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines() {
            let line = line.unwrap();
            if line.starts_with("NATIVE_LEAF_PID:") {
                println!("{line}");
                std::io::stdout().flush().unwrap();
                break;
            }
        }
        // Parent terminates this exact wrapper PID; the job must reap the leaf.
        std::thread::sleep(std::time::Duration::from_secs(60));
        panic!("test parent did not terminate tree wrapper");
    }
    if mode == "tree-leaf" {
        println!("NATIVE_LEAF_PID:{}", std::process::id());
        std::io::stdout().flush().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    println!(
        "NATIVE_FIXTURE:{}",
        serde_json::json!({
            "argv": std::env::args().collect::<Vec<_>>(),
            "cwd": std::env::current_dir().unwrap(),
            "wt_session": std::env::var("WT_SESSION").unwrap_or_default(),
            "wt_clsid": std::env::var("WT_COM_CLSID").unwrap_or_default(),
            "input": input
        })
    );
    eprintln!("NATIVE_STDERR");
    crate::logging::shutdown_flush();
    std::process::exit(37);
}

fn run_fixture(mut command: Command) -> (std::process::Output, serde_json::Value) {
    command
        .env("WTA_NATIVE_TEST_MODE", "child")
        .env("WT_SESSION", "unchanged-pane")
        .env("WT_COM_CLSID", "unchanged-com")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"inherited input")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    let payload = text
        .lines()
        .find_map(|line| line.strip_prefix("NATIVE_FIXTURE:"))
        .unwrap_or_else(|| panic!("missing fixture output: {text}"));
    let value = serde_json::from_str(payload).unwrap();
    (output, value)
}

#[test]
fn native_process_argv_roundtrips_and_preserves_exit_code() {
    let mut args = fixture_args();
    args.push("--".to_owned());
    let extra = strings(&[
        "with spaces",
        "日本語",
        "a&b",
        "%PATH%",
        "a\"b",
        r"C:\trailing\",
    ]);
    args.extend(extra.clone());
    let command = child_command(&std::env::current_exe().unwrap(), &args).unwrap();
    let (output, value) = run_fixture(command.into_std());
    assert_eq!(output.status.code(), Some(37));
    let argv: Vec<String> = serde_json::from_value(value["argv"].clone()).unwrap();
    assert_eq!(&argv[argv.len() - extra.len()..], extra);
    assert_eq!(value["wt_session"], "unchanged-pane");
    assert_eq!(value["wt_clsid"], "unchanged-com");
    assert_eq!(value["input"], "inherited input");
    assert!(String::from_utf8_lossy(&output.stderr).contains("NATIVE_STDERR"));
}

#[test]
fn batch_shim_preserves_spaces_unicode_and_metacharacters() {
    let scratch = Scratch::new();
    let script = scratch.0.join("native agent.cmd");
    std::fs::write(
        &script,
        format!(
            "@echo off\r\n\"{}\" --exact {FIXTURE} --nocapture -- %*\r\n",
            std::env::current_exe().unwrap().display()
        ),
    )
    .unwrap();
    let extra = strings(&[
        "with spaces",
        "日本語",
        "a&b",
        "a|b",
        "a>b",
        "(parentheses)",
        r"C:\trailing\",
    ]);
    let command = child_command(&script, &extra).unwrap();
    let (output, value) = run_fixture(command.into_std());
    assert_eq!(output.status.code(), Some(37));
    let argv: Vec<String> = serde_json::from_value(value["argv"].clone()).unwrap();
    assert_eq!(&argv[argv.len() - extra.len()..], extra);
}

#[test]
fn wrapper_inherits_streams_environment_cwd_and_child_status() {
    let scratch = Scratch::new();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(fixture_args())
        .env("WTA_NATIVE_TEST_MODE", "wrapper")
        .env("WT_SESSION", "pane-through-wrapper")
        .env("WT_COM_CLSID", "com-through-wrapper")
        .current_dir(&scratch.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"through wrapper")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(37));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("pane-through-wrapper"));
    assert!(stdout.contains("com-through-wrapper"));
    assert!(stdout.contains("through wrapper"));
    let expected_cwd = serde_json::to_string(&scratch.0).unwrap();
    assert!(stdout.contains(&expected_cwd));
    assert!(String::from_utf8_lossy(&output.stderr).contains("NATIVE_STDERR"));
}

#[test]
fn ctrl_c_reaches_child_without_terminating_wrapper() {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NEW_CONSOLE;
    let output = Command::new(std::env::current_exe().unwrap())
        .args(fixture_args())
        .env("WTA_NATIVE_TEST_MODE", "control-wrapper")
        .creation_flags(CREATE_NEW_CONSOLE)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(37), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("NATIVE_CONTROL_RECEIVED"));
}

#[test]
fn terminating_wrapper_reaps_only_owned_descendants() {
    use std::io::BufRead;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    let mut wrapper = Command::new(std::env::current_exe().unwrap())
        .args(fixture_args())
        .env("WTA_NATIVE_TEST_MODE", "tree-wrapper")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = std::io::BufReader::new(wrapper.stdout.take().unwrap())
        .lines()
        .find_map(|line| {
            line.unwrap()
                .strip_prefix("NATIVE_LEAF_PID:")
                .map(|pid| pid.parse::<u32>().unwrap())
        })
        .unwrap();
    // SAFETY: the PID was reported by our own child. Retain a wait-only handle
    // before terminating the wrapper so PID reuse cannot affect this assertion.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(!handle.is_null());
    let leaf = unsafe { OwnedHandle::from_raw_handle(handle) };
    wrapper.kill().unwrap();
    wrapper.wait().unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(leaf.as_raw_handle(), 5000) },
        WAIT_OBJECT_0
    );
}

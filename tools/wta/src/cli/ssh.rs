//! Foreground OpenSSH remains the terminal's only interactive I/O owner.

use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

use anyhow::{Context, Result};

use crate::coordinator::sh_quote;
use crate::ssh_hook_protocol::{
    Registration, Request, Response, RouteId, TrackingStatus, HEARTBEAT_INTERVAL,
};
use crate::ssh_sessions::SshTarget;

fn foreground_arguments(
    target: &SshTarget,
    no_pty: bool,
    route: Option<RouteId>,
    remote_command: Option<&str>,
) -> Result<Vec<String>> {
    if let Some(script) = remote_command {
        anyhow::ensure!(
            !script.contains('\0') && script.len() <= 16 * 1024,
            "Invalid SSH remote command"
        );
    }
    if route.is_none() && remote_command.is_none() {
        return Ok(login_arguments(target, no_pty));
    }
    let mut bootstrap =
        "unset IT_SSH_HOOK_ROUTE IT_SSH_HOOK_SOCKET IT_SSH_HOOK_SESSION; ".to_owned();
    if let Some(route) = route {
        bootstrap.push_str(&format!(
            "it_hook_home=$(getent passwd \"$(id -u)\" | cut -d: -f6); case \"$it_hook_home\" in /*) export IT_SSH_HOOK_ROUTE={}; export IT_SSH_HOOK_SOCKET=\"$it_hook_home/.intelligent-terminal/run/tmux-hooks.sock\"; export IT_SSH_HOOK_SESSION=it-hooks;; *) printf '%s\\n' 'wta ssh: canonical login home unavailable; hooks disabled' >&2;; esac; ",
            sh_quote(&route.to_string()),
        ));
    } else {
        bootstrap.push_str("export WTA_TMUX_HOOKS_DISABLED=1; ");
    }
    if let Some(script) = remote_command {
        bootstrap.push_str(&format!("exec sh -lc {}", sh_quote(script)));
    } else {
        bootstrap.push_str("exec \"${SHELL:-/bin/sh}\" -l");
    }
    let mut args = if remote_command.is_some() {
        // Explicit session resume retains the established strict/noninteractive
        // transport. Only an ordinary login inherits foreground authentication.
        let mut args = crate::ssh_sessions::ssh_arguments(target, !no_pty, &bootstrap);
        args.pop();
        args
    } else {
        login_arguments(target, no_pty)
    };
    args.push(format!("sh -c {}", sh_quote(&bootstrap)));
    Ok(args)
}

fn login_arguments(target: &SshTarget, no_pty: bool) -> Vec<String> {
    let mut args = vec![if no_pty { "-T" } else { "-t" }.to_owned()];
    if let Some(port) = target.port() {
        args.extend(["-p".to_owned(), port.to_string()]);
    }
    args.extend(["--".to_owned(), target.destination().to_owned()]);
    args
}

fn foreground_environment(
    command: &mut tokio::process::Command,
    environment: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    resume: bool,
) {
    if resume {
        crate::ssh_sessions::configure_ssh_environment(command, environment);
        return;
    }
    // Preserve TERM, SSH_ASKPASS, agent/authentication and user SendEnv setup.
    // Private terminal/MCP routing is not part of a foreground SSH environment.
    for (name, _) in environment {
        let upper = name.to_string_lossy().to_ascii_uppercase();
        if upper.starts_with("WT_")
            || upper.starts_with("WTA_")
            || upper.starts_with("IT_SSH_HOOK_")
        {
            command.env_remove(name);
        }
    }
}

fn check_effective_configuration(configuration: &str) -> Result<()> {
    let read = |key: &str| -> Result<Option<&str>> {
        let mut matches = configuration
            .lines()
            .filter_map(|line| line.strip_prefix(key));
        let value = matches.next();
        anyhow::ensure!(
            matches.next().is_none(),
            "OpenSSH effective configuration is ambiguous"
        );
        Ok(value.map(str::trim))
    };
    // Windows OpenSSH omits unset RemoteCommand from -G output, whereas
    // explicitly configured values are printed. Other required fields remain
    // mandatory so truncated configuration output cannot enable wrapping.
    anyhow::ensure!(
        read("remotecommand ")?.is_none_or(|value| value == "none"),
        "Configured RemoteCommand requires the original, untracked SSH login"
    );
    anyhow::ensure!(
        read("sessiontype ")? == Some("default"),
        "Configured SSH session type requires the original, untracked login"
    );
    anyhow::ensure!(
        read("forkafterauthentication ")? == Some("no"),
        "Backgrounding SSH configuration requires the original, untracked login"
    );
    Ok(())
}

async fn check_login_configuration(target: &SshTarget, no_pty: bool) -> Result<()> {
    const MAX_CONFIG: u64 = 256 * 1024;
    let mut command = tokio::process::Command::new(crate::ssh_sessions::system_ssh_executable()?);
    command
        .arg("-G")
        .args(login_arguments(target, no_pty))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    foreground_environment(&mut command, std::env::vars_os(), false);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .context("Inspect effective OpenSSH configuration")?;
    let mut output = child
        .stdout
        .take()
        .context("OpenSSH configuration output unavailable")?
        .take(MAX_CONFIG + 1);
    let inspected = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).await?;
        anyhow::ensure!(
            bytes.len() <= MAX_CONFIG as usize,
            "OpenSSH effective configuration exceeds limit"
        );
        anyhow::ensure!(
            child.wait().await?.success(),
            "OpenSSH effective configuration query failed"
        );
        let configuration =
            std::str::from_utf8(&bytes).context("OpenSSH effective configuration is not UTF-8")?;
        check_effective_configuration(configuration)
    })
    .await;
    match inspected {
        Ok(Ok(())) => Ok(()),
        result => {
            if child.try_wait()?.is_none() {
                tokio::time::timeout(Duration::from_secs(3), child.kill())
                    .await
                    .context("Reaping OpenSSH configuration probe timed out")??;
            }
            result.context("OpenSSH configuration query timed out")?
        }
    }
}

fn linux_probe_script(nonce: &str) -> String {
    format!(
        "test \"$(uname -s)\" = Linux && command -v getent >/dev/null && printf '%s\\n' {}",
        sh_quote(&format!("IT_SSH_LINUX/1 {nonce}"))
    )
}

fn linux_probe_succeeded(output: &[u8], nonce: &str) -> bool {
    let expected = format!("IT_SSH_LINUX/1 {nonce}");
    std::str::from_utf8(output)
        .ok()
        .is_some_and(|text| text.lines().any(|line| line == expected))
}

async fn check_remote_linux(target: &SshTarget) -> Result<()> {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let mut command = tokio::process::Command::new(crate::ssh_sessions::system_ssh_executable()?);
    command
        .args(crate::ssh_sessions::ssh_arguments(
            target,
            false,
            &linux_probe_script(&nonce),
        ))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    crate::ssh_sessions::configure_ssh_environment(&mut command, std::env::vars_os());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let mut child = command
        .spawn()
        .context("Probe remote SSH shell compatibility")?;
    let mut output = child
        .stdout
        .take()
        .context("SSH compatibility output unavailable")?
        .take(4097);
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).await?;
        anyhow::ensure!(
            bytes.len() <= 4096,
            "SSH compatibility output exceeds limit"
        );
        anyhow::ensure!(
            child.wait().await?.success() && linux_probe_succeeded(&bytes, &nonce),
            "Remote SSH shell is not confirmed Linux"
        );
        Ok(())
    })
    .await;
    if child.try_wait()?.is_none() {
        tokio::time::timeout(Duration::from_secs(3), child.kill())
            .await
            .context("Reaping SSH compatibility probe timed out")??;
    }
    result.context("SSH compatibility probe timed out")?
}

async fn request(request: &Request) -> Result<TrackingStatus> {
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        super::sessions::request_from_master(
            None,
            crate::ssh_hook_protocol::build_request(request)?,
        ),
    )
    .await
    .context("SSH hook registry request timed out")??;
    let response: Response =
        serde_json::from_str(response.0.get()).context("Invalid SSH hook registry response")?;
    Ok(response.status)
}

fn warning(reason: &str) {
    // These are operational SSH diagnostics, never remote hook payloads.
    eprintln!("wta ssh: status tracking unavailable: {reason}");
    tracing::warn!(target: "ssh_hooks", reason, "SSH status tracking unavailable; foreground login continues");
}

fn report_status(status: &TrackingStatus, previous: &mut Option<TrackingStatus>) {
    if previous.as_ref() == Some(status) {
        return;
    }
    if matches!(status, TrackingStatus::Connecting) && previous.is_some() {
        return;
    }
    match status {
        TrackingStatus::Unavailable { reason } => warning(reason),
        TrackingStatus::Partial {
            unavailable_providers,
        } => {
            let providers = unavailable_providers.join(", ");
            eprintln!(
                "wta ssh: hooks unavailable for {providers}; other providers remain connected"
            );
            tracing::warn!(target: "ssh_hooks", providers, "Some remote hook registrations are unavailable");
        }
        TrackingStatus::Connecting if previous.is_none() => {
            eprintln!("wta ssh: remote status setup is pending; foreground login continues");
        }
        _ => {}
    }
    *previous = Some(status.clone());
}

pub(crate) async fn run(
    target: SshTarget,
    no_pty: bool,
    no_hooks: bool,
    remote_command: Option<String>,
) -> Result<i32> {
    tokio::task::LocalSet::new().run_until(async move {
        let route = RouteId::new();
        let mut no_hooks = no_hooks || std::env::var_os("WTA_TMUX_HOOKS_DISABLED").is_some_and(|value| !value.is_empty());
        if !no_hooks && remote_command.is_none() && check_login_configuration(&target, no_pty).await.is_err() {
            warning("Configured RemoteCommand or unsupported OpenSSH behavior; preserving the original SSH login without tracking");
            no_hooks = true;
        }
        if !no_hooks && remote_command.is_none() && check_remote_linux(&target).await.is_err() {
            warning("Remote Linux compatibility could not be confirmed with trusted noninteractive SSH; preserving the original login without tracking");
            no_hooks = true;
        }
        let mut registration = std::env::var("WT_SESSION")
            .context("WT_SESSION is unavailable for the managed SSH pane")
            .and_then(|pane_id| {
                let mut registration = Registration {
                    route, target: target.clone(), pane_id,
                    wrapper_pid: std::process::id(), no_hooks,
                };
                registration.validate()?;
                Ok(registration)
            });
        let remote_route = (!no_hooks && registration.is_ok()).then_some(route);
        let arguments = foreground_arguments(&target, no_pty, remote_route, remote_command.as_deref())?;
        let executable = crate::ssh_sessions::system_ssh_executable()?;
        let mut last_status = None;
        if let Ok(registration) = &registration {
            let status = request(&Request::Register { registration: registration.clone() }).await
                .unwrap_or_else(|_| TrackingStatus::Unavailable {
                    reason: "Windows Terminal session registry is unavailable".to_owned(),
                });
            report_status(&status, &mut last_status);
        } else if !no_hooks {
            warning("native pane identity is unavailable");
        }
        let mut command = tokio::process::Command::new(executable);
        command.args(arguments)
            .stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
        foreground_environment(&mut command, std::env::vars_os(), remote_command.is_some());
        let mut child = match command.spawn().context("Launch Windows system OpenSSH") {
            Ok(child) => child,
            Err(error) => {
                if let Ok(registration) = &registration {
                    if request(&Request::Revoke { registration: registration.clone() }).await.is_err() {
                        warning("Failed SSH launch could not revoke its route; its lease will expire");
                    }
                }
                return Err(error);
            }
        };
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let result = loop {
            tokio::select! {
                status = child.wait() => break status.context("Wait for foreground SSH"),
                _ = heartbeat.tick() => {
                    if let Ok(registration) = &registration {
                        let status = request(&Request::Register { registration: registration.clone() }).await
                            .unwrap_or_else(|_| TrackingStatus::Unavailable {
                                reason: "Windows Terminal session registry is unavailable".to_owned(),
                            });
                        report_status(&status, &mut last_status);
                    }
                }
            }
        };
        if let Ok(registration) = registration.as_mut() {
            if request(&Request::Revoke { registration: registration.clone() }).await.is_err() {
                warning("SSH route revocation could not reach Windows Terminal; its lease will expire");
            }
        }
        Ok(result?.code().unwrap_or(255))
    }).await
}

pub(crate) async fn ensure_registered_targets() {
    let result = tokio::task::LocalSet::new()
        .run_until(request(&Request::Ensure))
        .await;
    match result {
        Ok(TrackingStatus::Unavailable { reason }) => warning(&reason),
        Ok(TrackingStatus::Partial {
            unavailable_providers,
        }) => {
            report_status(
                &TrackingStatus::Partial {
                    unavailable_providers,
                },
                &mut None,
            );
        }
        Err(_) => {
            tracing::warn!(target: "ssh_hooks", "Remote hook reconciliation could not reach the master; registered wrappers will retry");
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn ssh_linux_bootstrap_requires_a_successful_nonce_probe() {
        let script = linux_probe_script("nonce");
        assert!(script.contains("test \"$(uname -s)\" = Linux"));
        assert!(script.contains("IT_SSH_LINUX/1 nonce"));
        assert!(linux_probe_succeeded(b"IT_SSH_LINUX/1 nonce\n", "nonce"));
        assert!(!linux_probe_succeeded(b"sh: command not found\n", "nonce"));
        assert!(!linux_probe_succeeded(b"IT_SSH_LINUX/1 other\n", "nonce"));
        assert!(!linux_probe_succeeded(
            b"prefix IT_SSH_LINUX/1 nonce\n",
            "nonce"
        ));
        let target = SshTarget::new("windows-host", None).unwrap();
        let fallback = foreground_arguments(&target, false, None, None).unwrap();
        assert_eq!(fallback, vec!["-t", "--", "windows-host"]);
    }

    #[test]
    fn managed_ssh_cli_contract_parses_and_preserves_no_pty_optout() {
        let cli = crate::cli::args::Cli::try_parse_from([
            "wta",
            "ssh",
            "--destination",
            "user@alias",
            "--port",
            "2222",
            "--no-pty",
            "--no-hooks",
            "--remote-command",
            "cd '/home/a b' && exec copilot",
        ])
        .unwrap();
        assert!(matches!(cli.command, Some(crate::cli::args::Command::Ssh {
                    destination, port: Some(2222), no_pty: true, no_hooks: true, remote_command: Some(_),
                }) if destination == "user@alias"));
    }

    #[test]
    fn managed_ssh_arguments_leave_foreground_auth_to_openssh() {
        let target = SshTarget::new("user@alias", Some(2222)).unwrap();
        let route = RouteId::new();
        let args = foreground_arguments(&target, true, Some(route), None).unwrap();
        assert_eq!(args[0], "-T");
        assert!(!args
            .iter()
            .any(|arg| arg.contains("BatchMode") || arg.contains("StrictHostKeyChecking")));
        let bootstrap = args.last().unwrap();
        assert!(bootstrap.starts_with("sh -c "));
        assert!(bootstrap.contains("IT_SSH_HOOK_ROUTE="));
        assert!(bootstrap.contains(&route.to_string()));
        assert!(bootstrap.contains("getent passwd"));
        assert!(bootstrap.contains("$it_hook_home/.intelligent-terminal/run/tmux-hooks.sock"));
        assert!(!bootstrap.contains("export HOME="));
        assert!(!bootstrap.contains("mkdir"));
        assert!(!bootstrap.contains("tmux -C"));
        let disabled = foreground_arguments(&target, false, None, Some("exec cli")).unwrap();
        assert_eq!(disabled[0], "-t");
        assert!(disabled
            .last()
            .unwrap()
            .contains("WTA_TMUX_HOOKS_DISABLED=1"));
        assert!(!disabled.last().unwrap().contains(&route.to_string()));
        assert!(disabled.iter().any(|arg| arg == "BatchMode=yes"));
        assert!(disabled
            .iter()
            .any(|arg| arg == "StrictHostKeyChecking=yes"));
    }

    #[test]
    #[cfg(windows)]
    fn managed_ssh_resume_commandline_uses_the_encoded_source_launcher() {
        let target = SshTarget::new("alice@host", Some(2222)).unwrap();
        let line = crate::ssh_sessions::resume_commandline(
            &target,
            "copilot",
            "sid'quoted%PATH%",
            "/home/a b/%PATH%",
            true,
        )
        .unwrap();
        assert!(!line.contains('%'));
        let args = crate::ssh_sessions::tests::windows_argv(&line);
        let cli = crate::cli::args::Cli::try_parse_from(args).unwrap();
        let Some(crate::cli::args::Command::SshResume { payload }) = cli.command else {
            panic!("managed resumes must use the shared encoded launcher");
        };
        let request: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(request["target"]["destination"], "alice@host");
        assert_eq!(request["target"]["port"], 2222);
        assert_eq!(request["session_id"], "sid'quoted%PATH%");
        assert_eq!(request["cwd"], "/home/a b/%PATH%");
        assert_eq!(request["managed"], true);
    }

    #[test]
    fn managed_ssh_resume_child_does_not_forward_local_credentials() {
        let mut command = tokio::process::Command::new("ssh.exe");
        command.env("PREEXISTING_SECRET", "not-retained");
        foreground_environment(
            &mut command,
            [
                ("SystemRoot", r"C:\Windows"),
                ("SSH_AUTH_SOCK", "ssh-agent"),
                ("WT_SESSION", "native-pane"),
                ("IT_SSH_HOOK_ROUTE", "private-route"),
                ("OPENAI_API_KEY", "not-retained"),
                ("UNKNOWN_PROVIDER_SECRET", "not-retained"),
            ]
            .map(|(name, value)| (name.into(), value.into())),
            true,
        );
        let environment: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
        assert_eq!(environment.len(), 2);
        assert!(environment.contains_key(std::ffi::OsStr::new("SystemRoot")));
        assert!(environment.contains_key(std::ffi::OsStr::new("SSH_AUTH_SOCK")));
    }

    #[test]
    fn managed_ssh_configured_commands_fall_back_without_overriding_auth_or_remote_command() {
        let supported = "remotecommand none\nsessiontype default\nforkafterauthentication no\n";
        assert!(check_effective_configuration(supported).is_ok());
        assert!(
            check_effective_configuration("sessiontype default\nforkafterauthentication no\n")
                .is_ok()
        );
        for configuration in [
            supported.replace("remotecommand none", "remotecommand custom 'login command'"),
            supported.replace("sessiontype default", "sessiontype none"),
            supported.replace("forkafterauthentication no", "forkafterauthentication yes"),
            "remotecommand none\n".to_owned(),
            format!("{supported}remotecommand none\n"),
        ] {
            assert!(check_effective_configuration(&configuration).is_err());
        }
        let target = SshTarget::new("alias", None).unwrap();
        assert_eq!(
            foreground_arguments(&target, true, None, None).unwrap(),
            vec!["-T", "--", "alias"]
        );
        let mut command = tokio::process::Command::new("ssh.exe");
        command
            .env("TERM", "xterm-256color")
            .env("SSH_ASKPASS", "askpass.exe")
            .env("WT_SESSION", "private");
        foreground_environment(
            &mut command,
            [
                ("TERM", "xterm-256color"),
                ("SSH_ASKPASS", "askpass.exe"),
                ("WT_SESSION", "private"),
            ]
            .map(|(name, value)| (name.into(), value.into())),
            false,
        );
        let environment: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
        assert_eq!(
            environment.get(std::ffi::OsStr::new("TERM")),
            Some(&Some(std::ffi::OsStr::new("xterm-256color")))
        );
        assert_eq!(
            environment.get(std::ffi::OsStr::new("SSH_ASKPASS")),
            Some(&Some(std::ffi::OsStr::new("askpass.exe")))
        );
        assert_eq!(
            environment.get(std::ffi::OsStr::new("WT_SESSION")),
            Some(&None)
        );
    }
}

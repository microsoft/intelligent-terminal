//! One-shot, read-only remote history over the user's existing OpenSSH setup.
//! This transport never joins the master agent pool or exposes client tools.

use std::collections::HashSet;
use std::ffi::OsString;
use std::net::Ipv6Addr;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent_sessions::{AgentSession, CliSource, SessionLocation};
use crate::coordinator::{quote_windows_commandline_arg, sh_quote};

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
const REAP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "UncheckedSshTarget")]
pub struct SshTarget {
    destination: String,
    port: Option<u16>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UncheckedSshTarget {
    destination: String,
    port: Option<u16>,
}

impl TryFrom<UncheckedSshTarget> for SshTarget {
    type Error = anyhow::Error;

    fn try_from(value: UncheckedSshTarget) -> Result<Self> {
        Self::new(&value.destination, value.port)
    }
}

impl SshTarget {
    pub(crate) fn new(destination: &str, port: Option<u16>) -> Result<Self> {
        if port == Some(0) {
            bail!("SSH port must be between 1 and 65535");
        }
        let host = match destination.split_once('@') {
            Some((user, host)) => {
                if !valid_name(user) {
                    bail!("SSH destination has an invalid user name");
                }
                host
            }
            None => destination,
        };
        let valid_host = if host.contains(':') || host.starts_with('[') {
            let address = if host.starts_with('[') {
                host.strip_prefix('[')
                    .and_then(|s| s.strip_suffix(']'))
                    .unwrap_or("")
            } else {
                host
            };
            let address = match address.split_once('%') {
                Some((ip, zone)) if valid_name(zone) => ip,
                Some(_) => "",
                None => address,
            };
            address.parse::<Ipv6Addr>().is_ok()
        } else {
            valid_name(host)
        };
        if !valid_host {
            bail!("SSH destination must be an OpenSSH alias or [user@]host/IP, without options, whitespace, or shell metacharacters");
        }
        Ok(Self {
            destination: destination.to_string(),
            port,
        })
    }

    pub(crate) fn destination(&self) -> &str {
        &self.destination
    }

    pub(crate) fn port(&self) -> Option<u16> {
        self.port
    }

    pub(crate) fn display_name(&self) -> String {
        let Some(port) = self.port else {
            return self.destination.clone();
        };
        let (prefix, host) = self
            .destination
            .rsplit_once('@')
            .map(|(user, host)| (format!("{user}@"), host))
            .unwrap_or_else(|| (String::new(), self.destination.as_str()));
        if host.contains(':') && !host.starts_with('[') {
            format!("{prefix}[{host}]:{port}")
        } else {
            format!("{}:{port}", self.destination)
        }
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn known_profile(agent_id: &str) -> Result<&'static crate::agent_registry::AgentProfile> {
    if !crate::agent_registry::is_known_id(agent_id) {
        bail!("SSH session history requires a known built-in agent CLI id");
    }
    Ok(crate::agent_registry::lookup_profile_by_id(agent_id))
}

fn listing_script(agent_id: &str) -> Result<String> {
    known_profile(agent_id)?;
    // Only tokenize the registry's command. Never resolve Windows executables
    // or apply a local provider/credential environment to this remote agent.
    let command = crate::agent_registry::build_acp_command(agent_id, None);
    let args = crate::coordinator::split_windows_commandline(&command);
    if args.is_empty() {
        bail!("Remote ACP command is empty");
    }
    Ok(format!(
        "exec {}",
        args.iter()
            .map(|arg| sh_quote(arg))
            .collect::<Vec<_>>()
            .join(" ")
    ))
}

fn ssh_arguments(target: &SshTarget, interactive: bool, script: &str) -> Vec<String> {
    let mut args = vec![if interactive { "-t" } else { "-T" }.to_string()];
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ConnectTimeout=10",
        "ClearAllForwardings=yes",
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "ControlMaster=no",
        "ControlPath=none",
        "Tunnel=no",
    ] {
        args.extend(["-o".to_string(), option.to_string()]);
    }
    if let Some(port) = target.port() {
        args.extend(["-p".to_string(), port.to_string()]);
    }
    args.push("--".to_string());
    args.push(target.destination().to_string());
    // ssh passes the command through the server's shell before sh parses its
    // own -c argument. Quote the entire script as well as every CLI argument.
    // The login shell supplies the remote user's PATH (including /snap/bin).
    args.push(format!("sh -lc {}", sh_quote(script)));
    args
}

fn system_ssh_executable() -> Result<PathBuf> {
    let root = std::env::var_os("SystemRoot").context("SystemRoot is not set")?;
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        bail!("SystemRoot must be an absolute Windows path");
    }
    Ok(root.join(r"System32\OpenSSH\ssh.exe"))
}

fn configure_listing_environment(
    command: &mut tokio::process::Command,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) {
    // An SSH config may contain broad SendEnv patterns. Keep the environment
    // needed for Windows, user configuration, and SSH authentication, but not
    // inherited WTA routing/MCP metadata, provider keys, or agent credentials.
    command.env_clear();
    for (name, value) in environment {
        if matches!(
            name.to_string_lossy().to_ascii_uppercase().as_str(),
            "SYSTEMROOT"
                | "WINDIR"
                | "SYSTEMDRIVE"
                | "COMSPEC"
                | "PATH"
                | "PATHEXT"
                | "USERPROFILE"
                | "HOME"
                | "HOMEDRIVE"
                | "HOMEPATH"
                | "APPDATA"
                | "LOCALAPPDATA"
                | "PROGRAMDATA"
                | "PROGRAMFILES"
                | "PROGRAMFILES(X86)"
                | "TEMP"
                | "TMP"
                | "USERNAME"
                | "USERDOMAIN"
                | "LOGONSERVER"
                | "OS"
                | "SSH_AUTH_SOCK"
                | "SSH_AGENT_PID"
        ) {
            command.env(name, value);
        }
    }
}

/// Call inside a `LocalSet`. No session is created, loaded, or prompted.
pub(crate) async fn list_sessions(target: &SshTarget, agent_id: &str) -> Result<Vec<AgentSession>> {
    let script = listing_script(agent_id)?;
    let mut command = tokio::process::Command::new(system_ssh_executable()?);
    command
        .args(ssh_arguments(target, false, &script))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    configure_listing_environment(&mut command, std::env::vars_os());
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = command
        .spawn()
        .context("Start Windows system OpenSSH ssh.exe")?;

    let result = crate::protocol::acp::session_list::fetch_session_list(
        &mut child,
        &format!("ssh:{}:{agent_id}", target.display_name()),
        INITIALIZE_TIMEOUT,
        LIST_TIMEOUT,
    )
    .await
    .and_then(|(_, outcome)| outcome.map_err(anyhow::Error::msg));

    // Even a successful list must terminate and reap this exact one-shot SSH
    // process. Dropping/cancelling the future also kills it via kill_on_drop.
    let cleanup = tokio::time::timeout(REAP_TIMEOUT, async {
        if child
            .try_wait()
            .context("Check SSH child status")?
            .is_none()
        {
            child.kill().await.context("Kill and reap SSH child")?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow!("Timed out reaping SSH child"))
    .and_then(|result| result);
    let sessions = match (result, cleanup) {
        (Ok(sessions), Ok(())) => sessions,
        (Ok(_), Err(error)) => return Err(error),
        (Err(error), cleanup) => {
            let error = if let Err(cleanup_error) = cleanup {
                error.context(format!("SSH cleanup also failed: {cleanup_error:#}"))
            } else {
                error
            };
            return Err(error.context(format!(
                "Read SSH session history from {} failed. The host must already be verified in known_hosts and configured for non-interactive SSH authentication",
                target.display_name()
            )));
        }
    };
    Ok(map_remote_sessions(target, agent_id, &sessions))
}

fn map_remote_sessions(
    target: &SshTarget,
    agent_id: &str,
    sessions: &[agent_client_protocol::schema::v1::SessionInfo],
) -> Vec<AgentSession> {
    // Host origin IDs are not meaningful on another machine. Reuse only the
    // shared placeholder classifier, without touching any host registry/index.
    crate::session_history::classify_and_map(
        sessions,
        &HashSet::new(),
        SessionLocation::Ssh {
            target: target.clone(),
        },
        &CliSource::parse(Some(agent_id)),
    )
}

pub(crate) fn resume_commandline(
    target: &SshTarget,
    agent_id: &str,
    session_id: &str,
    cwd: &str,
) -> Result<String> {
    let profile = known_profile(agent_id)?;
    if profile.resume_flag.is_empty() {
        bail!("The remote agent CLI does not support session resume");
    }
    if session_id.trim().is_empty()
        || session_id.starts_with('-')
        || session_id.chars().any(char::is_control)
    {
        bail!("Remote session id must be nonempty, without controls or a leading option");
    }
    // Path::is_absolute on Windows rejects ordinary POSIX paths. The cwd is
    // remote data: validate its syntax, never inspect it on the host.
    if !cwd.starts_with('/') || cwd.chars().any(char::is_control) {
        bail!("Remote session cwd must be an absolute POSIX directory without controls");
    }
    let script = format!(
        "cd -- {} && exec {} {} {}",
        sh_quote(cwd),
        sh_quote(profile.id),
        sh_quote(profile.resume_flag),
        sh_quote(session_id)
    );
    let mut args = vec!["ssh.exe".to_string()];
    args.extend(ssh_arguments(target, true, &script));
    Ok(args
        .iter()
        .map(|arg| quote_windows_commandline_arg(arg))
        .collect::<Vec<_>>()
        .join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    fn windows_argv(commandline: &str) -> Vec<String> {
        let wide: Vec<u16> = commandline.encode_utf16().chain(Some(0)).collect();
        let mut count = 0;
        // SAFETY: the input is NUL-terminated. Windows returns count valid,
        // NUL-terminated strings in one allocation, freed after copying them.
        unsafe {
            let argv = windows_sys::Win32::UI::Shell::CommandLineToArgvW(wide.as_ptr(), &mut count);
            assert!(!argv.is_null());
            let result = std::slice::from_raw_parts(argv, count as usize)
                .iter()
                .map(|&arg| {
                    let mut len = 0;
                    while *arg.add(len) != 0 {
                        len += 1;
                    }
                    String::from_utf16_lossy(std::slice::from_raw_parts(arg, len))
                })
                .collect();
            windows_sys::Win32::Foundation::LocalFree(argv.cast());
            result
        }
    }

    #[test]
    fn targets_preserve_alias_case_and_support_user_ip_and_ipv6() {
        for destination in [
            "Production-Alias",
            "user@host.example",
            "127.0.0.1",
            "user@127.0.0.1",
            "::1",
            "[2001:db8::1]",
            "user@[fe80::1%eth0]",
        ] {
            let target = SshTarget::new(destination, Some(2222)).unwrap();
            assert_eq!(target.destination(), destination);
            assert_eq!(target.port(), Some(2222));
            let json = serde_json::to_string(&target).unwrap();
            assert_eq!(serde_json::from_str::<SshTarget>(&json).unwrap(), target);
        }
        assert_eq!(
            SshTarget::new("user@::1", Some(2222))
                .unwrap()
                .display_name(),
            "user@[::1]:2222"
        );
        assert_eq!(
            SshTarget::new("Alias", None).unwrap().display_name(),
            "Alias"
        );
    }

    #[test]
    fn invalid_targets_are_rejected_by_constructor_and_deserializer() {
        for destination in [
            "",
            "-oProxyCommand=evil",
            "user@-host",
            "@host",
            "user@",
            "a@b@host",
            "host name",
            "host\n",
            "host\0",
            "host\t",
            "host;id",
            "host&&id",
            "host|id",
            "$(id)",
            "`id`",
            "host'quote",
            "host\"quote",
            "host\\path",
            "ssh://host",
            "host:22",
            "[::1]:22",
            "[host]",
            "::invalid",
            "::1%eth0%more",
            "host*",
            "host?",
            "host#comment",
            "host>file",
            "host<file",
            "host(1)",
            "host{a,b}",
        ] {
            assert!(
                SshTarget::new(destination, None).is_err(),
                "{destination:?}"
            );
            assert!(
                serde_json::from_value::<SshTarget>(serde_json::json!({
                    "destination": destination, "port": null
                }))
                .is_err(),
                "{destination:?}"
            );
        }
        assert!(SshTarget::new("host", Some(0)).is_err());
        for port in [0, 65536, -1] {
            assert!(serde_json::from_value::<SshTarget>(serde_json::json!({
                "destination": "host", "port": port
            }))
            .is_err());
        }
        assert!(serde_json::from_value::<SshTarget>(serde_json::json!({
            "destination": "host", "command": "evil"
        }))
        .is_err());
    }

    #[test]
    fn listing_is_noninteractive_strict_and_has_no_forwarding_or_custom_command() {
        let target = SshTarget::new("Remote-Alias", Some(2222)).unwrap();
        let script = listing_script("copilot").unwrap();
        assert_eq!(script, "exec 'copilot' '--acp' '--stdio'");
        let args = ssh_arguments(&target, false, &script);
        assert_eq!(args[0], "-T");
        for option in [
            "BatchMode=yes",
            "StrictHostKeyChecking=yes",
            "ConnectTimeout=10",
            "ClearAllForwardings=yes",
            "ForwardAgent=no",
            "ForwardX11=no",
            "PermitLocalCommand=no",
            "RemoteCommand=none",
            "ControlPath=none",
        ] {
            assert!(
                args.windows(2).any(|pair| pair == ["-o", option]),
                "{option}"
            );
        }
        assert_eq!(
            &args[args.len() - 5..args.len() - 1],
            ["-p", "2222", "--", "Remote-Alias"]
        );
        assert_eq!(
            args.last().unwrap(),
            &format!("sh -lc {}", sh_quote(&script))
        );
        assert!(!args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-R" | "-L" | "-D" | "-A")));
        for id in [
            "custom:copilot",
            "unknown",
            "copilot --acp",
            "Copilot",
            "C:\\copilot.exe",
        ] {
            assert!(listing_script(id).is_err(), "{id}");
        }
    }

    #[test]
    fn listing_commands_use_each_builtin_registry_entry() {
        for profile in crate::agent_registry::KNOWN_AGENTS {
            let command = crate::agent_registry::build_acp_command(profile.id, None);
            let expected = crate::coordinator::split_windows_commandline(&command)
                .iter()
                .map(|arg| sh_quote(arg))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(
                listing_script(profile.id).unwrap(),
                format!("exec {expected}")
            );
        }
    }

    #[test]
    fn listing_environment_keeps_ssh_setup_but_not_local_credentials_or_routing() {
        let retained = [
            ("SystemRoot", r"C:\Windows"),
            ("Path", r"C:\Windows\System32"),
            ("USERPROFILE", r"C:\Users\ssh-user"),
            ("HOME", r"C:\Users\ssh-user"),
            ("SSH_AUTH_SOCK", r"\\.\pipe\openssh-ssh-agent"),
        ];
        let excluded = [
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "COPILOT_GITHUB_TOKEN",
            "COPILOT_PROVIDER_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "GEMINI_API_KEY",
            "OPENCODE_CONFIG_CONTENT",
            "INTELLIGENT_TERMINAL_MODEL_API_KEY",
            "WTA_CUSTOM_MODEL_CREDENTIAL_ID",
            "WTA_MCP_TOKEN",
            "WTA_CLI_PATH",
            "WT_COM_CLSID",
            "WT_SESSION",
            "UNRECOGNIZED_PROVIDER_SECRET",
        ];
        let mut command = tokio::process::Command::new("ssh.exe");
        command.env("PREEXISTING_SECRET", "not-retained");
        configure_listing_environment(
            &mut command,
            retained
                .iter()
                .copied()
                .chain(excluded.iter().map(|name| (*name, "not-retained")))
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        );
        let environment: std::collections::HashMap<_, _> = command.as_std().get_envs().collect();
        for (name, value) in retained {
            assert_eq!(
                environment.get(std::ffi::OsStr::new(name)),
                Some(&Some(std::ffi::OsStr::new(value)))
            );
        }
        assert_eq!(environment.len(), retained.len());
        for name in excluded {
            assert!(
                !environment.contains_key(std::ffi::OsStr::new(name)),
                "{name}"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn resume_uses_each_cli_resume_flag_and_remote_cwd_without_host_resolution() {
        let target = SshTarget::new("Alias", None).unwrap();
        for profile in crate::agent_registry::KNOWN_AGENTS {
            let command = resume_commandline(&target, profile.id, "sid", "/remote/repo").unwrap();
            let args = windows_argv(&command);
            assert_eq!(args[0], "ssh.exe");
            assert_eq!(args[1], "-t");
            assert!(!args.iter().any(|arg| arg == "cmd.exe"));
            let script = format!(
                "cd -- '/remote/repo' && exec '{}' '{}' 'sid'",
                profile.id, profile.resume_flag
            );
            assert_eq!(
                args.last().unwrap(),
                &format!("sh -lc {}", sh_quote(&script))
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn resume_quotes_shell_metacharacters_at_both_shell_layers_and_windows_argv() {
        let target = SshTarget::new("user@[::1]", Some(2222)).unwrap();
        let id = r#"session'";$(echo injected)`id`&%PATH%\"#;
        let cwd = r#"/home/a b/'";$(touch nope)\last"#;
        let command = resume_commandline(&target, "codex", id, cwd).unwrap();
        let args = windows_argv(&command);
        assert_eq!(args[0], "ssh.exe");
        assert_eq!(args[args.len() - 2], "user@[::1]");
        let script = format!(
            "cd -- {} && exec 'codex' 'resume' {}",
            sh_quote(cwd),
            sh_quote(id)
        );
        assert_eq!(
            args.last().unwrap(),
            &format!("sh -lc {}", sh_quote(&script))
        );
        assert!(args.last().unwrap().contains(r"'\''"));
    }

    #[test]
    fn resume_rejects_options_controls_unknown_agents_and_non_posix_cwd() {
        let target = SshTarget::new("host", None).unwrap();
        for id in ["", " ", "-x", "--help", "id\0", "id\n", "id\r"] {
            assert!(resume_commandline(&target, "copilot", id, "/repo").is_err());
        }
        for cwd in [
            "",
            ".",
            "relative/path",
            "~/repo",
            "C:\\repo",
            "\\\\host\\share",
            "/repo\0",
            "/repo\n",
        ] {
            assert!(resume_commandline(&target, "copilot", "sid", cwd).is_err());
        }
        for agent in ["unknown", "custom:copilot", "copilot;id"] {
            assert!(resume_commandline(&target, agent, "sid", "/repo").is_err());
        }
        assert!(resume_commandline(&target, "copilot", "sid", "/").is_ok());
    }

    #[test]
    fn remote_rows_are_historical_unbound_and_use_only_shared_placeholder_filter() {
        use agent_client_protocol::schema::v1::{SessionId, SessionInfo};
        let target = SshTarget::new("host", None).unwrap();
        let mut placeholder = SessionInfo::new(SessionId::new("empty"), PathBuf::from("/repo"));
        placeholder.title = Some("New session - 2026-07-23T01:14:00.422Z".into());
        let real = SessionInfo::new(SessionId::new("same-as-host-id"), PathBuf::from("/repo"));
        let rows = map_remote_sessions(&target, "opencode", &[placeholder, real]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "same-as-host-id");
        assert_eq!(rows[0].cli_source, CliSource::OpenCode);
        assert_eq!(rows[0].location, SessionLocation::Ssh { target });
        assert_eq!(
            rows[0].status,
            crate::agent_sessions::AgentStatus::Historical
        );
        assert_eq!(
            rows[0].origin,
            crate::agent_sessions::SessionOrigin::Unknown
        );
        assert!(rows[0].pane_session_id.is_none());
        assert!(rows[0].window_id.is_none());
        assert!(rows[0].tab_id.is_none());
    }
}

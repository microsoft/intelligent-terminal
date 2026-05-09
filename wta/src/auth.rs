//! Reusable auth module — checks whether an agent CLI is authenticated
//! and provides login guidance.
//!
//! Strategy (per agent):
//!   1. If `auth_check_command` is defined in the agent registry → run it.
//!      Exit 0 = authenticated, non-zero = needs auth.
//!   2. If empty → fallback: try to spawn the ACP agent briefly and check
//!      whether init succeeds or fails with an auth error.
//!   3. For `InProtocol` agents (Gemini) → skip, auth happens during connection.

use crate::agent_registry::{self, AcpAuthFlow, AgentProfile};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthStatus {
    /// Agent is authenticated and ready to use.
    Authenticated,
    /// Agent needs authentication before it can be used.
    NeedsAuth,
    /// Could not determine auth status.
    Unknown,
}

/// Result of an auth check, including agent-specific login guidance.
#[derive(Debug, Clone)]
pub struct AuthCheckResult {
    pub status: AuthStatus,
    pub agent_name: String,
    pub auth_hint: String,
    pub login_command: String,
}

/// Check whether the given agent is authenticated.
///
/// This is the main entry point — call it from any flow that needs to verify
/// auth (FRE setup, agent-missing recovery, manual switch, etc.).
pub async fn check_auth(agent_id: &str) -> AuthCheckResult {
    let profile = agent_registry::lookup_profile_by_id(agent_id);

    // InProtocol agents handle auth during ACP connection — skip check.
    if profile.acp_auth_flow == AcpAuthFlow::InProtocol {
        return AuthCheckResult {
            status: AuthStatus::Authenticated,
            agent_name: profile.display_name.to_string(),
            auth_hint: profile.auth_hint.to_string(),
            login_command: String::new(),
        };
    }

    // Strategy 1: use auth_check_command if defined
    if !profile.auth_check_command.is_empty() {
        let status = run_auth_check_command(profile.auth_check_command).await;
        return AuthCheckResult {
            status,
            agent_name: profile.display_name.to_string(),
            auth_hint: profile.auth_hint.to_string(),
            login_command: build_login_command(profile),
        };
    }

    // Strategy 2: check for known credential files
    if let Some(status) = check_credential_files(profile) {
        return AuthCheckResult {
            status,
            agent_name: profile.display_name.to_string(),
            auth_hint: profile.auth_hint.to_string(),
            login_command: build_login_command(profile),
        };
    }

    // Strategy 3: fallback — try ACP handshake probe
    // Skip for npx-adapter agents (too slow — npx downloads packages)
    if !profile.acp_launch_command.is_empty()
        && profile.acp_launch_command.contains("npx")
    {
        // Can't probe via ACP adapter, assume needs auth
        return AuthCheckResult {
            status: AuthStatus::NeedsAuth,
            agent_name: profile.display_name.to_string(),
            auth_hint: profile.auth_hint.to_string(),
            login_command: build_login_command(profile),
        };
    }

    let status = probe_acp_auth(profile).await;
    AuthCheckResult {
        status,
        agent_name: profile.display_name.to_string(),
        auth_hint: profile.auth_hint.to_string(),
        login_command: build_login_command(profile),
    }
}

/// Run an explicit auth-check command. Exit 0 = authenticated.
async fn run_auth_check_command(command: &str) -> AuthStatus {
    let result = tokio::task::spawn_blocking({
        let cmd = command.to_string();
        move || {
            #[cfg(windows)]
            {
                std::process::Command::new("cmd")
                    .args(["/C", &cmd])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
            }
            #[cfg(not(windows))]
            {
                std::process::Command::new("sh")
                    .args(["-c", &cmd])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
            }
        }
    })
    .await;

    match result {
        Ok(Ok(status)) if status.success() => AuthStatus::Authenticated,
        Ok(Ok(_)) => AuthStatus::NeedsAuth,
        _ => AuthStatus::Unknown,
    }
}

/// Fallback: try to spawn the ACP agent and see if the init handshake
/// succeeds or fails with an auth-related error.
///
/// Sends a minimal ACP `initialize` request over stdin/stdout. If the
/// process writes a response, we assume auth is OK. If it exits quickly
/// with auth-related stderr, we flag NeedsAuth. Timeout = 10s.
async fn probe_acp_auth(profile: &AgentProfile) -> AuthStatus {
    let exe = if !profile.acp_launch_command.is_empty() {
        profile.acp_launch_command.to_string()
    } else {
        // Find the executable (check WinGet Links + PATH)
        let exe_path = find_agent_exe(profile);
        let mut cmd = exe_path;
        for flag in profile.acp_flags {
            cmd.push(' ');
            cmd.push_str(flag);
        }
        cmd
    };

    let result = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};

        // Parse exe + args (avoid cmd /C quoting issues)
        let parts: Vec<&str> = exe.splitn(2, ' ').collect();
        let program = parts[0];
        let args: Vec<&str> = parts.get(1).map(|s| s.split_whitespace().collect()).unwrap_or_default();

        let mut child = match std::process::Command::new(program)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return AuthStatus::Unknown,
        };

        // Send minimal ACP init request
        if let Some(ref mut stdin) = child.stdin {
            let init_msg = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","clientInfo":{"name":"wta-auth-probe","version":"0.1"},"capabilities":{}}}"#;
            let header = format!("Content-Length: {}\r\n\r\n{}", init_msg.len(), init_msg);
            let _ = stdin.write_all(header.as_bytes());
            let _ = stdin.flush();
        }
        // Keep stdin open so the agent doesn't EOF-exit

        // Race stdout and stderr with a timeout:
        //   stdout "Content-Length:" → Authenticated (init response received)
        //   stderr auth keywords    → NeedsAuth (explicit auth failure)
        //   timeout (8s)            → NeedsAuth (conservative default)
        #[derive(Debug)]
        enum ProbeResult { GotResponse, AuthError(String), Nothing }

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx, rx) = std::sync::mpsc::channel();

        // Thread 1: read stdout for ACP response
        let tx_stdout = tx.clone();
        std::thread::spawn(move || {
            if let Some(mut stdout) = stdout {
                let mut buf = [0u8; 512];
                if let Ok(n) = stdout.read(&mut buf) {
                    if n > 0 {
                        let text = String::from_utf8_lossy(&buf[..n]);
                        if text.contains("Content-Length") {
                            let _ = tx_stdout.send(ProbeResult::GotResponse);
                            return;
                        }
                    }
                }
            }
            let _ = tx_stdout.send(ProbeResult::Nothing);
        });

        // Thread 2: read stderr for auth errors
        let tx_stderr = tx.clone();
        std::thread::spawn(move || {
            if let Some(mut stderr) = stderr {
                let mut buf = [0u8; 4096];
                if let Ok(n) = stderr.read(&mut buf) {
                    if n > 0 {
                        let text = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = tx_stderr.send(ProbeResult::AuthError(text));
                        return;
                    }
                }
            }
            let _ = tx_stderr.send(ProbeResult::Nothing);
        });
        drop(tx); // drop original so channel closes when both threads done

        // Wait for whichever responds first, up to 8 seconds
        let mut status = AuthStatus::NeedsAuth;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        for _ in 0..2 {
            let timeout = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(timeout) {
                Ok(ProbeResult::GotResponse) => {
                    status = AuthStatus::Authenticated;
                    break;
                }
                Ok(ProbeResult::AuthError(text)) => {
                    let lower = text.to_lowercase();
                    if lower.contains("not logged in")
                        || lower.contains("not authenticated")
                        || lower.contains("unauthorized")
                        || lower.contains("login")
                        || lower.contains("401")
                    {
                        status = AuthStatus::NeedsAuth;
                    }
                    // Got stderr output but not auth-specific — still NeedsAuth
                    break;
                }
                Ok(ProbeResult::Nothing) => continue,
                Err(_) => break, // timeout
            }
        }

        // Clean up
        let _ = child.kill();
        let _ = child.wait();
        status
    })
    .await
    .unwrap_or(AuthStatus::Unknown);

    result
}

/// Check for known credential/config files that indicate authentication.
/// Returns Some(AuthStatus) if we can determine status, None if unknown.
fn check_credential_files(profile: &AgentProfile) -> Option<AuthStatus> {
    let home = std::env::var("USERPROFILE").ok()?;
    let home = std::path::PathBuf::from(home);

    match profile.id {
        "claude" => {
            // Claude Code stores credentials in ~/.claude/.credentials.json
            if home.join(".claude").join(".credentials.json").exists() {
                Some(AuthStatus::Authenticated)
            } else {
                Some(AuthStatus::NeedsAuth)
            }
        }
        "codex" => {
            // Codex uses OPENAI_API_KEY env var or ~/.codex/ config
            if std::env::var("OPENAI_API_KEY").is_ok() {
                Some(AuthStatus::Authenticated)
            } else if home.join(".codex").exists() {
                Some(AuthStatus::Authenticated)
            } else {
                None // can't determine
            }
        }
        "copilot" => {
            // Copilot stores token in Windows Credential Manager
            // We check cmdkey — but this is slow, so return None and let probe handle it
            None
        }
        _ => None,
    }
}

/// Find the full path to an agent executable, checking WinGet Links, npm global, + PATH.
/// Returns the path WITHOUT quotes — caller decides how to use it.
pub fn find_agent_exe(profile: &AgentProfile) -> String {
    for ext in profile.exe_search_order {
        let exe_name = format!("{}{}", profile.id, ext);

        // Check WinGet Links
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let path = std::path::PathBuf::from(&local)
                .join("Microsoft").join("WinGet").join("Links").join(&exe_name);
            if path.exists() {
                return path.to_string_lossy().to_string();
            }
        }

        // Check npm global bin
        if let Ok(appdata) = std::env::var("APPDATA") {
            let path = std::path::PathBuf::from(&appdata).join("npm").join(&exe_name);
            if path.exists() {
                return path.to_string_lossy().to_string();
            }
        }

        // Check Claude CLI custom path
        if let Ok(home) = std::env::var("USERPROFILE") {
            let path = std::path::PathBuf::from(&home).join(".claude-cli").join("CurrentVersion").join(&exe_name);
            if path.exists() {
                return path.to_string_lossy().to_string();
            }
        }
    }
    // Fallback to bare name (relies on PATH)
    profile.id.to_string()
}

/// Build the login command for an agent, using full path if needed.
fn build_login_command(profile: &AgentProfile) -> String {
    let exe = find_agent_exe(profile);
    if exe.contains(' ') {
        format!("\"{}\" login", exe)
    } else {
        format!("{} login", exe)
    }
}

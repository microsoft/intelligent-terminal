// ΓöÇΓöÇΓöÇ Preflight Checks ΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇΓöÇ
//
// Pre-flight validation run before launching the ACP agent.
// Checks CLI presence on PATH and authentication status, producing
// structured results that the setup wizard can display.

use crate::agent_registry::{self, AcpAuthFlow, AgentProfile};

/// Status of a single preflight check.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckStatus {
    /// Check is in progress.
    Checking,
    /// Check passed successfully.
    Passed,
    /// Check failed with a reason.
    Failed(String),
    /// Check was skipped (prerequisite not met).
    Skipped,
}

/// Result of all preflight checks for an agent.
#[derive(Debug, Clone)]
pub struct PreflightResult {
    pub agent_id: String,
    pub display_name: String,
    pub cli_status: CheckStatus,
    pub cli_path: Option<String>,
    pub auth_status: CheckStatus,
    pub install_hint: String,
    pub install_url: String,
    pub auth_hint: String,
}

impl PreflightResult {
    /// Returns true if all required checks passed.
    pub fn all_passed(&self) -> bool {
        self.cli_status == CheckStatus::Passed
            && matches!(
                self.auth_status,
                CheckStatus::Passed | CheckStatus::Skipped
            )
    }
}

/// Extract the agent id (bare name) from an agent command string.
/// e.g. "copilot --acp --stdio" ΓåÆ "copilot"
pub fn extract_agent_id(agent_cmd: &str) -> &str {
    agent_cmd.split_whitespace().next().unwrap_or(agent_cmd)
}

/// Run all preflight checks for the given agent command.
pub async fn check_agent(agent_cmd: &str) -> PreflightResult {
    let agent_id = extract_agent_id(agent_cmd);
    let profile = agent_registry::lookup_profile(agent_id);

    let mut result = PreflightResult {
        agent_id: agent_id.to_string(),
        display_name: profile.display_name.to_string(),
        cli_status: CheckStatus::Checking,
        cli_path: None,
        auth_status: CheckStatus::Skipped,
        install_hint: profile.install_hint.to_string(),
        install_url: profile.install_url.to_string(),
        auth_hint: profile.auth_hint.to_string(),
    };

    // 1. Check if CLI is on PATH
    let resolved = agent_registry::resolve_bare_agent_name(agent_id);
    preflight_log(&format!("check_agent: agent_id={} resolved={}", agent_id, resolved));
    match find_on_path(&resolved, profile) {
        Some(path) => {
            preflight_log(&format!("check_agent: FOUND at {}", path));
            result.cli_status = CheckStatus::Passed;
            result.cli_path = Some(path);
        }
        None => {
            preflight_log("check_agent: NOT FOUND");
            result.cli_status = CheckStatus::Failed("Not found on PATH".to_string());
            // Skip auth check if CLI isn't even installed
            result.auth_status = CheckStatus::Skipped;
            return result;
        }
    }

    // 2. Check authentication (only for agents with external auth)
    if profile.acp_auth_flow == AcpAuthFlow::External
        && !profile.auth_check_command.is_empty()
    {
        result.auth_status = check_auth(profile.auth_check_command).await;
    } else if profile.acp_auth_flow == AcpAuthFlow::InProtocol {
        // In-protocol auth is handled during connection, mark as skipped
        result.auth_status = CheckStatus::Skipped;
    } else {
        result.auth_status = CheckStatus::Skipped;
    }

    result
}

fn preflight_log(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("wta-preflight.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "[{:.3}] {}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64(),
            msg
        );
    }
}

/// Find the agent executable on PATH and return its full path.
/// Re-reads PATH from the Windows registry so that newly installed programs
/// are found even if this process was started before the install.
fn find_on_path(resolved_name: &str, profile: &AgentProfile) -> Option<String> {
    let path_var = fresh_path();
    preflight_log(&format!("find_on_path: resolved_name={} path_len={}", resolved_name, path_var.len()));
    preflight_log(&format!("find_on_path: PATH={}", &path_var));

    // Try the resolved name first (e.g. "copilot.exe")
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(resolved_name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }

    // Try each extension from the profile
    let base = resolved_name
        .strip_suffix(".exe")
        .or_else(|| resolved_name.strip_suffix(".cmd"))
        .unwrap_or(resolved_name);

    for ext in profile.exe_search_order {
        let name = format!("{}{}", base, ext);
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().to_string());
            }
        }
    }

    None
}

/// Check authentication by running the auth check command.
/// Returns Passed if exit code is 0, Failed otherwise.
async fn check_auth(auth_check_command: &str) -> CheckStatus {
    let parts: Vec<&str> = auth_check_command.split_whitespace().collect();
    let (program, args) = match parts.split_first() {
        Some((prog, args)) => (*prog, args),
        None => return CheckStatus::Skipped,
    };

    match tokio::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => match tokio::time::timeout(
            std::time::Duration::from_secs(10),
            child.wait_with_output(),
        )
        .await
        {
            Ok(Ok(output)) => {
                if output.status.success() {
                    CheckStatus::Passed
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let reason = if stderr.trim().is_empty() {
                        "Not authenticated".to_string()
                    } else {
                        // Take first non-empty line from stderr as the reason
                        stderr
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("Not authenticated")
                            .trim()
                            .to_string()
                    };
                    CheckStatus::Failed(reason)
                }
            }
            Ok(Err(e)) => CheckStatus::Failed(format!("Auth check failed: {}", e)),
            Err(_) => CheckStatus::Failed("Auth check timed out".to_string()),
        },
        Err(_) => {
            // Can't run auth check command ΓÇö probably CLI not fully functional
            CheckStatus::Failed("Could not run auth check".to_string())
        }
    }
}

/// Refresh this process's PATH environment variable from the Windows registry.
/// Useful after a `winget install` so subsequent child processes inherit the
/// updated PATH (and `resolve_bare_agent_name` finds the new binary).
pub fn refresh_process_path() {
    let path = fresh_path();
    if !path.is_empty() {
        std::env::set_var("PATH", &path);
        preflight_log(&format!("refresh_process_path: PATH refreshed (len={})", path.len()));
    }
}

/// Run `winget install --id GitHub.Copilot ...`, streaming each output line via
/// `on_line`. Returns Ok on exit code 0, Err otherwise.  On success, refreshes
/// this process's PATH so subsequent spawns find the new binary.
pub async fn winget_install_copilot<F>(mut on_line: F) -> Result<(), String>
where
    F: FnMut(String) + Send + 'static,
{
    use tokio::io::{AsyncBufReadExt, BufReader};
    use std::process::Stdio;

    let mut cmd = tokio::process::Command::new("winget");
    cmd.args([
        "install",
        "--id",
        "GitHub.Copilot",
        "--exact",
        "--silent",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    // Hide the winget window if any console pop-up tries to spawn.
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    on_line("Running: winget install GitHub.Copilot".to_string());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Err(format!("Failed to launch winget: {}. Is winget on PATH?", e));
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    if let Some(stdout) = stdout {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx.send(line);
            }
        });
    }
    if let Some(stderr) = stderr {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx.send(line);
            }
        });
    }
    drop(line_tx);

    // Forward lines to caller while the process runs.
    let forward = tokio::spawn(async move {
        while let Some(line) = line_rx.recv().await {
            let trimmed = line.trim_end_matches('\r').to_string();
            if !trimmed.is_empty() {
                on_line(trimmed);
            }
        }
    });

    let status = match child.wait().await {
        Ok(s) => s,
        Err(e) => return Err(format!("winget exited unexpectedly: {}", e)),
    };

    let _ = forward.await;

    if status.success() {
        refresh_process_path();
        Ok(())
    } else {
        let code = status.code().unwrap_or(-1);
        Err(format!("winget install failed (exit code {})", code))
    }
}

/// Read the current PATH by combining the system and user PATH values from the
/// Windows registry.  This picks up programs installed after this process started.
/// Falls back to the process's inherited PATH if registry reads fail.
fn fresh_path() -> String {
    use std::os::windows::ffi::OsStringExt;

    fn read_reg_path(hkey: windows_sys::Win32::System::Registry::HKEY, subkey: &str) -> Option<String> {
        use windows_sys::Win32::System::Registry::*;

        let subkey_wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let value_name: Vec<u16> = "Path".encode_utf16().chain(std::iter::once(0)).collect();

        let mut hk: HKEY = std::ptr::null_mut();
        let ret = unsafe {
            RegOpenKeyExW(
                hkey,
                subkey_wide.as_ptr(),
                0,
                KEY_READ,
                &mut hk,
            )
        };
        if ret != 0 {
            return None;
        }

        let mut buf_size: u32 = 8192;
        let mut buffer: Vec<u16> = vec![0u16; buf_size as usize / 2];
        let mut kind: u32 = 0;
        let ret = unsafe {
            RegQueryValueExW(
                hk,
                value_name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                buffer.as_mut_ptr() as *mut u8,
                &mut buf_size,
            )
        };
        unsafe { RegCloseKey(hk) };
        if ret != 0 {
            return None;
        }

        let len = (buf_size as usize / 2).saturating_sub(1); // strip null terminator
        let raw = std::ffi::OsString::from_wide(&buffer[..len]);
        let raw_str = raw.to_string_lossy().to_string();

        // Expand environment variables like %USERPROFILE%, %APPDATA%, etc.
        // PATH is typically stored as REG_EXPAND_SZ.
        if kind == REG_EXPAND_SZ {
            expand_env_vars(&raw_str)
        } else {
            Some(raw_str)
        }
    }

    let system_path = read_reg_path(
        windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE,
        r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
    );
    let user_path = read_reg_path(
        windows_sys::Win32::System::Registry::HKEY_CURRENT_USER,
        r"Environment",
    );

    match (system_path, user_path) {
        (Some(s), Some(u)) => format!("{};{}", s, u),
        (Some(s), None) => s,
        (None, Some(u)) => u,
        (None, None) => std::env::var("PATH").unwrap_or_default(),
    }
}

/// Expand environment variable references (%VAR%) in a string using
/// the Win32 ExpandEnvironmentStringsW API.
fn expand_env_vars(s: &str) -> Option<String> {
    use std::os::windows::ffi::OsStringExt;

    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();

    // First call to get required buffer size
    let needed = unsafe {
        windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    if needed == 0 {
        return Some(s.to_string());
    }

    let mut out: Vec<u16> = vec![0u16; needed as usize];
    let written = unsafe {
        windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW(
            wide.as_ptr(),
            out.as_mut_ptr(),
            needed,
        )
    };
    if written == 0 {
        return Some(s.to_string());
    }

    let len = (written as usize).saturating_sub(1); // strip null terminator
    let os_str = std::ffi::OsString::from_wide(&out[..len]);
    Some(os_str.to_string_lossy().to_string())
}

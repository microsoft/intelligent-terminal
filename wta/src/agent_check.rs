//! Unified agent detection + auth module.
//!
//! Basic functions (atomic, single-responsibility):
//!   - `find_exe`          — find agent executable on PATH (registry-fresh)
//!   - `has_credential`    — fast credential check (cmdkey / config files)
//!   - `run_auth_command`  — run auth_check_command from registry
//!   - `build_login_cmd`   — build login command with full path
//!   - `install`           — install agent via winget (async, streaming logs)
//!   - `refresh_path`      — re-read PATH from Windows registry
//!
//! Composite functions (combine basics):
//!   - `check_agent`       — find_exe + has_credential → AgentStatus
//!   - `check_all_agents`  — check_agent for all known agents
//!   - `ensure_installed`  — find_exe → install if missing → refresh_path → find_exe

use crate::agent_registry::{self, AgentProfile, KNOWN_AGENTS};

// ─── Data types ─────────────────────────────────────────────────────────────

/// Status of a single agent, combining CLI detection + credential check.
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub id: String,
    pub display_name: String,
    pub cli_found: bool,
    pub cli_path: Option<String>,
    pub has_credential: bool,
    pub install_hint: String,
    pub auth_hint: String,
}

impl AgentStatus {
    /// User-facing status string for agent list.
    pub fn status_label(&self) -> String {
        if !self.cli_found {
            "Not found".to_string()
        } else if self.id == "copilot" {
            "Installed by default".to_string()
        } else {
            "Detected".to_string()
        }
    }

    /// Whether this agent can be auto-installed (e.g. via winget).
    pub fn can_auto_install(&self) -> bool {
        self.id == "copilot"
    }
}

// ─── Basic functions ────────────────────────────────────────────────────────

/// Find the agent executable using a fresh PATH read from the Windows registry.
/// Returns the full path if found, None otherwise.
pub fn find_exe(agent_id: &str) -> Option<String> {
    let profile = agent_registry::lookup_profile_by_id(agent_id);
    let path_var = fresh_path();
    let resolved = agent_registry::resolve_bare_agent_name(agent_id);

    // Try resolved name first (e.g. "copilot.exe")
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(&resolved);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }

    // Try each extension from the profile
    let base = resolved
        .strip_suffix(".exe")
        .or_else(|| resolved.strip_suffix(".cmd"))
        .unwrap_or(&resolved);

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

/// Fast synchronous credential check. Returns true if a credential is
/// likely present. Used to decide: connect directly vs show auth screen.
///
/// Strategy:
///   1. If `auth_check_command` is defined → run it (exit 0 = true)
///   2. Else → agent-specific fast check (cmdkey / config files)
pub fn has_credential(agent_id: &str) -> bool {
    let profile = agent_registry::lookup_profile_by_id(agent_id);

    // Strategy 1: auth_check_command
    if let Some(result) = run_auth_command(profile.auth_check_command) {
        return result;
    }

    // Strategy 2: agent-specific fast check
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let home = std::path::PathBuf::from(&home);

    match agent_id {
        "copilot" => {
            std::process::Command::new("cmd")
                .args(["/C", "cmdkey /list | findstr /i copilot-cli"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false)
        }
        "claude" => home.join(".claude").join(".credentials.json").exists(),
        "codex" => {
            std::env::var("OPENAI_API_KEY").is_ok() || home.join(".codex").exists()
        }
        "gemini" => true, // InProtocol — always try connect
        _ => false,
    }
}

/// Run an auth_check_command from the agent registry.
/// Returns Some(true) if authenticated, Some(false) if not, None if command is empty.
pub fn run_auth_command(command: &str) -> Option<bool> {
    if command.is_empty() {
        return None;
    }

    let parts: Vec<&str> = command.split_whitespace().collect();
    let (program, args) = match parts.split_first() {
        Some((prog, args)) => (*prog, args),
        None => return None,
    };

    let result = std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match result {
        Ok(status) => Some(status.success()),
        Err(_) => Some(false),
    }
}

/// Build the login command for an agent, resolving the full executable path.
pub fn build_login_cmd(agent_id: &str) -> String {
    let exe_path = find_exe(agent_id)
        .unwrap_or_else(|| agent_id.to_string());

    if exe_path.contains(' ') {
        format!("\"{}\" login", exe_path)
    } else {
        format!("{} login", exe_path)
    }
}

/// Install an agent via winget. Streams output lines through `on_line` callback.
/// On success, refreshes the process PATH so subsequent `find_exe` calls find
/// the new binary.
pub async fn install(agent_id: &str, on_line: impl FnMut(String) + Send + 'static) -> Result<(), String> {
    match agent_id {
        "copilot" => install_copilot(on_line).await,
        _ => Err(format!("Automatic install not supported for {}", agent_id)),
    }
}

/// Refresh the current process's PATH from the Windows registry.
/// Call after installing software so `find_exe` picks up the new binary.
pub fn refresh_path() {
    let path = fresh_path();
    if !path.is_empty() {
        std::env::set_var("PATH", &path);
    }
}

// ─── Composite functions ────────────────────────────────────────────────────

/// Check a single agent: find executable + check credential.
pub fn check_agent(agent_id: &str) -> AgentStatus {
    let profile = agent_registry::lookup_profile_by_id(agent_id);
    let cli_path = find_exe(agent_id);
    let cli_found = cli_path.is_some();
    let cred = if cli_found { has_credential(agent_id) } else { false };

    AgentStatus {
        id: agent_id.to_string(),
        display_name: profile.display_name.to_string(),
        cli_found,
        cli_path,
        has_credential: cred,
        install_hint: profile.install_hint.to_string(),
        auth_hint: profile.auth_hint.to_string(),
    }
}

/// Check all known agents.
pub fn check_all_agents() -> Vec<AgentStatus> {
    KNOWN_AGENTS.iter().map(|p| check_agent(p.id)).collect()
}

/// Check an agent from a full command string (e.g. "copilot --acp --stdio"
/// or "my-custom-agent --acp"). Supports both known and custom agents.
pub fn check_agent_cmd(agent_cmd: &str) -> AgentStatus {
    let exe_name = agent_cmd.split_whitespace().next().unwrap_or(agent_cmd);

    // Try to match a known agent first
    let profile = agent_registry::lookup_profile(exe_name);
    if profile.id != "unknown" {
        return check_agent(profile.id);
    }

    // Custom agent: check if the executable exists on fresh PATH
    let path_var = fresh_path();
    let mut cli_path = None;

    // Check as-is (might be a full path)
    if std::path::Path::new(exe_name).is_file() {
        cli_path = Some(exe_name.to_string());
    }

    // Check on PATH with common extensions
    if cli_path.is_none() {
        for ext in &["", ".exe", ".cmd"] {
            let name = format!("{}{}", exe_name, ext);
            for dir in std::env::split_paths(&path_var) {
                let candidate = dir.join(&name);
                if candidate.is_file() {
                    cli_path = Some(candidate.to_string_lossy().to_string());
                    break;
                }
            }
            if cli_path.is_some() { break; }
        }
    }

    let cli_found = cli_path.is_some();

    AgentStatus {
        id: exe_name.to_string(),
        display_name: exe_name.to_string(),
        cli_found,
        cli_path,
        has_credential: false, // unknown for custom agents
        install_hint: String::new(),
        auth_hint: format!("Make sure {} is installed and on your PATH.", exe_name),
    }
}

/// Ensure an agent is installed: find → install if missing → refresh PATH → find again.
pub async fn ensure_installed(
    agent_id: &str,
    on_line: impl FnMut(String) + Send + 'static,
) -> Result<Option<String>, String> {
    if let Some(path) = find_exe(agent_id) {
        return Ok(Some(path));
    }

    install(agent_id, on_line).await?;
    refresh_path();

    Ok(find_exe(agent_id))
}

// ─── Internal helpers ───────────────────────────────────────────────────────

/// Install GitHub Copilot via winget with streaming output.
async fn install_copilot(mut on_line: impl FnMut(String) + Send + 'static) -> Result<(), String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use std::process::Stdio;

    let mut cmd = tokio::process::Command::new("winget");
    cmd.args([
        "install",
        "--id", "GitHub.Copilot",
        "--exact",
        "--silent",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    on_line("Running: winget install GitHub.Copilot".to_string());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(format!("Failed to launch winget: {}", e)),
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
        refresh_path();
        Ok(())
    } else {
        let code = status.code().unwrap_or(-1);
        Err(format!("winget install failed (exit code {})", code))
    }
}

/// Read PATH from the Windows registry (system + user), picking up programs
/// installed after this process started.
fn fresh_path() -> String {
    use std::os::windows::ffi::OsStringExt;

    fn read_reg_path(hkey: windows_sys::Win32::System::Registry::HKEY, subkey: &str) -> Option<String> {
        use windows_sys::Win32::System::Registry::*;

        let subkey_wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let value_name: Vec<u16> = "Path".encode_utf16().chain(std::iter::once(0)).collect();

        let mut hk: HKEY = std::ptr::null_mut();
        let ret = unsafe {
            RegOpenKeyExW(hkey, subkey_wide.as_ptr(), 0, KEY_READ, &mut hk)
        };
        if ret != 0 { return None; }

        let mut buf_size: u32 = 8192;
        let mut buffer: Vec<u16> = vec![0u16; buf_size as usize / 2];
        let mut kind: u32 = 0;
        let ret = unsafe {
            RegQueryValueExW(
                hk, value_name.as_ptr(), std::ptr::null(),
                &mut kind, buffer.as_mut_ptr() as *mut u8, &mut buf_size,
            )
        };
        unsafe { RegCloseKey(hk) };
        if ret != 0 { return None; }

        let len = (buf_size as usize / 2).saturating_sub(1);
        let raw = std::ffi::OsString::from_wide(&buffer[..len]);
        let raw_str = raw.to_string_lossy().to_string();

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

/// Expand %VAR% references using Win32 ExpandEnvironmentStringsW.
fn expand_env_vars(s: &str) -> Option<String> {
    use std::os::windows::ffi::OsStringExt;

    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    let needed = unsafe {
        windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW(
            wide.as_ptr(), std::ptr::null_mut(), 0,
        )
    };
    if needed == 0 { return Some(s.to_string()); }

    let mut out: Vec<u16> = vec![0u16; needed as usize];
    let written = unsafe {
        windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW(
            wide.as_ptr(), out.as_mut_ptr(), needed,
        )
    };
    if written == 0 { return Some(s.to_string()); }

    let len = (written as usize).saturating_sub(1);
    let os_str = std::ffi::OsString::from_wide(&out[..len]);
    Some(os_str.to_string_lossy().to_string())
}

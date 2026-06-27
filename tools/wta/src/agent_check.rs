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
            if self.id == "copilot" {
                t!("agent.status.not_installed_auto").into_owned()
            } else {
                t!("agent.status.not_found").into_owned()
            }
        } else {
            t!("agent.status.detected").into_owned()
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
            let found = std::process::Command::new("cmd")
                .args(["/C", "cmdkey /list | findstr /i copilot-cli"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .map(|o| !o.stdout.is_empty())
                .unwrap_or(false);
            tracing::debug!(target: "agent_check", agent = "copilot", found, "copilot credential check (cmdkey)");
            found
        }
        "claude" => {
            let path = home.join(".claude").join(".credentials.json");
            let exists = path.exists();
            tracing::debug!(target: "agent_check", path = %path.display(), exists, "claude credential check");
            exists
        }
        "codex" => {
            std::env::var("OPENAI_API_KEY").is_ok() || home.join(".codex").exists()
        }
        "gemini" => {
            // Check GEMINI_API_KEY or GOOGLE_API_KEY env var, or OAuth token in ~/.gemini/
            std::env::var("GEMINI_API_KEY").is_ok()
                || std::env::var("GOOGLE_API_KEY").is_ok()
                || home.join(".gemini").exists()
        }
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
///
/// For Copilot, an optional GitHub Enterprise host (e.g. `"mycompany.ghe.com"`)
/// is appended as `--host https://<domain>` so users on a GHE / `ghe.com`
/// tenant can sign in (mirroring the CLI's own `copilot login --host …`).
/// Other agents ignore `enterprise_host`.
pub fn build_login_cmd(agent_id: &str, enterprise_host: Option<&str>) -> String {
    let exe_path = find_exe(agent_id)
        .unwrap_or_else(|| agent_id.to_string());

    // Agent-specific login subcommand
    let subcommand = match agent_id {
        "codex" => "auth",
        "gemini" => "auth login",
        _ => "login",
    };

    // Only Copilot supports a custom enterprise host on the login command.
    let host_arg = if agent_id == "copilot" {
        enterprise_host
            .and_then(normalize_enterprise_host)
            .map(|h| format!(" --host https://{}", h))
            .unwrap_or_default()
    } else {
        String::new()
    };

    if exe_path.contains(' ') {
        format!("\"{}\" {}{}", exe_path, subcommand, host_arg)
    } else {
        format!("{} {}{}", exe_path, subcommand, host_arg)
    }
}

/// Normalize a user-entered GitHub Enterprise domain into a bare host suitable
/// for `--host https://<host>`. Strips any scheme (case-insensitively) and any
/// path/query/fragment, keeping only `host[:port]`. An empty value or plain
/// `github.com` means "no enterprise host" (returns `None`).
pub fn normalize_enterprise_host(raw: &str) -> Option<String> {
    let mut host = raw.trim();
    // Strip an optional scheme, case-insensitively (so `HTTPS://…` works too).
    for scheme in ["https://", "http://"] {
        if host.len() >= scheme.len() && host[..scheme.len()].eq_ignore_ascii_case(scheme) {
            host = &host[scheme.len()..];
            break;
        }
    }
    // Keep only the authority (`host[:port]`); drop any path/query/fragment so a
    // pasted full URL like `corp.ghe.com/foo` doesn't leak into the command or
    // the device-verification URL.
    let host = host.split(['/', '?', '#']).next().unwrap_or("").trim();
    if host.is_empty() || host.eq_ignore_ascii_case("github.com") {
        None
    } else {
        Some(host.to_string())
    }
}

/// Path to the small JSON file that persists Copilot auth preferences (the
/// last-used GitHub Enterprise host). Lives in the package-private state root
/// alongside the WT app's own settings.
fn copilot_auth_config_path() -> Option<std::path::PathBuf> {
    crate::runtime_paths::intelligent_terminal_root().map(|r| r.join("copilot-auth.json"))
}

/// Load the persisted Copilot GitHub Enterprise host, if any.
pub fn load_copilot_enterprise_host() -> Option<String> {
    let path = copilot_auth_config_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
    let host = parsed
        .get("enterpriseHost")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    tracing::debug!(target: "agent_check", host = ?host, "loaded copilot enterprise host");
    host
}

/// Persist (or clear) the Copilot GitHub Enterprise host for next time.
pub fn save_copilot_enterprise_host(host: &str) {
    let Some(path) = copilot_auth_config_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({ "enterpriseHost": host });
    if let Ok(text) = serde_json::to_string_pretty(&body) {
        let _ = std::fs::write(&path, text);
    }
    tracing::debug!(target: "agent_check", host = %host, "saved copilot enterprise host");
}

/// Install an agent via winget. Streams output lines through `on_line` callback.
/// On success, refreshes the process PATH so subsequent `find_exe` calls find
/// the new binary.
pub async fn install(agent_id: &str, on_line: impl FnMut(String) + Send + 'static) -> Result<(), String> {
    match agent_id {
        "copilot" => install_copilot(on_line).await,
        _ => Err(t!("agent.install.unsupported", agent = agent_id).into_owned()),
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

/// Build the PATH a freshly-spawned child process should inherit.
///
/// Windows Terminal (and therefore the `wta-master` / `wta` children it
/// spawns) captures its environment block at process start. When an agent
/// CLI is installed *after* WT is already running — e.g. the FRE
/// winget-installs `copilot` mid-session — our inherited PATH stays stale,
/// so `CreateProcess` (or `cmd /c <cli>`) can't resolve the bare CLI name
/// and the spawn fails with "is not recognized", which surfaces as an
/// immediate ACP-initialize failure. Rebuild PATH from the registry
/// (system + user) so a just-installed CLI resolves without a full WT
/// restart, merging in the current process PATH so no runtime-only entry
/// is lost. Returns `None` when the registry read yields nothing usable.
pub fn spawn_path() -> Option<String> {
    let fresh = fresh_path();
    if fresh.is_empty() {
        return None;
    }
    let current = std::env::var("PATH").unwrap_or_default();
    Some(merge_paths(&fresh, &current))
}

/// Concatenate two `;`-separated PATH strings, preferring `fresh` ordering
/// and dropping case-insensitive duplicates (ignoring a trailing
/// backslash). Skips empty segments.
fn merge_paths(fresh: &str, current: &str) -> String {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<&str> = Vec::new();
    for part in fresh.split(';').chain(current.split(';')) {
        if part.is_empty() {
            continue;
        }
        let key = part.trim_end_matches('\\').to_ascii_lowercase();
        if seen.insert(key) {
            out.push(part);
        }
    }
    out.join(";")
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

    on_line(t!("agent.install.running_winget").into_owned());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Err(t!("agent.install.launch_failed", error = e.to_string()).into_owned()),
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
        Err(e) => return Err(t!("agent.install.winget_exited", error = e.to_string()).into_owned()),
    };

    let _ = forward.await;

    if status.success() {
        refresh_path();
        Ok(())
    } else {
        let code = status.code().unwrap_or(-1);
        Err(t!("agent.install.winget_failed_code", code = code.to_string()).into_owned())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_paths_prefers_fresh_and_removes_duplicates_case_insensitively() {
        let fresh = r"C:\WinGet\Links;C:\Windows\System32";
        let current = r"C:\windows\system32\;C:\Runtime\Only";
        let merged = merge_paths(fresh, current);
        assert_eq!(
            merged,
            r"C:\WinGet\Links;C:\Windows\System32;C:\Runtime\Only"
        );
    }

    #[test]
    fn merge_paths_skips_empty_segments() {
        let merged = merge_paths(r"C:\A;;C:\B", r";C:\B;");
        assert_eq!(merged, r"C:\A;C:\B");
    }

    #[test]
    fn merge_paths_keeps_runtime_only_entries() {
        // An entry present only in the live process PATH (not the registry)
        // must survive so we never regress a runtime-injected directory.
        let merged = merge_paths(r"C:\Reg", r"C:\Reg;C:\OnlyAtRuntime");
        assert_eq!(merged, r"C:\Reg;C:\OnlyAtRuntime");
    }

    #[test]
    fn normalize_enterprise_host_strips_scheme_and_rejects_default() {
        assert_eq!(
            normalize_enterprise_host("mycompany.ghe.com"),
            Some("mycompany.ghe.com".to_string())
        );
        assert_eq!(
            normalize_enterprise_host("  mycompany.ghe.com  "),
            Some("mycompany.ghe.com".to_string())
        );
        assert_eq!(
            normalize_enterprise_host("https://mycompany.ghe.com/"),
            Some("mycompany.ghe.com".to_string())
        );
        assert_eq!(
            normalize_enterprise_host("http://mycompany.ghe.com"),
            Some("mycompany.ghe.com".to_string())
        );
        // Empty, whitespace, or plain github.com mean "no enterprise host".
        assert_eq!(normalize_enterprise_host(""), None);
        assert_eq!(normalize_enterprise_host("   "), None);
        assert_eq!(normalize_enterprise_host("github.com"), None);
        assert_eq!(normalize_enterprise_host("GitHub.com"), None);
    }

    /// Hardening (review fix ③): an uppercase scheme must still be stripped, a
    /// pasted full URL must keep only `host[:port]` (dropping any path/query),
    /// and a `github.com` with scheme/path is still the default (None).
    #[test]
    fn normalize_enterprise_host_strips_uppercase_scheme_and_path() {
        assert_eq!(
            normalize_enterprise_host("HTTPS://corp.ghe.com"),
            Some("corp.ghe.com".to_string())
        );
        assert_eq!(
            normalize_enterprise_host("https://corp.ghe.com/some/path"),
            Some("corp.ghe.com".to_string())
        );
        assert_eq!(
            normalize_enterprise_host("corp.ghe.com/foo"),
            Some("corp.ghe.com".to_string())
        );
        // A port is part of the authority and must be preserved.
        assert_eq!(
            normalize_enterprise_host("corp.ghe.com:8443"),
            Some("corp.ghe.com:8443".to_string())
        );
        assert_eq!(
            normalize_enterprise_host("https://corp.ghe.com:8443/x?y#z"),
            Some("corp.ghe.com:8443".to_string())
        );
        // github.com with a scheme/path is still the default (no enterprise).
        assert_eq!(normalize_enterprise_host("github.com/foo"), None);
        assert_eq!(normalize_enterprise_host("HTTP://GitHub.com"), None);
    }

    #[test]
    fn build_login_cmd_copilot_appends_enterprise_host() {
        // exe path may resolve to a full path on dev machines, so assert on
        // the suffix / substring rather than an exact string.
        let base = build_login_cmd("copilot", None);
        assert!(base.trim_end().ends_with("login"), "default copilot: {base}");
        assert!(!base.contains("--host"), "default must not add --host: {base}");

        let ghe = build_login_cmd("copilot", Some("mycompany.ghe.com"));
        assert!(
            ghe.contains("login --host https://mycompany.ghe.com"),
            "GHE login: {ghe}"
        );

        // A scheme-prefixed domain is normalized (no double scheme).
        let ghe2 = build_login_cmd("copilot", Some("https://corp.ghe.com/"));
        assert!(
            ghe2.contains("login --host https://corp.ghe.com"),
            "normalized GHE login: {ghe2}"
        );
        assert!(!ghe2.contains("https://https://"), "no double scheme: {ghe2}");

        // Plain github.com is the default — no --host.
        let gh = build_login_cmd("copilot", Some("github.com"));
        assert!(!gh.contains("--host"), "github.com must not add --host: {gh}");
    }

    #[test]
    fn build_login_cmd_non_copilot_ignores_host() {
        // Only Copilot honors an enterprise host; other agents never get one.
        let claude = build_login_cmd("claude", Some("mycompany.ghe.com"));
        assert!(!claude.contains("--host"), "claude must ignore host: {claude}");
        assert!(claude.contains("login"), "claude login: {claude}");

        let codex = build_login_cmd("codex", Some("mycompany.ghe.com"));
        assert!(codex.contains("auth"), "codex auth: {codex}");
        assert!(!codex.contains("--host"), "codex must ignore host: {codex}");
    }
}

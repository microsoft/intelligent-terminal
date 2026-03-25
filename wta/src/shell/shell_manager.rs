use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;
use tokio::process::Child;
use tokio::process::Command;

use super::wt_channel::ConnectionInfo;
use super::wt_channel::WtChannel;

/// Configuration for creating a new terminal.
pub struct TerminalConfig {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
}

/// Output from a managed terminal.
pub struct TerminalOutput {
    pub data: String,
    pub exit_status: Option<u32>,
}

/// Snapshot of the active Windows Terminal pane plus its recent visible content.
pub struct ActivePaneSnapshot {
    pub window_id: String,
    pub tab_id: String,
    pub pane_id: String,
    pub title: Option<String>,
    pub profile: Option<String>,
    pub content: Option<String>,
    pub line_count: Option<u64>,
    pub truncated: bool,
}

/// A local subprocess terminal.
struct LocalTerminal {
    child: Mutex<Child>,
    output: Arc<Mutex<String>>,
    exited: Arc<Mutex<Option<u32>>>,
}

/// A terminal backed by a Windows Terminal pane (via pipe protocol).
struct WtPaneTerminal {
    pane_id: String,
}

/// Either a local subprocess or a WT pane.
enum Terminal {
    Local(LocalTerminal),
    WtPane(WtPaneTerminal),
}

/// Protocol-agnostic shell integration layer.
/// Manages terminal subprocesses — shared between ACP and MCP modes.
/// When a WtChannel is available, `create_terminal` creates real WT panes
/// instead of headless subprocesses. All other operations (get_output,
/// wait_for_exit, kill, release) are routed accordingly.
pub struct ShellManager {
    terminals: Mutex<HashMap<String, Terminal>>,
    next_id: Mutex<u64>,
    wt_channel: Option<Arc<dyn WtChannel>>,
    wt_connection_info: Option<ConnectionInfo>,
}

impl ShellManager {
    fn value_to_string(value: Option<&serde_json::Value>) -> Option<String> {
        match value {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    pub fn new() -> Self {
        Self {
            terminals: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
            wt_channel: None,
            wt_connection_info: None,
        }
    }

    pub fn with_wt_channel(mut self, channel: Arc<dyn WtChannel>) -> Self {
        self.wt_channel = Some(channel);
        self
    }

    pub fn with_wt_connection_info(mut self, info: ConnectionInfo) -> Self {
        self.wt_connection_info = Some(info);
        self
    }

    /// Whether a Windows Terminal channel is connected.
    pub fn has_wt_channel(&self) -> bool {
        self.wt_channel
            .as_ref()
            .map_or(false, |ch| ch.is_available())
    }

    fn next_id(&self) -> String {
        let mut next = self.next_id.lock().unwrap();
        let id = format!("term_{}", *next);
        *next += 1;
        id
    }

    // ── create_terminal ─────────────────────────────────────────────────

    /// Create a terminal. Routes to WT pane if available, else local subprocess.
    /// Falls back to local if WT fails.
    pub async fn create_terminal(&self, mut config: TerminalConfig) -> anyhow::Result<String> {
        // Nested `wta` CLI commands are control helpers, not interactive jobs.
        // Run them locally so they don't create background WT tabs.
        if self.should_force_local(&config) {
            self.inject_wt_env(&mut config);
            return self.create_terminal_local(config).await;
        }

        if self.has_wt_channel() {
            match self.create_terminal_wt(&config).await {
                Ok(id) => return Ok(id),
                Err(e) => {
                    eprintln!("[wta] WT create_tab failed, falling back to local: {}", e);
                    // fall through to local
                }
            }
        }
        self.create_terminal_local(config).await
    }

    fn should_force_local(&self, config: &TerminalConfig) -> bool {
        let file_name = Path::new(&config.command)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&config.command);

        file_name.eq_ignore_ascii_case("wta") || file_name.eq_ignore_ascii_case("wta.exe")
    }

    fn inject_wt_env(&self, config: &mut TerminalConfig) {
        let Some(info) = &self.wt_connection_info else {
            return;
        };

        if !config
            .env
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("WT_PIPE_NAME"))
        {
            config
                .env
                .push(("WT_PIPE_NAME".to_string(), info.pipe_name.clone()));
        }

        if !config
            .env
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("WT_MCP_TOKEN"))
        {
            config
                .env
                .push(("WT_MCP_TOKEN".to_string(), info.token.clone()));
        }
    }

    /// Create a WT pane-backed terminal via the pipe protocol.
    async fn create_terminal_wt(&self, config: &TerminalConfig) -> anyhow::Result<String> {
        let id = self.next_id();
        let wt = self.wt()?;

        // Build the commandline string: "command arg1 arg2 ..."
        let mut cmdline = config.command.clone();
        for arg in &config.args {
            cmdline.push(' ');
            // Quote args containing spaces
            if arg.contains(' ') {
                cmdline.push('"');
                cmdline.push_str(arg);
                cmdline.push('"');
            } else {
                cmdline.push_str(arg);
            }
        }

        // Create a new tab in WT with the command
        let mut params = serde_json::Map::new();
        params.insert("commandline".into(), cmdline.into());
        if let Some(ref dir) = config.cwd {
            params.insert("cwd".into(), dir.clone().into());
        }
        params.insert("title".into(), format!("[wta] {}", config.command).into());
        // Create in background so it doesn't steal focus from wta's TUI
        params.insert("background".into(), true.into());

        let result = wt
            .request("create_tab", serde_json::Value::Object(params))
            .await?;

        // Extract the pane_id from the response
        let pane_id = result
            .get("pane_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("create_tab response missing pane_id: {}", result))?
            .to_string();

        self.terminals
            .lock()
            .unwrap()
            .insert(id.clone(), Terminal::WtPane(WtPaneTerminal { pane_id }));

        Ok(id)
    }

    /// Create a local subprocess terminal (original behavior).
    async fn create_terminal_local(&self, config: TerminalConfig) -> anyhow::Result<String> {
        let id = self.next_id();

        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(ref dir) = config.cwd {
            cmd.current_dir(dir);
        }
        for (k, v) in &config.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn()?;

        let output = Arc::new(Mutex::new(String::new()));
        let exited = Arc::new(Mutex::new(None));

        // Spawn stdout capture task
        let out_buf = output.clone();
        if let Some(mut stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match stdout.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                                if let Ok(mut out) = out_buf.lock() {
                                    out.push_str(s);
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        // Spawn stderr capture task (into same buffer)
        let out_buf2 = output.clone();
        if let Some(mut stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                loop {
                    match stderr.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            if let Ok(s) = std::str::from_utf8(&buf[..n]) {
                                if let Ok(mut out) = out_buf2.lock() {
                                    out.push_str(s);
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        }

        self.terminals.lock().unwrap().insert(
            id.clone(),
            Terminal::Local(LocalTerminal {
                child: Mutex::new(child),
                output,
                exited,
            }),
        );

        Ok(id)
    }

    // ── get_output ──────────────────────────────────────────────────────

    /// Get output. For WT panes this must be async (pipe call).
    pub async fn get_output(&self, terminal_id: &str) -> anyhow::Result<TerminalOutput> {
        let is_wt_pane = {
            let terminals = self.terminals.lock().unwrap();
            let term = terminals
                .get(terminal_id)
                .ok_or_else(|| anyhow::anyhow!("unknown terminal: {}", terminal_id))?;
            matches!(term, Terminal::WtPane(_))
        };

        if is_wt_pane {
            self.get_output_wt(terminal_id).await
        } else {
            self.get_output_local(terminal_id)
        }
    }

    async fn get_output_wt(&self, terminal_id: &str) -> anyhow::Result<TerminalOutput> {
        let pane_id = {
            let terminals = self.terminals.lock().unwrap();
            match terminals.get(terminal_id) {
                Some(Terminal::WtPane(wt)) => wt.pane_id.clone(),
                _ => return Err(anyhow::anyhow!("unknown terminal: {}", terminal_id)),
            }
        };

        let wt = self.wt()?;

        // Read pane output
        let output_result = wt
            .request(
                "read_pane_output",
                serde_json::json!({ "pane_id": pane_id }),
            )
            .await?;

        let data = output_result
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Check process status for exit code
        let status_result = wt
            .request(
                "get_process_status",
                serde_json::json!({ "pane_id": pane_id }),
            )
            .await?;

        let exit_status = status_result
            .get("exit_code")
            .and_then(|v| v.as_u64())
            .map(|c| c as u32);

        Ok(TerminalOutput { data, exit_status })
    }

    fn get_output_local(&self, terminal_id: &str) -> anyhow::Result<TerminalOutput> {
        let terminals = self.terminals.lock().unwrap();
        match terminals.get(terminal_id) {
            Some(Terminal::Local(term)) => {
                let data = {
                    let mut buf = term.output.lock().unwrap();
                    let s = buf.clone();
                    buf.clear();
                    s
                };
                let exit_status = *term.exited.lock().unwrap();
                Ok(TerminalOutput { data, exit_status })
            }
            _ => Err(anyhow::anyhow!("unknown terminal: {}", terminal_id)),
        }
    }

    // ── wait_for_exit ───────────────────────────────────────────────────

    /// Wait for a terminal to exit, return exit code.
    pub async fn wait_for_exit(&self, terminal_id: &str) -> anyhow::Result<u32> {
        let is_wt_pane = {
            let terminals = self.terminals.lock().unwrap();
            let term = terminals
                .get(terminal_id)
                .ok_or_else(|| anyhow::anyhow!("unknown terminal: {}", terminal_id))?;
            matches!(term, Terminal::WtPane(_))
        };

        if is_wt_pane {
            self.wait_for_exit_wt(terminal_id).await
        } else {
            self.wait_for_exit_local(terminal_id).await
        }
    }

    async fn wait_for_exit_wt(&self, terminal_id: &str) -> anyhow::Result<u32> {
        let pane_id = {
            let terminals = self.terminals.lock().unwrap();
            match terminals.get(terminal_id) {
                Some(Terminal::WtPane(wt)) => wt.pane_id.clone(),
                _ => return Err(anyhow::anyhow!("unknown terminal: {}", terminal_id)),
            }
        };

        let wt = self.wt()?;

        // Poll process status until we get an exit code
        loop {
            let result = wt
                .request(
                    "get_process_status",
                    serde_json::json!({ "pane_id": pane_id }),
                )
                .await?;

            if let Some(code) = result.get("exit_code").and_then(|v| v.as_u64()) {
                return Ok(code as u32);
            }

            // Check if the process is still running
            let is_running = result
                .get("state")
                .and_then(|v| v.as_str())
                .map(|s| s == "running")
                .unwrap_or(true);

            if !is_running {
                // Process exited but no exit code available
                return Ok(result
                    .get("exit_code")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32);
            }

            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    async fn wait_for_exit_local(&self, terminal_id: &str) -> anyhow::Result<u32> {
        loop {
            {
                let terminals = self.terminals.lock().unwrap();
                if let Some(Terminal::Local(term)) = terminals.get(terminal_id) {
                    let mut child = term.child.lock().unwrap();
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            let code = status.code().unwrap_or(1) as u32;
                            *term.exited.lock().unwrap() = Some(code);
                            return Ok(code);
                        }
                        Ok(None) => {} // still running
                        Err(e) => return Err(e.into()),
                    }
                } else {
                    return Err(anyhow::anyhow!("unknown terminal: {}", terminal_id));
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    // ── kill ─────────────────────────────────────────────────────────────

    /// Kill a terminal's process.
    pub async fn kill(&self, terminal_id: &str) -> anyhow::Result<()> {
        let is_wt_pane = {
            let terminals = self.terminals.lock().unwrap();
            let term = terminals
                .get(terminal_id)
                .ok_or_else(|| anyhow::anyhow!("unknown terminal: {}", terminal_id))?;
            matches!(term, Terminal::WtPane(_))
        };

        if is_wt_pane {
            let pane_id = {
                let terminals = self.terminals.lock().unwrap();
                match terminals.get(terminal_id) {
                    Some(Terminal::WtPane(wt)) => wt.pane_id.clone(),
                    _ => return Err(anyhow::anyhow!("unknown terminal: {}", terminal_id)),
                }
            };
            let wt = self.wt()?;
            wt.request("close_pane", serde_json::json!({ "pane_id": pane_id }))
                .await?;
        } else {
            let terminals = self.terminals.lock().unwrap();
            if let Some(Terminal::Local(term)) = terminals.get(terminal_id) {
                let mut child = term.child.lock().unwrap();
                let _ = child.start_kill();
            }
        }
        Ok(())
    }

    // ── release ──────────────────────────────────────────────────────────

    /// Release (kill + remove) a terminal.
    pub async fn release(&self, terminal_id: &str) -> anyhow::Result<()> {
        // Kill first (for WT panes, closes the pane)
        let _ = self.kill(terminal_id).await;

        // Remove from tracking
        self.terminals.lock().unwrap().remove(terminal_id);
        Ok(())
    }

    // ── Windows Terminal protocol operations ────────────────────────────

    fn wt(&self) -> anyhow::Result<&dyn WtChannel> {
        self.wt_channel
            .as_deref()
            .filter(|ch| ch.is_available())
            .ok_or_else(|| anyhow::anyhow!("No Windows Terminal channel available"))
    }

    /// List all WT windows.
    pub async fn wt_list_windows(&self) -> anyhow::Result<serde_json::Value> {
        self.wt()?
            .request("list_windows", serde_json::json!({}))
            .await
    }

    /// List tabs in a window.
    pub async fn wt_list_tabs(&self, window_id: &str) -> anyhow::Result<serde_json::Value> {
        self.wt()?
            .request("list_tabs", serde_json::json!({ "window_id": window_id }))
            .await
    }

    /// List panes in a tab.
    pub async fn wt_list_panes(&self, tab_id: &str) -> anyhow::Result<serde_json::Value> {
        self.wt()?
            .request("list_panes", serde_json::json!({ "tab_id": tab_id }))
            .await
    }

    /// Create a new tab in WT. Returns the raw response JSON.
    pub async fn wt_create_tab(
        &self,
        commandline: Option<&str>,
        cwd: Option<&str>,
        title: Option<&str>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut params = serde_json::Map::new();
        if let Some(cmd) = commandline {
            params.insert("commandline".into(), cmd.into());
        }
        if let Some(dir) = cwd {
            params.insert("cwd".into(), dir.into());
        }
        if let Some(t) = title {
            params.insert("title".into(), t.into());
        }
        self.wt()?
            .request("create_tab", serde_json::Value::Object(params))
            .await
    }

    /// Split an existing pane. Returns the raw response JSON.
    pub async fn wt_split_pane(
        &self,
        pane_id: &str,
        commandline: Option<&str>,
        direction: Option<&str>,
        size: Option<f64>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut params = serde_json::Map::new();
        params.insert("pane_id".into(), pane_id.into());
        if let Some(cmd) = commandline {
            params.insert("commandline".into(), cmd.into());
        }
        if let Some(dir) = direction {
            params.insert("direction".into(), dir.into());
        }
        if let Some(s) = size {
            params.insert("size".into(), s.into());
        }
        self.wt()?
            .request("split_pane", serde_json::Value::Object(params))
            .await
    }

    /// Send keystrokes / text to a pane.
    pub async fn wt_send_input(
        &self,
        pane_id: &str,
        input: &str,
    ) -> anyhow::Result<serde_json::Value> {
        self.wt()?
            .request(
                "send_input",
                serde_json::json!({ "pane_id": pane_id, "text": input }),
            )
            .await
    }

    /// Read recent output from a pane.
    pub async fn wt_read_pane_output(
        &self,
        pane_id: &str,
        max_lines: Option<u32>,
    ) -> anyhow::Result<serde_json::Value> {
        let mut params = serde_json::Map::new();
        params.insert("pane_id".into(), pane_id.into());
        if let Some(n) = max_lines {
            params.insert("max_lines".into(), n.into());
        }
        self.wt()?
            .request("read_pane_output", serde_json::Value::Object(params))
            .await
    }

    /// Close a pane.
    pub async fn wt_close_pane(&self, pane_id: &str) -> anyhow::Result<serde_json::Value> {
        self.wt()?
            .request("close_pane", serde_json::json!({ "pane_id": pane_id }))
            .await
    }

    /// Get process status for a pane.
    pub async fn wt_get_process_status(&self, pane_id: &str) -> anyhow::Result<serde_json::Value> {
        self.wt()?
            .request(
                "get_process_status",
                serde_json::json!({ "pane_id": pane_id }),
            )
            .await
    }

    /// Get the active pane info.
    pub async fn wt_get_active_pane(&self) -> anyhow::Result<serde_json::Value> {
        self.wt()?
            .request("get_active_pane", serde_json::json!({}))
            .await
    }

    /// Capture the current active pane and a snapshot of its recent output.
    pub async fn wt_active_pane_snapshot(
        &self,
        max_lines: Option<u32>,
    ) -> anyhow::Result<ActivePaneSnapshot> {
        let active = self.wt_get_active_pane().await?;
        let pane_id = Self::value_to_string(active.get("pane_id"))
            .ok_or_else(|| anyhow::anyhow!("active pane response missing pane_id"))?;
        let tab_id = Self::value_to_string(active.get("tab_id"))
            .ok_or_else(|| anyhow::anyhow!("active pane response missing tab_id"))?;
        let window_id = Self::value_to_string(active.get("window_id"))
            .ok_or_else(|| anyhow::anyhow!("active pane response missing window_id"))?;

        let output = self.wt_read_pane_output(&pane_id, max_lines).await.ok();

        Ok(ActivePaneSnapshot {
            window_id,
            tab_id,
            pane_id,
            title: active
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            profile: active
                .get("profile")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            content: output
                .as_ref()
                .and_then(|val| val.get("content"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            line_count: output
                .as_ref()
                .and_then(|val| val.get("line_count"))
                .and_then(|v| v.as_u64()),
            truncated: output
                .as_ref()
                .and_then(|val| val.get("truncated"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        })
    }

    /// Show a quick-pick dialog in WT and return the user's selection.
    pub async fn wt_quick_pick(
        &self,
        title: &str,
        choices: &[String],
        allow_free_input: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let choices_json: Vec<serde_json::Value> = choices
            .iter()
            .map(|c| serde_json::Value::String(c.clone()))
            .collect();
        self.wt()?
            .request(
                "quick_pick",
                serde_json::json!({
                    "title": title,
                    "choices": choices_json,
                    "allow_free_input": allow_free_input,
                }),
            )
            .await
    }
}

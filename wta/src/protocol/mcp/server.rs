use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Implementation, InitializeRequestParams, InitializeResult, ServerCapabilities, ServerInfo,
};
use rmcp::schemars;
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use serde::Deserialize;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

use crate::shell::{ActivePaneSnapshot, ShellManager, TerminalConfig};

const ACTIVE_PANE_CONTEXT_MAX_LINES: u32 = 80;
const ACTIVE_PANE_CONTEXT_MAX_CHARS: usize = 4000;

/// Write a line to wta-mcp-debug.log (tool call tracing).
fn mcp_log(msg: &str) {
    use std::io::Write;
    if std::env::var("WTA_DEBUG_LOG").as_deref() == Ok("0") {
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("wta-mcp-debug.log")
    {
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let _ = writeln!(f, "[{:.3}] {}", elapsed.as_secs_f64(), msg);
        let _ = f.flush();
    }
}

#[derive(Clone)]
pub struct WtaMcpServer {
    shell_mgr: Arc<ShellManager>,
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RunCommandParams {
    /// The command to run
    pub command: String,
    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory (optional)
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateTerminalParams {
    /// The command to run in the terminal
    pub command: String,
    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,
    /// Working directory (optional)
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TerminalIdParams {
    /// The terminal ID
    pub terminal_id: String,
}

// ── WT protocol tool params ─────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WindowIdParams {
    /// The window ID (from list_windows)
    pub window_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TabIdParams {
    /// The tab ID (from list_tabs)
    pub tab_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PaneIdParams {
    /// The pane ID
    pub pane_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadPaneOutputParams {
    /// The pane ID to read output from
    pub pane_id: String,
    /// Maximum number of lines to return (optional)
    pub max_lines: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendInputParams {
    /// The pane ID to send input to
    pub pane_id: String,
    /// The text/keystrokes to send
    pub input: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CreateTabParams {
    /// Command to run in the new tab (optional, defaults to default shell)
    pub commandline: Option<String>,
    /// Working directory (optional)
    pub cwd: Option<String>,
    /// Tab title (optional)
    pub title: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SplitPaneParams {
    /// The pane ID to split
    pub pane_id: String,
    /// Command to run in the new pane (optional)
    pub commandline: Option<String>,
    /// Split direction: "horizontal" or "vertical" (optional, default: "auto")
    pub direction: Option<String>,
    /// Size fraction 0.0-1.0 (optional, default: 0.5)
    pub size: Option<f64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct QuickPickParams {
    /// Title/question shown to the user
    pub title: String,
    /// Choices to present
    pub choices: Vec<String>,
    /// Whether to allow freeform text input in addition to choices
    #[serde(default)]
    pub allow_free_input: bool,
}

impl WtaMcpServer {
    pub fn new(shell_mgr: Arc<ShellManager>) -> Self {
        Self {
            shell_mgr,
            tool_router: Self::tool_router(),
        }
    }

    fn json_pretty(val: &serde_json::Value) -> String {
        serde_json::to_string_pretty(val).unwrap_or_else(|_| val.to_string())
    }

    fn truncate(s: &str, max: usize) -> String {
        if s.len() > max {
            format!("{}...", &s[..max])
        } else {
            s.to_string()
        }
    }

    fn truncate_for_prompt(s: &str, max_chars: usize) -> String {
        if s.chars().count() <= max_chars {
            s.to_string()
        } else {
            let truncated: String = s.chars().take(max_chars).collect();
            format!("{truncated}\n...<truncated>")
        }
    }

    fn format_active_pane_context(snapshot: &ActivePaneSnapshot) -> String {
        let mut context = format!(
            "Startup active pane context:\n\
             - window_id={}\n\
             - tab_id={}\n\
             - pane_id={}\n",
            snapshot.window_id, snapshot.tab_id, snapshot.pane_id
        );

        if let Some(title) = &snapshot.title {
            context.push_str(&format!("             - title={}\n", title));
        }
        if let Some(profile) = &snapshot.profile {
            context.push_str(&format!("             - profile={}\n", profile));
        }
        if let Some(line_count) = snapshot.line_count {
            context.push_str(&format!(
                "             - captured_line_count={}\n",
                line_count
            ));
        }
        if snapshot.truncated {
            context.push_str("             - captured_output_truncated=true\n");
        }
        if let Some(content) = &snapshot.content {
            context.push_str("             - recent pane content:\n");
            context.push_str("```text\n");
            context.push_str(&Self::truncate_for_prompt(
                content,
                ACTIVE_PANE_CONTEXT_MAX_CHARS,
            ));
            if !content.ends_with('\n') {
                context.push('\n');
            }
            context.push_str("```\n");
        }

        context
    }

    async fn build_initialize_instructions(&self) -> String {
        let mut instructions = String::from(
            "WTA (Windows Terminal Agent) — provides shell tools and Windows Terminal integration. \
             Use the wt_* tools to inspect and control Windows Terminal windows, tabs, and panes. \
             Use wt_list_windows → wt_list_tabs → wt_list_panes to discover pane IDs, then \
             wt_read_pane_output to see what's on screen, wt_send_input to type commands, \
             and wt_create_tab / wt_split_pane to create new sessions.",
        );

        if let Ok(snapshot) = self
            .shell_mgr
            .wt_active_pane_snapshot(Some(ACTIVE_PANE_CONTEXT_MAX_LINES))
            .await
        {
            instructions.push_str("\n\n");
            instructions.push_str(&Self::format_active_pane_context(&snapshot));
        }

        instructions
    }
}

#[tool_router]
impl WtaMcpServer {
    // ── Local shell tools ───────────────────────────────────────────

    /// Run a command to completion and return its output.
    #[tool(description = "Run a command and return its output")]
    async fn run_command(&self, Parameters(params): Parameters<RunCommandParams>) -> String {
        mcp_log(&format!(
            ">>> run_command({:?}, args={:?}, cwd={:?})",
            params.command, params.args, params.cwd
        ));
        let config = TerminalConfig {
            command: params.command,
            args: params.args,
            cwd: params.cwd,
            env: vec![],
        };

        let id = match self.shell_mgr.create_terminal(config).await {
            Ok(id) => id,
            Err(e) => {
                let r = format!("Error creating terminal: {}", e);
                mcp_log(&format!("<<< run_command: {}", r));
                return r;
            }
        };

        let exit_code = match self.shell_mgr.wait_for_exit(&id).await {
            Ok(code) => code,
            Err(e) => {
                let r = format!("Error waiting for exit: {}", e);
                mcp_log(&format!("<<< run_command: {}", r));
                return r;
            }
        };

        let output = match self.shell_mgr.get_output(&id).await {
            Ok(o) => o.data,
            Err(e) => {
                let r = format!("Error getting output: {}", e);
                mcp_log(&format!("<<< run_command: {}", r));
                return r;
            }
        };

        let _ = self.shell_mgr.release(&id).await;

        let result = format!("Exit code: {}\n\n{}", exit_code, output);
        mcp_log(&format!(
            "<<< run_command: exit_code={}, output_len={}",
            exit_code,
            output.len()
        ));
        result
    }

    /// Create a persistent terminal session and return its ID.
    #[tool(description = "Create a persistent terminal session")]
    async fn create_terminal(
        &self,
        Parameters(params): Parameters<CreateTerminalParams>,
    ) -> String {
        mcp_log(&format!(
            ">>> create_terminal({:?}, args={:?}, cwd={:?})",
            params.command, params.args, params.cwd
        ));
        let config = TerminalConfig {
            command: params.command,
            args: params.args,
            cwd: params.cwd,
            env: vec![],
        };

        let result = match self.shell_mgr.create_terminal(config).await {
            Ok(id) => format!("Terminal created: {}", id),
            Err(e) => format!("Error creating terminal: {}", e),
        };
        mcp_log(&format!("<<< create_terminal: {}", result));
        result
    }

    /// Get buffered output from a terminal session.
    #[tool(description = "Get output from a terminal session")]
    async fn get_terminal_output(
        &self,
        Parameters(params): Parameters<TerminalIdParams>,
    ) -> String {
        mcp_log(&format!(">>> get_terminal_output({})", params.terminal_id));
        let result = match self.shell_mgr.get_output(&params.terminal_id).await {
            Ok(output) => {
                let status = match output.exit_status {
                    Some(code) => format!(" (exited: {})", code),
                    None => " (running)".to_string(),
                };
                format!("[{}{}]\n{}", params.terminal_id, status, output.data)
            }
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!("<<< get_terminal_output: {} bytes", result.len()));
        result
    }

    /// Wait for a terminal to exit and return the exit code.
    #[tool(description = "Wait for a terminal to exit")]
    async fn wait_for_terminal(&self, Parameters(params): Parameters<TerminalIdParams>) -> String {
        mcp_log(&format!(">>> wait_for_terminal({})", params.terminal_id));
        let result = match self.shell_mgr.wait_for_exit(&params.terminal_id).await {
            Ok(code) => format!("Terminal {} exited with code {}", params.terminal_id, code),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!("<<< wait_for_terminal: {}", result));
        result
    }

    /// Kill a terminal session.
    #[tool(description = "Kill a terminal session")]
    async fn kill_terminal(&self, Parameters(params): Parameters<TerminalIdParams>) -> String {
        mcp_log(&format!(">>> kill_terminal({})", params.terminal_id));
        let result = match self.shell_mgr.kill(&params.terminal_id).await {
            Ok(()) => format!("Terminal {} killed", params.terminal_id),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!("<<< kill_terminal: {}", result));
        result
    }

    // ── Windows Terminal state tools ────────────────────────────────

    /// List all Windows Terminal windows. Returns window IDs, titles, and tab counts.
    #[tool(description = "List all Windows Terminal windows with their IDs and titles")]
    async fn wt_list_windows(&self) -> String {
        mcp_log(">>> wt_list_windows()");
        let result = match self.shell_mgr.wt_list_windows().await {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_list_windows: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    /// List all tabs in a Windows Terminal window.
    #[tool(
        description = "List all tabs in a Windows Terminal window. Use window_id from wt_list_windows."
    )]
    async fn wt_list_tabs(&self, Parameters(params): Parameters<WindowIdParams>) -> String {
        mcp_log(&format!(">>> wt_list_tabs(window_id={})", params.window_id));
        let result = match self.shell_mgr.wt_list_tabs(&params.window_id).await {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_list_tabs: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    /// List all panes in a tab.
    #[tool(description = "List all panes in a tab. Use tab_id from wt_list_tabs.")]
    async fn wt_list_panes(&self, Parameters(params): Parameters<TabIdParams>) -> String {
        mcp_log(&format!(">>> wt_list_panes(tab_id={})", params.tab_id));
        let result = match self.shell_mgr.wt_list_panes(&params.tab_id).await {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_list_panes: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    /// Get info about the currently active/focused pane.
    #[tool(description = "Get the currently active pane's ID and info")]
    async fn wt_get_active_pane(&self) -> String {
        mcp_log(">>> wt_get_active_pane()");
        let result = match self.shell_mgr.wt_get_active_pane().await {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_get_active_pane: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    /// Read recent output from a terminal pane.
    #[tool(
        description = "Read recent output text from a terminal pane. Use pane_id from wt_list_panes or wt_get_active_pane."
    )]
    async fn wt_read_pane_output(
        &self,
        Parameters(params): Parameters<ReadPaneOutputParams>,
    ) -> String {
        mcp_log(&format!(
            ">>> wt_read_pane_output(pane_id={}, max_lines={:?})",
            params.pane_id, params.max_lines
        ));
        let result = match self
            .shell_mgr
            .wt_read_pane_output(&params.pane_id, params.max_lines)
            .await
        {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!("<<< wt_read_pane_output: {} bytes", result.len()));
        result
    }

    /// Get the process status of a pane (running, exit code, etc.).
    #[tool(
        description = "Get the process status of a pane — whether it's running and its exit code"
    )]
    async fn wt_get_process_status(&self, Parameters(params): Parameters<PaneIdParams>) -> String {
        mcp_log(&format!(
            ">>> wt_get_process_status(pane_id={})",
            params.pane_id
        ));
        let result = match self.shell_mgr.wt_get_process_status(&params.pane_id).await {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_get_process_status: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    // ── Windows Terminal control tools ──────────────────────────────

    /// Create a new tab in Windows Terminal.
    #[tool(description = "Create a new tab in Windows Terminal. Returns the new pane's ID.")]
    async fn wt_create_tab(&self, Parameters(params): Parameters<CreateTabParams>) -> String {
        mcp_log(&format!(
            ">>> wt_create_tab(cmd={:?}, cwd={:?}, title={:?})",
            params.commandline, params.cwd, params.title
        ));
        let result = match self
            .shell_mgr
            .wt_create_tab(
                params.commandline.as_deref(),
                params.cwd.as_deref(),
                params.title.as_deref(),
            )
            .await
        {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_create_tab: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    /// Split an existing pane in Windows Terminal.
    #[tool(description = "Split a pane in Windows Terminal. Returns the new pane's ID.")]
    async fn wt_split_pane(&self, Parameters(params): Parameters<SplitPaneParams>) -> String {
        mcp_log(&format!(
            ">>> wt_split_pane(pane_id={}, dir={:?}, size={:?})",
            params.pane_id, params.direction, params.size
        ));
        let result = match self
            .shell_mgr
            .wt_split_pane(
                &params.pane_id,
                params.commandline.as_deref(),
                params.direction.as_deref(),
                params.size,
            )
            .await
        {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_split_pane: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    /// Send text input to a terminal pane (keystrokes).
    #[tool(
        description = "Send text/keystrokes to a terminal pane. Use this to type commands into a pane."
    )]
    async fn wt_send_input(&self, Parameters(params): Parameters<SendInputParams>) -> String {
        mcp_log(&format!(
            ">>> wt_send_input(pane_id={}, input={:?})",
            params.pane_id,
            Self::truncate(&params.input, 100)
        ));
        let result = match self
            .shell_mgr
            .wt_send_input(&params.pane_id, &params.input)
            .await
        {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_send_input: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    /// Close a terminal pane.
    #[tool(description = "Close a terminal pane")]
    async fn wt_close_pane(&self, Parameters(params): Parameters<PaneIdParams>) -> String {
        mcp_log(&format!(">>> wt_close_pane(pane_id={})", params.pane_id));
        let result = match self.shell_mgr.wt_close_pane(&params.pane_id).await {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_close_pane: {}",
            Self::truncate(&result, 200)
        ));
        result
    }

    /// Show a quick-pick dialog in Windows Terminal and return the user's selection.
    /// Use this to ask the user questions with predefined choices (and optional freeform input).
    #[tool(
        description = "Show a quick-pick dialog to the user with choices. Returns the selected text. Use for confirmations, next-step suggestions, or any user decision."
    )]
    async fn wt_quick_pick(&self, Parameters(params): Parameters<QuickPickParams>) -> String {
        mcp_log(&format!(
            ">>> wt_quick_pick(title={:?}, choices={:?}, free_input={})",
            params.title, params.choices, params.allow_free_input
        ));
        let result = match self
            .shell_mgr
            .wt_quick_pick(&params.title, &params.choices, params.allow_free_input)
            .await
        {
            Ok(val) => Self::json_pretty(&val),
            Err(e) => format!("Error: {}", e),
        };
        mcp_log(&format!(
            "<<< wt_quick_pick: {}",
            Self::truncate(&result, 200)
        ));
        result
    }
}

#[tool_handler]
impl ServerHandler for WtaMcpServer {
    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = Result<InitializeResult, rmcp::ErrorData>> + Send + '_ {
        let client_name = request.client_info.name.clone();
        let client_version = request.client_info.version.clone();
        let protocol_version = request.protocol_version.clone();

        async move {
            mcp_log(&format!(
                "<<< client initialize received: client={} version={} protocol={:?}",
                client_name, client_version, protocol_version
            ));
            if context.peer.peer_info().is_none() {
                context.peer.set_peer_info(request);
            }
            let mut info = self.get_info();
            info.instructions = Some(self.build_initialize_instructions().await.into());
            mcp_log(">>> initialize response ready");
            Ok(info)
        }
    }

    fn on_initialized(
        &self,
        _context: rmcp::service::NotificationContext<rmcp::service::RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        async move {
            mcp_log("<<< client initialized notification received");
        }
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = Implementation::from_build_env();
        info
    }
}

/// Run WTA as a headless MCP server over stdio.
pub async fn run_mcp_server(shell_mgr: Arc<ShellManager>) -> anyhow::Result<()> {
    let startup = Instant::now();
    mcp_log("=== MCP server starting ===");
    mcp_log(&format!("pid={}", std::process::id()));
    mcp_log(&format!(
        "cwd={}",
        std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "<unknown>".to_string())
    ));
    mcp_log(&format!(
        "WT_PIPE_NAME={:?}",
        std::env::var("WT_PIPE_NAME").ok()
    ));
    let server = WtaMcpServer::new(shell_mgr);
    mcp_log("Waiting for MCP client initialize handshake...");
    mcp_log("Calling server.serve(stdio())...");

    let service = match server.serve(rmcp::transport::stdio()).await {
        Ok(service) => {
            mcp_log(&format!(
                "MCP initialize handshake complete (t+{:.3}s)",
                startup.elapsed().as_secs_f64()
            ));
            service
        }
        Err(e) => {
            mcp_log(&format!(
                "MCP initialize handshake failed (t+{:.3}s): {:?}",
                startup.elapsed().as_secs_f64(),
                e
            ));
            return Err(anyhow::anyhow!("MCP server error: {:?}", e));
        }
    };

    match service.waiting().await {
        Ok(reason) => {
            mcp_log(&format!(
                "MCP server exiting normally (t+{:.3}s): {:?}",
                startup.elapsed().as_secs_f64(),
                reason
            ));
        }
        Err(e) => {
            mcp_log(&format!(
                "MCP server waiting failed (t+{:.3}s): {:?}",
                startup.elapsed().as_secs_f64(),
                e
            ));
            return Err(anyhow::anyhow!("MCP server error: {:?}", e));
        }
    }

    Ok(())
}

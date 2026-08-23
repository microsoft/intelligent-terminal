//! Per-pane helper runtime bootstrap and orchestration.

use anyhow::{Context, Result};
use crossterm::{
    cursor::{SetCursorStyle, Show},
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::prelude::*;
use std::io;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use crate::shell::wt_channel::{CliChannel, WtChannel};
use crate::shell::ShellManager;
use crate::{
    agent_hooks_installer, agent_registry, app, event, logging, protocol, shell,
};

use super::config::{HelperConfig, InitialView};

#[cfg(windows)]
fn install_descendant_job() -> Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error())
                .context("failed to create helper descendant job");
        }
        let job = OwnedHandle::from_raw_handle(job);

        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            std::mem::size_of_val(&limits) as u32,
        ) == 0
        {
            let error = io::Error::last_os_error();
            drop(job);
            return Err(error).context("failed to configure helper descendant job");
        }

        if AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) == 0 {
            let error = io::Error::last_os_error();
            drop(job);
            return Err(error).context("failed to assign helper to descendant job");
        }

        // This process must retain the only job handle for its entire lifetime.
        // Windows closes it on every exit path, including TerminateProcess, and
        // KILL_ON_JOB_CLOSE then reclaims wtcli and other helper descendants.
        std::mem::forget(job);
        tracing::info!(
            target: "helper",
            helper_pid = std::process::id(),
            "helper descendant job installed"
        );
        Ok(())
    }
}

#[cfg(not(windows))]
fn install_descendant_job() -> Result<()> {
    Ok(())
}

/// Drive the standard ACP TUI but use `pipe_name` as the ACP transport
/// (helper mode). The helper attaches to wta-master over the supplied
/// named pipe and forwards ACP traffic over it.
pub(super) async fn run_default_tui_over_pipe(
    mut config: HelperConfig,
    pipe_name: String,
) -> Result<()> {
    install_descendant_job()?;
    tracing::info!(target: "helper", pipe = %pipe_name, "=== wta-helper starting (TUI) ===");
    let agent_source = crate::agent_source::AgentSource::from_wire(
        config.agent_source.as_deref(),
        config.agent_wsl_distro.as_deref(),
    );
    config.agent_source_cwd =
        crate::agent_source::resolve_source_cwd(&agent_source, config.agent_source_cwd.as_deref())
            .await;

    // Debug channel for the helper TUI.
    let (debug_tx, debug_rx) = tokio::sync::mpsc::unbounded_channel::<app::DebugMessage>();

    let mut shell_mgr = ShellManager::new().with_agent_source(agent_source);
    let mut wt_event_rx = None;
    let mut wt_protocol_channel: Option<Arc<CliChannel>> = None;
    let wt_connected = match connect_to_wt_protocol(debug_tx.clone()).await {
        Ok(channel) => {
            tracing::info!(target: "helper", "Connected to WT COM protocol — subscribing to events");
            wt_event_rx = Some(channel.subscribe_events());
            let cli_arc = Arc::new(channel);
            wt_protocol_channel = Some(Arc::clone(&cli_arc));
            shell_mgr =
                shell_mgr.with_wt_channel(cli_arc.clone() as Arc<dyn shell::wt_channel::WtChannel>);
            true
        }
        Err(e) => {
            tracing::warn!(target: "helper", error = %e, "NO WT protocol connection");
            false
        }
    };
    let shell_mgr = Arc::new(shell_mgr);

    let pane_identity = if wt_connected {
        discover_pane_identity(&shell_mgr).await
    } else {
        None
    };

    // Connection failures to wta-master (pipe connect give-up, ACP initialize
    // timeout/failure) are logged at their source (target=helper) and again in
    // `run_acp_tui_mode`'s exit branch, which `process::exit`s rather than
    // returning Err — so there's no point wrapping the result here.
    run_acp_tui_mode(
        config,
        shell_mgr,
        wt_connected,
        debug_rx,
        pane_identity,
        wt_event_rx,
        wt_protocol_channel,
        pipe_name,
    )
    .await
}

/// Discover our own pane identity by matching our PID against WT's pane list.
async fn discover_pane_identity(shell_mgr: &ShellManager) -> Option<(String, String, String)> {
    let our_pid = std::process::id();

    // WT IDs may arrive as JSON strings or numbers (COM returns numeric) — accept both.
    fn id_str(v: Option<&serde_json::Value>) -> Option<String> {
        match v {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Number(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    let windows = shell_mgr.wt_list_windows().await.ok()?;
    let windows_arr = windows.get("windows")?.as_array()?;

    for win in windows_arr {
        let window_id = match id_str(win.get("window_id")) {
            Some(w) => w,
            None => continue,
        };
        let tabs = shell_mgr.wt_list_tabs(&window_id).await.ok()?;
        let tabs_arr = tabs.get("tabs")?.as_array()?;

        for tab in tabs_arr {
            let tab_id_str = match id_str(tab.get("tab_id")) {
                Some(t) => t,
                None => continue,
            };
            let panes = shell_mgr
                .wt_list_panes(&tab_id_str, Some(&window_id))
                .await
                .ok()?;
            let panes_arr = panes.get("panes")?.as_array()?;

            for pane in panes_arr {
                if let Some(pid) = pane.get("pid").and_then(|v| v.as_u64()) {
                    if pid == our_pid as u64 {
                        let pane_id = match id_str(pane.get("session_id")) {
                            Some(p) => p,
                            None => continue,
                        };
                        return Some((pane_id, tab_id_str.clone(), window_id.to_string()));
                    }
                }
            }
        }
    }
    None
}

struct TuiRestoreGuard {
    armed: bool,
}

impl TuiRestoreGuard {
    fn new() -> Self {
        Self { armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TuiRestoreGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(
            stdout,
            DisableMouseCapture,
            SetCursorStyle::DefaultUserShape,
            LeaveAlternateScreen,
            Show
        );
    }
}

async fn run_acp_tui_mode(
    config: HelperConfig,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    debug_rx: tokio::sync::mpsc::UnboundedReceiver<app::DebugMessage>,
    pane_identity: Option<(String, String, String)>,
    wt_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>,
    wt_protocol_channel: Option<Arc<CliChannel>>,
    connect_master_pipe: String,
) -> Result<()> {
    enable_raw_mode()?;
    let mut restore_guard = TuiRestoreGuard::new();
    let mut stdout = io::stdout();
    // Mouse tracking gives the app real wheel events instead of overloading
    // Up/Down, which remain dedicated to prompt-history navigation.
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Deliberately do NOT emit `OSC 11` to force a background color: the pane
    // must inherit the profile's color scheme background so it tracks the
    // user's theme like any other pane (#234). Cells render on the terminal's
    // default (scheme) background; `Color::Reset` resolves to it.
    // Steady block (DECSCUSR Ps=2): solid filled rectangle, no blink.
    // Survives the alt-screen swap; restored on exit below.
    execute!(stdout, SetCursorStyle::SteadyBlock)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_acp_app(
        &mut terminal,
        config,
        shell_mgr,
        wt_connected,
        debug_rx,
        pane_identity,
        wt_event_rx,
        wt_protocol_channel,
        connect_master_pipe,
    )
    .await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        SetCursorStyle::DefaultUserShape,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    restore_guard.disarm();

    if let Err(e) = result {
        // This is the real exit point for a TUI/helper failure (connection
        // failures to wta-master propagate up to here). `process::exit` below
        // bypasses both `main()`'s catch-all and any caller's wrapper, so log
        // it here before exiting — it lands in this process's log file
        // (wta-main_helper-{pid}.log in helper mode).
        tracing::error!(error = ?e, "wta TUI exiting with error");
        eprintln!("Error: {e:?}");
        // Flush the file appender — process::exit skips the guard drop.
        logging::shutdown_flush();
        std::process::exit(1);
    }
    Ok(())
}

/// Try to connect to the WT protocol via the inherited WT_COM_CLSID env var.
async fn connect_to_wt_protocol(
    debug_tx: tokio::sync::mpsc::UnboundedSender<app::DebugMessage>,
) -> Result<shell::wt_channel::CliChannel> {
    let channel = CliChannel::connect().await?;
    Ok(channel.with_debug_sender(debug_tx))
}

fn spawn_agent_lifecycle_forwarder(
    mut restart_rx: tokio::sync::mpsc::UnboundedReceiver<
        protocol::acp::client::AgentLifecycleRequest,
    >,
    event_tx: tokio::sync::mpsc::UnboundedSender<app::AppEvent>,
) {
    tokio::task::spawn_local(async move {
        while let Some(req) = restart_rx.recv().await {
            match req {
                protocol::acp::client::AgentLifecycleRequest::RestartMaster => {
                    tracing::info!(
                        target: "helper",
                        "restart requested before ACP task is running; asking WT to replace the shared master"
                    );
                    crate::wt_protocol_events::send(
                        crate::wt_protocol_events::restart_agent_stack_event(),
                    );
                }
                protocol::acp::client::AgentLifecycleRequest::RebindAgent(request) => {
                    let _ = event_tx.send(app::AppEvent::AgentReconnectReady(request));
                }
            }
        }
    });
}

async fn run_acp_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: HelperConfig,
    shell_mgr: Arc<ShellManager>,
    wt_connected: bool,
    mut debug_rx: tokio::sync::mpsc::UnboundedReceiver<app::DebugMessage>,
    pane_identity: Option<(String, String, String)>,
    wt_event_rx: Option<tokio::sync::mpsc::UnboundedReceiver<serde_json::Value>>,
    wt_protocol_channel: Option<Arc<CliChannel>>,
    connect_master_pipe: String,
) -> Result<()> {
    let agent_cmd = config.agent.clone();
    let agent_source = crate::agent_source::AgentSource::from_wire(
        config.agent_source.as_deref(),
        config.agent_wsl_distro.as_deref(),
    );
    let agent_source_cwd = config.agent_source_cwd.clone();

    let local_set = tokio::task::LocalSet::new();
    local_set
        .run_until(async move {
            let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
            let (prompt_tx, prompt_rx) = tokio::sync::mpsc::unbounded_channel();
            let proposal_channels =
                Arc::new(
                    crate::agent_tools::action_proposal::channel::ProposalChannelManager::new(),
                );
            let (proposal_pipe_tx, mut proposal_pipe_rx) =
                tokio::sync::mpsc::unbounded_channel();
            let proposal_server_manager = Arc::clone(&proposal_channels);
            let proposal_server_lifecycle = Arc::clone(&proposal_channels);
            tokio::task::spawn_local(async move {
                if let Err(error) =
                    crate::agent_tools::action_proposal::pipe::run_server(
                        proposal_server_manager,
                        proposal_pipe_tx,
                    )
                    .await
                {
                    proposal_server_lifecycle.set_pipe_available(false);
                    tracing::error!(
                        target: "proposal_pipe",
                        error = %format!("{error:#}"),
                        "proposal pipe server stopped"
                    );
                }
            });
            let proposal_event_tx = event_tx.clone();
            tokio::task::spawn_local(async move {
                while let Some(event) = proposal_pipe_rx.recv().await {
                    let app_event = match event {
                        crate::agent_tools::action_proposal::pipe::ProposalPipeEvent::Validate {
                            context,
                            payload,
                            source,
                            responder,
                        } => app::AppEvent::DirectTerminalActionProposal {
                            context,
                            payload,
                            source,
                            responder,
                        },
                        crate::agent_tools::action_proposal::pipe::ProposalPipeEvent::Commit {
                            proposal_id,
                            responder,
                        } => app::AppEvent::DirectTerminalActionProposalCommit {
                            proposal_id,
                            responder,
                        },
                        crate::agent_tools::action_proposal::pipe::ProposalPipeEvent::Invalidate {
                            proposal_id,
                            session_id,
                        } => app::AppEvent::DirectTerminalActionProposalInvalidate {
                            proposal_id,
                            session_id,
                        },
                    };
                    if proposal_event_tx.send(app_event).is_err() {
                        break;
                    }
                }
            });

            let evt_tx = event_tx.clone();
            tokio::task::spawn_local(event::read_crossterm_events(evt_tx));

            let dbg_event_tx = event_tx.clone();
            tokio::task::spawn_local(async move {
                while let Some(msg) = debug_rx.recv().await {
                    let _ = dbg_event_tx.send(app::AppEvent::DebugPipeMessage(msg));
                }
            });

            // Start the background protocol reader and trigger lazy event registration.
            // start_reader() claims stdout/stderr streams and must complete before any requests.
            // get_capabilities triggers _ensurePageEventsRegistered() on the WT server.
            if let Some(ref protocol_ch) = wt_protocol_channel {
                tracing::info!("start_reader: starting...");
                protocol_ch.start_reader().await;
                tracing::info!("start_reader: done, sending get_capabilities...");
                match protocol_ch
                    .request("get_capabilities", serde_json::json!({}))
                    .await
                {
                    Ok(v) => tracing::info!(result = %v, "get_capabilities OK"),
                    Err(e) => tracing::warn!(error = %e, "get_capabilities FAILED"),
                }
            } else {
                tracing::warn!("no wt_pipe_channel — events won't work");
            }

            // Background WT event reader: forwards push events from the protocol channel to the TUI.
            if let Some(mut wt_rx) = wt_event_rx {
                tracing::info!("wt_event_rx: starting background reader task");
                let wt_event_tx = event_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Some(event_json) = wt_rx.recv().await {
                        let method = event_json
                            .get("method")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        // The full event envelope carries `vt_sequence` (raw
                        // terminal output/scrollback) — keep it out of debug;
                        // log only the method there, full JSON at trace.
                        tracing::debug!(method = %method, "wt_event_rx: received event");
                        if method == "agent_paste_text" {
                            let mut redacted = event_json.clone();
                            let paste_len = redacted
                                .get("params")
                                .and_then(|p| p.get("text"))
                                .and_then(|v| v.as_str())
                                .map(str::len);
                            if let Some(paste_len) = paste_len {
                                if let Some(params) =
                                    redacted.get_mut("params").and_then(|v| v.as_object_mut())
                                {
                                    params.insert(
                                        "text".to_string(),
                                        serde_json::json!(format!(
                                            "<redacted {} bytes>",
                                            paste_len
                                        )),
                                    );
                                }
                            }
                            tracing::trace!(target: "wt_event.content", event = %redacted, "wt_event_rx: full event");
                        } else {
                            tracing::trace!(target: "wt_event.content", event = %event_json, "wt_event_rx: full event");
                        }

                        let params = event_json
                            .get("params")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        // Read `pane_id` (current name) with a fallback
                        // to `session_id` (the old name before the
                        // per-tab autofix routing PR renamed it). The
                        // C++ TerminalPage side now emits `pane_id` for
                        // `connection_state` / `vt_sequence`, but the
                        // wtcli `send-event` builder
                        // (`BuildSendEventJson`) was missed in that
                        // rename pass — `agent_event` envelopes from
                        // hook bridge still carried `session_id`.
                        // Without this fallback every hook event
                        // arrived with `pane_id = ""`, and downstream
                        // `route_agent_event_to_registry` collided all
                        // sessions on the empty-string key in
                        // `active_by_pane`, triggering spurious
                        // orphan-handover demotions whenever a second
                        // session started in the same window (e.g.
                        // session A → Ended the moment session B's
                        // first hook fires). Keep the fallback even
                        // after wtcli is fixed so an old wtcli build
                        // can talk to a new wta without surprises.
                        let pane_id = params
                            .get("pane_id")
                            .and_then(|v| v.as_str())
                            .filter(|s| !s.is_empty())
                            .or_else(|| params.get("session_id").and_then(|v| v.as_str()))
                            .unwrap_or("")
                            .to_string();
                        let tab_id = params
                            .get("tab_id")
                            .and_then(|v| v.as_str())
                            .map(str::to_string);
                        let _ = wt_event_tx.send(app::AppEvent::WtEvent {
                            method,
                            pane_id,
                            tab_id,
                            params,
                        });
                    }
                });
            }

            let shell_mgr_for_recs = Arc::clone(&shell_mgr);

            // Cancel channel for Ctrl+C handling: App produces, ACP client
            // task consumes (one listener task inside the ACP client loop).
            let (cancel_tx, cancel_rx) = tokio::sync::mpsc::unbounded_channel();
            // /new channel: App emits a NewSessionForTab, the ACP client
            // drops the cached SessionId for that tab and re-issues
            // new_session(). The resulting SessionAttached event flows
            // back through event_tx like the lazy-create path.
            let (new_session_tx, new_session_rx) = tokio::sync::mpsc::unbounded_channel();
            // load_session channel: App emits a LoadSessionForTab in
            // response to WT's `load_session` event (the back-half of
            // the session management view's Shift+Enter -> "resume in
            // new tab's agent pane" flow). The ACP client calls
            // `conn.load_session` and binds the rehydrated session to
            // the tab via SessionAttached.
            let (load_session_tx, load_session_rx) = tokio::sync::mpsc::unbounded_channel();
            // Clone for the boot-time initial-load injection below. The
            // primary `load_session_tx` is moved into `App::new` further
            // down; this clone is used once (if `--initial-load-session-id`
            // was passed) to synthesize a LoadSessionForTab as soon as the
            // helper has finished its owner_tab_id seed. The receiver in
            // `run_acp_client_over_pipe` then drives `session/load` through
            // its standard runtime arm — no race vs. a separate VT
            // `load_session` broadcast.
            let initial_load_tx = load_session_tx.clone();
            // Lifecycle channel: App either asks WT to replace the shared
            // master or retires this helper's ACP transport for an Agent
            // rebind. The helper process itself stays alive.
            let (restart_tx, restart_rx) = tokio::sync::mpsc::unbounded_channel();
            // reset_tab_session channel: App emits a DropSessionRequest when
            // WT tells us to release a tab's binding (Ctrl+C×2 hide path).
            // ACP client removes the SessionId from tab_to_session and
            // closes the session after cancelling any in-flight prompt; the
            // next prompt on that tab lazily creates a fresh session.
            let (drop_session_tx, drop_session_rx) = tokio::sync::mpsc::unbounded_channel();
            // tab-drag rename channel: App emits a RenameSessionRequest when
            // WT mints a new stable tab id for an existing tab (cross-window
            // tab drag). ACP client rekeys tab_to_session so the next prompt
            // on the dragged tab finds the existing ACP SessionId — without
            // this the agent loses turn context after a drag.
            let (rename_session_tx, rename_session_rx) =
                tokio::sync::mpsc::unbounded_channel();
            // Helper mode always speaks to wta-master, so the session-hook
            // channel is always live.
            let (session_hook_tx, session_hook_rx) = tokio::sync::mpsc::unbounded_channel();
            let (master_ext_tx, master_ext_rx) = tokio::sync::mpsc::unbounded_channel();

            // Seed the process-wide owner tab StableId so `inject_wta_pane_meta`
            // stamps `_meta.wta.owner_tab_id` on every session/new + session/load.
            // Master uses it to resolve close-by-tab ownership.
            protocol::acp::client::set_helper_owner_tab_id(config.owner_tab_id.as_deref());

            let explicit_agent_id = config
                .agent_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let canonical_agent_id: String = explicit_agent_id
                .map(str::to_ascii_lowercase)
                .unwrap_or_else(|| {
                    agent_registry::resolve_agent_id_from_cmd(&agent_cmd).to_string()
                });
            let canonical_agent_source = if explicit_agent_id.is_some() {
                "--agent-id"
            } else {
                "resolved-from-cmd"
            };
            let initial_load_requested = config
                .initial_load_session_id
                .as_deref()
                .map(str::trim)
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            let initial_auth_agent = match config
                .initial_auth_agent
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                Some(requested) if config.start_stashed => {
                    tracing::warn!(
                        target: "initial_auth",
                        requested_agent = %requested,
                        "--initial-auth-agent ignored because --start-stashed is active"
                    );
                    None
                }
                Some(requested) if config.setup.is_some() => {
                    tracing::warn!(
                        target: "initial_auth",
                        requested_agent = %requested,
                        "--initial-auth-agent ignored because --setup is active"
                    );
                    None
                }
                Some(requested) if initial_load_requested => {
                    tracing::warn!(
                        target: "initial_auth",
                        requested_agent = %requested,
                        "--initial-auth-agent ignored because --initial-load-session-id is active"
                    );
                    None
                }
                Some(requested) => {
                    let requested_agent = requested.to_ascii_lowercase();
                    if requested_agent != canonical_agent_id {
                        tracing::warn!(
                            target: "initial_auth",
                            requested_agent = %requested_agent,
                            current_agent = %canonical_agent_id,
                            "--initial-auth-agent ignored because it does not match the effective agent"
                        );
                        None
                    } else if requested_agent != "copilot" {
                        tracing::warn!(
                            target: "initial_auth",
                            requested_agent = %requested_agent,
                            "--initial-auth-agent ignored for unsupported agent"
                        );
                        None
                    } else {
                        Some(requested_agent)
                    }
                }
                None => None,
            };
            let start_in_initial_auth = initial_auth_agent.as_deref() == Some("copilot");
            let is_host_agent_source =
                matches!(&agent_source, crate::agent_source::AgentSource::Host);
            // This snapshot was probed by the Windows host. A WSL agent must
            // advertise/probe its own catalog rather than inheriting Host models.
            let cloud_models = if is_host_agent_source {
                config
                    .cloud_models
                    .as_deref()
                    .and_then(|models| match serde_json::from_str(models) {
                        Ok(models) => Some(models),
                        Err(error) => {
                            tracing::error!(
                                target: "cloud_models",
                                %error,
                                "invalid --cloud-models metadata"
                            );
                            None
                        }
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };

            // Spawn the ACP client. In helper mode (`--connect-master <pipe>`)
            // master owns the agent lifecycle, so normal panes spawn the
            // pipe-attached variant immediately. FRE-installed Copilot is the
            // exception: `--initial-auth-agent copilot` starts on Auth and lets
            // `LoginComplete` spawn the first pipe client after sign-in.
            if start_in_initial_auth {
                tracing::info!(
                    target: "initial_auth",
                    agent_id = %canonical_agent_id,
                    "starting helper on auth screen; initial ACP task skipped"
                );
                // The Auth screen's LoginComplete path uses
                // `set_master_pipe_acp_params` below and `try_start_acp` to
                // create fresh channels and reconnect through the master pipe
                // after login. Dropping the boot channels here avoids an
                // explicit initial ACP race and makes the startup ordering
                // independent from tokio task polling.
                //
                // Keep the /restart path alive even though no ACP task is
                // running yet. The boot App holds the sole restart sender; when
                // LoginComplete calls `try_start_acp`, it replaces that sender
                // with a fresh channel and this forwarder exits.
                spawn_agent_lifecycle_forwarder(restart_rx, event_tx.clone());
                drop((
                    prompt_rx,
                    cancel_rx,
                    new_session_rx,
                    load_session_rx,
                    drop_session_rx,
                    rename_session_rx,
                    session_hook_rx,
                    master_ext_rx,
                ));
            } else {
                let pipe_name = connect_master_pipe.clone();
                let event_tx_for_pipe = event_tx.clone();
                let shell_mgr_for_pipe = Arc::clone(&shell_mgr);
                let acp_model = config.acp_model.clone();
                let custom_model_selection = config.custom_model_selection.clone();
                let cloud_models_for_client = cloud_models.clone();
                // Pass per-tab agent identity through the initialize handshake.
                let agent_id = config.agent_id.clone();
                let agent_source_for_client = agent_source.clone();
                let source_cwd = agent_source_cwd.clone();
                let owner_tab = config.owner_tab_id.clone();
                let initial_load_sid = config.initial_load_session_id.clone();
                let proposal_channels_for_pipe = Arc::clone(&proposal_channels);
                tokio::task::spawn_local(async move {
                    match protocol::acp::client::run_acp_client_over_pipe(
                        pipe_name,
                        acp_model,
                        custom_model_selection,
                        cloud_models_for_client,
                        agent_id,
                        agent_source_for_client,
                        source_cwd,
                        owner_tab,
                        initial_load_sid,
                        event_tx_for_pipe.clone(),
                        prompt_rx,
                        cancel_rx,
                        new_session_rx,
                        load_session_rx,
                        drop_session_rx,
                        rename_session_rx,
                        restart_rx,
                        session_hook_rx,
                        master_ext_rx,
                        shell_mgr_for_pipe,
                        wt_connected,
                        false, // post_login_reconnect: first connection, no authenticate needed
                        proposal_channels_for_pipe,
                    )
                    .await
                    {
                        Ok(protocol::acp::client::AcpClientExit::RebindAgent(request)) => {
                            let _ = event_tx_for_pipe
                                .send(app::AppEvent::AgentReconnectReady(request));
                        }
                        Ok(protocol::acp::client::AcpClientExit::ChannelsClosed) => {}
                        Err(e) => {
                            tracing::error!(
                                target: "helper",
                                error = %e,
                                "run_acp_client_over_pipe failed"
                            );
                            // Notify the App before the visible failure so a
                            // queued Agent rebind can continue with its
                            // latest target instead of becoming stranded.
                            let _ = event_tx_for_pipe.send(app::AppEvent::AgentClientFailed);
                            // Recover the typed classification: an auth error
                            // attached at the handshake `new_session` site survives
                            // the `?`-collapse into `anyhow` via downcast, so it
                            // still routes to the sign-in screen; other handshake
                            // failures fall back to `HandshakeFailed`. The raw
                            // `{e:#}` is also in the log above for diagnosis.
                            let failure = protocol::acp::failure::classify_anyhow(
                                &e,
                                protocol::acp::failure::HandshakeStage::Initialize,
                            );
                            let message = match &failure {
                                protocol::acp::failure::AgentFailure::HandshakeFailed {
                                    detail,
                                    ..
                                } => detail.clone(),
                                _ => format!("helper ACP transport failed: {e:#}"),
                            };
                            let _ = event_tx_for_pipe.send(app::AppEvent::AgentError {
                                session_id: None,
                                failure,
                                message,
                            });
                        }
                    }
                });
            }

            let (recommendation_tx, recommendation_rx) =
                tokio::sync::mpsc::unbounded_channel();
            let (permission_tx, _permission_rx) = tokio::sync::mpsc::unbounded_channel();
            let debug_capture_enabled = Arc::new(AtomicBool::new(false));
            let (_ui_event_tx, ui_event_rx) = tokio::sync::mpsc::unbounded_channel();

            // Spawn the recommendation executor so selected choices actually run.
            let rec_event_tx = event_tx.clone();
            // Shared so a runtime `agent_config_changed` settings update can
            // hot-swap the configured delegate agent/model in place (handled
            // in App::handle_event) without restarting the agent pane. The
            // executor snapshots it per choice; the App rebuilds it on change.
            let delegate_agents = Arc::new(std::sync::Mutex::new(
                crate::coordinator::default_delegate_agent_runtimes(
                    config.delegate_agent.as_deref(),
                    Some(config.agent.as_str()),
                    config.delegate_model.as_deref(),
                ),
            ));
            tokio::spawn(crate::coordinator::run_recommendation_executor(
                recommendation_rx,
                rec_event_tx,
                shell_mgr_for_recs,
                Arc::clone(&delegate_agents),
            ));

            let autofix_enabled = !config.no_autofix;
            let mut app_state = app::App::new(prompt_tx, recommendation_tx, permission_tx, cancel_tx, new_session_tx, load_session_tx, drop_session_tx, rename_session_tx, restart_tx, master_ext_tx, debug_capture_enabled, wt_connected, autofix_enabled, Arc::clone(&shell_mgr));
            app_state.set_render_agent_markdown(config.render_agent_markdown);
            app_state.set_proposal_channels(Arc::clone(&proposal_channels));
            app_state.set_allowed_agent_ids(config.allowed_agent_ids.clone());
            // Seed the hot-updatable runtime agent config: the shared
            // delegate runtime table, the helper's own agent_cmd (needed to
            // re-derive the delegate commandline when only the delegate
            // agent/model change), and the configured acp-model override
            // (re-applied to future sessions so /new stays on the model).
            app_state.set_runtime_agent_config(
                Arc::clone(&delegate_agents),
                config.agent.clone(),
                config.acp_model.clone(),
                config.follows_global_acp_model,
            );
            app_state.set_cloud_models(cloud_models);
            if is_host_agent_source {
                match config.custom_models.as_deref() {
                    Some(custom_models) => match serde_json::from_str(custom_models) {
                        Ok(models) => app_state.set_custom_model_config(
                            models,
                            config.custom_model_selection.clone(),
                        ),
                        Err(error) => tracing::error!(
                            target: "custom_models",
                            %error,
                            "invalid --custom-models metadata"
                        ),
                    },
                    None => app_state.set_custom_model_config(
                        Vec::new(),
                        config.custom_model_selection.clone(),
                    ),
                }
            } else {
                if config.custom_models.is_some() || config.custom_model_selection.is_some() {
                    tracing::warn!(
                        target: "custom_models",
                        agent_source = %agent_source,
                        "ignoring Host custom-provider startup metadata for WSL helper"
            );
                }
                app_state.set_custom_model_config(Vec::new(), None);
            }
            // Backward compatibility: older Terminal builds supplied the full
            // custom catalog on argv. New builds deliver it after Connected
            // over agent_config_changed, so the initial status requests it.
            app_state
                .set_host_catalog_ready(!is_host_agent_source || config.custom_models.is_some());
            app_state.set_session_hook_tx(session_hook_tx);

            // Pipe-mode reconnect pre-stash. In helper mode the initial
            // `run_acp_client_over_pipe` task fails immediately with
            // `Authentication required` if the user is in FRE (not yet
            // logged in). The post-login `LoginComplete` handler fires
            // `try_start_acp`; without this stash it would have no master
            // pipe to reconnect with and could not resume the agent pane
            // — breaking every `intellterm.wta/...`
            // ext-method (e.g. `sessions/list` — session view would stay
            // empty on the first tab forever). With the stash in place,
            // `try_start_acp` sees `master_pipe_name = Some(...)` and
            // routes the reconnect back through master.
            //
            // No effect when the initial connection succeeds: the
            // stashed params just sit unused for the helper's lifetime.
            app_state.set_master_pipe_acp_params(
                connect_master_pipe.clone(),
                agent_cmd.clone(),
                config.agent_id.clone(),
                config.acp_model.clone(),
                config.custom_model_selection.clone(),
                agent_source.clone(),
                agent_source_cwd.clone(),
                config.owner_tab_id.clone(),
                Arc::clone(&shell_mgr),
                wt_connected,
            );

            if config.setup.is_none() {
                app_state.current_agent_id = canonical_agent_id.clone();
                app_state.current_agent_source = agent_source.clone();
                tracing::info!(
                    target: "agents_view_filter",
                    agent_id = %canonical_agent_id,
                    agent_cmd = %agent_cmd,
                    source = canonical_agent_source,
                    "current_agent_id assigned",
                );
            }
            if start_in_initial_auth {
                app_state.show_copilot_auth_screen();
            }

            // ── Preflight: check the agent CLI before connecting ──────────
            // Skip preflight when FRE is active — FRE has its own agent
            // selection + auth flow and doesn't need the preflight wizard.
            if config.setup.is_none() && !start_in_initial_auth {
                let agent_id = canonical_agent_id.as_str();
                let preflight_result =
                    app::preflight_agent_in_source(agent_id, &agent_source).await;
                tracing::info!(
                    target: "preflight",
                    agent_id = %preflight_result.agent_id,
                    cli = ?preflight_result.cli_status,
                    auth = ?preflight_result.auth_status,
                    "preflight done (via agent_check)"
                );
                let _ = event_tx.send(app::AppEvent::PreflightComplete(preflight_result));
            }

            // ── install-hooks request channel ─────────────────────────────
            // The Settings UI / in-TUI install button signals via this
            // channel; main.rs runs `agent_hooks_installer::ensure_installed`
            // off the UI thread so the TUI stays responsive.
            let (install_req_tx, mut install_req_rx) =
                tokio::sync::mpsc::unbounded_channel::<()>();
            tokio::task::spawn_local(async move {
                while let Some(()) = install_req_rx.recv().await {
                    tracing::info!(target: "install_hooks", "received install request");
                    // Run the (potentially slow, IO-bound) installer on the
                    // blocking pool so we don't park the LocalSet.
                    let _ = tokio::task::spawn_blocking(|| {
                        agent_hooks_installer::ensure_installed();
                    })
                    .await;
                }
            });
            app_state.set_install_request_tx(install_req_tx);

            // Wire the agent_event channel so dispatch_resume's split-pane
            // background callback can post AgentSessionEvent (specifically
            // ResumePaneAssigned) back into the event loop.
            app_state.set_agent_event_tx(event_tx.clone());

            // Seed `app_state.tab_id` + `pane_open` from `--owner-tab-id`
            // BEFORE the `--initial-view` block + the `project_active_tab_state`
            // emit below. Two failure modes if we don't:
            //   1. `current_tab_mut` in the --initial-view block falls back
            //      to DEFAULT_TAB_ID — the view setting lands on the wrong
            //      tab, the echo C++ receives doesn't match any real tab
            //      and is dropped.
            //   2. The initial echo has `pane_open=false` (default), which
            //      C++'s `OnAgentStateChanged` interprets as "hide" and
            //      stashes the just-spawned agent pane.
            // The full seed block further down (which logs + redundantly
            // sets the same fields) becomes idempotent now.
            //
            // `--start-stashed` inverts (2): in the pre-warm path the
            // C++ side has *already stashed* the pane after spawning the
            // helper, so the helper must seed `pane_open = false` to
            // match. Without this, helper echoes `pane_open=true`, C++
            // sees a stashed pane and a `pane_open=true` echo, and
            // restores the pane — defeating pre-warm.
            if let Some(ref owner_tab_id) = config.owner_tab_id {
                if !owner_tab_id.is_empty() && app_state.tab_id.is_none() {
                    let tab = app_state
                        .tab_sessions
                        .entry(owner_tab_id.clone())
                        .or_default();
                    tab.pane_open = !config.start_stashed;
                    app_state.tab_id = Some(owner_tab_id.clone());
                    app_state.owner_tab_id = Some(owner_tab_id.clone());
                }
            }

            // Plan-C boot-time initial-load: if WT spawned us with
            // `--initial-load-session-id` (+ optional `--initial-load-cwd`)
            // synthesize an `AppEvent::WtEvent { method:"load_session" }`
            // and queue it on `event_tx`. The App's event loop will pick
            // it up after startup and route it through the same handler
            // that the runtime `wt_event` path uses (app.rs ~4039) —
            // which:
            //   1) clears the tab's chat and sets `loading_session=true`,
            //      so the chunk handlers ACCEPT replay chunks during the
            //      ensuing `session/load`. Going through the channel
            //      directly (the old design) skipped this, and the
            //      master DID route the replay chunks back to the
            //      helper, but the App's AgentMessageChunk handler
            //      dropped them because `turn.is_in_flight() == false`
            //      and `loading_session == false` — user-visible
            //      symptom: "Session loaded." footer with no past
            //      content above.
            //   2) emits a "Resuming session …" system message so the
            //      user has a visible cue while the load is in flight,
            //   3) forwards into the same `load_session_tx` channel the
            //      runtime arm uses, which drives `conn.load_session`
            //      on the ACP client side — atomically replacing the
            //      bootstrap session created by `session/new` moments
            //      earlier.
            //
            // This replaces the prior race-prone design where C++
            // broadcast a separate `load_session` VT event right after
            // spawning the helper — which often landed in the wrong
            // helper because the new helper's pipe attach hadn't yet
            // completed.
            //
            // Pair-only: both flags meaningless without `--owner-tab-id`
            // (the load_session handler routes by tab id), so we
            // silently skip if owner_tab_id is unset. Logged so a
            // misconfigured spawn is easy to diagnose.
            if let Some(ref sid) = config.initial_load_session_id {
                if !sid.is_empty() {
                    let tab_id_opt = app_state
                        .owner_tab_id
                        .clone()
                        .or_else(|| config.owner_tab_id.clone());
                    match tab_id_opt {
                        Some(tab_id) if !tab_id.is_empty() => {
                            let cwd = config
                                .initial_load_cwd
                                .as_deref()
                                .map(str::to_string)
                                .filter(|s| !s.is_empty())
                                .and_then(|s| {
                                    let v = crate::cwd_util::validate_starting_directory(&s);
                                    if v.is_none() {
                                        tracing::warn!(
                                            target: "acp_load_session",
                                            "--initial-load-cwd refers to a missing directory; dropping from load_session params",
                                        );
                                    }
                                    v
                                });
                            tracing::info!(
                                target: "acp_load_session",
                                session_id = sid,
                                tab_id = %tab_id,
                                "queueing boot-time initial load_session via AppEvent::WtEvent"
                            );
                            let mut params = serde_json::Map::new();
                            params.insert(
                                "tab_id".to_string(),
                                serde_json::Value::String(tab_id.clone()),
                            );
                            params.insert(
                                "session_id".to_string(),
                                serde_json::Value::String(sid.clone()),
                            );
                            if let Some(cwd_str) = cwd {
                                params.insert(
                                    "cwd".to_string(),
                                    serde_json::Value::String(cwd_str),
                                );
                            }
                            let _ = event_tx.send(app::AppEvent::WtEvent {
                                method: "load_session".to_string(),
                                pane_id: String::new(),
                                tab_id: Some(tab_id),
                                params: serde_json::Value::Object(params),
                            });
                        }
                        _ => {
                            tracing::warn!(
                                target: "acp_load_session",
                                "--initial-load-session-id given without --owner-tab-id; ignoring"
                            );
                        }
                    }
                }
            }
            // `initial_load_tx` is no longer used (the runtime
            // `load_session_tx` path is now reached via the App's
            // WtEvent handler) but we still need to drop the cloned
            // sender so the receiver future inside the ACP client loop
            // doesn't keep an extra producer alive past shutdown.
            drop(initial_load_tx);

            // Apply --initial-view: if `sessions`, jump straight into the
            // agent session view (mirrors the Chat→Agents toggle). Wired to
            // WT's Ctrl+Shift+/ binding via `--initial-view sessions` on
            // the wta cmdline. `open_agents_view_for_tab` fires the
            // `session/list` refetch to master that populates the view.
            //
            // Skip in setup mode: --setup takes the diagnostic path and the user
            // shouldn't be dropped into an empty session list.
            if config.setup.is_none()
                && !start_in_initial_auth
                && config.initial_view == InitialView::Sessions
            {
                tracing::info!(target: "initial_view", "starting in agent session view");
                let tab_id = app_state
                    .tab_id
                    .clone()
                    .unwrap_or_else(|| app::DEFAULT_TAB_ID.to_string());
                app_state.open_agents_view_for_tab(tab_id);
            }

            // Project the initial active-tab state to C++ once, after the
            // --initial-view block has had its say. Without this push,
            // C++'s `_agentSessionsViewActive` and `Tab.AgentPaneOpen`
            // mirrors (single writer lives in `OnAgentStateChanged`)
            // would stay on their defaults until the user's first
            // interaction, leaving the bar mislabelled in the
            // `--initial-view sessions` case and the pane-open flag
            // out of sync with the seeded `pane_open=true` on the
            // owner tab. Cheap and idempotent.
            //
            // Safe before the `Setup` mode block below: that block runs
            // its own UI and doesn't read the view flag; if we end up in
            // setup mode the initial "chat" emission is harmless.
            if wt_connected {
                app_state.project_active_tab_state();
            }

            // NOTE: the helper no longer scans on-disk history at all. The
            // session view renders from master's `session/list` snapshot, and
            // master performs the single CLI-filtered scan at its startup.
            // See doc/specs/per-cli-history-filtering.md.

            // Enter setup mode if --setup <reason> was passed.
            tracing::info!("cli.setup = {:?}", config.setup);
            if let Some(ref reason_str) = config.setup {
                tracing::info!("Entering diagnostic setup mode: reason={}", reason_str);
                let reason = app::SetupReason::from_str(reason_str);

                app_state.mode = app::AppMode::Setup;
                let options = app::build_setup_options(&reason, None);
                let title = reason.title().to_string();
                let subtitle = "Fix the issue to continue".to_string();
                app_state.setup = Some(app::SetupState {
                    reason,
                    selected_index: 0,
                    preflight: app::PreflightResult {
                        agent_id: String::new(),
                        display_name: String::new(),
                        cli_status: app::CheckStatus::Skipped,
                        cli_path: None,
                        auth_status: app::CheckStatus::Skipped,
                        install_hint: String::new(),
                        install_url: String::new(),
                        auth_hint: String::new(),
                    },
                    install_in_progress: false,
                    install_log: Vec::new(),
                    install_error: None,
                    options,
                    title,
                    subtitle,
                });
            }

            app_state.set_event_tx(event_tx.clone());

            // The helper does not scan on-disk history: master performs the
            // single (CLI-filtered) scan and the session view renders from
            // its `session/list` snapshot. See
            // doc/specs/per-cli-history-filtering.md.

            if let Some((pane_id, _tab_id, window_id)) = pane_identity {
                app_state.pane_id = Some(pane_id);
                // discover_pane_identity returns the legacy unstable tab
                // index, not the GUID — ignore it. The stable owner-tab GUID
                // is passed by WT via --owner-tab-id (see below) and seeded
                // directly into app_state.tab_id.
                app_state.window_id = Some(window_id);
            }
            else if let Some(pane_id) = std::env::var("WT_SESSION")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            {
                tracing::info!(
                    target: "tab_session",
                    pane_id = %pane_id,
                    "seeded app_state.pane_id from WT_SESSION fallback"
                );
                app_state.pane_id = Some(pane_id);
            }

            // WT knows the owning window authoritatively when it creates the
            // helper. Prefer that seed over best-effort PID discovery so
            // outbound per-window events work from the first render.
            if let Some(owner_window_id) = config
                .owner_window_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                tracing::info!(
                    target: "tab_session",
                    window_id = %owner_window_id,
                    "seeded app_state.window_id from --owner-window-id"
                );
                app_state.window_id = Some(owner_window_id.to_string());
            }

            // Seed tab_id from --owner-tab-id (passed by TerminalPage when
            // spawning the agent pane). With this set, AgentConnected binds
            // the initial session under the correct GUID immediately, and
            // tab_changed events later are plain switches — no implicit
            // DEFAULT_TAB_ID placeholder, no migration heuristics. Falls
            // back to None for non-pane invocations (manual `wta` runs, the
            // `wta delegate` subcommand), where the legacy DEFAULT_TAB_ID
            // path handles routing.
            //
            // Materialize the matching `tab_sessions` entry alongside the
            // tab_id assignment — `current_tab()` borrows immutably and
            // expects the active key to already be present, so without
            // pre-inserting we'd panic on the first render before any
            // event has had a chance to lazy-create it.
            if let Some(owner_tab_id) = config.owner_tab_id.clone() {
                if !owner_tab_id.is_empty() {
                    tracing::info!(
                        target: "tab_session",
                        tab_id = %owner_tab_id,
                        "seeded app_state.tab_id from --owner-tab-id"
                    );
                    let tab = app_state
                        .tab_sessions
                        .entry(owner_tab_id.clone())
                        .or_default();
                    // wta is the source of truth for "does this tab want
                    // the pane visible". The pane is being spawned right
                    // now for this owner tab; under the normal user-
                    // initiated open the user wants it visible, so default
                    // pane_open=true. The exception is `--start-stashed`
                    // (pre-warm path) where C++ has already stashed the
                    // pane — see comment on the earlier seed block.
                    tab.pane_open = !config.start_stashed;
                    app_state.tab_id = Some(owner_tab_id.clone());

                    // Publish an initial chip-target state for this tab so
                    // the C++ side can sync regardless of which transitions
                    // it has seen so far. At startup no Send card is
                    // selected, so the published target is `None` — i.e.
                    // "release any override, fall back to the source-of-
                    // agent flag". This is harmless when the C++ side is
                    // already in that state and load-bearing in the race
                    // where the agent pane was just restored from a stash
                    // and the chip-visibility hook on the C++ side hasn't
                    // run with the right `previousActive` yet.
                    app_state.recompute_chip_override_initial(&owner_tab_id);
                }
            }

            // ── source-pane context (autofix attribution) ─────────────────
            app_state.source_session_id = std::env::var("WTA_SOURCE_SESSION_ID")
                .ok()
                .filter(|s| !s.is_empty());
            app_state.source_cwd = std::env::var("WTA_SOURCE_CWD")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| agent_source_cwd.clone());

            // ── env-gated raw agent_event chat logging (diagnostics) ──────
            app_state.log_agent_events = std::env::var("WTA_LOG_AGENT_EVENT")
                .map(|v| matches!(v.as_str(), "1" | "true" | "yes"))
                .unwrap_or(false);

            // If a prompt was passed via CLI arg (e.g., from command palette creating
            // a new agent pane), delegate it to a new tab agent on startup.
            if let Some(ref initial_prompt) = config.prompt {
                if !initial_prompt.is_empty() {
                    app_state.delegate_to_tab_agent(initial_prompt);
                }
            }

            app_state.run(terminal, event_rx, ui_event_rx).await
        })
        .await
}

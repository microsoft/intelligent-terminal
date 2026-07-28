// tools/wta/src/helper/mod.rs
//
// `wta-helper` mode — the per-pane half of the helper+master
// architecture (see doc/specs/Multi-window-agent-pane.md). Spawned by
// Windows Terminal with `--connect-master <pipe-name>`. Drives the
// usual Ratatui TUI but, instead of spawning the agent CLI itself,
// connects to a wta-master singleton over the named pipe whose path
// is passed in and speaks ACP JSON-RPC over it. From the helper's
// perspective, master IS the agent.
//
// The helper implementation lives in `helper/tui.rs`: WT connection,
// terminal lifecycle, channel wiring, and the event loop.

use anyhow::Result;

use crate::Cli;

mod tui;

/// Helper-mode entry point. Routes the ACP traffic through a named pipe
/// to the wta-master singleton instead of spawning a private agent CLI
/// subprocess.
pub async fn run_helper_mode(cli: Cli, pipe_name: String) -> Result<()> {
    tui::run_default_tui_over_pipe(cli, pipe_name).await
}

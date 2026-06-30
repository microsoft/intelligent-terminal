//! WSL historical agent-session discovery via ACP `session/list`.
//!
//! For each *running* WSL distro, spawns the distro's agent CLI in ACP
//! mode through `wsl.exe` (`wsl -d <distro> -- bash -lc "<cli> --acp …"`)
//! and asks it for `session/list`. The CLI enumerates and parses its own
//! on-disk transcripts inside the distro, so we get structured rows
//! (id / title / cwd / updated_at) without reading distro files or parsing
//! per-CLI JSONL on the host. Rows are stamped
//! `SessionLocation::Wsl { distro }`.
//!
//! This replaces the earlier tar-based scan (the removed
//! `crate::wsl::scan_running_distros`); see
//! `doc/specs/session-history-via-acp.md`.
//!
//! **Async by design (plan D).** The scan runs on the master's existing
//! tokio `LocalSet` (the ACP 0.10 connection is `!Send`) — no temporary
//! runtime, no `block_on`. The per-`(distro, CLI)` `async fn` is the
//! intended seam for a future migration to the ACP 1.0 proxy/conductor
//! model, where it would become a `ConnectTo` component instead of a
//! one-shot client.
//!
//! **Running distros only:** touching a *stopped* distro's filesystem
//! auto-boots its VM (GH#9541), so we never do it — distro enumeration
//! reuses [`crate::wsl::running_distros`].

use crate::agent_sessions::{AgentSession, CliSource};
use std::time::Duration;

/// Per-`(distro, CLI)` ACP `initialize` budget. WSL adds a `wsl.exe` hop
/// and a login shell, and snap-packaged CLIs (Ubuntu's default copilot)
/// pay a one-time `Package extraction` (~5–6 s) on first `--acp` launch;
/// npx adapters (claude/codex) may download on first run. Generous
/// headroom keeps a cold start from being misread as a failure.
const WSL_ACP_INIT_TIMEOUT: Duration = Duration::from_secs(40);

/// `session/list` is answered from on-disk state once connected; bound it
/// modestly so one wedged distro can't stall the whole scan.
const WSL_ACP_LIST_TIMEOUT: Duration = Duration::from_secs(15);

/// Scan every running WSL distro for `cli`'s historical sessions over ACP,
/// merging the results. `cli` is the agent the session view is showing;
/// `None` (custom / unrecognized agent) scans the three ACP-capable
/// built-ins.
///
/// Gemini is intentionally excluded: its CLI does not implement
/// `session/list` (and is dropping ACP upstream), so it has no ACP path.
/// Any distro/CLI that fails to launch or answer contributes no rows
/// (logged, never fatal).
pub(crate) async fn scan_running_distros_acp(cli: Option<&CliSource>) -> Vec<AgentSession> {
    let clis = clis_to_scan(cli);
    if clis.is_empty() {
        return Vec::new();
    }
    let distros = crate::wsl::running_distros();
    if distros.is_empty() {
        return Vec::new();
    }

    // Load the host-side agent-pane index ONCE for the whole scan instead of
    // per (distro, CLI) pair. (In practice WTA agent-pane session_ids never
    // appear in an in-distro CLI's `session/list`, so this filter is a cheap
    // safety net — but reading + parsing the index per pair added needless disk
    // IO to an already-expensive scan.)
    let idx = crate::agent_pane_origin::load_default_set();

    // Run the (distro × CLI) probes with bounded concurrency so one wedged
    // distro/CLI can't stall the whole scan: each pair is still capped by the
    // init/list timeouts; this just stops those timeouts from summing serially.
    // `buffer_unordered` polls on the current task (no spawn), so the `!Send`
    // ACP connections are fine on the master's LocalSet.
    use futures::stream::StreamExt as _;
    const WSL_SCAN_CONCURRENCY: usize = 4;
    let idx = &idx;
    let pairs: Vec<(&String, &CliSource)> = distros
        .iter()
        .flat_map(|d| clis.iter().map(move |c| (d, c)))
        .collect();

    futures::stream::iter(pairs)
        .map(|(distro, c)| async move {
            let rows = list_distro_cli_sessions(distro, c, idx).await;
            tracing::info!(
                target: "wsl_acp",
                distro = %distro,
                cli = ?c,
                rows = rows.len(),
                "scanned distro/cli over ACP"
            );
            rows
        })
        .buffer_unordered(WSL_SCAN_CONCURRENCY)
        .concat()
        .await
}

/// Which CLIs to query for a given filter. Known ACP-capable CLIs map to
/// themselves. `None` (the all-CLI session view) and custom/unknown agents both
/// map to "scan the built-in ACP-capable CLIs" (copilot, claude, codex) — WTA
/// has no bespoke launch command for a custom agent inside a distro, so it falls
/// back to the known set. Gemini is always excluded: it has no ACP
/// `session/list`.
fn clis_to_scan(cli: Option<&CliSource>) -> Vec<CliSource> {
    match cli {
        Some(CliSource::Copilot) => vec![CliSource::Copilot],
        Some(CliSource::Claude) => vec![CliSource::Claude],
        Some(CliSource::Codex) => vec![CliSource::Codex],
        Some(CliSource::Gemini) => Vec::new(),
        // `None` = the all-CLI view; a custom/unknown agent likewise maps
        // to "scan the known ACP-capable built-ins".
        None | Some(CliSource::Unknown(_)) => {
            vec![CliSource::Copilot, CliSource::Claude, CliSource::Codex]
        }
    }
}

/// Query one `(distro, CLI)` pair. Returns rows on success, empty on any
/// failure (CLI absent in the distro, ACP unsupported, timeout, …).
async fn list_distro_cli_sessions(
    distro: &str,
    cli: &CliSource,
    idx: &std::collections::HashSet<String>,
) -> Vec<AgentSession> {
    let Some(acp_cmd) = acp_command_for(cli) else {
        return Vec::new();
    };
    let mut child = match spawn_wsl_acp(distro, &acp_cmd) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(target: "wsl_acp", distro, %e, "spawn failed");
            return Vec::new();
        }
    };

    let label = format!("wsl:{distro}:{}", crate::session_history::cli_label(cli));
    let result = crate::protocol::acp::session_list::fetch_session_list(
        &mut child,
        &label,
        WSL_ACP_INIT_TIMEOUT,
        WSL_ACP_LIST_TIMEOUT,
    )
    .await;
    // We have what we need; ask the distro CLI to exit. `kill_on_drop`
    // is also set as a backstop.
    let _ = child.start_kill();

    match result {
        Ok((_init, Ok(sessions))) => crate::session_history::classify_and_map(
            &sessions,
            idx,
            crate::agent_sessions::SessionLocation::Wsl {
                distro: distro.to_string(),
            },
            cli,
        ),
        Ok((_init, Err(reason))) => {
            tracing::debug!(target: "wsl_acp", distro, cli = ?cli, "session/list unavailable: {reason}");
            Vec::new()
        }
        Err(e) => {
            tracing::debug!(target: "wsl_acp", distro, cli = ?cli, "ACP handshake failed: {e:#}");
            Vec::new()
        }
    }
}

/// Build the in-distro ACP launch command for `cli` (e.g.
/// `copilot --acp --stdio`, or the npx adapter for claude/codex). `None`
/// for CLIs without an ACP path (Gemini / custom), which are filtered out
/// before this point.
fn acp_command_for(cli: &CliSource) -> Option<String> {
    let id = match cli {
        CliSource::Copilot => "copilot",
        CliSource::Claude => "claude",
        CliSource::Codex => "codex",
        CliSource::Gemini | CliSource::Unknown(_) => return None,
    };
    Some(crate::agent_registry::build_acp_command(id, None))
}

/// Argv for `wsl -d <distro> -- bash -lc "<acp_cmd>"`.
///
/// A **login** shell (`bash -lc`) is required: `wsl.exe -- <cmd>` runs the
/// command under a non-login `bash -c`, where a PATH-installed CLI
/// (npm `~/.local/...`, snap `/snap/bin`) is not found. `acp_cmd` is a
/// single argv element so its internal spaces survive intact (no quoting
/// games).
fn wsl_acp_argv(distro: &str, acp_cmd: &str) -> Vec<String> {
    vec![
        "-d".to_string(),
        distro.to_string(),
        "--".to_string(),
        "bash".to_string(),
        "-lc".to_string(),
        acp_cmd.to_string(),
    ]
}

/// Spawn the distro's ACP CLI with piped stdio.
fn spawn_wsl_acp(distro: &str, acp_cmd: &str) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new("wsl.exe")
        .args(wsl_acp_argv(distro, acp_cmd))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_sessions::{AgentStatus, SessionLocation, SessionOrigin};
    use agent_client_protocol as acp;
    use std::path::PathBuf;
    use std::time::SystemTime;

    fn session_info(id: &str, cwd: &str) -> acp::schema::v1::SessionInfo {
        acp::schema::v1::SessionInfo::new(acp::schema::v1::SessionId::new(id.to_string()), PathBuf::from(cwd))
    }

    fn map_wsl_session(
        info: &acp::schema::v1::SessionInfo,
        distro: &str,
        cli: &CliSource,
    ) -> AgentSession {
        crate::session_history::acp_session_to_agent_session(
            info,
            SessionLocation::Wsl {
                distro: distro.to_string(),
            },
            cli,
        )
    }

    #[test]
    fn clis_to_scan_filters_per_cli_and_excludes_gemini() {
        assert_eq!(
            clis_to_scan(Some(&CliSource::Copilot)),
            vec![CliSource::Copilot]
        );
        assert_eq!(
            clis_to_scan(Some(&CliSource::Claude)),
            vec![CliSource::Claude]
        );
        assert_eq!(
            clis_to_scan(Some(&CliSource::Codex)),
            vec![CliSource::Codex]
        );
        // Gemini has no ACP session/list → never queried.
        assert!(clis_to_scan(Some(&CliSource::Gemini)).is_empty());
        // None / custom → the three ACP-capable built-ins (no Gemini).
        let all = clis_to_scan(None);
        assert_eq!(
            all,
            vec![CliSource::Copilot, CliSource::Claude, CliSource::Codex]
        );
        assert!(!all.contains(&CliSource::Gemini));
        assert_eq!(
            clis_to_scan(Some(&CliSource::Unknown("custom:x".into()))),
            vec![CliSource::Copilot, CliSource::Claude, CliSource::Codex]
        );
    }

    #[test]
    fn acp_command_for_known_clis_and_none_for_rest() {
        assert_eq!(
            acp_command_for(&CliSource::Copilot).as_deref(),
            Some("copilot --acp --stdio")
        );
        assert_eq!(
            acp_command_for(&CliSource::Claude).as_deref(),
            Some("npx -y @agentclientprotocol/claude-agent-acp")
        );
        assert_eq!(
            acp_command_for(&CliSource::Codex).as_deref(),
            Some("npx -y @zed-industries/codex-acp")
        );
        assert!(acp_command_for(&CliSource::Gemini).is_none());
        assert!(acp_command_for(&CliSource::Unknown("x".into())).is_none());
    }

    #[test]
    fn wsl_acp_argv_uses_login_shell_and_single_cmd_arg() {
        let argv = wsl_acp_argv("Ubuntu", "copilot --acp --stdio");
        assert_eq!(
            argv,
            vec!["-d", "Ubuntu", "--", "bash", "-lc", "copilot --acp --stdio"]
        );
        // The whole ACP command must be ONE argv element (login shell sees
        // it as the script string), not split on spaces.
        assert_eq!(argv.len(), 6);
        assert_eq!(argv[5], "copilot --acp --stdio");
    }

    #[test]
    fn maps_acp_session_to_historical_wsl_row() {
        let mut info = session_info("abc-123", "/home/user");
        info.title = Some("Introduction To Debian".to_string());
        info.updated_at = Some("2026-06-24T04:42:14.588Z".to_string());

        let row = map_wsl_session(&info, "Debian", &CliSource::Copilot);

        assert_eq!(row.key, "abc-123");
        assert_eq!(row.cli_source, CliSource::Copilot);
        assert_eq!(row.title, "Introduction To Debian");
        assert_eq!(row.cwd, PathBuf::from("/home/user"));
        assert_eq!(row.status, AgentStatus::Historical);
        assert_eq!(
            row.location,
            SessionLocation::Wsl {
                distro: "Debian".to_string()
            }
        );
        assert_eq!(row.origin, SessionOrigin::default());
        // updated_at parsed; started_at == last_activity_at == that instant.
        assert!(row.last_activity_at > SystemTime::UNIX_EPOCH);
        assert_eq!(row.started_at, row.last_activity_at);
    }

    #[test]
    fn missing_title_falls_back_to_short_id() {
        let info = session_info("0123456789abcdef", "/mnt/c/Users/u");
        let row = map_wsl_session(&info, "Ubuntu", &CliSource::Claude);
        // short_id = "<cli> <first 8 chars>".
        assert_eq!(row.title, "claude 01234567");
    }

    #[test]
    fn empty_title_falls_back_to_short_id() {
        let mut info = session_info("abcdefgh-rest", "/home/u");
        info.title = Some(String::new());
        let row = map_wsl_session(&info, "Ubuntu", &CliSource::Codex);
        assert_eq!(row.title, "codex abcdefgh");
    }

    #[test]
    fn malformed_updated_at_sorts_oldest_not_dropped() {
        let mut info = session_info("k", "/home/u");
        info.updated_at = Some("not-a-timestamp".to_string());
        let row = map_wsl_session(&info, "Ubuntu", &CliSource::Copilot);
        assert_eq!(row.last_activity_at, SystemTime::UNIX_EPOCH);
    }
}

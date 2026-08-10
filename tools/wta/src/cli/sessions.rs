use agent_client_protocol as acp;
use anyhow::{Context, Result};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

const MASTER_NOT_RUNNING: &str = "wta-master not running. Start Windows Terminal first.";

pub(crate) async fn run_list(
    master_override: Option<String>,
    origin_filter: crate::agent_sessions::OriginFilter,
    json_mode: bool,
) -> Result<()> {
    let local = tokio::task::LocalSet::new();
    let sessions = local.run_until(fetch_from_master(master_override)).await?;
    // Origin filter is applied client-side: master always returns the
    // full registry so this command can act as the debug eye-of-god
    // view (default `--origin all`). `--origin shell` matches what
    // the MVP sessions picker shows; `--origin agent-pane` surfaces the
    // rows MVP sessions hides.
    let mut filtered: Vec<crate::session_registry::SessionInfo> = sessions
        .into_iter()
        .filter(|s| origin_filter.matches_opt(s.origin.as_ref()))
        .collect();
    // Match the `/sessions` picker, which renders newest-activity-first.
    // `None` (no timestamp) sorts last.
    filtered.sort_by(|a, b| b.last_activity_at_ms.cmp(&a.last_activity_at_ms));
    if json_mode {
        print!("{}", format_json_lines(&filtered)?);
    } else {
        print!("{}", format_table(&filtered));
    }
    Ok(())
}

async fn fetch_from_master(
    master_override: Option<String>,
) -> Result<Vec<crate::session_registry::SessionInfo>> {
    let pipe_name = resolve_master_pipe(master_override).await?;
    let pipe = open_master_pipe(&pipe_name).await?;
    let (read_half, write_half) = tokio::io::split(pipe);
    let outgoing = write_half.compat_write();
    let incoming = read_half.compat();
    let (conn, handle_io) = crate::protocol::acp::conn::spawn_client(
        acp::Client.builder().name("wta-sessions"),
        crate::protocol::acp::conn::byte_streams(outgoing, incoming),
    );
    tokio::task::spawn_local(async move {
        let _ = handle_io.await;
    });

    let init_started = std::time::Instant::now();
    let init_result = conn
        .initialize(
            acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
                .client_capabilities(acp::schema::v1::ClientCapabilities::new())
                .client_info(
                    acp::schema::v1::Implementation::new("wta-sessions", env!("CARGO_PKG_VERSION"))
                        .title("Windows Terminal Agent sessions CLI"),
                ),
        )
        .await;
    crate::telemetry::log_acp_initialize_complete(
        init_started.elapsed().as_secs_f64() * 1000.0,
        init_result.is_ok(),
        "SessionsCli",
        if init_result.is_ok() { "" } else { "AcpError" },
        init_result
            .as_ref()
            .err()
            .map(|e| e.code.into())
            .unwrap_or(0),
    );
    init_result.map_err(|_| anyhow::anyhow!(MASTER_NOT_RUNNING))?;

    let req = crate::session_registry::build_sessions_list_request(false);
    let resp = conn
        .ext_method(req)
        .await
        .map_err(|_| anyhow::anyhow!(MASTER_NOT_RUNNING))?;
    let parsed = crate::session_registry::parse_sessions_list_response(&resp.0)
        .context("parse sessions/list response")?;
    Ok(parsed.sessions)
}

/// Best-effort: register a WTA-launched CLI session with `wta-master` as a
/// *born-bound* row — bound to its pane, with no hooks involved. Sends a
/// `SessionStarted` over the `intellterm.wta/session_born_bound` method, which
/// the master turns into a Class-B (`origin = Unknown`) row whose
/// `pane_session_id` is the pane we just created and records as binding-only
/// (so the file watcher may still supply activity/status when no hook is
/// installed). Best-effort: if master is unreachable there is no registry to
/// populate, so the registration is dropped (logged at `warn`) and the tab
/// still opens normally.
pub(crate) async fn register_launched(
    session_id: &str,
    pane_session_id: &str,
    cli_id: &str,
    cwd: Option<&str>,
    wsl_distro: Option<&str>,
) {
    let event = crate::agent_sessions::SessionEvent::SessionStarted {
        key: session_id.to_string(),
        cli_source: crate::agent_sessions::CliSource::from(
            crate::session_registry::SessionHookCliSource::Known(cli_id.to_string()),
        ),
        pane_session_id: pane_session_id.to_string(),
        cwd: cwd.map(std::path::PathBuf::from).unwrap_or_default(),
        // Empty title: the master refreshes the row's title from the CLI's
        // on-disk session artefacts once they appear.
        title: String::new(),
    };
    // A WSL delegate carries its distro so the master stamps the row
    // `Wsl { distro }` → the session view shows the `[WSL-<distro>]`
    // prefix.
    let req = match wsl_distro {
        Some(distro) => crate::session_registry::build_born_bound_request_wsl(&event, distro),
        None => crate::session_registry::build_born_bound_request(&event),
    };

    // Own LocalSet so the `spawn_local` transport works regardless of how the
    // delegate's runtime was set up (mirrors `run_list`).
    let local = tokio::task::LocalSet::new();
    let result: Result<()> = local
        .run_until(async move {
            let pipe_name = resolve_master_pipe(None).await?;
            let pipe = open_master_pipe(&pipe_name).await?;
            let (read_half, write_half) = tokio::io::split(pipe);
            let outgoing = write_half.compat_write();
            let incoming = read_half.compat();
            let (conn, handle_io) = crate::protocol::acp::conn::spawn_client(
                acp::Client.builder().name("wta-delegate"),
                crate::protocol::acp::conn::byte_streams(outgoing, incoming),
            );
            tokio::task::spawn_local(async move {
                let _ = handle_io.await;
            });

            conn.initialize(
                acp::schema::v1::InitializeRequest::new(acp::schema::ProtocolVersion::V1)
                    .client_capabilities(acp::schema::v1::ClientCapabilities::new())
                    .client_info(
                        acp::schema::v1::Implementation::new(
                            "wta-delegate",
                            env!("CARGO_PKG_VERSION"),
                        )
                        .title("Windows Terminal Agent delegate"),
                    ),
            )
            .await
            .map_err(|_| anyhow::anyhow!(MASTER_NOT_RUNNING))?;

            conn.ext_method(req)
                .await
                .map_err(|_| anyhow::anyhow!(MASTER_NOT_RUNNING))?;
            Ok(())
        })
        .await;

    if let Err(e) = result {
        tracing::warn!(
            target: "delegate",
            error = %e,
            "register born-bound session with master failed (best-effort)"
        );
    }
}

async fn resolve_master_pipe(master_override: Option<String>) -> Result<String> {
    if let Some(pipe) = master_override.filter(|s| !s.trim().is_empty()) {
        return Ok(pipe);
    }

    for attempt in 0..2 {
        if let Some(path) = crate::runtime_paths::master_pipe_file_path() {
            if let Ok(contents) = std::fs::read_to_string(path) {
                let pipe = contents.trim();
                if !pipe.is_empty() {
                    return Ok(pipe.to_string());
                }
            }
        }
        if attempt == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    Err(anyhow::anyhow!(MASTER_NOT_RUNNING))
}

async fn open_master_pipe(
    pipe_name: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    for attempt in 0..2 {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name) {
            Ok(pipe) => return Ok(pipe),
            Err(_) if attempt == 0 => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await
            }
            Err(_) => return Err(anyhow::anyhow!(MASTER_NOT_RUNNING)),
        }
    }
    Err(anyhow::anyhow!(MASTER_NOT_RUNNING))
}

fn format_json_lines(sessions: &[crate::session_registry::SessionInfo]) -> Result<String> {
    let mut out = String::new();
    for session in sessions {
        out.push_str(&serde_json::to_string(session)?);
        out.push('\n');
    }
    Ok(out)
}

fn format_table(sessions: &[crate::session_registry::SessionInfo]) -> String {
    let mut out = String::new();
    if sessions.is_empty() {
        out.push_str("No sessions.\n");
        return out;
    }
    out.push_str(&format!(
        "{:<4} {:<24} {:<10} {:<10} {:<10} {:<16} {:<20} {:<20} {}\n",
        "#", "SESSION", "STATUS", "CLI", "ORIGIN", "LOCATION", "PANE", "UPDATED", "TITLE"
    ));
    for (i, session) in sessions.iter().enumerate() {
        let sid = session.session_id.to_string();
        let short_sid = if sid.len() > 24 {
            &sid[..24]
        } else {
            sid.as_str()
        };
        out.push_str(&format!(
            "{:<4} {:<24} {:<10} {:<10} {:<10} {:<16} {:<20} {:<20} {}\n",
            i + 1,
            short_sid,
            status_label(session.status.as_ref()),
            cli_source_label(session.cli_source.as_ref()),
            origin_label(session.origin.as_ref()),
            location_label(&session.location),
            session.pane_session_id.as_deref().unwrap_or("-"),
            updated_label(session),
            session.title.as_deref().unwrap_or("-"),
        ));
    }
    out
}

fn status_label(status: Option<&crate::agent_sessions::AgentStatus>) -> String {
    status
        .map(|s| format!("{s:?}"))
        .unwrap_or_else(|| "-".to_string())
}

fn cli_source_label(source: Option<&crate::agent_sessions::CliSource>) -> String {
    match source {
        Some(crate::agent_sessions::CliSource::Claude) => "Claude".to_string(),
        Some(crate::agent_sessions::CliSource::Codex) => "Codex".to_string(),
        Some(crate::agent_sessions::CliSource::Copilot) => "Copilot".to_string(),
        Some(crate::agent_sessions::CliSource::Gemini) => "Gemini".to_string(),
        Some(crate::agent_sessions::CliSource::OpenCode) => "OpenCode".to_string(),
        Some(crate::agent_sessions::CliSource::Unknown(s)) if !s.is_empty() => s.clone(),
        _ => "-".to_string(),
    }
}

/// Render a `SessionOrigin` for the `wta sessions list` table. `None`
/// is the on-the-wire representation for "field absent" (legacy rows
/// or notification paths that don't carry origin) — we print `-`
/// rather than fabricating an origin so the operator can tell
/// "untagged" from "shell".
fn origin_label(origin: Option<&crate::agent_sessions::SessionOrigin>) -> &'static str {
    match origin {
        Some(crate::agent_sessions::SessionOrigin::AgentPane) => "AgentPane",
        Some(crate::agent_sessions::SessionOrigin::Unknown) => "Shell",
        None => "-",
    }
}

/// Render a `SessionLocation` for the `wta sessions list` table: `host`
/// for Windows-profile sessions, `wsl:<distro>` for sessions discovered
/// inside a WSL distro.
fn location_label(location: &crate::agent_sessions::SessionLocation) -> String {
    match location {
        crate::agent_sessions::SessionLocation::Host => "host".to_string(),
        crate::agent_sessions::SessionLocation::Wsl { distro } => format!("wsl:{distro}"),
        crate::agent_sessions::SessionLocation::Remote { host, .. } => {
            format!("remote:{host}")
        }
    }
}

/// Render the UPDATED column. Prefers the `updated_at` ISO string (set for
/// live sessions); for history-scanned rows that only carry an epoch-ms
/// `last_activity_at_ms`, formats that as a `YYYY-MM-DD HH:MM` UTC stamp so
/// the column isn't blank. `-` when neither is available.
fn updated_label(s: &crate::session_registry::SessionInfo) -> String {
    if let Some(u) = s.updated_at.as_deref() {
        return u.to_string();
    }
    match s.last_activity_at_ms {
        Some(ms) => format_epoch_ms_utc(ms),
        None => "-".to_string(),
    }
}

/// Format epoch milliseconds as `YYYY-MM-DD HH:MM` (UTC) without pulling in a
/// date crate. Uses Howard Hinnant's `civil_from_days` algorithm.
fn format_epoch_ms_utc(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (hour, min) = (tod / 3600, (tod % 3600) / 60);
    // civil_from_days: days since 1970-01-01 -> (year, month, day).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_lines_prints_one_session_info_per_line() {
        let mut row = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-json"),
            std::path::PathBuf::from("C:\\repo"),
        );
        row.status = Some(crate::agent_sessions::AgentStatus::Working);
        row.cli_source = Some(crate::agent_sessions::CliSource::Copilot);
        row.current_tool = Some("shell".into());

        let out = format_json_lines(&[row]).expect("format jsonl");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 1);
        let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(value["session_id"], "sid-json");
        assert_eq!(value["status"], "Working");
        assert_eq!(value["cli_source"], "Copilot");
        assert_eq!(value["current_tool"], "shell");
    }

    #[test]
    fn table_prints_header_and_rows() {
        let mut row = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-table"),
            std::path::PathBuf::from("C:\\repo"),
        );
        row.title = Some("fix build".into());
        row.status = Some(crate::agent_sessions::AgentStatus::Idle);
        row.cli_source = Some(crate::agent_sessions::CliSource::Claude);
        row.pane_session_id = Some("pane-table".into());

        let out = format_table(&[row]);
        assert!(out.contains("SESSION"));
        assert!(out.contains("sid-table"));
        assert!(out.contains("Idle"));
        assert!(out.contains("Claude"));
        assert!(out.contains("pane-table"));
        assert!(out.contains("ORIGIN"));
        let body = out.lines().nth(1).expect("body row present");
        assert!(
            body.contains(" - "),
            "untagged origin renders as '-' got: {body}"
        );
        assert!(
            out.lines().next().expect("header").starts_with("#"),
            "header has # column"
        );
        assert!(
            body.starts_with("1"),
            "first row is numbered 1, got: {body}"
        );
    }

    #[test]
    fn table_renders_origin_labels() {
        let mut shell = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-shell"),
            std::path::PathBuf::from("C:\\repo"),
        );
        shell.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
        let mut pane = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-pane"),
            std::path::PathBuf::from("C:\\repo"),
        );
        pane.origin = Some(crate::agent_sessions::SessionOrigin::AgentPane);

        let out = format_table(&[shell, pane]);
        assert!(out.contains("Shell"), "shell origin label present: {out}");
        assert!(
            out.contains("AgentPane"),
            "agent-pane origin label present: {out}"
        );
    }

    #[test]
    fn table_renders_location_labels() {
        let mut host = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-host"),
            std::path::PathBuf::from("C:\\repo"),
        );
        host.location = crate::agent_sessions::SessionLocation::Host;
        let mut wsl = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-wsl"),
            std::path::PathBuf::from("/home/u"),
        );
        wsl.location = crate::agent_sessions::SessionLocation::Wsl {
            distro: "Ubuntu".into(),
        };

        let out = format_table(&[host, wsl]);
        assert!(out.contains("LOCATION"), "LOCATION header present: {out}");
        assert!(out.contains("host"), "host location label present: {out}");
        assert!(
            out.contains("wsl:Ubuntu"),
            "wsl distro label present: {out}"
        );
    }

    #[test]
    fn epoch_ms_utc_formats_known_values() {
        assert_eq!(format_epoch_ms_utc(0), "1970-01-01 00:00");
        assert_eq!(format_epoch_ms_utc(1_609_459_200_000), "2021-01-01 00:00");
        assert_eq!(format_epoch_ms_utc(1_614_556_800_000), "2021-03-01 00:00");
    }

    #[test]
    fn updated_label_falls_back_to_last_activity_ms() {
        let mut session = crate::session_registry::SessionInfo::new(
            acp::schema::v1::SessionId::new("sid-u"),
            std::path::PathBuf::from("/home/u"),
        );
        session.updated_at = None;
        session.last_activity_at_ms = Some(1_609_459_200_000);
        assert_eq!(updated_label(&session), "2021-01-01 00:00");
        session.updated_at = Some("2026-06-22T03:33:46Z".into());
        assert_eq!(updated_label(&session), "2026-06-22T03:33:46Z");
    }
}

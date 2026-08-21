use anyhow::{Context, Result};

/// Drive [`crate::protocol::acp::probe::probe_models`] on a tokio `LocalSet`
/// (the ACP client connection is `!Send`), serialize the result to
/// stdout, force-exit. See exit notes below.
pub(crate) async fn run_models(agent: &str) -> Result<()> {
    // Logging is initialized in `main()` (file, not stderr — the Settings UI
    // captures our stdout for the JSON payload and stderr would pollute it).
    tracing::info!("probe-models start: agent={}", agent);

    let local = tokio::task::LocalSet::new();
    let result = match local
        .run_until(crate::protocol::acp::probe::probe_models(agent))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("probe-models failed: {:#}", e);
            eprintln!("probe-models failed: {:#}", e);
            let _ = std::io::Write::flush(&mut std::io::stderr());
            // Flush the file appender — process::exit skips the guard drop.
            crate::logging::shutdown_flush();
            // See exit rationale below.
            std::process::exit(1);
        }
    };
    tracing::info!(
        "probe-models ok: {} model(s), current={:?}",
        result.available_models.len(),
        result.current_model_id
    );
    let payload = serde_json::to_string(&result).context("serialize probe result")?;
    println!("{}", payload);

    // Force-exit before the tokio runtime tries to drop. The agent we
    // spawned is e.g. `cmd /c npx ...`; kill_on_drop kills cmd but
    // the npx → node grandchildren survive as orphans. Tokio's IOCP
    // reactor stays blocked on handles those orphans inherited and
    // the runtime drop hangs for ~35s. Runtime cleanup is meaningless
    // for a one-shot CLI — the caller is blocked on our process
    // handle, exit now. Orphan grandchildren self-exit shortly after
    // when they notice their pipes are broken.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // Flush the file appender — process::exit skips the guard drop.
    crate::logging::shutdown_flush();
    std::process::exit(0);
}

#[derive(serde::Serialize)]
struct AgentSourceProbeEntry {
    id: &'static str,
    display_name: &'static str,
}

#[derive(serde::Serialize)]
struct AgentSourceProbeResult {
    wsl_distro: String,
    agents: Vec<AgentSourceProbeEntry>,
}

#[derive(serde::Serialize)]
struct HostAgentProbeResult {
    agents: Vec<AgentSourceProbeEntry>,
}

pub(crate) fn run_host_agents() -> Result<()> {
    let agents = crate::agent_registry::KNOWN_AGENTS
        .iter()
        .filter(|profile| crate::agent_check::host_agent_available(profile.id))
        .map(|profile| AgentSourceProbeEntry {
            id: profile.id,
            display_name: profile.display_name,
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string(&HostAgentProbeResult { agents })
            .context("serialize Host agent probe")?
    );
    Ok(())
}

pub(crate) async fn run_agent_sources(wsl_distro: &str) -> Result<()> {
    let distro = wsl_distro.trim();
    anyhow::ensure!(!distro.is_empty(), "--wsl-distro must not be empty");

    use futures::StreamExt as _;
    let agents = futures::stream::iter(crate::agent_registry::KNOWN_AGENTS)
        .map(|profile| async move {
            crate::agent_check::wsl_agent_available(distro, profile.id)
                .await
                .then_some(AgentSourceProbeEntry {
                    id: profile.id,
                    display_name: profile.display_name,
                })
        })
        .buffer_unordered(crate::agent_registry::KNOWN_AGENTS.len())
        .filter_map(async move |entry| entry)
        .collect()
        .await;

    println!(
        "{}",
        serde_json::to_string(&AgentSourceProbeResult {
            wsl_distro: distro.to_string(),
            agents,
        })
        .context("serialize agent source probe")?
    );
    Ok(())
}

/// Drive [`crate::protocol::acp::probe::probe_sessions`] on a tokio `LocalSet`
/// (the ACP client connection is `!Send`), print the result as pretty
/// JSON to stdout, force-exit. Diagnostic-only: evaluates whether an
/// agent CLI answers ACP `session/list` and what it returns.
pub(crate) async fn run_sessions(agent: &str) -> Result<()> {
    tracing::info!("probe-sessions start: agent={}", agent);

    let local = tokio::task::LocalSet::new();
    let result = match local
        .run_until(crate::protocol::acp::probe::probe_sessions(agent))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("probe-sessions failed: {:#}", e);
            eprintln!("probe-sessions failed: {:#}", e);
            let _ = std::io::Write::flush(&mut std::io::stderr());
            crate::logging::shutdown_flush();
            std::process::exit(1);
        }
    };
    tracing::info!(
        "probe-sessions ok: list_ok={} sessions={} err={:?}",
        result.list_sessions_ok,
        result.sessions.len(),
        result.list_sessions_error
    );
    let payload = serde_json::to_string_pretty(&result).context("serialize session probe")?;
    println!("{payload}");

    // Same force-exit rationale as run_models (orphan npx/node
    // grandchildren keep the tokio reactor blocked on drop).
    let _ = std::io::Write::flush(&mut std::io::stdout());
    crate::logging::shutdown_flush();
    std::process::exit(0);
}

/// Diagnostic host-history smoke test: run one ACP CLI, fetch
/// `session/list`, apply the production Class-A filter, and print the
/// rows in the same compact shape used by the WSL probe.
pub(crate) async fn run_host_sessions(agent: &str) -> Result<()> {
    use crate::agent_sessions::{CliSource, SessionLocation};
    use std::time::Duration;

    tracing::info!("probe-host-sessions start: agent={}", agent);

    // Resolve the CliSource from the agent command so the probe labels and
    // classifies rows the way production seeding does (which uses the real
    // `state.cli_source`), instead of assuming Copilot for every agent.
    let cli_source = CliSource::parse(Some(crate::agent_registry::resolve_agent_id_from_cmd(
        agent,
    )));

    let local = tokio::task::LocalSet::new();
    let rows = match local
        .run_until(async {
            let mut spawned = crate::protocol::acp::spawn::spawn_agent_process(
                agent,
                None,
                None,
                crate::protocol::acp::spawn::ChildEnvironmentPolicy::ApplySharedProvider,
            )?;
            let label = format!("host:{}", crate::session_history::cli_label(&cli_source));
            let init_timeout = Duration::from_secs(if spawned.is_npx { 25 } else { 10 });
            let result = crate::protocol::acp::session_list::fetch_session_list(
                &mut spawned.child,
                &label,
                init_timeout,
                Duration::from_secs(10),
            )
            .await;
            let _ = spawned.child.start_kill();
            let (_init, list_result) = result?;
            // session/list unsupported (e.g. `Method not found`) is the production
            // "empty history, no fallback" case — surface it as `[]` + exit 0, not a
            // diagnostic failure. A genuine spawn/init error still propagates above.
            let sessions = list_result.unwrap_or_else(|e| {
                tracing::info!("probe-host-sessions: session/list unavailable ({e}); returning []");
                Vec::new()
            });
            let idx = crate::agent_pane_origin::load_default_set();
            Ok::<_, anyhow::Error>(crate::session_history::classify_and_map(
                &sessions,
                &idx,
                SessionLocation::Host,
                &cli_source,
            ))
        })
        .await
    {
        Ok(r) => r,
        Err(e) => {
            // Same force-exit rationale as run_sessions: orphan npx/node
            // grandchildren keep the tokio reactor blocked ~35s on drop.
            tracing::error!("probe-host-sessions failed: {:#}", e);
            eprintln!("probe-host-sessions failed: {:#}", e);
            let _ = std::io::Write::flush(&mut std::io::stderr());
            crate::logging::shutdown_flush();
            std::process::exit(1);
        }
    };

    let json: Vec<_> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "key": r.key,
                "cli": format!("{:?}", r.cli_source),
                "title": r.title,
                "cwd": r.cwd.to_string_lossy(),
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json).context("serialize host session probe")?
    );

    tracing::info!("probe-host-sessions ok: {} row(s)", rows.len());
    let _ = std::io::Write::flush(&mut std::io::stdout());
    crate::logging::shutdown_flush();
    std::process::exit(0);
}

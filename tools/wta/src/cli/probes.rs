use anyhow::{Context, Result};

pub(crate) async fn run_install_acp(
    agent_id: &str,
    wsl_distro: Option<&str>,
    retry_startup: bool,
) -> Result<()> {
    let provider = crate::agent_lifecycle::providers::lookup(agent_id)
        .context("unknown ACP provider")?;
    anyhow::ensure!(
        provider.installer_kind()
            == crate::agent_lifecycle::providers::InstallerKind::StandaloneAcp,
        "this agent has no separately installable ACP dependency"
    );
    let source = match wsl_distro {
        Some(distro) => crate::agent_source::AgentSource::Wsl {
            distro: distro.to_string(),
        },
        None => crate::agent_source::AgentSource::Host,
    };
    let command =
        crate::agent_registry::build_acp_command_for_source(provider.family_id(), None, &source);
    run_verify_acp(&command, provider.family_id(), wsl_distro, retry_startup).await
}

pub(crate) async fn run_verify_acp(
    command: &str,
    agent_id: &str,
    wsl_distro: Option<&str>,
    retry_startup: bool,
) -> Result<()> {
    let source = match wsl_distro {
        Some(distro) => crate::agent_source::AgentSource::Wsl {
            distro: distro.to_string(),
        },
        None => crate::agent_source::AgentSource::Host,
    };
    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(crate::agent_lifecycle::verification::verify(
            command,
            agent_id,
            &source,
            retry_startup,
        ))
        .await;
    match result {
        Ok(verified) => {
            println!(
                "{}",
                serde_json::json!({
                    "agent_id": agent_id,
                    "server": verified.executable,
                    "dependency_present": true,
                    "startup_verified": true,
                    "agent_name": verified.name,
                    "agent_version": verified.version
                })
            );
            Ok(())
        }
        Err(failure) => {
            println!(
                "{}",
                serde_json::json!({
                    "agent_id": agent_id,
                    "dependency_present": failure.dependency_present,
                    "startup_verified": failure.startup_verified,
                    "startup_failure": failure
                })
            );
            Err(failure.into())
        }
    }
}

/// Drive [`crate::protocol::acp::probe::probe_models`] on a tokio `LocalSet`
/// (the ACP client connection is `!Send`), serialize the result to
/// stdout, force-exit. See exit notes below.
pub(crate) async fn run_models(
    agent: &str,
    agent_id: Option<&str>,
    retry_startup: bool,
) -> Result<()> {
    // Logging is initialized in `main()` (file, not stderr — the Settings UI
    // captures our stdout for the JSON payload and stderr would pollute it).
    tracing::info!("probe-models start: agent={}", agent);

    let local = tokio::task::LocalSet::new();
    let result = match local
        .run_until(crate::protocol::acp::probe::probe_models_with_intent(
            agent,
            agent_id,
            retry_startup,
        ))
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let failure = crate::agent_lifecycle::StartupFailure::from_error(&e, 0, None, false);
            println!(
                "{}",
                serde_json::json!({
                    "dependency_present": failure.dependency_present,
                    "startup_verified": failure.startup_verified,
                    "startup_failure": failure
                })
            );
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

    // Process-tree cleanup is confirmed by the probe before reaching this
    // one-shot exit. Flush output and logs before skipping runtime teardown.
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
    availability: Vec<HostAgentAvailabilityEntry>,
    npx_found: bool,
}

#[derive(serde::Serialize)]
struct HostAgentAvailabilityEntry {
    id: &'static str,
    display_name: &'static str,
    native_cli_found: bool,
    launch_ready: bool,
    requires_npx: bool,
    acp_bootstrap_available: bool,
}

pub(crate) fn run_host_agents() -> Result<()> {
    let npx_found = crate::agent_check::host_npx_available();
    let availability: Vec<_> = crate::agent_registry::KNOWN_AGENTS
        .iter()
        .map(|profile| {
            let status = crate::agent_check::check_host_agent_availability(profile.id, npx_found);
            HostAgentAvailabilityEntry {
                id: profile.id,
                display_name: profile.display_name,
                native_cli_found: status.native_cli_found,
                launch_ready: status.launch_ready,
                requires_npx: status.requires_npx,
                acp_bootstrap_available: status.acp_bootstrap_available,
            }
        })
        .collect();
    let agents = availability
        .iter()
        .filter(|status| status.launch_ready || status.acp_bootstrap_available)
        .map(|status| AgentSourceProbeEntry {
            id: status.id,
            display_name: status.display_name,
        })
        .collect();

    println!(
        "{}",
        serde_json::to_string(&HostAgentProbeResult {
            agents,
            availability,
            npx_found,
        })
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
pub(crate) async fn run_sessions(agent: &str, retry_startup: bool) -> Result<()> {
    tracing::info!("probe-sessions start: agent={}", agent);

    let local = tokio::task::LocalSet::new();
    let result = match local
        .run_until(crate::protocol::acp::probe::probe_sessions(
            agent,
            retry_startup,
        ))
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

    // The probe has already confirmed process-tree cleanup.
    let _ = std::io::Write::flush(&mut std::io::stdout());
    crate::logging::shutdown_flush();
    std::process::exit(0);
}

/// Diagnostic host-history smoke test: run one ACP CLI, fetch
/// `session/list`, apply the production Class-A filter, and print the
/// rows in the same compact shape used by the WSL probe.
pub(crate) async fn run_host_sessions(agent: &str, retry_startup: bool) -> Result<()> {
    use crate::agent_sessions::{CliSource, SessionLocation};

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
            let (_init, list_result) =
                crate::protocol::acp::session_list::fetch_session_list(agent, retry_startup)
                    .await?;
            // session/list unsupported (e.g. `Method not found`) is the production
            // "empty history, no fallback" case — surface it as `[]` + exit 0, not a
            // diagnostic failure. A genuine spawn/init error still propagates above.
            let sessions = list_result.unwrap_or_else(|e| {
                tracing::info!("probe-host-sessions: session/list unavailable ({e}); returning []");
                Vec::new()
            });
            let idx = crate::agent_pane_origin::load_default_index();
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
            // Shared startup has already confirmed cleanup or persisted quarantine.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_agent_probe_keeps_strict_agents_and_adds_fre_availability() {
        let payload = serde_json::to_value(HostAgentProbeResult {
            agents: vec![AgentSourceProbeEntry {
                id: "copilot",
                display_name: "GitHub Copilot",
            }],
            availability: vec![
                HostAgentAvailabilityEntry {
                    id: "copilot",
                    display_name: "GitHub Copilot",
                    native_cli_found: true,
                    launch_ready: true,
                    requires_npx: false,
                    acp_bootstrap_available: false,
                },
                HostAgentAvailabilityEntry {
                    id: "claude",
                    display_name: "Claude",
                    native_cli_found: true,
                    launch_ready: false,
                    requires_npx: true,
                    acp_bootstrap_available: false,
                },
            ],
            npx_found: false,
        })
        .expect("serialize probe result");

        let strict_agents = payload["agents"].as_array().expect("strict agent list");
        assert_eq!(strict_agents.len(), 1);
        assert_eq!(strict_agents[0]["id"], "copilot");

        let availability = payload["availability"]
            .as_array()
            .expect("availability list");
        assert_eq!(availability.len(), 2);
        assert_eq!(availability[1]["id"], "claude");
        assert_eq!(availability[1]["native_cli_found"], true);
        assert_eq!(availability[1]["launch_ready"], false);
        assert_eq!(availability[1]["requires_npx"], true);
        assert_eq!(payload["npx_found"], false);
    }

    #[test]
    fn antigravity_bootstrap_metadata_does_not_claim_acp_readiness() {
        let payload = serde_json::to_value(HostAgentAvailabilityEntry {
            id: "antigravity",
            display_name: "Google Antigravity",
            native_cli_found: false,
            launch_ready: false,
            requires_npx: false,
            acp_bootstrap_available: true,
        })
        .unwrap();
        assert_eq!(payload["acp_bootstrap_available"], true);
        assert_eq!(payload["launch_ready"], false);
        assert_eq!(payload["native_cli_found"], false);
    }
}

use anyhow::{Context, Result};

pub(crate) async fn run_install_acp(
    agent_id: &str,
    wsl_distro: Option<&str>,
    retry_startup: bool,
) -> Result<()> {
    let provider =
        crate::agent_lifecycle::providers::lookup(agent_id).context("unknown ACP provider")?;
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
    wsl_distro: Option<&str>,
    retry_startup: bool,
) -> Result<()> {
    // Logging is initialized in `main()` (file, not stderr — the Settings UI
    // captures our stdout for the JSON payload and stderr would pollute it).
    tracing::info!("probe-models start: agent={}", agent);
    let source =
        crate::agent_source::AgentSource::from_wire(wsl_distro.map(|_| "wsl"), wsl_distro)?;

    let local = tokio::task::LocalSet::new();
    let result = match local
        .run_until(
            crate::protocol::acp::probe::probe_models_for_source_with_intent(
                agent,
                agent_id,
                &source,
                retry_startup,
            ),
        )
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
    interactive_agents: Vec<AgentSourceProbeEntry>,
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
    anyhow::ensure!(
        crate::agent_source::is_safe_wsl_distro_name(distro),
        "Invalid --wsl-distro"
    );
    crate::agent_source::ensure_wsl_running(distro).await?;
    let (agents, interactive_agents, error) = probe_distro_agents(distro, false).await;
    anyhow::ensure!(error.is_none(), "{}", error.unwrap_or_default());

    println!(
        "{}",
        serde_json::to_string(&AgentSourceProbeResult {
            wsl_distro: distro.to_string(),
            agents,
            interactive_agents,
        })
        .context("serialize agent source probe")?
    );
    Ok(())
}

async fn probe_distro_agents(
    distro: &str,
    start_stopped_wsl: bool,
) -> (
    Vec<AgentSourceProbeEntry>,
    Vec<AgentSourceProbeEntry>,
    Option<String>,
) {
    use futures::StreamExt as _;
    let results: Vec<_> = futures::stream::iter(crate::agent_registry::KNOWN_AGENTS)
        .map(|profile| async move {
            if !start_stopped_wsl {
                // Passive callers must not turn a running-state failure into permission.
                crate::agent_source::ensure_wsl_running(distro).await?;
            }
            Ok::<_, anyhow::Error>(
                probe_agent_capabilities(profile, distro, |executable| async move {
                    crate::agent_check::find_wsl_exe_checked(distro, executable)
                        .await
                        .map(|path| path.is_some())
                })
                .await,
            )
        })
        // Keep registry order when associating the completed results below.
        .buffered(4)
        .collect()
        .await;
    let mut agents = Vec::new();
    let mut interactive_agents = Vec::new();
    let mut errors = Vec::new();
    for (profile, result) in crate::agent_registry::KNOWN_AGENTS.iter().zip(results) {
        match result {
            Ok((acp, interactive, failures)) => {
                if acp {
                    agents.push(AgentSourceProbeEntry {
                        id: profile.id,
                        display_name: profile.display_name,
                    });
                }
                if interactive {
                    interactive_agents.push(AgentSourceProbeEntry {
                        id: profile.id,
                        display_name: profile.display_name,
                    });
                }
                errors.extend(failures);
            }
            Err(error) => errors.push(error.to_string()),
        }
    }
    agents.sort_by_key(|agent| agent.id);
    interactive_agents.sort_by_key(|agent| agent.id);
    errors.sort();
    errors.dedup();
    (
        agents,
        interactive_agents,
        (!errors.is_empty()).then(|| errors.join("; ")),
    )
}

async fn probe_agent_capabilities<F, Fut>(
    profile: &crate::agent_registry::AgentProfile,
    distro: &str,
    probe: F,
) -> (bool, bool, Vec<String>)
where
    F: Fn(&'static str) -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    let source = crate::agent_source::AgentSource::Wsl {
        distro: distro.to_string(),
    };
    let mut errors = Vec::new();
    let mut record = |result: Result<bool>| match result {
        Ok(found) => found,
        Err(error) => {
            errors.push(format!("{}: {error:#}", profile.id));
            false
        }
    };
    let interactive = record(probe(profile.cli_executable).await);
    let executable = profile.acp_executable(&source);
    let mut acp = if executable == profile.cli_executable {
        interactive
    } else {
        record(probe(executable).await)
    };
    if acp && profile.acp_command_override(&source).starts_with("npx ") {
        acp = record(probe("npx").await);
    }
    // Antigravity's native CLI can bootstrap its ACP dependency on demand.
    if profile.id == crate::agent_registry::ANTIGRAVITY_AGENT_ID {
        acp |= interactive;
    }
    (acp, interactive, errors)
}

#[derive(serde::Serialize)]
struct WslSourceProbeResult {
    wsl_distro: String,
    agents: Vec<AgentSourceProbeEntry>,
    interactive_agents: Vec<AgentSourceProbeEntry>,
    running: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn collect_wsl_source_probes<F, Fut>(
    registered: Vec<String>,
    running: Result<Vec<String>>,
    start_stopped_wsl: bool,
    probe: F,
) -> Vec<WslSourceProbeResult>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<
        Output = (
            Vec<AgentSourceProbeEntry>,
            Vec<AgentSourceProbeEntry>,
            Option<String>,
        ),
    >,
{
    use futures::StreamExt as _;
    let probe = &probe;
    // Both levels are bounded: at most two distributions, four agents each.
    let mut sources: Vec<_> = futures::stream::iter(registered)
        .map(|distro| {
            let running = &running;
            async move {
                let (is_running, error) = match running {
                    Ok(names) => (
                        names.iter().any(|name| name.eq_ignore_ascii_case(&distro)),
                        None,
                    ),
                    Err(error) => (
                        false,
                        Some(format!("Running-state query failed: {error:#}")),
                    ),
                };
                let (agents, interactive_agents, error) = if is_running || start_stopped_wsl {
                    probe(distro.clone()).await
                } else {
                    (Vec::new(), Vec::new(), error)
                };
                WslSourceProbeResult {
                    wsl_distro: distro,
                    // A completed active path probe also establishes running state.
                    running: is_running
                        || (start_stopped_wsl
                            && (error.is_none()
                                || !agents.is_empty()
                                || !interactive_agents.is_empty())),
                    agents,
                    interactive_agents,
                    error,
                }
            }
        })
        .buffer_unordered(2)
        .collect()
        .await;
    sources.sort_by_key(|source| {
        (
            source.wsl_distro.to_ascii_lowercase(),
            source.wsl_distro.clone(),
        )
    });
    sources
}

pub(crate) async fn run_all_agent_sources(start_stopped_wsl: bool) -> Result<()> {
    let registered = crate::agent_source::list_wsl_distros(false).await?;
    let running = crate::agent_source::list_wsl_distros(true).await;
    let sources = collect_wsl_source_probes(
        registered,
        running,
        start_stopped_wsl,
        |distro| async move { probe_distro_agents(&distro, start_stopped_wsl).await },
    )
    .await;
    println!("{}", serde_json::json!({ "sources": sources }));
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

    #[tokio::test]
    async fn wsl_capabilities_keep_native_adapters_without_npx_interactive() {
        for id in ["claude", "codex"] {
            let profile = crate::agent_registry::lookup_profile_by_id(id);
            let (acp, interactive, errors) =
                probe_agent_capabilities(profile, "Ubuntu", |executable| async move {
                    Ok(executable == profile.cli_executable)
                })
                .await;
            assert!(!acp, "{id} needs npx for ACP");
            assert!(interactive, "{id} does not need npx interactively");
            assert!(errors.is_empty());
        }
    }

    #[tokio::test]
    async fn wsl_capabilities_separate_antigravity_server_from_native_cli() {
        let profile = crate::agent_registry::lookup_profile_by_id("antigravity");
        for native in [false, true] {
            let (acp, interactive, errors) =
                probe_agent_capabilities(profile, "Ubuntu", |executable| async move {
                    Ok(if executable == "agy" { native } else { !native })
                })
                .await;
            assert!(acp, "server or native bootstrap remains selectable");
            assert_eq!(interactive, native);
            assert!(errors.is_empty());
        }
    }

    #[tokio::test]
    async fn wsl_capabilities_keep_native_success_when_adapter_probe_fails() {
        let profile = crate::agent_registry::lookup_profile_by_id("claude");
        let (acp, interactive, errors) =
            probe_agent_capabilities(profile, "Ubuntu", |executable| async move {
                if executable == "npx" {
                    anyhow::bail!("probe timed out");
                }
                Ok(true)
            })
            .await;
        assert!(!acp);
        assert!(interactive);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("probe timed out"));
    }

    #[tokio::test]
    async fn all_wsl_active_discovery_probes_stopped_sources_only_with_explicit_permission() {
        for start_stopped_wsl in [false, true] {
            let calls = std::cell::Cell::new(0);
            let sources = collect_wsl_source_probes(
                vec!["Ubuntu".into(), "Debian".into()],
                Ok(Vec::new()),
                start_stopped_wsl,
                |_| {
                    calls.set(calls.get() + 1);
                    async {
                        (
                            vec![AgentSourceProbeEntry {
                                id: "copilot",
                                display_name: "GitHub Copilot",
                            }],
                            Vec::new(),
                            None,
                        )
                    }
                },
            )
            .await;
            assert_eq!(calls.get(), if start_stopped_wsl { 2 } else { 0 });
            for source in sources {
                assert_eq!(source.running, start_stopped_wsl);
                assert_eq!(source.agents.len(), usize::from(start_stopped_wsl));
                assert!(source.error.is_none());
            }
        }
    }

    #[tokio::test]
    async fn all_wsl_active_permission_is_independent_of_running_query_failure() {
        let sources = collect_wsl_source_probes(
            vec!["Ubuntu".into()],
            Err(anyhow::anyhow!("query failed")),
            true,
            |_| async { (Vec::new(), Vec::new(), None) },
        )
        .await;
        assert!(sources[0].running);
        assert!(sources[0].error.is_none());
    }

    #[tokio::test]
    async fn all_wsl_active_partial_success_preserves_running_evidence_and_source_errors() {
        let sources = collect_wsl_source_probes(
            vec!["Ubuntu".into(), "Debian".into()],
            Ok(Vec::new()),
            true,
            |distro| async move {
                let agents = if distro == "Ubuntu" {
                    vec![AgentSourceProbeEntry {
                        id: "copilot",
                        display_name: "GitHub Copilot",
                    }]
                } else {
                    Vec::new()
                };
                let interactive_agents = if distro == "Debian" {
                    vec![AgentSourceProbeEntry {
                        id: "claude",
                        display_name: "Claude",
                    }]
                } else {
                    Vec::new()
                };
                (
                    agents,
                    interactive_agents,
                    Some("another executable probe failed".into()),
                )
            },
        )
        .await;
        assert_eq!(sources[0].wsl_distro, "Debian");
        assert!(sources[0].running);
        assert!(sources[0].agents.is_empty());
        assert_eq!(sources[0].interactive_agents.len(), 1);
        assert!(sources[0].error.is_some());
        assert_eq!(sources[1].wsl_distro, "Ubuntu");
        assert!(sources[1].running);
        assert_eq!(sources[1].agents.len(), 1);
        assert!(sources[1].error.is_some());
    }

    #[tokio::test]
    async fn all_wsl_probes_only_running_distros_and_preserves_partial_errors() {
        let sources = collect_wsl_source_probes(
            vec!["Ubuntu".into(), "Debian".into(), "Fedora".into()],
            Ok(vec!["Ubuntu".into(), "Fedora".into()]),
            false,
            |distro| async move {
                assert_ne!(
                    distro, "Debian",
                    "stopped distro must never execute a probe"
                );
                if distro == "Fedora" {
                    (Vec::new(), Vec::new(), Some("probe timed out".into()))
                } else {
                    (
                        vec![AgentSourceProbeEntry {
                            id: "copilot",
                            display_name: "GitHub Copilot",
                        }],
                        Vec::new(),
                        None,
                    )
                }
            },
        )
        .await;
        let payload = serde_json::to_value(sources).unwrap();
        assert_eq!(payload[0]["wsl_distro"], "Debian");
        assert_eq!(payload[0]["running"], false);
        assert_eq!(payload[0]["agents"], serde_json::json!([]));
        assert!(payload[0].get("error").is_none());
        assert_eq!(payload[1]["error"], "probe timed out");
        assert_eq!(payload[2]["agents"][0]["id"], "copilot");
    }

    #[tokio::test]
    async fn failed_running_query_never_authorizes_a_distro_probe() {
        let sources = collect_wsl_source_probes(
            vec!["Ubuntu".into()],
            Err(anyhow::anyhow!("query failed")),
            false,
            |_| async { panic!("unknown running state must not start a distro") },
        )
        .await;
        assert!(!sources[0].running);
        assert!(sources[0].agents.is_empty());
        assert!(sources[0]
            .error
            .as_deref()
            .unwrap()
            .contains("query failed"));
    }

    #[tokio::test]
    async fn all_wsl_distribution_probe_concurrency_is_bounded() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let names: Vec<_> = (0..8).map(|i| format!("Distro{i}")).collect();
        let sources = collect_wsl_source_probes(names.clone(), Ok(names), false, |_| async {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);
            tokio::task::yield_now().await;
            active.fetch_sub(1, Ordering::SeqCst);
            (Vec::new(), Vec::new(), None)
        })
        .await;
        assert_eq!(sources.len(), 8);
        assert_eq!(peak.load(Ordering::SeqCst), 2);
    }

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

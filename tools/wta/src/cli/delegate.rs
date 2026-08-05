use anyhow::Result;
use std::sync::Arc;

use crate::agent_source::AgentSource;
use crate::shell::wt_channel::{CliChannel, WtChannel};
use crate::shell::ShellManager;

pub(crate) async fn run(
    prompt: Option<&str>,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    delegate_source: Option<&str>,
    delegate_wsl_distro: Option<&str>,
    cwd: Option<&str>,
) -> Result<()> {
    // Log the prompt length, not the text — the prompt is user content.
    // Log only the executable (first token) of agent_cmd, not the full
    // command line with args — a custom agent command can embed
    // tokens/credentials that shouldn't land in the log.
    let agent_exe = crate::coordinator::split_windows_commandline(agent_cmd.trim())
        .into_iter()
        .next()
        .unwrap_or_default();
    tracing::info!(
        prompt_chars = prompt.map(|p| p.chars().count()),
        agent = %agent_exe,
        "run_delegate started"
    );
    tracing::trace!(target: "delegate.content", prompt = ?prompt, "run_delegate prompt");

    let requested_source = parse_delegate_source(delegate_source, delegate_wsl_distro)?;
    require_delegate_agent_for_explicit_source(delegate_source, delegate_agent_cmd)?;

    let (debug_tx, _) = tokio::sync::mpsc::unbounded_channel::<crate::app::DebugMessage>();
    let channel = match CliChannel::connect()
        .await
        .map(|channel| channel.with_debug_sender(debug_tx))
    {
        Ok(ch) => {
            tracing::info!("WT protocol connected");
            ch
        }
        Err(e) => {
            tracing::warn!(error = %e, "WT protocol connection FAILED");
            return Err(e);
        }
    };
    let shell_mgr = ShellManager::new().with_wt_channel(Arc::new(channel) as Arc<dyn WtChannel>);

    match delegate_with_context(
        &shell_mgr,
        prompt,
        agent_cmd,
        delegate_agent_cmd,
        delegate_model,
        &requested_source,
        cwd,
    )
    .await
    {
        Ok(()) => {
            tracing::info!("delegate OK");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(error = %e, "delegate FAILED");
            Err(e)
        }
    }
}

/// Whether the delegate agent CLI is actually available inside `distro`.
///
/// Explicit WSL delegation launches the agent inside the selected distro
/// (`wsl -d <distro> -- bash -lc "<agent> …"`). Re-probe immediately before
/// launch because an agent discovered when settings were loaded may since
/// have been removed. The probe uses a **login** shell because common CLI
/// installs (npm-global, snap, `~/.local/bin`) only put the agent on the login
/// PATH; a non-login `bash -c` would miss them. Only a native Linux install is
/// accepted — a Windows CLI leaking in through `appendWindowsPath` and
/// resolving under `/mnt/…` is rejected (see
/// [`crate::agent_check::wsl_agent_probe_script`]).
/// This only gates an explicit `--delegate-source wsl` selection: an
/// unavailable agent keeps WSL as the source (see
/// [`delegate_launchable_for_source`]) so its command-not-found error
/// remains visible, rather than silently switching to the host.
async fn wsl_delegate_agent_available(distro: &str, agent_exe: &str) -> bool {
    crate::agent_check::find_wsl_exe(distro, agent_exe)
        .await
        .is_some()
}

/// Parse `--delegate-source`/`--delegate-wsl-distro` into a concrete
/// [`AgentSource`].
///
/// Omitting `--delegate-source` defaults to `Host` — WTA never inspects the
/// active pane's shell/distro to pick a source and never routes to WSL
/// unless the caller asks for it explicitly. `--delegate-wsl-distro` is only
/// meaningful (and required) alongside an explicit `--delegate-source wsl`.
fn parse_delegate_source(source: Option<&str>, wsl_distro: Option<&str>) -> Result<AgentSource> {
    match source.map(str::trim) {
        None => {
            anyhow::ensure!(
                wsl_distro.is_none(),
                "--delegate-wsl-distro requires --delegate-source wsl"
            );
            Ok(AgentSource::Host)
        }
        Some(source) if source.eq_ignore_ascii_case(AgentSource::HOST_KIND) => {
            anyhow::ensure!(
                wsl_distro.is_none(),
                "--delegate-wsl-distro is invalid with --delegate-source host"
            );
            Ok(AgentSource::Host)
        }
        Some(source) if source.eq_ignore_ascii_case(AgentSource::WSL_KIND) => {
            let distro = wsl_distro
                .map(str::trim)
                .filter(|distro| !distro.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!("--delegate-source wsl requires --delegate-wsl-distro")
                })?;
            Ok(AgentSource::Wsl {
                distro: distro.to_string(),
            })
        }
        Some(source) => anyhow::bail!("unsupported delegate source '{source}'"),
    }
}

/// An explicit `--delegate-source` (host or wsl) commits the caller to a
/// specific agent command, so a bare source with no delegate agent is a
/// caller error rather than something to paper over with the `agent_cmd`
/// fallback used when the source is omitted entirely.
fn require_delegate_agent_for_explicit_source(
    delegate_source: Option<&str>,
    delegate_agent_cmd: Option<&str>,
) -> Result<()> {
    anyhow::ensure!(
        delegate_source.is_none()
            || delegate_agent_cmd
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .is_some(),
        "--delegate-agent is required when --delegate-source is supplied"
    );
    Ok(())
}

/// Whether the delegate agent is launchable for the caller's requested
/// execution target. An explicit `--delegate-source` selects exactly one
/// target — `Host` is gated on the Windows PATH check, `Wsl` is gated on the
/// in-distro probe — and neither ever substitutes for the other (no more
/// `host_launchable || wsl_agent_available` auto-routing).
pub(crate) fn delegate_launchable_for_source(
    source: &AgentSource,
    host_launchable: bool,
    wsl_agent_available: bool,
) -> bool {
    match source {
        AgentSource::Host => host_launchable,
        AgentSource::Wsl { .. } => wsl_agent_available,
    }
}

/// Max bytes of captured terminal context baked into a delegate prompt.
const MAX_DELEGATE_CONTEXT_BYTES: usize = 12 * 1024;

/// Keep the most recent terminal output within the command-line size budget.
fn cap_delegate_context(context: &str, max_bytes: usize) -> String {
    if context.len() <= max_bytes {
        return context.to_string();
    }
    const TRUNCATION_MARKER: &str = "…(truncated)\n";
    let marker = if TRUNCATION_MARKER.len() <= max_bytes {
        TRUNCATION_MARKER
    } else {
        ""
    };
    let tail_bytes = max_bytes - marker.len();
    let mut start = context.len() - tail_bytes;
    while start < context.len() && !context.is_char_boundary(start) {
        start += 1;
    }
    format!("{marker}{}", &context[start..])
}

/// Selects the POSIX cwd to record/use for an explicit WSL delegate launch.
///
/// A WSL session's cwd must be a POSIX path (`/…`) — falling back without
/// validation to a Windows/UNC `--cwd` is misleading (the active pane may be
/// a Windows pane when `--delegate-source wsl` is forced) and breaks
/// downstream assumptions about WSL session cwd formatting. Trims whitespace
/// off each candidate, requires an absolute POSIX path (`/…`), and rejects
/// `"` (which would break the `wsl --cd "<cwd>"` quoting). Prefers the
/// active pane's cwd over the explicit CLI `--cwd`; returns `None` if
/// neither is a valid POSIX path. The same value feeds both the `wsl --cd`
/// argument and `super::sessions::register_launched`, so the recorded
/// session cwd always matches the shell's actual working directory — never
/// the raw Windows cwd fallback.
fn select_wsl_delegate_cwd<'a>(
    active_pane_cwd: Option<&'a str>,
    explicit_cwd: Option<&'a str>,
) -> Option<&'a str> {
    fn valid_posix_cwd(candidate: &str) -> Option<&str> {
        let trimmed = candidate.trim();
        (trimmed.starts_with('/') && !trimmed.contains('"')).then_some(trimmed)
    }

    active_pane_cwd
        .and_then(valid_posix_cwd)
        .or_else(|| explicit_cwd.and_then(valid_posix_cwd))
}

/// Shared delegation logic: enrich the prompt with the active pane's recent
/// output (when available), build the delegate-agent commandline, and create
/// a new tab to launch it. `requested_source` is always a concrete,
/// caller-chosen [`AgentSource`] — defaulting to `Host` when
/// `--delegate-source` is omitted (see `parse_delegate_source`). The active
/// pane is fetched only to enrich the prompt and to supply a WSL cwd
/// fallback; it never decides which target to launch into — WT's
/// GetActivePane already resolves the agent pane to the user's working
/// pane, so a single query is enough.
async fn delegate_with_context(
    shell_mgr: &ShellManager,
    prompt: Option<&str>,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    requested_source: &AgentSource,
    cwd: Option<&str>,
) -> Result<()> {
    let delegate_agents = crate::coordinator::default_delegate_agent_runtimes(
        delegate_agent_cmd,
        Some(agent_cmd),
        delegate_model,
    );
    let runtime = delegate_agents
        .first()
        .ok_or_else(|| anyhow::anyhow!("no delegate agent configured"))?;

    // A non-launchable command still gets a tab with the bare command so the
    // real shell error remains visible. It stays out of prompt enrichment,
    // which could otherwise make arbitrary pane output alter cmd.exe parsing.
    let launchable = crate::coordinator::delegate_command_launchable(&runtime.commandline);

    // Fetch the active pane up front — it feeds the enriched-prompt block and
    // the WSL cwd lookup further down, not source selection: `requested_source`
    // is the sole input to that decision (see `parse_delegate_source`), never
    // the active pane's shell/distro.
    let active = shell_mgr.wt_get_active_pane().await.ok();

    // `requested_source` is always a concrete, caller-chosen source (defaults
    // to `Host` when `--delegate-source` is omitted; see
    // `parse_delegate_source`). Probe WSL availability only for an explicit
    // WSL selection — an unavailable agent keeps WSL as the source (never
    // swaps to Host) so its command-not-found error stays visible.
    let candidate_wsl_distro = match requested_source {
        AgentSource::Host => None,
        AgentSource::Wsl { distro } => Some(distro.as_str()),
    };
    let wsl_agent_available = match candidate_wsl_distro {
        Some(distro) => {
            let agent_exe =
                crate::coordinator::split_windows_commandline(runtime.commandline.trim())
                    .into_iter()
                    .next()
                    .unwrap_or_default();
            let available = wsl_delegate_agent_available(distro, &agent_exe).await;
            if !available {
                tracing::warn!(
                    target: "delegate",
                    distro,
                    agent = %agent_exe,
                    "selected delegate agent is unavailable in its WSL distro; keeping WSL as the source",
                );
            }
            available
        }
        None => false,
    };

    let launchable_for_target =
        delegate_launchable_for_source(requested_source, launchable, wsl_agent_available);

    if !launchable_for_target {
        // Log only the executable: custom commands can contain credentials.
        let exe = crate::coordinator::split_windows_commandline(&runtime.commandline)
            .into_iter()
            .next()
            .unwrap_or_default();
        tracing::warn!(
            target: "delegate",
            agent = %exe,
            "delegate agent not launchable — opening its tab with the bare command so the real error stays visible",
        );
    }

    // Pin sessions for agents that support an explicit new-session ID. The
    // same registry lookup is used by command construction, keeping the flag
    // and born-bound registration in agreement.
    let pinned_session_id: Option<String> = if launchable_for_target {
        crate::agent_registry::lookup_profile_by_id(
            crate::agent_registry::resolve_agent_id_from_cmd(&runtime.commandline),
        )
        .new_session_id_flag
        .map(|_| uuid::Uuid::new_v4().to_string())
    } else {
        None
    };

    let enriched_prompt: Option<String> = match prompt {
        Some(prompt) if !prompt.trim().is_empty() && launchable_for_target => {
            let active_pane_id = active
                .as_ref()
                .and_then(|v| v.get("session_id"))
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });

            let pane_context = if let Some(ref pane_id) = active_pane_id {
                match shell_mgr.wt_read_pane_output(pane_id, Some(30)).await {
                    Ok(value) => value
                        .get("content")
                        .and_then(|c| c.as_str())
                        .map(str::to_string),
                    Err(_) => None,
                }
            } else {
                None
            };

            // The shared marker lets master exclude an echoed context heading
            // from session titles.
            Some(match (pane_context, active_pane_id) {
                (Some(context), Some(pane_id)) => format!(
                    "{}\n\n{}{})\n```\n{}\n```",
                    prompt,
                    crate::session_registry::TERMINAL_CONTEXT_TITLE_MARKER,
                    pane_id,
                    cap_delegate_context(&context, MAX_DELEGATE_CONTEXT_BYTES)
                ),
                _ => prompt.to_string(),
            })
        }
        _ => None,
    };

    let commandline = crate::coordinator::build_delegate_launch_commandline_with_session(
        runtime,
        enriched_prompt.as_deref(),
        pinned_session_id.as_deref(),
    )?;

    // ── WSL delegate path ────────────────────────────────────────────────
    // An explicit `--delegate-source wsl` selection always stays in its
    // configured distro. When the CLI is missing there, launch the selected
    // command there anyway, without a prompt, so the new tab reports the
    // real command-not-found error rather than silently switching to the
    // host.
    if let AgentSource::Wsl { distro } = requested_source {
        let wsl_agent_cmd = crate::coordinator::build_wsl_delegate_commandline(
            runtime,
            enriched_prompt.as_deref(),
            pinned_session_id.as_deref(),
        )?;
        let escaped = crate::coordinator::quote_windows_commandline_arg(&wsl_agent_cmd);
        let login_invocation = format!("bash -lc {}", escaped);
        let distro_arg = crate::coordinator::quote_windows_commandline_arg(distro);
        let active_pane_cwd = active
            .as_ref()
            .and_then(|pane| pane.get("cwd"))
            .and_then(|v| v.as_str());
        let wsl_cwd = select_wsl_delegate_cwd(active_pane_cwd, cwd);
        let wsl_commandline = match wsl_cwd {
            Some(cwd) => {
                format!("wsl -d {distro_arg} --cd \"{cwd}\" -- {login_invocation}")
            }
            None => format!("wsl -d {distro_arg} -- {login_invocation}"),
        };

        tracing::debug!("delegate_with_context: launching in WSL ({distro})");
        tracing::trace!(
            target: "delegate.content",
            commandline = %wsl_commandline,
            "wsl delegate commandline",
        );

        let create_resp = shell_mgr
            .wt_create_tab(Some(&wsl_commandline), None, None, None)
            .await?;
        let pane_guid = create_resp
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        tracing::info!(
            target: "delegate",
            pane_guid = ?pane_guid,
            pinned = ?pinned_session_id,
            distro,
            "delegate WSL tab created",
        );

        // Born-bound registration for the WSL delegate session — but only
        // when WSL sessions are enabled. The whole WSL surface is gated on
        // `WTA_WSL_SESSIONS`; with it off we must not surface any WSL
        // session, born-bound delegate rows included. The tab still opens
        // and the CLI still runs — it's just untracked, exactly like every
        // other WSL session while the flag is off.
        //
        // `wsl_cwd` (the same POSIX-validated value used for `wsl --cd`
        // above) is reused here rather than falling back to the raw
        // Windows `cwd`, which would be misleading for a WSL session.
        if crate::history_loader::wsl_sessions_enabled() {
            if let (Some(sid), Some(pane)) = (pinned_session_id.as_deref(), pane_guid.as_deref()) {
                super::sessions::register_launched(sid, pane, &runtime.id, wsl_cwd, Some(distro))
                    .await;
            }
        }
        return Ok(());
    }

    // ── Windows (existing) path ──────────────────────────────────────────
    tracing::debug!("delegate_with_context: launching");
    tracing::trace!(target: "delegate.content", commandline, "delegate_with_context commandline");

    let windows_home = std::env::var("USERPROFILE").ok();
    let sanitized_cwd =
        crate::coordinator::sanitize_windows_agent_cwd(cwd, windows_home.as_deref());

    let create_resp = shell_mgr
        .wt_create_tab(Some(&commandline), sanitized_cwd.as_deref(), None, None)
        .await?;
    let pane_guid = create_resp
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    tracing::info!(
        target: "delegate",
        pane_guid = ?pane_guid,
        pinned = ?pinned_session_id,
        "delegate tab created",
    );

    if let (Some(sid), Some(pane)) = (pinned_session_id.as_deref(), pane_guid.as_deref()) {
        super::sessions::register_launched(sid, pane, &runtime.id, cwd, None).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::cap_delegate_context;
    use crate::agent_source::AgentSource;

    #[test]
    fn cap_returns_short_context_unchanged() {
        let ctx = "small output";
        assert_eq!(cap_delegate_context(ctx, 1024), ctx);
    }

    #[test]
    fn cap_keeps_tail_and_marks_truncation() {
        let ctx: String = (0..5000u32)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let out = cap_delegate_context(&ctx, 1000);
        assert!(out.starts_with("…(truncated)\n"));
        assert!(out.ends_with(&ctx[ctx.len() - 100..]));
        assert!(out.len() <= 1000);
    }

    #[test]
    fn cap_is_char_boundary_safe() {
        let ctx: String = std::iter::repeat_n('⭐', 500).collect();
        let out = cap_delegate_context(&ctx, 100);
        assert!(out.len() <= 100);
        assert!(out.ends_with('⭐'));
        assert!(out
            .chars()
            .all(|c| c == '⭐' || "…(truncated)\n".contains(c)));
    }

    #[test]
    fn cap_omits_marker_when_limit_is_too_small() {
        assert_eq!(cap_delegate_context("prefix-tail", 4), "tail");
    }

    // ── parse_delegate_source ────────────────────────────────────────────

    #[test]
    fn parse_delegate_source_defaults_to_host_when_omitted() {
        let source = super::parse_delegate_source(None, None).expect("parses");
        assert_eq!(source, AgentSource::Host);
    }

    #[test]
    fn parse_delegate_source_rejects_distro_without_explicit_source() {
        let err = super::parse_delegate_source(None, Some("Ubuntu")).unwrap_err();
        assert!(err.to_string().contains("requires --delegate-source wsl"));
        assert!(super::parse_delegate_source(None, Some("")).is_err());
        assert!(super::parse_delegate_source(None, Some("   ")).is_err());
    }

    #[test]
    fn parse_delegate_source_accepts_host_case_insensitively() {
        let source = super::parse_delegate_source(Some("HOST"), None).expect("parses");
        assert_eq!(source, AgentSource::Host);
    }

    #[test]
    fn parse_delegate_source_rejects_distro_with_explicit_host() {
        let err = super::parse_delegate_source(Some("host"), Some("Ubuntu")).unwrap_err();
        assert!(err
            .to_string()
            .contains("invalid with --delegate-source host"));
        assert!(super::parse_delegate_source(Some("host"), Some("")).is_err());
        assert!(super::parse_delegate_source(Some("host"), Some("   ")).is_err());
    }

    #[test]
    fn parse_delegate_source_wsl_requires_nonempty_distro() {
        let missing = super::parse_delegate_source(Some("wsl"), None).unwrap_err();
        assert!(missing
            .to_string()
            .contains("requires --delegate-wsl-distro"));

        let empty = super::parse_delegate_source(Some("wsl"), Some("")).unwrap_err();
        assert!(empty.to_string().contains("requires --delegate-wsl-distro"));

        let whitespace = super::parse_delegate_source(Some("wsl"), Some("   ")).unwrap_err();
        assert!(whitespace
            .to_string()
            .contains("requires --delegate-wsl-distro"));
    }

    #[test]
    fn parse_delegate_source_wsl_trims_distro_name() {
        let source =
            super::parse_delegate_source(Some("wsl"), Some("  Ubuntu-22.04  ")).expect("parses");
        assert_eq!(
            source,
            AgentSource::Wsl {
                distro: "Ubuntu-22.04".to_string()
            }
        );
    }

    #[test]
    fn parse_delegate_source_rejects_unsupported_value() {
        let err = super::parse_delegate_source(Some("mac"), None).unwrap_err();
        assert!(err.to_string().contains("unsupported delegate source"));
    }

    // ── require_delegate_agent_for_explicit_source ──────────────────────

    #[test]
    fn require_delegate_agent_allows_omitted_source_without_agent() {
        assert!(super::require_delegate_agent_for_explicit_source(None, None).is_ok());
    }

    #[test]
    fn require_delegate_agent_rejects_explicit_source_without_agent() {
        assert!(super::require_delegate_agent_for_explicit_source(Some("host"), None).is_err());
        assert!(
            super::require_delegate_agent_for_explicit_source(Some("wsl"), Some("   ")).is_err()
        );
    }

    #[test]
    fn require_delegate_agent_accepts_explicit_source_with_agent() {
        assert!(
            super::require_delegate_agent_for_explicit_source(Some("host"), Some("codex")).is_ok()
        );
    }

    // ── delegate_launchable_for_source ──────────────────────────────────

    #[test]
    fn delegate_launchable_for_source_host_never_switches_to_wsl() {
        // An explicit Host selection is gated only on the host launchable
        // flag — a WSL-available agent must never make an explicit Host
        // request launchable.
        assert!(!super::delegate_launchable_for_source(
            &AgentSource::Host,
            false,
            true
        ));
        assert!(super::delegate_launchable_for_source(
            &AgentSource::Host,
            true,
            false
        ));
    }

    #[test]
    fn delegate_launchable_for_source_wsl_never_switches_to_host() {
        // An explicit WSL selection is gated only on the in-distro probe —
        // a host-launchable agent must never make an explicit WSL request
        // launchable when the distro lacks it.
        let wsl = AgentSource::Wsl {
            distro: "Ubuntu".to_string(),
        };
        assert!(!super::delegate_launchable_for_source(&wsl, true, false));
        assert!(super::delegate_launchable_for_source(&wsl, false, true));
    }

    // ── select_wsl_delegate_cwd ──────────────────────────────────────────

    #[test]
    fn select_wsl_delegate_cwd_prefers_active_pane_over_explicit_cwd() {
        assert_eq!(
            super::select_wsl_delegate_cwd(Some("/home/active"), Some("/home/explicit")),
            Some("/home/active")
        );
    }

    #[test]
    fn select_wsl_delegate_cwd_falls_back_to_explicit_cwd() {
        assert_eq!(
            super::select_wsl_delegate_cwd(None, Some("/home/explicit")),
            Some("/home/explicit")
        );
    }

    #[test]
    fn select_wsl_delegate_cwd_trims_whitespace() {
        assert_eq!(
            super::select_wsl_delegate_cwd(Some("  /home/active  "), None),
            Some("/home/active")
        );
    }

    #[test]
    fn select_wsl_delegate_cwd_rejects_non_posix_and_quoted_paths() {
        // Windows-style path is not a valid WSL cwd.
        assert_eq!(
            super::select_wsl_delegate_cwd(Some("C:\\Users\\me"), None),
            None
        );
        // A quote would break the `wsl --cd "<cwd>"` quoting.
        assert_eq!(
            super::select_wsl_delegate_cwd(Some("/home/\"me\""), None),
            None
        );
        // Falls through to a valid explicit cwd when the active pane's is
        // invalid.
        assert_eq!(
            super::select_wsl_delegate_cwd(Some("C:\\Users\\me"), Some("/home/explicit")),
            Some("/home/explicit")
        );
        // Neither candidate valid -> None (never the raw Windows fallback).
        assert_eq!(
            super::select_wsl_delegate_cwd(Some("C:\\Users\\me"), Some("D:\\other")),
            None
        );
    }
}

use anyhow::Result;
use std::sync::Arc;

use crate::shell::wt_channel::{CliChannel, WtChannel};
use crate::shell::ShellManager;

pub(crate) async fn run(
    prompt: Option<&str>,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
    cwd: Option<&str>,
) -> Result<()> {
    // Log the prompt length, not the text — the prompt is user content.
    tracing::info!(
        prompt_chars = prompt.map(|p| p.chars().count()),
        agent = agent_cmd,
        "run_delegate started"
    );
    tracing::trace!(target: "delegate.content", prompt = ?prompt, "run_delegate prompt");

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
/// The probe runs under a login shell because common CLI installs only put the
/// agent on that PATH. Windows executables leaking in through WSL interop are
/// rejected by the shared probe, so failures safely fall back to the host CLI.
async fn wsl_delegate_agent_available(distro: &str, agent_exe: &str) -> bool {
    crate::agent_check::find_wsl_exe(distro, agent_exe)
        .await
        .is_some()
}

/// Whether the delegate agent is launchable in either the host or WSL target.
pub(crate) fn delegate_launchable_for_target(
    host_launchable: bool,
    wsl_agent_available: bool,
) -> bool {
    host_launchable || wsl_agent_available
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

/// Enrich a delegate prompt with pane context and launch it in a new tab.
async fn delegate_with_context(
    shell_mgr: &ShellManager,
    prompt: Option<&str>,
    agent_cmd: &str,
    delegate_agent_cmd: Option<&str>,
    delegate_model: Option<&str>,
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

    // Fetch the active pane before deciding whether the target is the Windows
    // host or a WSL distro.
    let active = shell_mgr.wt_get_active_pane().await.ok();
    let wsl_distro: Option<String> =
        crate::agent_source::active_pane_wsl_distro(active.as_ref()).map(str::to_string);
    let wsl_agent_available = match wsl_distro.as_deref() {
        Some(distro) => {
            let agent_exe =
                crate::coordinator::split_windows_commandline(runtime.commandline.trim())
                    .into_iter()
                    .next()
                    .unwrap_or_default();
            let available = wsl_delegate_agent_available(distro, &agent_exe).await;
            if !available {
                tracing::info!(
                    target: "delegate",
                    distro,
                    agent = %agent_exe,
                    "delegate agent not available in WSL distro — falling back to Windows host CLI",
                );
            }
            available
        }
        None => false,
    };

    let launchable_for_target = delegate_launchable_for_target(launchable, wsl_agent_available);

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

    // Prefer an agent installed in the active WSL distro. The prompt is carried
    // as a base64 payload by the coordinator so it survives both Windows and
    // shell command-line parsing.
    if wsl_agent_available {
        if let (Some(distro), Some(active_pane)) = (wsl_distro.as_deref(), active.as_ref()) {
            let wsl_agent_cmd = crate::coordinator::build_wsl_delegate_commandline(
                runtime,
                enriched_prompt.as_deref(),
                pinned_session_id.as_deref(),
            )?;
            let escaped = crate::coordinator::quote_windows_commandline_arg(&wsl_agent_cmd);
            let login_invocation = format!("bash -lc {}", escaped);
            let distro_arg = crate::coordinator::quote_windows_commandline_arg(distro);
            let wsl_cwd = active_pane
                .get("cwd")
                .and_then(|v| v.as_str())
                .filter(|s| s.starts_with('/') && !s.contains('"'));
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

            // WSL session tracking is independently feature-gated; launching
            // the tab remains valid when tracking is disabled.
            if crate::history_loader::wsl_sessions_enabled() {
                if let (Some(sid), Some(pane)) =
                    (pinned_session_id.as_deref(), pane_guid.as_deref())
                {
                    super::sessions::register_launched(
                        sid,
                        pane,
                        &runtime.id,
                        wsl_cwd.or(cwd),
                        Some(distro),
                    )
                    .await;
                }
            }
            return Ok(());
        }
    }

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
}

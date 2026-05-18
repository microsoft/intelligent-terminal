//! Shared agent-process spawn logic for the ACP layer.
//!
//! Both [`super::client::run_inner`] and [`super::probe::probe_models`]
//! need to spawn an ACP agent the same way: parse the user-facing
//! cmdline, resolve bare names via [`crate::agent_registry`], optionally
//! wrap in `cmd /c`, scrub `CLAUDECODE`, and pipe stdio with
//! `kill_on_drop`. They diverge only after `spawn()` — the full client
//! wraps stdio with instrumentation and drives a prompt loop; the probe
//! attaches raw stdio, runs `initialize` + `new_session`, and exits.

use anyhow::{anyhow, Result};

pub(crate) struct AgentSpawn {
    pub child: tokio::process::Child,
    /// Original first token of `agent_cmd`, before path resolution.
    pub raw_program: String,
    /// Resolved program path (post `resolve_bare_agent_name`).
    pub resolved_program: String,
    /// True when the resolved program is an `npx` launcher. Callers
    /// stretch their initialize timeout when this is set — first npx
    /// run downloads the adapter package.
    pub is_npx: bool,
    /// For npx launches, the first `@`-prefixed arg (the adapter
    /// package id, e.g. `@zed-industries/claude-code-acp`).
    pub adapter_package: Option<String>,
}

impl AgentSpawn {
    /// Human-readable agent label for error messages. Prefers the npx
    /// adapter package id when present.
    pub fn label(&self) -> &str {
        self.adapter_package
            .as_deref()
            .unwrap_or(&self.raw_program)
    }
}

pub(crate) fn spawn_agent_process(agent_cmd: &str) -> Result<AgentSpawn> {
    let parts: Vec<&str> = agent_cmd.split_whitespace().collect();
    let raw_program = parts
        .first()
        .copied()
        .ok_or_else(|| anyhow!("empty agent command"))?;
    let args: Vec<&str> = parts[1..].to_vec();
    let resolved_program = crate::agent_registry::resolve_bare_agent_name(raw_program);
    let needs_cmd = crate::coordinator::needs_shell_launch(&resolved_program);

    let is_npx = resolved_program.eq_ignore_ascii_case("npx")
        || resolved_program.eq_ignore_ascii_case("npx.cmd")
        || resolved_program.eq_ignore_ascii_case("npx.exe");
    let adapter_package = if is_npx {
        args.iter()
            .find(|a| a.starts_with('@'))
            .map(|s| s.to_string())
    } else {
        None
    };

    let program = if needs_cmd { "cmd" } else { resolved_program.as_str() };
    let mut cmd = tokio::process::Command::new(program);
    if needs_cmd {
        cmd.arg("/c").arg(&resolved_program);
    }
    // claude-code-acp refuses to start when CLAUDECODE=1 is set — that
    // guard exists to block recursive `claude` shells from sharing
    // runtime, but doesn't apply to an ACP host. Scrub unconditionally;
    // other agents don't care.
    cmd.env_remove("CLAUDECODE");
    let child = cmd
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow!("failed to spawn agent '{}': {}", agent_cmd, e))?;

    Ok(AgentSpawn {
        child,
        raw_program: raw_program.to_string(),
        resolved_program,
        is_npx,
        adapter_package,
    })
}

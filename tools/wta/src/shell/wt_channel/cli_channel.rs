use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, bail, Context};
use tokio::sync::mpsc;

use super::WtChannel;
use crate::app::DebugMessage;

/// Extract a JSON value as a string, handling both String and Number types.
/// Protocol IDs may arrive as either strings or numbers depending on the caller.
fn json_id_as_str(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Resolve the full path to `wtcli.exe` at startup.
pub(crate) fn resolve_wtcli_path() -> String {
    // 1. Explicit override via environment variable.
    if let Ok(p) = std::env::var("WT_WTCLI_PATH") {
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        // 2. Sibling of current exe (installed scenario: wta.exe and wtcli.exe co-located).
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("wtcli.exe");
            if sibling.exists() {
                return sibling.to_string_lossy().to_string();
            }
        }

        // 3. Walk up from exe to repo root, check bin/x64/{Debug,Release}/wtcli/wtcli.exe (dev builds).
        //    The project output directory (wtcli/) contains the .winmd needed for MBM marshaling.
        let mut cursor = exe.parent().map(|p| p.to_path_buf());
        while let Some(dir) = cursor {
            for sub in &[
                "bin/x64/Debug/wtcli/wtcli.exe",
                "bin/x64/Release/wtcli/wtcli.exe",
            ] {
                let candidate = dir.join(sub);
                if candidate.exists() {
                    return candidate.to_string_lossy().to_string();
                }
            }
            let parent = dir.parent().map(|p| p.to_path_buf());
            if parent.as_deref() == Some(dir.as_path()) {
                break;
            }
            cursor = parent;
        }
    }

    // 4. Fall back to PATH search.
    "wtcli".to_string()
}

/// Classification of a `wtcli focus-pane` failure. Lets wta tell apart
/// "pane GUID is no longer in any window" (caller should demote the
/// stale row) from transient/infrastructure failures (caller should
/// leave the row alone).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusPaneFailureReason {
    /// Confirmed: the COM server iterated all windows/pages and no pane
    /// matched the supplied GUID. WT signals this via
    /// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)` (= 0x80070490). Safe to demote.
    NotFound,
    /// Generic non-zero exit (legacy `E_FAIL` from older WT builds, RPC
    /// failure, broken wtcli install, etc.). Caller should NOT demote on this
    /// because the pane may still be live — log only.
    Other {
        exit_code: Option<i32>,
        stderr: String,
    },
}

/// Run `wtcli focus-pane -t <id>` on a background thread and log stdout/stderr
/// on failure. Replaces `spawn_wtcli_async` for the focus-pane case so that
/// silent failures (wrong GUID, dead pane, COM error) leave a trace in
/// wta-main.log.
///
/// Thin wrapper over `spawn_wtcli_focus_pane_with_callback` for callers that
/// don't care about distinguishing failure modes.
pub fn spawn_wtcli_focus_pane(pane_session_id: &str) {
    spawn_wtcli_focus_pane_with_callback(pane_session_id, None);
}

/// Same as `spawn_wtcli_focus_pane` but invokes `on_failure` (on the worker
/// thread) when the spawned wtcli process exits non-zero. Used by
/// `dispatch_focus_pane` to demote stale-IDLE rows back to `Ended` when the
/// underlying pane is gone (`FocusPaneFailureReason::NotFound`).
#[allow(dead_code)]
pub fn spawn_wtcli_focus_pane_with_callback(
    pane_session_id: &str,
    on_failure: Option<Box<dyn FnOnce(FocusPaneFailureReason) + Send + 'static>>,
) {
    let path = resolve_wtcli_path();
    let pane = pane_session_id.to_string();
    std::thread::spawn(move || {
        let args = ["focus-pane".to_string(), "-t".to_string(), pane.clone()];
        let res = std::process::Command::new(&path)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();
        match res {
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                if out.status.success() {
                    tracing::info!(
                        target: "wtcli",
                        target_pane = %pane,
                        "focus-pane succeeded",
                    );
                } else {
                    let exit_code = out.status.code();
                    // wtcli prints `FocusPane failed: 0x80070490` for
                    // ERROR_NOT_FOUND. Match the literal HRESULT in stderr
                    // — this is the only signal we have from a void IDL
                    // method whose projection can't return a structured
                    // result.
                    let reason = if stderr.contains("0x80070490") {
                        FocusPaneFailureReason::NotFound
                    } else {
                        FocusPaneFailureReason::Other {
                            exit_code,
                            stderr: stderr.clone(),
                        }
                    };
                    tracing::warn!(
                        target: "wtcli",
                        target_pane = %pane,
                        code = exit_code,
                        stderr = %stderr,
                        reason = ?reason,
                        "focus-pane exited non-zero",
                    );
                    if let Some(cb) = on_failure {
                        cb(reason);
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    target: "wtcli",
                    target_pane = %pane,
                    %err,
                    "focus-pane spawn failed",
                );
                // Don't fire on_failure for spawn errors (wtcli not on PATH,
                // permission issues, etc.) — these are infrastructure
                // problems, not "pane gone".
            }
        }
    });
}

/// Fire-and-forget invocation of wtcli for one-shot UI actions
/// (focus-pane, split-pane). Errors are logged but not surfaced.
///
/// Redirects child stdout/stderr/stdin to null so wtcli's own status output
/// (e.g. "Created pane <id>") does not bleed into the parent TUI's screen
/// buffer underneath ratatui.
pub fn spawn_wtcli_async(args: &[String]) {
    let path = resolve_wtcli_path();
    match std::process::Command::new(&path)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_child) => {
            tracing::debug!(target: "wtcli", path = %path, ?args, "spawned");
        }
        Err(err) => {
            tracing::warn!(target: "wtcli", path = %path, ?args, %err, "spawn failed");
        }
    }
}

/// Run `wtcli --json <args>`, parse the resulting `sessionId` (or `SessionId`)
/// from stdout, then run `wtcli focus-pane -t <id>`. All performed on a
/// background thread so the UI stays responsive.
///
/// Why this exists: wtcli's split-pane subcommand passes `background=true` to
/// the COM `SplitPane` call (see `src/tools/wtcli/main.cpp:446`), which leaves
/// focus on the splitting pane. For interactive paths like resuming a history
/// session from the session management list, we want the new pane focused. Rather than
/// rebuild the C++ binary every dev cycle, we issue an explicit FocusPane
/// after the split returns the new pane's GUID.
///
/// The args slice is the subcommand + its options (e.g. `["split-pane", "-c",
/// "<commandline>"]`). `--json` is prepended automatically.
pub fn spawn_wtcli_split_then_focus(args: &[String]) {
    spawn_wtcli_split_then_focus_with_callback(args, None);
}

/// Variant of [`spawn_wtcli_split_then_focus`] that also delivers the new
/// pane's GUID to a caller-supplied callback after parsing it from stdout
/// (and before issuing the follow-up `focus-pane`). Used by `dispatch_resume`
/// so the agent session registry can bind the resumed pane to its row even
/// for CLIs without a hook bridge (Gemini): without this binding, the
/// `connection_state: closed` → `PaneClosed` path can't transition the row
/// to Ended when the user later closes the pane.
///
/// The callback runs on the same background thread that issued the split,
/// after a successful JSON parse. It is NOT invoked when the split fails
/// (process spawn error, non-zero exit, malformed JSON, missing
/// `session_id`).
pub fn spawn_wtcli_split_then_focus_with_callback(
    args: &[String],
    on_pane_id: Option<Box<dyn FnOnce(String) + Send + 'static>>,
) {
    let path = resolve_wtcli_path();
    let owned_args: Vec<String> = std::iter::once("--json".to_string())
        .chain(args.iter().cloned())
        .collect();

    std::thread::spawn(move || {
        let output = std::process::Command::new(&path)
            .args(&owned_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();

        let output = match output {
            Ok(o) => o,
            Err(err) => {
                tracing::warn!(
                    target: "wtcli",
                    path = %path,
                    ?owned_args,
                    %err,
                    "split-pane spawn failed",
                );
                return;
            }
        };

        if !output.status.success() {
            tracing::warn!(
                target: "wtcli",
                path = %path,
                ?owned_args,
                code = output.status.code(),
                "split-pane exited non-zero",
            );
            return;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value = match serde_json::from_str(stdout.trim()) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "wtcli",
                    %err,
                    stdout = %stdout,
                    "split-pane stdout was not valid JSON",
                );
                return;
            }
        };

        // CreationResultToJson emits `session_id` (snake_case — see
        // src/tools/wtcli/Formatting.cpp::CreationResultToJson). Older /
        // alternate camel-case spellings are kept as fallbacks for
        // forward-compat. Strip braces if the GUID arrived in `{...}`
        // form, since FocusPane resolves either form.
        let session_id = parsed
            .get("session_id")
            .or_else(|| parsed.get("SessionId"))
            .or_else(|| parsed.get("sessionId"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim_matches(|c| c == '{' || c == '}').to_string());

        let Some(session_id) = session_id else {
            tracing::warn!(
                target: "wtcli",
                json = %parsed,
                "split-pane JSON had no session_id field",
            );
            return;
        };

        tracing::info!(
            target: "wtcli",
            %session_id,
            "split-pane returned new pane GUID, issuing focus-pane",
        );

        if let Some(cb) = on_pane_id {
            cb(session_id.clone());
        }

        let focus_args = vec![
            "focus-pane".to_string(),
            "-t".to_string(),
            session_id.clone(),
        ];
        match std::process::Command::new(&path)
            .args(&focus_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                if out.status.success() {
                    tracing::info!(
                        target: "wtcli",
                        %session_id,
                        "split-then-focus completed",
                    );
                } else {
                    tracing::warn!(
                        target: "wtcli",
                        %session_id,
                        code = out.status.code(),
                        stderr = %stderr,
                        "focus-pane after split exited non-zero",
                    );
                }
            }
            Err(err) => {
                tracing::warn!(
                    target: "wtcli",
                    %session_id,
                    %err,
                    "focus-pane spawn failed after split",
                );
            }
        }
    });
}

/// Channel that invokes `wtcli.exe` for protocol operations.
pub struct CliChannel {
    available: AtomicBool,
    debug_tx: Option<mpsc::UnboundedSender<DebugMessage>>,
    event_tx: std::sync::Mutex<Option<mpsc::UnboundedSender<serde_json::Value>>>,
    wtcli_path: String,
}

impl CliChannel {
    pub async fn connect() -> anyhow::Result<Self> {
        // WT_COM_CLSID must be set — wtcli reads it from the environment.
        if std::env::var("WT_COM_CLSID").is_err() {
            bail!("WT_COM_CLSID not set. Must run inside a Windows Terminal pane.");
        }

        Ok(Self {
            available: AtomicBool::new(true),
            debug_tx: None,
            event_tx: std::sync::Mutex::new(None),
            wtcli_path: resolve_wtcli_path(),
        })
    }

    pub fn with_debug_sender(mut self, tx: mpsc::UnboundedSender<DebugMessage>) -> Self {
        self.debug_tx = Some(tx);
        self
    }

    pub fn subscribe_events(&self) -> mpsc::UnboundedReceiver<serde_json::Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.event_tx.lock().unwrap() = Some(tx);
        rx
    }

    /// Start background event listener (wraps `wtcli listen --json`).
    /// wtcli inherits WT_COM_CLSID from this process's env.
    ///
    /// If the `wtcli listen` child exits unexpectedly (silent COM blip,
    /// transient RPC error, WT restart), we retry **once**. If the retry
    /// also exits, we give up — at that point something is fundamentally
    /// broken and a tight respawn loop would just spam logs.
    ///
    /// Session-management freshness does not depend on this path: the
    /// 30 s master disk rescan in `serve_master` covers new on-disk
    /// sessions even when the listener is dead. Autofix and other push
    /// events do go dark after a permanent failure, which is the
    /// accepted tradeoff for keeping the recovery logic trivial.
    pub async fn start_reader(self: &std::sync::Arc<Self>) {
        const MAX_ATTEMPTS: u32 = 2;
        let wtcli = self.wtcli_path.clone();
        let weak = std::sync::Arc::downgrade(self);
        tokio::spawn(async move {
            for attempt in 1..=MAX_ATTEMPTS {
                if weak.upgrade().is_none() {
                    return;
                }
                run_wtcli_listen_once(&wtcli, &weak, attempt).await;
            }
            tracing::error!(
                target: "wt_channel",
                wtcli = %wtcli,
                attempts = MAX_ATTEMPTS,
                "wtcli listen exited after retry; giving up. \
                 Push events (autofix, agent_event hooks) are now dark for the lifetime of this helper. \
                 Session management still recovers via master's 30s disk rescan."
            );
        });
    }

    /// Run a wtcli subcommand and return the parsed JSON output.
    /// wtcli inherits WT_COM_CLSID from this process's env.
    async fn run_wtcli(&self, args: &[&str]) -> anyhow::Result<serde_json::Value> {
        let mut cmd = tokio::process::Command::new(&self.wtcli_path);
        cmd.arg("--json").args(args);

        let output = cmd.output().await.context("Failed to run wtcli")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("wtcli failed: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(serde_json::Value::Null);
        }
        let val: serde_json::Value =
            serde_json::from_str(trimmed).context("Failed to parse wtcli JSON output")?;
        Ok(val)
    }
}

/// Spawn `wtcli --json listen` once, forward each parsed JSON line to the
/// channel's `event_tx`, and return when the child exits (clean EOF or
/// pipe error). Returns control to the caller so it can decide whether to
/// retry. Lifecycle (start / exit reason / events seen) is logged.
async fn run_wtcli_listen_once(
    wtcli: &str,
    weak: &std::sync::Weak<CliChannel>,
    attempt: u32,
) {
    use tokio::io::AsyncBufReadExt;

    let mut child = match tokio::process::Command::new(wtcli)
        .args(["--json", "listen"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // CliChannel-dropped-mid-read → Child dropped without explicit
        // kill → kill_on_drop reaps wtcli instead of orphaning it.
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => {
            tracing::info!(target: "wt_channel", wtcli, attempt, "wtcli listen spawned");
            c
        }
        Err(err) => {
            tracing::warn!(target: "wt_channel", wtcli, attempt, error = %err, "wtcli listen spawn failed");
            return;
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            tracing::warn!(target: "wt_channel", attempt, "wtcli listen child missing stdout pipe");
            let _ = child.start_kill();
            let _ = child.wait().await;
            return;
        }
    };

    let mut reader = tokio::io::BufReader::new(stdout);
    let mut line = String::new();
    let mut events_seen: u64 = 0;
    let started = std::time::Instant::now();

    let exit_reason = loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break "stdout EOF",
            Ok(_) => {
                let Some(this) = weak.upgrade() else {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return;
                };
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(line.trim()) {
                    events_seen += 1;
                    let tx = this.event_tx.lock().unwrap();
                    if let Some(tx) = tx.as_ref() {
                        let _ = tx.send(val);
                    }
                }
            }
            Err(_) => break "stdout read error",
        }
    };

    // Pipe error doesn't imply the child has exited yet — kill before
    // wait so we don't block on a child that's otherwise healthy.
    // Harmless on the "stdout EOF" path.
    let _ = child.start_kill();
    let exit_status = child.wait().await.ok();

    tracing::warn!(
        target: "wt_channel",
        attempt,
        events_seen,
        exit_reason,
        exit_status = ?exit_status,
        lived_ms = started.elapsed().as_millis() as u64,
        "wtcli listen exited"
    );
}

#[async_trait::async_trait]
impl WtChannel for CliChannel {
    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        // Map protocol method names to wtcli subcommands + args.
        match method {
            "list_windows" => self.run_wtcli(&["list-windows"]).await,
            "list_tabs" => {
                let mut args = vec!["list-tabs"];
                let wid = params
                    .get("window_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                if !wid.is_empty() {
                    args.extend(["-w", &wid]);
                }
                self.run_wtcli(&args).await
            }
            "list_panes" => {
                let mut args = vec!["list-panes"];
                let wid = params
                    .get("window_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                let tid = params
                    .get("tab_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                if !wid.is_empty() {
                    args.extend(["-w", &wid]);
                }
                if !tid.is_empty() {
                    args.extend(["-t", &tid]);
                }
                self.run_wtcli(&args).await
            }
            "get_active_pane" => self.run_wtcli(&["active-pane"]).await,
            "read_pane_output" => {
                let pane_id = params
                    .get("session_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                let max_lines = params
                    .get("max_lines")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(200);
                let source = params.get("source").and_then(|v| v.as_str()).unwrap_or("");
                let lines_owned = max_lines.to_string();
                let mut args = vec!["capture-pane"];
                if !pane_id.is_empty() {
                    args.extend(["-t", &pane_id]);
                }
                if source == "last_prompt" {
                    args.push("--last-prompt");
                } else {
                    args.extend(["-l", &lines_owned]);
                }
                self.run_wtcli(&args).await
            }
            "get_process_status" => {
                let pane_id = params
                    .get("session_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                let mut args = vec!["pane-status"];
                if !pane_id.is_empty() {
                    args.extend(["-t", &pane_id]);
                }
                self.run_wtcli(&args).await
            }
            "create_tab" => {
                let mut args = vec!["new-tab"];
                let cmd = params
                    .get("commandline")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let title = params.get("title").and_then(|v| v.as_str()).unwrap_or("");
                let cwd = params.get("cwd").and_then(|v| v.as_str()).unwrap_or("");
                let cmd_owned;
                let title_owned;
                let cwd_owned;
                if !cmd.is_empty() {
                    cmd_owned = cmd.to_string();
                    args.extend(["-c", &cmd_owned]);
                }
                if !title.is_empty() {
                    title_owned = title.to_string();
                    args.extend(["-n", &title_owned]);
                }
                if !cwd.is_empty() {
                    cwd_owned = cwd.to_string();
                    args.extend(["-d", &cwd_owned]);
                }
                self.run_wtcli(&args).await
            }
            "split_pane" => {
                let pane_id = params
                    .get("session_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                let cmd = params
                    .get("commandline")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let dir = params
                    .get("direction")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let cmd_owned;
                let dir_owned;
                let mut args = vec!["split-pane"];
                if !pane_id.is_empty() {
                    args.extend(["-t", &pane_id]);
                }
                // Pass the direction string straight through to wtcli, which
                // forwards it verbatim to the COM SplitPane call. wtcli accepts
                // "right" | "left" | "up" | "down" | "auto"|"automatic", and the
                // COM server also tolerates the legacy "horizontal"/"vertical".
                if !dir.is_empty() {
                    dir_owned = dir.to_string();
                    args.extend(["-d", &dir_owned]);
                }
                if !cmd.is_empty() {
                    cmd_owned = cmd.to_string();
                    args.extend(["-c", &cmd_owned]);
                }
                self.run_wtcli(&args).await
            }
            "close_pane" => {
                let pane_id = params
                    .get("session_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                self.run_wtcli(&["kill-pane", "-t", &pane_id]).await
            }
            "focus_pane" => {
                let pane_id = params
                    .get("session_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                self.run_wtcli(&["focus-pane", "-t", &pane_id]).await
            }
            "send_input" => {
                let pane_id = params
                    .get("session_id")
                    .and_then(json_id_as_str)
                    .unwrap_or_default();
                // Strict validation: caller errors must surface as errors,
                // not silently degrade to a no-op SendInput("") that wta
                // then reports as success. The downstream COM layer treats
                // empty text as a deliberate no-op, so a malformed request
                // that fell through to "" here would otherwise look
                // identical to a successful send and be very hard to
                // diagnose.
                let text = params
                    .get("text")
                    .ok_or_else(|| anyhow!("send_input: missing 'text' parameter"))?
                    .as_str()
                    .ok_or_else(|| anyhow!("send_input: 'text' must be a JSON string"))?;
                // Empty text is an explicit no-op — short-circuit before
                // spawning wtcli; the downstream COM SendInput would treat
                // it the same way. Mirror the response shape that
                // `wtcli send-keys --json` produces (`ok` + `session_id`)
                // so callers see a consistent schema regardless of whether
                // the spawn happened. `noop: true` is an extra hint.
                if text.is_empty() {
                    return Ok(serde_json::json!({
                        "ok": true,
                        "session_id": pane_id,
                        "noop": true,
                    }));
                }
                let text_owned = text.to_string();
                // `--raw` bypasses wtcli's tmux-style token translation so a
                // payload that literally equals "Enter" / "Tab" / "C-c" is
                // sent verbatim instead of being rewritten to CR / TAB /
                // Ctrl+C. wta forwards agent-supplied text — token semantics
                // belong on the human-facing `wtcli send-keys` path, not on
                // this routed transport.
                let mut args = vec!["send-keys", "--raw"];
                if !pane_id.is_empty() {
                    args.extend(["-t", &pane_id]);
                }
                // `--` stops CLI11 option parsing so a text payload starting
                // with `-`/`--` (e.g. `--release`, `--help`) is treated as
                // positional key data instead of being interpreted as a
                // wtcli flag.
                args.push("--");
                args.push(&text_owned);
                self.run_wtcli(&args).await
            }
            "get_capabilities" => self.run_wtcli(&["info"]).await,
            other => bail!("Unsupported method: {}", other),
        }
    }

    fn is_available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }
}

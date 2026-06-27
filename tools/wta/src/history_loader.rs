// tools/wta/src/history_loader.rs
//
// Discover historical CLI agent sessions by scanning each CLI's on-disk
// log/state layout. Used to seed the AgentSessionRegistry with `Historical`
// entries on App startup so users can resume past sessions from session management view.
//
// Layouts (verified 2026-05):
//   Copilot:  ~/.copilot/session-state/<UUID>/{workspace.yaml,events.jsonl}
//             - session id   = directory name
//             - cwd          = workspace.yaml `cwd:` field
//             - title        = workspace.yaml `name:` (legacy fallback `summary:`)
//             - last_activity= events.jsonl mtime (fallback workspace.yaml mtime)
//             - in-use marker= inuse.<PID>.lock files (skip those)
//
//   Claude:   ~/.claude/projects/<encoded-cwd>/<UUID>.jsonl
//             - session id   = filename stem
//             - cwd          = decode parent directory name (drive-dash format)
//             - title        = first user message in jsonl (best-effort)
//             - last_activity= file mtime
//             - skip "memory" project + */subagents/*.jsonl
//             - skip "phantom" sessions whose jsonl contains only meta
//               records (permission-mode, file-history-snapshot, isMeta
//               caveats, `<command-...>` / `<local-command-...>` slash
//               echoes) — `claude --resume <id>` rejects these with
//               `No conversation found with session ID: <id>`.
//
//   Gemini:   ~/.gemini/tmp/<project-slug>/chats/session-*.jsonl
//             - session id   = first JSONL line `sessionId` field
//             - cwd          = ~/.gemini/projects.json reverse lookup
//             - title        = first JSONL line whose `type:"user"` carries
//                              a content[0].text (best-effort)
//             - last_activity= file mtime
//             - skip "phantom" sessions whose jsonl contains only the
//               session-header line(s) (no record carrying a `type`
//               field). Opening `gemini` and exiting without
//               exchanging a turn leaves these on disk — Enter on
//               the row would launch `gemini --resume <id>` and
//               dead-end on a similar "no session" rejection.
//
//   Codex:    ~/.codex/sessions/YYYY/MM/DD/rollout-<iso-ts>-<UUID>.jsonl
//             - session id   = first JSONL line `session_meta` payload.id
//             - cwd          = `session_meta` payload.cwd
//             - title        = first `event_msg` payload.user_message,
//                              else first `response_item` role=user content
//                              (skipping codex's synthetic injections —
//                              `<environment_context>` & friends plus the
//                              `# AGENTS.md instructions for <dir>` block;
//                              see `codex_user_text_is_synthetic`)
//             - last_activity= `session_meta` payload.timestamp (fallback file mtime)
//             - skip "phantom" sessions whose jsonl contains only the
//               `session_meta` header and/or synthetic injected
//               response_items (`<environment_context>`, AGENTS.md docs, …;
//               see `codex_user_text_is_synthetic`) with no real user
//               turn. `codex resume <id>` would reject these as having
//               no conversation to resume.
//
// (Note: per-subagent JSONL files may live in nested `<UUID>/` subdirs of
// `chats/`. Top-level Gemini sessions are flat files named `session-*.jsonl`.
// under `<UUID>/<name>.json`. We only pick up `session-*.json` at the
// top level.)
//
// Sort each list by last_activity desc; cap each CLI at MAX_PER_CLI.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::agent_sessions::{AgentSession, AgentStatus, CliSource};

/// Per-CLI discovery-phase acquisition cap: at most this many newest
/// candidates survive `select_top_candidates` into the expensive content
/// parse. It bounds phase-2 IO, so it is a pre-filter threshold — not a
/// guaranteed post-filter row count. See `select_top_candidates`.
pub(crate) const MAX_PER_CLI: usize = 50;
const TITLE_TAIL_BYTES: u64 = 64 * 1024;

/// Upper bound on bytes read by the `*_has_real_content` classifiers
/// when streaming a JSONL line-by-line. Picked at 8 MB so a session
/// whose early meta records (e.g. Claude's `file-history-snapshot`
/// for a large project) push past `TITLE_TAIL_BYTES` still gets
/// scanned far enough to find the first real user/assistant record.
/// The classifiers short-circuit on first hit, so this cap only
/// matters for genuine phantoms (which are tiny by nature) and the
/// pathological "JSONL contains only meta records up to the cap"
/// case (treated as phantom — conservative but safe).
const CLASSIFY_SCAN_BYTES_CAP: u64 = 8 * 1024 * 1024;

/// Whether to scan WSL distros for historical sessions. Single
/// choke point + env kill-switch (`WTA_WSL_SESSIONS=0|false|no|off`).
/// Defaults to **enabled**. A future `wslSessions` setting will pass a
/// flag that overrides this same function — no other call site changes.
pub(crate) fn wsl_sessions_enabled() -> bool {
    // Trim + case-fold so the kill-switch is forgiving in scripts / CI
    // (` False `, `NO`, `off`, …), mirroring the env-bool parsing in
    // `resolve_sessions_origin_filter`.
    match std::env::var("WTA_WSL_SESSIONS") {
        Ok(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

/// Run the per-CLI scanners against a specific `home` (the real user
/// profile for host rows, or a temp `$HOME`-mirror extracted from a WSL
/// distro), restricted to a single CLI when `cli_filter` is `Some(known)`
/// (see [`cli_scan_flags`]). Caps each CLI at [`MAX_PER_CLI`]. Used by the
/// WSL scan (`crate::wsl`) to reuse the host parsers verbatim over an
/// extracted distro `$HOME`; the host path goes through [`load_for_cli`].
///
/// Filtering here — *before* the parse — means a CLI the view isn't showing
/// is never parsed, so the WSL scan doesn't pay to read + parse the other
/// three CLIs' transcripts out of the extracted mirror just to discard them.
pub(crate) fn load_all_in(home: &Path, cli_filter: Option<&CliSource>) -> Vec<AgentSession> {
    // The WSL temp `$HOME` has no agent-pane (Class A) sessions, so pass an
    // empty index to the production `_indexed` loaders (the bare wrappers are
    // test-only). Reuses the host parsers verbatim over the extracted distro
    // home.
    let empty = HashSet::new();
    let (cop, cla, gem, cod) = cli_scan_flags(cli_filter);
    let mut out = Vec::new();
    if cop {
        out.extend(take_n(load_copilot_indexed(home, &empty), MAX_PER_CLI));
    }
    if cla {
        out.extend(take_n(load_claude_indexed(home, &empty), MAX_PER_CLI));
    }
    if gem {
        out.extend(take_n(load_gemini_indexed(home, &empty), MAX_PER_CLI));
    }
    if cod {
        out.extend(take_n(load_codex_indexed(home, &empty), MAX_PER_CLI));
    }
    out
}

/// Cap the discovery-phase first-line read (`read_first_line`) so a corrupt
/// / non-JSONL transcript that is one giant line with no newline can't pull
/// an unbounded amount into memory during the *cheap* phase. A real header
/// line is a single small JSON object; a read that hits this cap without a
/// newline yields truncated text that fails the downstream JSON parse, so
/// the candidate is skipped (treated as unparseable).
const HEADER_LINE_BYTES_CAP: u64 = 64 * 1024;

/// Decide which per-CLI loaders to run for a given filter.
///
/// The session management view only ever shows the current agent's CLI, so
/// callers that know their CLI pass it to avoid scanning (and parsing) the
/// other three CLIs' transcripts. `None` — or a custom / unrecognized agent
/// (`CliSource::Unknown`) — scans everything, matching the view, which shows
/// all CLIs when `current_cli_filter()` is `None`.
fn cli_scan_flags(cli_filter: Option<&CliSource>) -> (bool, bool, bool, bool) {
    match cli_filter {
        Some(CliSource::Copilot) => (true, false, false, false),
        Some(CliSource::Claude) => (false, true, false, false),
        Some(CliSource::Gemini) => (false, false, true, false),
        Some(CliSource::Codex) => (false, false, false, true),
        None | Some(CliSource::Unknown(_)) => (true, true, true, true),
    }
}

/// Scan on-disk session history, restricted to a single CLI when
/// `cli_filter` is `Some(known)`. See [`cli_scan_flags`] for the dispatch
/// rules (custom / unknown agents scan all four).
pub fn load_for_cli(cli_filter: Option<&CliSource>) -> Vec<AgentSession> {
    let scan_started = std::time::Instant::now();
    let mut out = Vec::new();

    // WSL historical sessions (in-distro, **running** distros only — fast,
    // no VM boot). Scanned before the host-home early return because they
    // live in the distro's own `$HOME`, independent of the Windows profile.
    // Stopped (non-running) distros are intentionally skipped: reading one
    // boots its WSL VM, which is too costly to do just to build a list.
    if wsl_sessions_enabled() {
        // Filtering happens inside the parse (`load_all_in` via
        // `cli_scan_flags`), so an unselected CLI's transcripts are never
        // parsed out of the extracted mirror — no post-hoc retain needed.
        out.extend(crate::wsl::scan_running_distros(cli_filter));
    }

    let Some(home) = home_dir() else { return out };

    // Load the agent-pane (Class A) index once. Each per-CLI loader uses it
    // to skip WTA-created agent-pane sessions in its cheap discovery phase,
    // *before* paying for any content read — these are hidden from the
    // session picker (MVP `OriginFilter::ShellOnly`) and their live variants
    // are tracked via `new_session`, not this disk scan, so dropping the
    // historical ones here is safe. This is what keeps the expensive parse
    // off Gemini's many seeded-prompt agent-pane phantoms.
    let agent_pane_index = crate::agent_pane_origin::load_default_set();

    // Each loader already caps at MAX_PER_CLI; take_n is a defensive no-op.
    let (cop, cla, gem, cod) = cli_scan_flags(cli_filter);
    if cop {
        out.extend(take_n(load_copilot_indexed(&home, &agent_pane_index), MAX_PER_CLI));
    }
    if cla {
        out.extend(take_n(load_claude_indexed(&home, &agent_pane_index), MAX_PER_CLI));
    }
    if gem {
        out.extend(take_n(load_gemini_indexed(&home, &agent_pane_index), MAX_PER_CLI));
    }
    if cod {
        out.extend(take_n(load_codex_indexed(&home, &agent_pane_index), MAX_PER_CLI));
    }

    // Single low-overhead timing line for this scan. Kept at debug: the
    // master startup caller already emits an info-level scan-complete with
    // `elapsed_ms`, so info here would just duplicate that in release builds.
    tracing::debug!(
        target: "history_loader",
        cli = ?cli_filter,
        total_ms = scan_started.elapsed().as_secs_f64() * 1000.0,
        rows = out.len(),
        "history scan complete"
    );
    out
}

/// Best-effort title lookup for a single live session. Reads the same
/// per-CLI on-disk artefacts that `load_all` scans, but only for the
/// specific `key`. Used to upgrade synthetic titles (cwd basename) into
/// real ones (workspace.yaml name / first user prompt) once the CLI
/// has had a chance to write that data — typically a few seconds after
/// the first hook event arrives. Returns `None` if no usable title is
/// on disk (caller keeps whatever synthetic title it had).
pub fn lookup_title_for_session(cli: CliSource, key: &str) -> Option<String> {
    let home = home_dir()?;
    lookup_title_for_session_in(&home, cli, key)
}

/// Testable variant of [`lookup_title_for_session`] that accepts a
/// caller-supplied `home` directory. Production code uses the
/// `USERPROFILE` / `HOME` env var via `home_dir()`; tests pin a tmp
/// dir without racing on env mutation. Returns `None` for CLIs whose
/// titles aren't sourced from on-disk artefacts (`Unknown`).
pub fn lookup_title_for_session_in(
    home: &Path,
    cli: CliSource,
    key: &str,
) -> Option<String> {
    match cli {
        CliSource::Copilot => copilot_title_for_key(home, key),
        CliSource::Claude  => claude_title_for_key(home, key),
        CliSource::Gemini  => gemini_title_for_key(home, key),
        CliSource::Codex   => codex_title_for_key(home, key),
        CliSource::Unknown(_) => None,
    }
}

fn copilot_title_for_key(home: &Path, key: &str) -> Option<String> {
    let dir = home.join(".copilot").join("session-state").join(key);
    let workspace = dir.join("workspace.yaml");
    let yaml = fs::read_to_string(&workspace).ok()?;
    // Copilot writes the session title to `name`. `summary` is a removed
    // legacy field kept only as a fallback for very old sessions that may
    // still carry it; current workspace.yaml files have only `name`.
    parse_simple_yaml(&yaml, "name").filter(|s| !s.is_empty())
        .or_else(|| parse_simple_yaml(&yaml, "summary").filter(|s| !s.is_empty()))
}

fn claude_title_for_key(home: &Path, key: &str) -> Option<String> {
    claude_jsonl_path_for_key(home, key)
        .and_then(|p| first_user_text_jsonl(&p, ClaudeOrGemini::Claude))
}

/// Locate the on-disk Claude JSONL for `key` by scanning every
/// `~/.claude/projects/<encoded-cwd>/` directory for a `<key>.jsonl`
/// file. Returns `None` when no matching file exists.
pub(crate) fn claude_jsonl_path_for_key(home: &Path, key: &str) -> Option<PathBuf> {
    let projects = home.join(".claude").join("projects");
    let rd = fs::read_dir(&projects).ok()?;
    for proj in rd.flatten() {
        if !proj.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
        let candidate = proj.path().join(format!("{}.jsonl", key));
        if candidate.is_file() { return Some(candidate); }
    }
    None
}

fn gemini_title_for_key(home: &Path, key: &str) -> Option<String> {
    gemini_jsonl_path_for_key(home, key)
        .and_then(|p| first_user_text_jsonl(&p, ClaudeOrGemini::Gemini))
}

/// Locate the on-disk Gemini JSONL whose first-line `sessionId` matches
/// `key`. Scans every `~/.gemini/tmp/<slug>/chats/session-*.jsonl` until
/// it finds one — Gemini doesn't expose the session id in the filename,
/// so per-key lookup is O(n) over the chat directory. Returns `None`
/// when no matching file exists.
pub(crate) fn gemini_jsonl_path_for_key(home: &Path, key: &str) -> Option<PathBuf> {
    let tmp = home.join(".gemini").join("tmp");
    let rd = fs::read_dir(&tmp).ok()?;
    for proj in rd.flatten() {
        if !proj.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
        let chats = proj.path().join("chats");
        let Ok(files) = fs::read_dir(&chats) else { continue; };
        for f in files.flatten() {
            let p = f.path();
            if !is_gemini_session_file(&p) { continue; }
            let (sid, _) = parse_gemini_meta(&p);
            if sid.as_deref() == Some(key) { return Some(p); }
        }
    }
    None
}

fn take_n(mut v: Vec<AgentSession>, n: usize) -> Vec<AgentSession> {
    v.truncate(n);
    v
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

// ─── Per-CLI resumability probes ────────────────────────────────────────

/// Dispatch [`agent_key_is_resumable_on_disk_in`] against the real user
/// home. Returns `true` (conservative — allow the resume to proceed) if
/// `$USERPROFILE`/`$HOME` is unavailable, so the absence of a home
/// directory doesn't silently block all resume attempts.
///
/// The per-CLI semantics are:
///
///   * **Claude**:  the JSONL must contain at least one non-meta,
///                  non-slash-command user record OR any assistant
///                  record. `claude --resume <id>` rejects sessions
///                  without those with
///                  `No conversation found with session ID: <id>`.
///
///   * **Copilot**: `~/.copilot/session-state/<id>/events.jsonl` must
///                  exist and be non-empty. `copilot --resume=<id>`
///                  rejects sessions without events with
///                  `Error: No session, task, or name matched '<id>'`.
///
///   * **Gemini**:  the JSONL must contain at least one record beyond
///                  the session-header line (i.e. the user actually
///                  exchanged a turn — header-only sessions are the
///                  result of opening `gemini` and immediately exiting).
///
///   * Anything else (unknown CLI / synthetic pane-keyed sessions):
///                  resumability is undefined; return `true` so the
///                  pre-launch guard never blocks them.
///
/// Always returns `true` for keys that don't have any on-disk artefact
/// — those may be in-flight (flush not yet landed) or live in some
/// other home, and we let the CLI itself validate.
pub fn key_is_resumable_on_disk(cli: &crate::agent_sessions::CliSource, key: &str) -> bool {
    match home_dir() {
        Some(h) => key_is_resumable_on_disk_in(&h, cli, key),
        None    => true,
    }
}

/// Testable variant: dispatches against a caller-supplied home so unit
/// tests can pin a tmp dir without racing on `USERPROFILE` mutation.
pub(crate) fn key_is_resumable_on_disk_in(
    home: &Path,
    cli: &crate::agent_sessions::CliSource,
    key: &str,
) -> bool {
    use crate::agent_sessions::CliSource;
    match cli {
        CliSource::Claude  => claude_key_is_resumable_on_disk_in(home, key),
        CliSource::Codex   => codex_key_is_resumable_on_disk_in(home, key),
        CliSource::Copilot => copilot_key_is_resumable_on_disk_in(home, key),
        CliSource::Gemini  => gemini_key_is_resumable_on_disk_in(home, key),
        CliSource::Unknown(_) => true,
    }
}

/// **Strict** variant of [`key_is_resumable_on_disk`]: treats a
/// missing on-disk artefact as definite evidence of a phantom
/// session. Use this in flows where the row is *already in wta's
/// live registry* (so we know the session really existed in this
/// process), e.g. the prune that fires after `SessionStopped` /
/// `PaneClosed`.
///
/// Example: the user opens `claude` via the agent pane (ACP-launched),
/// exchanges zero turns, then closes the pane. Claude never wrote a
/// JSONL under `~/.claude/projects/...` for that session id (it
/// flushes only when there's something to flush), so a follow-up
/// `claude --resume <id>` would fail with
/// `No conversation found with session ID: <id>`. The lenient
/// [`key_is_resumable_on_disk`] would defer to Claude here (and
/// leave the row stuck), but the row's lifecycle is fully observed
/// in-process — the absence of any JSONL is conclusive, so strict
/// returns `false` and the prune drops the row immediately.
pub fn key_has_definite_resumable_content(
    cli: &crate::agent_sessions::CliSource,
    key: &str,
) -> bool {
    match home_dir() {
        Some(h) => key_has_definite_resumable_content_in(&h, cli, key),
        // No home → can't probe. Be conservative and leave the row
        // alone (mirrors the lenient probe's default).
        None    => true,
    }
}

/// Testable variant of [`key_has_definite_resumable_content`].
pub(crate) fn key_has_definite_resumable_content_in(
    home: &Path,
    cli: &crate::agent_sessions::CliSource,
    key: &str,
) -> bool {
    use crate::agent_sessions::CliSource;
    match cli {
        CliSource::Claude  => claude_key_has_definite_resumable_content_in(home, key),
        CliSource::Codex   => codex_key_has_definite_resumable_content_in(home, key),
        CliSource::Copilot => copilot_key_has_definite_resumable_content_in(home, key),
        CliSource::Gemini  => gemini_key_has_definite_resumable_content_in(home, key),
        CliSource::Unknown(_) => true,
    }
}

/// Strict counterpart of [`claude_key_is_resumable_on_disk_in`]:
/// missing JSONL → `false` (treat as phantom). See
/// [`key_has_definite_resumable_content`].
pub(crate) fn claude_key_has_definite_resumable_content_in(
    home: &Path,
    key: &str,
) -> bool {
    match claude_jsonl_path_for_key(home, key) {
        None    => false,
        Some(p) => claude_session_has_real_content(&p),
    }
}

/// Strict counterpart of [`copilot_key_is_resumable_on_disk_in`]:
/// missing session-state dir → `false` (treat as phantom). For the
/// live-tracked case this is rare in practice — Copilot eagerly
/// creates `workspace.yaml` on launch — but the strict check covers
/// the edge case symmetrically.
pub(crate) fn copilot_key_has_definite_resumable_content_in(
    home: &Path,
    key: &str,
) -> bool {
    let dir = copilot_session_dir_for_key(home, key);
    if !dir.is_dir() { return false; }
    let events = dir.join("events.jsonl");
    events.metadata()
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Strict counterpart of [`gemini_key_is_resumable_on_disk_in`]:
/// missing JSONL → `false` (treat as phantom).
pub(crate) fn gemini_key_has_definite_resumable_content_in(
    home: &Path,
    key: &str,
) -> bool {
    match gemini_jsonl_path_for_key(home, key) {
        None    => false,
        Some(p) => gemini_jsonl_has_real_content(&p),
    }
}

// ─── Claude per-key helpers ─────────────────────────────────────────────

/// Returns `true` iff Claude's on-disk JSONL for `key` either doesn't
/// exist (defer to Claude's own validation) OR exists and contains at
/// least one record `claude --resume <key>` would treat as real
/// conversational content. Returns `false` only when a JSONL exists
/// but consists solely of meta records — the precise "phantom" pattern
/// `claude --resume` rejects with
/// `No conversation found with session ID: <id>`.
pub(crate) fn claude_key_is_resumable_on_disk_in(home: &Path, key: &str) -> bool {
    match claude_jsonl_path_for_key(home, key) {
        // No JSONL — could be a fresh session that hasn't flushed, a
        // test fixture, or a session in some other home directory.
        // Conservatively treat as resumable.
        None    => true,
        Some(p) => claude_session_has_real_content(&p),
    }
}

// ─── Copilot per-key helpers ────────────────────────────────────────────

/// Resolve the Copilot session-state directory for `key`.
/// Always returns a path (no I/O); callers must `is_dir`/`exists` it.
pub(crate) fn copilot_session_dir_for_key(home: &Path, key: &str) -> PathBuf {
    home.join(".copilot").join("session-state").join(key)
}

/// Returns `true` iff Copilot's on-disk session state for `key` is
/// missing (defer to Copilot) OR has a non-empty `events.jsonl` (the
/// same marker `load_copilot` uses to decide whether a session is real
/// vs. ephemeral). Returns `false` only when the session dir exists
/// but `events.jsonl` is missing or zero-bytes — the precise phantom
/// pattern `copilot --resume=<id>` rejects with
/// `Error: No session, task, or name matched '<id>'`.
pub(crate) fn copilot_key_is_resumable_on_disk_in(home: &Path, key: &str) -> bool {
    let dir = copilot_session_dir_for_key(home, key);
    // No directory at all → defer to Copilot (parallels the Claude
    // "JSONL missing" branch).
    if !dir.is_dir() { return true; }
    let events = dir.join("events.jsonl");
    events.metadata()
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

// ─── Gemini per-key helpers ─────────────────────────────────────────────

/// Returns `true` iff Gemini's on-disk JSONL for `key` is missing
/// (defer to Gemini) OR has at least one non-header record. The
/// header line is the first non-empty JSON object carrying a
/// top-level `sessionId` field; everything else (`type:"user"`,
/// `type:"tool"`, `type:"info"`, ...) counts as real activity.
/// Returns `false` only when the JSONL exists and contains nothing
/// but header line(s) — the pattern Gemini writes when the user
/// opens the CLI and immediately exits without exchanging a turn.
pub(crate) fn gemini_key_is_resumable_on_disk_in(home: &Path, key: &str) -> bool {
    match gemini_jsonl_path_for_key(home, key) {
        None    => true,
        Some(p) => gemini_jsonl_has_real_content(&p),
    }
}

/// Returns `true` iff the Gemini JSONL at `path` contains at least
/// one record carrying a `type` field (i.e. user / tool / info
/// activity beyond the bare session header). Mirrors the
/// `claude_session_has_real_content` filter — including the
/// streaming + capped read, and the conservative-on-I/O-error
/// behavior — so a large early header (or duplicated headers)
/// can't push real records past a fixed window, and so a
/// transient open failure doesn't drop a real session.
pub(crate) fn gemini_jsonl_has_real_content(path: &Path) -> bool {
    let Some(lines) = stream_jsonl_lines(path, CLASSIFY_SCAN_BYTES_CAP) else {
        // I/O failure → conservative: assume real content (don't
        // drop the row on a transient open error). See the matching
        // comment on `claude_session_has_real_content`.
        return true;
    };
    for line in lines {
        if line.trim().is_empty() { continue; }
        let Ok(val): Result<serde_json::Value, _> = serde_json::from_str(&line) else { continue };
        // Header lines are recognised by a `sessionId` field with no
        // `type` field (see `parse_gemini_meta`). Any record carrying
        // a `type` field is real session activity.
        if val.get("type").is_some() { return true; }
    }
    false
}

// ─── Codex per-key helpers ──────────────────────────────────────────────

fn codex_key_is_resumable_on_disk_in(home: &Path, id: &str) -> bool {
    match find_codex_rollout_by_id(home, id) {
        None => true,
        Some(path) => codex_session_has_real_content(&path),
    }
}

fn codex_key_has_definite_resumable_content_in(home: &Path, id: &str) -> bool {
    match find_codex_rollout_by_id(home, id) {
        None => false,
        Some(path) => codex_session_has_real_content(&path),
    }
}

// ─── Two-phase scan helpers ─────────────────────────────────────────────
//
// Each per-CLI loader runs in two phases:
//   1. cheap discovery — enumerate session artefacts with minimal IO,
//      collecting a `Candidate` (id + sort signal + path). Class A
//      (agent-pane) sessions are dropped here via the index, before any
//      content is read.
//   2. expensive parse — only for the newest `MAX_PER_CLI` survivors:
//      phantom filtering, title, and cwd extraction.
//
// This bounds `load_all`'s content reads at ~`MAX_PER_CLI` per CLI instead
// of "every file on disk", which on a populated machine is the difference
// between ~50 reads and several hundred (the bulk being WTA's own
// agent-pane phantoms — Gemini in particular writes a seeded-prompt
// snapshot per pre-warm that costs an 8 KB read to classify).

/// A lightweight session candidate from a loader's cheap discovery phase.
/// Carries only what the Class A skip and the mtime top-N selection need;
/// the expensive content parse runs later, for survivors only.
struct Candidate {
    /// Session key / id — used for the agent-pane (Class A) index lookup
    /// and becomes the row key.
    id: String,
    /// Last-activity signal used to rank candidates newest-first.
    sort_time: SystemTime,
    /// Path to the session artefact: the session-state dir for Copilot,
    /// the transcript file for Claude / Gemini / Codex.
    path: PathBuf,
    /// cwd already extracted by the cheap phase (Codex reads it from the
    /// `session_meta` first line). `None` for CLIs that derive cwd during
    /// the expensive phase.
    cwd: Option<PathBuf>,
}

/// Drop Class A (agent-pane) candidates, rank the rest newest-first, and
/// keep at most `n`. This is the cheap pre-filter that lets the expensive
/// content parse touch only the most-recent `n` shell-pane sessions per
/// CLI instead of every file on disk.
///
/// `n` is a *discovery-phase acquisition cap*, not a guaranteed result
/// count. The caller's phase-2 content filter — which drops phantom
/// sessions that hold no real turn — runs *after* this truncation, so the
/// final row count can be fewer than `n` when some of the newest `n`
/// candidates turn out to be phantoms. That is intentional: keeping the
/// truncation ahead of the content read bounds phase-2 at `n` content
/// reads per CLI. We deliberately do not back-fill from older candidates
/// to top the result back up to `n`.
fn select_top_candidates(
    mut candidates: Vec<Candidate>,
    agent_pane_index: &HashSet<String>,
    n: usize,
) -> Vec<Candidate> {
    candidates.retain(|c| !agent_pane_index.contains(&c.id));
    candidates.sort_by(|a, b| b.sort_time.cmp(&a.sort_time));
    candidates.truncate(n);
    candidates
}

/// Read only the first non-empty line of a file, stopping at the first
/// newline instead of slurping a fixed-size prefix. Used by the cheap
/// discovery phase to pull a session id / header out of Gemini and Codex
/// transcripts without reading the whole (potentially multi-MB) file.
fn read_first_line(path: &Path) -> Option<String> {
    use std::io::{BufRead, BufReader, Read};
    let file = fs::File::open(path).ok()?;
    // Bound the read so a corrupt / non-JSONL file that is one giant line
    // with no newline can't slurp unbounded bytes during the cheap
    // discovery phase. See `HEADER_LINE_BYTES_CAP`.
    let mut reader = BufReader::new(file.take(HEADER_LINE_BYTES_CAP));
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).ok()?;
        if n == 0 {
            return None; // EOF (or cap reached) with no non-empty line
        }
        if !line.trim().is_empty() {
            return Some(line);
        }
    }
}

// ─── Copilot ────────────────────────────────────────────────────────────

#[cfg(test)]
fn load_copilot(home: &Path) -> Vec<AgentSession> {
    load_copilot_indexed(home, &HashSet::new())
}

fn load_copilot_indexed(home: &Path, agent_pane_index: &HashSet<String>) -> Vec<AgentSession> {
    let base = home.join(".copilot").join("session-state");
    let Ok(rd) = fs::read_dir(&base) else { return Vec::new() };

    // Phase 1 (cheap): one dir scan + stat per session. The phantom filter
    // here is stat-only — a non-empty `events.jsonl` marks "the user did
    // something" — so Copilot's many never-used pre-warm dirs (which only
    // ever get a `workspace.yaml`) are dropped without reading any content.
    // Whenever WT (or wta itself) spawns a Copilot CLI process — agent-pane
    // back-end, `?prompt` delegate, coordinator — it eagerly creates
    // `~/.copilot/session-state/<UUID>/workspace.yaml` before the user types
    // anything; if the user never interacts, no `events.jsonl` is written.
    let mut candidates = Vec::new();
    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
        let dir = entry.path();
        let id = match dir.file_name().and_then(|n| n.to_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let events = dir.join("events.jsonl");
        let has_real_activity = events.metadata()
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if !has_real_activity { continue; }
        let last_activity = events.metadata()
            .and_then(|m| m.modified()).ok()
            .or_else(|| dir.join("workspace.yaml").metadata().and_then(|m| m.modified()).ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        candidates.push(Candidate { id, sort_time: last_activity, path: dir, cwd: None });
    }

    // Phase 2 (expensive): read `workspace.yaml` for title + cwd, newest
    // `MAX_PER_CLI` shell-pane sessions only.
    let mut out = Vec::new();
    for c in select_top_candidates(candidates, agent_pane_index, MAX_PER_CLI) {
        let dir = c.path;
        let workspace = dir.join("workspace.yaml");
        let events = dir.join("events.jsonl");
        let started_at = workspace.metadata()
            .and_then(|m| m.modified()).ok()
            .unwrap_or(c.sort_time);
        let yaml = fs::read_to_string(&workspace).unwrap_or_default();
        let cwd = parse_simple_yaml(&yaml, "cwd")
            .map(PathBuf::from)
            .unwrap_or_default();
        // Copilot writes the session title to `name`; `summary` is a removed
        // legacy field kept only as a fallback for very old sessions. Fall
        // back to a short id when neither is present yet.
        let title = parse_simple_yaml(&yaml, "name")
            .filter(|s| !s.is_empty())
            .or_else(|| parse_simple_yaml(&yaml, "summary").filter(|s| !s.is_empty()))
            .unwrap_or_else(|| short_id(&c.id, "copilot"));

        out.push(AgentSession {
            key:               c.id,
            cli_source:        CliSource::Copilot,
            pane_session_id:   None,
            window_id:         None,
            tab_id:            None,
            title,
            cwd,
            started_at,
            last_activity_at:  c.sort_time,
            status:            AgentStatus::Historical,
            last_error:        None,
            current_tool:      None,
            attention_reason:  None,
            log_path:          Some(events),
            origin:            crate::agent_sessions::SessionOrigin::default(),
            location:          crate::agent_sessions::SessionLocation::Host,
        });
    }
    out.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
    out
}

// ─── Claude ─────────────────────────────────────────────────────────────

#[cfg(test)]
fn load_claude(home: &Path) -> Vec<AgentSession> {
    load_claude_indexed(home, &HashSet::new())
}

fn load_claude_indexed(home: &Path, agent_pane_index: &HashSet<String>) -> Vec<AgentSession> {
    let base = home.join(".claude").join("projects");
    let Ok(rd) = fs::read_dir(&base) else { return Vec::new() };

    // Phase 1 (cheap): enumerate transcripts; id = filename stem, sort by
    // mtime. No content is read here — Claude phantoms (e.g. `/model` then
    // Ctrl+D) can only be told apart by content, so that filter is deferred
    // to phase 2.
    let mut candidates = Vec::new();
    for proj_entry in rd.flatten() {
        let proj_dir = proj_entry.path();
        let is_project = proj_dir.file_name()
            .and_then(|n| n.to_str())
            .map(|s| s != "memory")
            .unwrap_or(false);
        if !is_project { continue; }
        let Ok(files) = fs::read_dir(&proj_dir) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.is_dir() { continue; }
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            let id = match path.file_stem().and_then(|n| n.to_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            let sort_time = path.metadata().and_then(|m| m.modified()).ok()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            candidates.push(Candidate { id, sort_time, path, cwd: None });
        }
    }

    // Phase 2 (expensive): content read for the newest `MAX_PER_CLI` only.
    let mut out = Vec::new();
    for c in select_top_candidates(candidates, agent_pane_index, MAX_PER_CLI) {
        let path = c.path;
        // Reproduces the "ghost Claude session" bug: launching `claude` and
        // exiting without typing a real prompt (e.g. just running `/model`
        // then Ctrl+D) still leaves a JSONL on disk, but `claude --resume
        // <id>` rejects it with `No conversation found with session ID:
        // <id>`. Mirror the Copilot ghost-session filter so these rows never
        // appear in the session management view, where Enter would dead-end.
        if !claude_session_has_real_content(&path) { continue; }
        // Claude's directory-name encoding (`\` -> `-`) is lossy: paths whose
        // segments contain `-` can't be recovered from the directory name
        // alone. Use it only as a fallback — prefer the per-record `cwd`
        // embedded in the JSONL, which preserves the original path verbatim.
        let cwd_fallback = path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(decode_claude_cwd)
            .unwrap_or_default();
        let title = first_user_text_jsonl(&path, ClaudeOrGemini::Claude)
            .unwrap_or_else(|| short_id(&c.id, "claude"));
        let cwd = read_cwd_from_claude_jsonl(&path).unwrap_or(cwd_fallback);

        out.push(AgentSession {
            key:               c.id,
            cli_source:        CliSource::Claude,
            location:          crate::agent_sessions::SessionLocation::Host,
            pane_session_id:   None,
            window_id:         None,
            tab_id:            None,
            title,
            cwd,
            started_at:        c.sort_time,
            last_activity_at:  c.sort_time,
            status:            AgentStatus::Historical,
            last_error:        None,
            current_tool:      None,
            attention_reason:  None,
            log_path:          Some(path),
            origin:            crate::agent_sessions::SessionOrigin::default(),
        });
    }
    out.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
    out
}

// ─── Gemini ─────────────────────────────────────────────────────────────

#[cfg(test)]
fn load_gemini(home: &Path) -> Vec<AgentSession> {
    load_gemini_indexed(home, &HashSet::new())
}

fn load_gemini_indexed(home: &Path, agent_pane_index: &HashSet<String>) -> Vec<AgentSession> {
    let tmp = home.join(".gemini").join("tmp");
    let Ok(rd) = fs::read_dir(&tmp) else { return Vec::new() };

    let projects_json = home.join(".gemini").join("projects.json");
    let cwd_lookup    = parse_gemini_projects(&projects_json);

    // Phase 1 (cheap): read only line 1 of each transcript to pull the
    // sessionId (Gemini doesn't put it in the filename); sort by mtime.
    // This is the key win — Gemini's seeded-prompt agent-pane phantoms are
    // identified by id and skipped here in `select_top_candidates`, instead
    // of each costing a full `gemini_jsonl_has_real_content` scan.
    let mut candidates = Vec::new();
    for proj_entry in rd.flatten() {
        if !proj_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
        let chats = proj_entry.path().join("chats");
        let Ok(files) = fs::read_dir(&chats) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if !is_gemini_session_file(&path) { continue; }
            // A JSONL with content must have a resolvable `sessionId` in its
            // header; if we can't parse it, skip rather than synthesise an
            // un-resumable key (Enter on such rows used to silently no-op).
            let Some(sid) = gemini_session_id_from_header(&path) else { continue; };
            let sort_time = path.metadata().and_then(|m| m.modified()).ok()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            candidates.push(Candidate { id: sid, sort_time, path, cwd: None });
        }
    }

    // Phase 2 (expensive): phantom filter + title for the newest
    // `MAX_PER_CLI` only.
    let mut out = Vec::new();
    for c in select_top_candidates(candidates, agent_pane_index, MAX_PER_CLI) {
        let path = c.path;
        // Drop phantom Gemini sessions: opening `gemini` and exiting without
        // exchanging a turn leaves a JSONL containing only header line(s) —
        // `gemini --resume <id>` would reject it. Mirrors the Claude and
        // Copilot loader-side filters.
        if !gemini_jsonl_has_real_content(&path) { continue; }
        // cwd: `~/.gemini/tmp/<slug>/chats/session-*.jsonl` → look up <slug>.
        let cwd = path.parent()
            .and_then(|p| p.parent())
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .and_then(|slug| cwd_lookup.get(slug).cloned())
            .unwrap_or_default();
        let title = first_user_text_jsonl(&path, ClaudeOrGemini::Gemini)
            .unwrap_or_else(|| short_id(&c.id, "gemini"));

        out.push(AgentSession {
            key:               c.id,
            cli_source:        CliSource::Gemini,
            location:          crate::agent_sessions::SessionLocation::Host,
            pane_session_id:   None,
            window_id:         None,
            tab_id:            None,
            title,
            cwd,
            started_at:        c.sort_time,
            last_activity_at:  c.sort_time,
            status:            AgentStatus::Historical,
            last_error:        None,
            current_tool:      None,
            attention_reason:  None,
            log_path:          Some(path),
            origin:            crate::agent_sessions::SessionOrigin::default(),
        });
    }
    out.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
    out
}

/// Extract a Gemini session's `sessionId` from its header (first non-empty
/// line) with a single cheap line read. A header line carries `sessionId`
/// and no `type` field; a leading record that has a `type` field means the
/// header is missing or not first, so we skip it (matches `parse_gemini_meta`).
fn gemini_session_id_from_header(path: &Path) -> Option<String> {
    let line = read_first_line(path)?;
    let val: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if val.get("type").is_some() {
        return None;
    }
    val.get("sessionId").and_then(|v| v.as_str()).map(String::from)
}

/// Top-level Gemini chat files are `~/.gemini/tmp/<slug>/chats/session-*.jsonl`.
/// Files inside a per-subagent `<UUID>/` subdirectory are NOT session files
/// and must be skipped.
fn is_gemini_session_file(p: &Path) -> bool {
    if !p.is_file() { return false; }
    let Some(name) = p.file_name().and_then(|n| n.to_str()) else { return false; };
    if !name.starts_with("session-") { return false; }
    name.ends_with(".jsonl")
}

// ─── Codex ──────────────────────────────────────────────────────────────

#[cfg(test)]
fn load_codex(home: &Path) -> Vec<AgentSession> {
    load_codex_indexed(home, &HashSet::new())
}

fn load_codex_indexed(home: &Path, agent_pane_index: &HashSet<String>) -> Vec<AgentSession> {
    let root = home.join(".codex").join("sessions");
    let Ok(years) = fs::read_dir(&root) else { return Vec::new() };

    // Phase 1 (cheap): read only the `session_meta` first line of each
    // rollout to get id + cwd + timestamp + subagent flag. Subagent forks
    // are dropped here; Class A is dropped in `select_top_candidates`.
    let mut candidates = Vec::new();
    for y in years.flatten() {
        let Ok(months) = fs::read_dir(y.path()) else { continue };
        for m in months.flatten() {
            let Ok(days) = fs::read_dir(m.path()) else { continue };
            for d in days.flatten() {
                let Ok(files) = fs::read_dir(d.path()) else { continue };
                for f in files.flatten() {
                    let path = f.path();
                    let Some(name) = path.file_name().and_then(|s| s.to_str()) else { continue };
                    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") { continue; }
                    let Some(meta) = read_codex_session_meta(&path) else { continue; };
                    // Skip Codex internal multi-agent subagent forks: they get
                    // their own rollout file but inherit the parent's history
                    // (same title) and are not user-facing sessions.
                    if meta.is_subagent { continue; }
                    let sort_time = meta.timestamp
                        .or_else(|| fs::metadata(&path).and_then(|m| m.modified()).ok())
                        .unwrap_or_else(SystemTime::now);
                    candidates.push(Candidate {
                        id: meta.id,
                        sort_time,
                        path,
                        cwd: Some(meta.cwd),
                    });
                }
            }
        }
    }

    // Phase 2 (expensive): full-content phantom filter + title for the
    // newest `MAX_PER_CLI` only. cwd was already read in phase 1.
    let mut out = Vec::new();
    for c in select_top_candidates(candidates, agent_pane_index, MAX_PER_CLI) {
        let path = c.path;
        if !codex_session_has_real_content(&path) { continue; }
        let title = codex_title_from_file(&path)
            .unwrap_or_else(|| short_id(&c.id, "codex"));
        let cwd = c.cwd.unwrap_or_default();
        out.push(AgentSession {
            key:               c.id,
            cli_source:        CliSource::Codex,
            location:          crate::agent_sessions::SessionLocation::Host,
            pane_session_id:   None,
            window_id:         None,
            tab_id:            None,
            title,
            cwd,
            started_at:        c.sort_time,
            last_activity_at:  c.sort_time,
            status:            AgentStatus::Historical,
            last_error:        None,
            current_tool:      None,
            attention_reason:  None,
            log_path:          Some(path),
            origin:            crate::agent_sessions::SessionOrigin::default(),
        });
    }
    out.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
    out
}

struct CodexSessionMeta {
    id:          String,
    cwd:         PathBuf,
    timestamp:   Option<SystemTime>,
    is_subagent: bool,
}

fn read_codex_session_meta(path: &Path) -> Option<CodexSessionMeta> {
    use std::io::BufRead;
    let f = fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(f);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type")?.as_str()? != "session_meta" { return None; }
    let payload = v.get("payload")?;
    let ts_str = payload.get("timestamp").and_then(|s| s.as_str());
    Some(CodexSessionMeta {
        id:          payload.get("id")?.as_str()?.to_string(),
        cwd:         PathBuf::from(payload.get("cwd")?.as_str()?),
        timestamp:   ts_str.and_then(parse_iso_to_system_time),
        is_subagent: codex_payload_is_subagent(payload),
    })
}

/// True if a Codex rollout record is the `session_meta` of an internal
/// multi-agent subagent / forked thread. Codex's `multi_agent_v1` / `spawn_agent`
/// tool forks a child thread that gets its own `rollout-*.jsonl` (carrying
/// `source.subagent` in its meta) and inherits the parent's full history — so it
/// shows the same first user message / title. It is a codex-internal worker, not
/// a user-facing session, and must not surface as its own session row.
pub(crate) fn codex_record_is_subagent_meta(v: &serde_json::Value) -> bool {
    v.get("type").and_then(|t| t.as_str()) == Some("session_meta")
        && v.get("payload").map(codex_payload_is_subagent).unwrap_or(false)
}

/// True if a Codex `session_meta` payload's `source` is the subagent variant
/// (`{"subagent": …}`) rather than a top-level session (`"cli"` / `"user"`).
pub(crate) fn codex_payload_is_subagent(payload: &serde_json::Value) -> bool {
    payload
        .get("source")
        .and_then(|s| s.get("subagent"))
        .is_some()
}

fn codex_session_has_real_content(path: &Path) -> bool {
    let Some(lines) = stream_jsonl_lines(path, CLASSIFY_SCAN_BYTES_CAP) else {
        return true; // conservative on IO error
    };
    for line in lines {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let ty = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
        match ty {
            "event_msg" => {
                let pty = v.get("payload")
                    .and_then(|p| p.get("type"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                if matches!(pty, "user_message" | "agent_message") { return true; }
            }
            "response_item" => {
                let Some(payload) = v.get("payload") else { continue };
                let role = payload.get("role").and_then(|s| s.as_str()).unwrap_or("");
                if role == "assistant" { return true; }
                if role == "user" {
                    let text = payload.get("content")
                        .and_then(|c| c.get(0))
                        .and_then(|c0| c0.get("text"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if !codex_user_text_is_synthetic(text) { return true; }
                }
            }
            _ => {}
        }
    }
    false
}

fn codex_title_from_file(path: &Path) -> Option<String> {
    let lines = stream_jsonl_lines(path, CLASSIFY_SCAN_BYTES_CAP)?;
    for line in lines {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        let ty = v.get("type").and_then(|s| s.as_str()).unwrap_or("");
        match ty {
            "event_msg" => {
                let Some(payload) = v.get("payload") else { continue };
                let pty = payload.get("type").and_then(|s| s.as_str()).unwrap_or("");
                if pty == "user_message" {
                    let msg = payload.get("message").and_then(|s| s.as_str()).unwrap_or("");
                    let title = first_nonblank_line(msg);
                    if !title.is_empty() { return Some(title); }
                }
            }
            "response_item" => {
                let Some(payload) = v.get("payload") else { continue };
                let role = payload.get("role").and_then(|s| s.as_str()).unwrap_or("");
                if role == "user" {
                    let text = payload.get("content")
                        .and_then(|c| c.get(0))
                        .and_then(|c0| c0.get("text"))
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if !codex_user_text_is_synthetic(text) {
                        let title = first_nonblank_line(text);
                        if !title.is_empty() { return Some(title); }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn first_nonblank_line(raw: &str) -> String {
    raw.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string()
}

/// Is this codex user-role `content.text` a *synthetic* record codex injects
/// around the real conversation rather than something the user typed?
///
/// Codex prepends/interleaves several non-prompt user-role records: XML-ish
/// wrapper blocks (`<environment_context>`, `<user_instructions>`,
/// `<subagent_notification>`, `<turn_aborted>`, …) and the auto-loaded AGENTS.md,
/// headed by codex's `# AGENTS.md instructions` marker. That marker comes in two
/// forms (codex `UserInstructions::body`): `# AGENTS.md instructions for <dir>`
/// for a project AGENTS.md (`directory = Some`) and the *bare*
/// `# AGENTS.md instructions` for a global `~/.codex/AGENTS.md`
/// (`directory = None`). All appear *before* the user's first real prompt in the
/// rollout, so both the title scanner and the "has real content" (phantom) check
/// must skip them — otherwise a freshly opened, never-prompted codex session is
/// treated as real and titled with a doc heading (e.g.
/// `# AGENTS.md instructions for C:\…\intelligent-terminal`, or the bare
/// `# AGENTS.md instructions`) instead of the user's prompt. Add new codex
/// wrapper tags to `WRAPPER_TAGS` as they appear.
fn codex_user_text_is_synthetic(text: &str) -> bool {
    const WRAPPER_TAGS: &[&str] = &[
        "<environment_context",
        "<user_instructions",
        "<subagent_notification",
        "<turn_aborted",
    ];
    const AGENTS_MD_HEADING: &str = "# AGENTS.md instructions";
    let t = text.trim_start();
    // Match the AGENTS.md marker only when it stands as a whole heading line so a
    // real prompt that merely opens with the phrase mid-line isn't swallowed: the
    // bare global form is followed by end-of-text or a newline, and the project
    // form continues with ` for <dir>`.
    WRAPPER_TAGS.iter().any(|tag| t.starts_with(tag))
        || t.strip_prefix(AGENTS_MD_HEADING).is_some_and(|rest| {
            rest.is_empty()
                || rest.starts_with('\n')
                || rest.starts_with('\r')
                || rest.starts_with(" for ")
        })
}

pub fn codex_title_for_key(home: &Path, key: &str) -> Option<String> {
    let path = find_codex_rollout_by_id(home, key)?;
    codex_title_from_file(&path)
}

/// Read a Codex session's working directory from its rollout `session_meta`
/// record (always the first line). Shell-pane Codex rows have no path-encoded
/// cwd (unlike Claude), and Codex writes no title until the user's first
/// message — so without this the row would have an empty cwd and the session
/// view's cwd-basename title fallback would render a placeholder for the ~20s
/// before that first message. Returns `None` if the file/field is absent.
pub(crate) fn codex_cwd_from_rollout(path: &Path) -> Option<PathBuf> {
    let first = stream_jsonl_lines(path, CLASSIFY_SCAN_BYTES_CAP)?.next()?;
    let v: serde_json::Value = serde_json::from_str(&first).ok()?;
    let cwd = v.get("payload")?.get("cwd")?.as_str()?;
    if cwd.is_empty() {
        return None;
    }
    Some(PathBuf::from(cwd))
}

/// Locate the rollout file for a given session UUID.
///
/// Defensive walking: only an unreadable ROOT (`~/.codex/sessions`) returns
/// None. Subtree errors (an unreadable year / month / day directory)
/// `continue` so the search proceeds across siblings — same contract as
/// `load_codex`.
///
/// The filename suffix `<id>.jsonl` is a fast pre-filter; we still verify
/// `payload.id == id` to guard against renamed files or UUID-prefix
/// collisions.
pub(crate) fn find_codex_rollout_by_id(home: &Path, id: &str) -> Option<PathBuf> {
    let root = home.join(".codex").join("sessions");
    let Ok(years) = fs::read_dir(&root) else { return None };
    for y in years.flatten() {
        let Ok(months) = fs::read_dir(y.path()) else { continue };
        for m in months.flatten() {
            let Ok(days) = fs::read_dir(m.path()) else { continue };
            for d in days.flatten() {
                let Ok(files) = fs::read_dir(d.path()) else { continue };
                for f in files.flatten() {
                    let p = f.path();
                    let Some(name) = p.file_name().and_then(|s| s.to_str()) else { continue };
                    if !(name.starts_with("rollout-") && name.ends_with(&format!("-{}.jsonl", id))) {
                        continue;
                    }
                    if let Some(meta) = read_codex_session_meta(&p) {
                        if meta.id == id {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }
    None
}

/// Parse a subset of ISO 8601 timestamps into `SystemTime`.
///
/// Handles the UTC shapes Codex `session_meta` emits
/// (`YYYY-MM-DDTHH:MM:SSZ` and `YYYY-MM-DDTHH:MM:SS.fffZ`) plus the
/// numeric offset variants (`±HH:MM`), e.g. `2026-05-27T10:53:09+08:00`.
/// Returns `None` for any out-of-range / overflowing / malformed input
/// (never panics).
fn parse_iso_to_system_time(s: &str) -> Option<SystemTime> {
    let s = s.trim();
    
    // Detect and parse timezone offset (+HH:MM or -HH:MM, or Z for UTC)
    let offset_seconds = if s.ends_with('Z') {
        0
    } else if s.len() >= 25 {
        // Check if last 6 characters match ±HH:MM pattern
        let offset_part = s.get(s.len()-6..)?;
        if let Some(sign_idx) = offset_part.rfind(|c| c == '+' || c == '-') {
            if sign_idx == 0 {
                // Parse HH:MM
                let hm = offset_part.get(1..)?;
                if hm.len() == 5 && hm.chars().nth(2) == Some(':') {
                    let hh: i32 = hm.get(..2)?.parse().ok()?;
                    let mm: i32 = hm.get(3..)?.parse().ok()?;
                    // Reject out-of-range offsets (e.g. `+99:99`) so they
                    // don't silently skew the timestamp.
                    if !(0..=23).contains(&hh) || !(0..=59).contains(&mm) {
                        return None;
                    }
                    let total_seconds = hh * 3600 + mm * 60;
                    if offset_part.starts_with('-') { -total_seconds } else { total_seconds }
                } else {
                    return None;
                }
            } else {
                0
            }
        } else {
            0
        }
    } else {
        0
    };
    
    // Determine the core portion to parse (strip Z or offset)
    let core = if s.ends_with('Z') {
        s.strip_suffix('Z')?
    } else if offset_seconds != 0 && s.len() >= 6 {
        s.get(..s.len()-6)?
    } else {
        s.get(..19)?
    };
    
    // Split at 'T' → date + time
    let (date_part, time_part) = core.split_once('T')?;
    let mut date_iter = date_part.split('-');
    let year: u64 = date_iter.next()?.parse().ok()?;
    let month: u64 = date_iter.next()?.parse().ok()?;
    let day: u64 = date_iter.next()?.parse().ok()?;
    let time_no_frac = time_part.split('.').next().unwrap_or(time_part);
    let mut time_iter = time_no_frac.split(':');
    let hour: u64 = time_iter.next()?.parse().ok()?;
    let min: u64 = time_iter.next()?.parse().ok()?;
    let sec: u64 = time_iter.next()?.parse().ok()?;

    // Pre-1970 underflow check, and bound the year so the day/seconds
    // arithmetic below cannot overflow u64 (the documented subset of
    // ISO 8601 only needs 4-digit years anyway).
    if year < 1970 || year > 9999 {
        return None;
    }

    // Validate hour/min/sec bounds
    if hour > 23 || min > 59 || sec > 59 {
        return None;
    }

    // Convert to Unix timestamp (simplified — no leap seconds).
    // Days from year 0 to start of `year`, then add months+day.
    fn days_before_year(y: u64) -> u64 {
        let y = y - 1;
        365 * y + y / 4 - y / 100 + y / 400
    }
    fn is_leap(y: u64) -> bool {
        y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
    }
    let days_in_month: [u64; 12] = [31, if is_leap(year) { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    
    // Validate month bounds
    if month < 1 || month > 12 {
        return None;
    }
    
    // Validate day bounds
    let days_in_current_month = days_in_month[(month - 1) as usize];
    if day < 1 || day > days_in_current_month {
        return None;
    }
    
    let mut total_days = days_before_year(year) - days_before_year(1970);
    for i in 0..(month - 1) as usize {
        total_days += days_in_month[i];
    }
    total_days += day - 1;
    let mut secs = (total_days * 86400 + hour * 3600 + min * 60 + sec) as i64;
    // Subtract offset to convert from local time to UTC
    secs -= offset_seconds as i64;
    
    if secs < 0 {
        return None;
    }
    // `checked_add` so malformed / far-future timestamps fail closed
    // (return `None`) instead of panicking on overflow.
    SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(secs as u64))
}

// ─── Helpers ────────────────────────────────────────────────────────────

fn short_id(id: &str, cli: &str) -> String {
    let head: String = id.chars().take(8).collect();
    format!("{} {}", cli, head)
}

/// Extract a value from a flat key:value YAML file. Strings may be unquoted
/// (Copilot's workspace.yaml) or quoted. Supports block scalars (`|`, `|-`,
/// `|+`, `>`, `>-`, `>+`) for multi-line values — Copilot writes long
/// `summary:` fields this way, and a naive line read would otherwise
/// surface the literal `|-` indicator instead of the prose. Does NOT
/// support nested mapping structures.
pub(crate) fn parse_simple_yaml(text: &str, key: &str) -> Option<String> {
    let mut lines = text.lines().enumerate().peekable();
    while let Some((_, line)) = lines.next() {
        let key_indent = line.len() - line.trim_start().len();
        let trimmed = &line[key_indent..];
        let Some(rest) = trimmed.strip_prefix(key) else { continue };
        // Reject prefix matches like key="summa" against "summary: ...".
        // Allow only whitespace or `:` immediately after the key.
        let next = rest.chars().next();
        if !matches!(next, Some(':') | Some(' ') | Some('\t') | None) {
            continue;
        }
        let rest = rest.trim_start();
        let Some(after_colon) = rest.strip_prefix(':') else { continue };
        let inline = after_colon.trim();

        // Block scalar: `|`, `|-`, `|+`, `>`, `>-`, `>+`. Anything trailing
        // (a comment after the indicator) is tolerated but ignored.
        if let Some(style) = block_scalar_style(inline) {
            return Some(read_block_scalar(&mut lines, key_indent, style));
        }

        let mut v = inline.to_string();
        if (v.starts_with('"') && v.ends_with('"') && v.len() >= 2)
            || (v.starts_with('\'') && v.ends_with('\'') && v.len() >= 2)
        {
            v = v[1..v.len() - 1].to_string();
        }
        return Some(v);
    }
    None
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum BlockScalarStyle {
    /// `|` — keep newlines, default chomping (single trailing newline kept).
    LiteralClip,
    /// `|-` — keep newlines, strip trailing newlines.
    LiteralStrip,
    /// `|+` — keep newlines, keep all trailing newlines.
    LiteralKeep,
    /// `>` — fold newlines to spaces, default chomping.
    FoldedClip,
    /// `>-` — fold newlines to spaces, strip trailing newlines.
    FoldedStrip,
    /// `>+` — fold newlines to spaces, keep all trailing newlines.
    FoldedKeep,
}

fn block_scalar_style(inline: &str) -> Option<BlockScalarStyle> {
    // Strip a trailing `#`-comment if present so `summary: |- # note` parses.
    let head = inline.split('#').next().unwrap_or("").trim_end();
    match head {
        "|"  => Some(BlockScalarStyle::LiteralClip),
        "|-" => Some(BlockScalarStyle::LiteralStrip),
        "|+" => Some(BlockScalarStyle::LiteralKeep),
        ">"  => Some(BlockScalarStyle::FoldedClip),
        ">-" => Some(BlockScalarStyle::FoldedStrip),
        ">+" => Some(BlockScalarStyle::FoldedKeep),
        _ => None,
    }
}

/// Read content lines of a YAML block scalar. Consumes lines from `iter`
/// up to (but not including) the first line whose indent is `<= key_indent`
/// and which is non-blank — that line belongs to the next mapping entry
/// and must not be eaten. Blank lines inside the block are preserved.
///
/// Folded styles (`>`) collapse consecutive non-empty content lines into a
/// single space-joined run; blank lines remain as paragraph separators
/// (rendered as `\n`). Literal styles (`|`) keep every line as-is.
/// Chomping (`-` strip / `+` keep / default clip) controls trailing
/// newlines, matching YAML 1.2 §8.1.1.
fn read_block_scalar<'a, I>(
    iter:       &mut std::iter::Peekable<I>,
    key_indent: usize,
    style:      BlockScalarStyle,
) -> String
where
    I: Iterator<Item = (usize, &'a str)>,
{
    let mut content_indent: Option<usize> = None;
    let mut raw: Vec<String> = Vec::new();

    while let Some(&(_, line)) = iter.peek() {
        let trimmed = line.trim_start();
        let indent  = line.len() - trimmed.len();

        if trimmed.is_empty() {
            // Blank lines belong to the block regardless of indent.
            raw.push(String::new());
            iter.next();
            continue;
        }
        if indent <= key_indent {
            // Dedented to the parent mapping level — block ends here.
            break;
        }
        // First non-blank line fixes the block's content indent. All
        // subsequent lines indent ≥ this will be stripped of `content_indent`
        // leading spaces; lines that happen to be more indented keep the
        // extra indent (matching YAML semantics).
        let ci = *content_indent.get_or_insert(indent);
        // Defensive: if a later line is *less* indented than the first
        // content line but still > key_indent, just strip what we can.
        let strip = ci.min(indent);
        raw.push(line[strip..].to_string());
        iter.next();
    }

    join_block(&raw, style)
}

fn join_block(raw: &[String], style: BlockScalarStyle) -> String {
    use BlockScalarStyle::*;
    let folded = matches!(style, FoldedClip | FoldedStrip | FoldedKeep);
    let chomp_strip = matches!(style, LiteralStrip | FoldedStrip);
    let chomp_keep  = matches!(style, LiteralKeep  | FoldedKeep);

    let mut out = String::new();
    if folded {
        // Group consecutive non-empty lines and join them with a single
        // space; blank lines become `\n` paragraph separators.
        let mut pending_blank = false;
        let mut wrote_run = false;
        for line in raw {
            if line.is_empty() {
                pending_blank = true;
                continue;
            }
            if pending_blank {
                out.push('\n');
                pending_blank = false;
                wrote_run = false;
            }
            if wrote_run {
                out.push(' ');
            }
            out.push_str(line);
            wrote_run = true;
        }
    } else {
        for (i, line) in raw.iter().enumerate() {
            if i > 0 { out.push('\n'); }
            out.push_str(line);
        }
    }

    // Chomping. YAML's default (clip) keeps a single trailing newline.
    // `-` strips all; `+` keeps all. For our title-extraction use case
    // we always trim trailing whitespace at the call site, but honor
    // the semantics so the function is correct for other callers.
    if chomp_strip {
        while out.ends_with('\n') { out.pop(); }
    } else if !chomp_keep {
        while out.ends_with("\n\n") { out.pop(); }
        if !out.ends_with('\n') && !out.is_empty() {
            // clip keeps exactly one trailing \n iff the block had any content;
            // a fully-empty block stays empty.
            out.push('\n');
        }
    }
    // Trim trailing whitespace from the final value: callers (title
    // extraction) treat the result as a single-line label, and trailing
    // newlines/spaces would render as awkward gaps after the prose.
    while matches!(out.chars().last(), Some(c) if c.is_whitespace()) {
        out.pop();
    }
    out
}

/// Decode Claude's drive-dash project directory back into a CWD path.
///
/// Layout: `C--Users-name-repo` ⇒ `C:\Users\name\repo`. The first `--`
/// after the drive letter is the drive separator; remaining `-` become
/// path separators. Cannot disambiguate hyphens inside actual file names
/// (best-effort; reference impl backtracks via filesystem probing).
pub(crate) fn decode_claude_cwd(encoded: &str) -> PathBuf {
    let bytes = encoded.as_bytes();
    if bytes.len() >= 4
        && bytes[0].is_ascii_alphabetic()
        && &bytes[1..3] == b"--"
    {
        let drive = bytes[0] as char;
        let rest = &encoded[3..];
        let path_part = rest.replace('-', "\\");
        return PathBuf::from(format!("{}:\\{}", drive, path_part));
    }
    // Linux/macOS encoding: leading `-` -> root
    if let Some(stripped) = encoded.strip_prefix('-') {
        return PathBuf::from(format!("/{}", stripped.replace('-', "/")));
    }
    PathBuf::from(encoded)
}

/// Parse `~/.gemini/projects.json` `{projects: {<cwd>: <name>}}`.
/// Returns map of project_name -> cwd (reversed direction).
pub(crate) fn parse_gemini_projects(path: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let Ok(text) = fs::read_to_string(path) else { return out };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) else { return out };
    let Some(map) = val.get("projects").and_then(|v| v.as_object()) else { return out };
    for (cwd_str, name_val) in map {
        if let Some(name) = name_val.as_str() {
            out.insert(name.to_string(), PathBuf::from(cwd_str));
        }
    }
    out
}

/// Read the first non-empty JSONL line of a Gemini session file and extract
/// `sessionId`. Timestamps are not exposed by Gemini's JSONL header — the
/// caller falls back to file mtime for `last_activity`.
pub(crate) fn parse_gemini_meta(path: &Path) -> (Option<String>, Option<SystemTime>) {
    let Ok(text) = read_first_bytes(path, 64 * 1024) else { return (None, None) };
    for line in text.lines() {
        if line.trim().is_empty() { continue; }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        // Hook events such as `{"type":"user", ...}` show up before the
        // session header on rare occasion; skip those.
        if val.get("type").is_some() { return (None, None); }
        let sid = val.get("sessionId").and_then(|v| v.as_str()).map(String::from);
        return (sid, None);
    }
    (None, None)
}

#[derive(Copy, Clone)]
enum ClaudeOrGemini { Claude, Gemini }

/// Best-effort: scan first chunk of JSONL for a user-message line and
/// return its text content, truncated to 60 chars.
fn first_user_text_jsonl(path: &Path, kind: ClaudeOrGemini) -> Option<String> {
    let text = read_first_bytes(path, TITLE_TAIL_BYTES).ok()?;
    for line in text.lines() {
        if line.trim().is_empty() { continue; }
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty != "user" { continue; }
        // Skip Claude meta entries
        if val.get("isMeta").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }

        let raw = match kind {
            ClaudeOrGemini::Claude => extract_claude_user_text(&val),
            ClaudeOrGemini::Gemini => extract_gemini_user_text(&val),
        };
        let cleaned = raw?.trim().lines().next().unwrap_or("").trim().to_string();
        if cleaned.is_empty() { continue; }
        return Some(truncate_chars(&cleaned, 60));
    }
    None
}

fn extract_claude_user_text(v: &serde_json::Value) -> Option<String> {
    let msg = v.get("message")?;
    if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
        return Some(s.to_string());
    }
    if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
        for part in arr {
            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                return Some(t.to_string());
            }
        }
    }
    msg.get("text").and_then(|t| t.as_str()).map(String::from)
        .or_else(|| v.get("content").and_then(|c| c.as_str()).map(String::from))
}

fn extract_gemini_user_text(v: &serde_json::Value) -> Option<String> {
    let arr = v.get("content")?.as_array()?;
    for part in arr {
        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
            return Some(t.to_string());
        }
    }
    None
}

/// Read the first non-empty `cwd` string from a Claude JSONL session
/// file. Claude writes a `cwd` field on every assistant/user/system
/// record, so the first record that carries one gives us the original
/// working directory verbatim — without going through the lossy
/// directory-name encoding that maps `\` and `-` to the same character.
fn read_cwd_from_claude_jsonl(path: &Path) -> Option<PathBuf> {
    let text = read_first_bytes(path, TITLE_TAIL_BYTES).ok()?;
    for line in text.lines() {
        if line.trim().is_empty() { continue; }
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(s) = val.get("cwd").and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return Some(PathBuf::from(s));
            }
        }
    }
    None
}

/// Returns `true` iff the Claude JSONL at `path` contains at least one
/// record that `claude --resume` would treat as real conversational
/// content. The check accepts:
///
///   * Any `type:"assistant"` line (a model reply implies the session
///     completed at least one round trip).
///   * Any `type:"user"` line that is **not** a meta record
///     (`isMeta:true`), **not** a sidechain/subagent record
///     (`isSidechain:true`), and whose `message.content` is not a
///     slash-command echo (`<command-...>` / `<local-command-...>`).
///
/// This matches Claude's own resumability rule: a session that contains
/// only `permission-mode`, `file-history-snapshot`, `last-prompt`, and
/// meta/slash-command user records is rejected with
/// `No conversation found with session ID: <id>`. Filtering those
/// "phantom" JSONL files out of the loader prevents Enter on a session management row
/// from dead-ending in that error.
///
/// Streams the JSONL line-by-line (bounded by [`CLASSIFY_SCAN_BYTES_CAP`])
/// rather than reading a fixed 64 KB head, because Claude's early meta
/// records (notably `file-history-snapshot` for large projects) can
/// individually exceed 64 KB and push the first real user/assistant
/// record past a fixed window — misclassifying a real session as
/// phantom. Short-circuits on first real-content hit.
///
/// **Conservative-on-I/O-error**: when the file can't be opened
/// (locked by AV, transient permission error, race with another
/// writer), returns `true` to treat the session as resumable. The
/// caller (loader / strict prune) takes "true" to mean "keep the
/// row", so this avoids dropping real sessions on transient
/// filesystem failures. Only a successful open + scan that finds
/// no real content classifies as phantom.
fn claude_session_has_real_content(path: &Path) -> bool {
    let Some(lines) = stream_jsonl_lines(path, CLASSIFY_SCAN_BYTES_CAP) else {
        // I/O failure → conservative: assume real content. See the
        // doc comment above for why "true" is the safer default here.
        return true;
    };
    for line in lines {
        if line.trim().is_empty() { continue; }
        let val: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if ty == "assistant" { return true; }
        if ty != "user" { continue; }
        if val.get("isMeta").and_then(|v| v.as_bool()).unwrap_or(false) { continue; }
        if val.get("isSidechain").and_then(|v| v.as_bool()).unwrap_or(false) { continue; }
        // `message.content` may be a string or an array of parts. Treat
        // a string starting with `<command-` / `<local-command-` as a
        // slash-command echo (the only shape Claude emits for those).
        let content_str = val
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .trim_start();
        if content_str.starts_with("<command-") || content_str.starts_with("<local-command-") {
            continue;
        }
        return true;
    }
    false
}

fn read_first_bytes(path: &Path, max: u64) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut buf = Vec::with_capacity(max as usize);
    let _ = (&mut f).take(max).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Open `path` and return an iterator that yields each line as a
/// `String`, with the underlying read capped at `cap_bytes` total.
/// Used by the `*_has_real_content` classifiers so a single huge
/// early meta record (e.g. Claude's `file-history-snapshot` for a
/// large project) can't push real records past the read window and
/// cause the file to be misclassified as phantom.
///
/// Lines that fail to decode as UTF-8 cleanly or fail I/O mid-read
/// are silently skipped — the classifiers parse each line as JSON
/// independently and treat junk lines as "not real content", which
/// matches the previous read-then-split-on-lines behavior.
fn stream_jsonl_lines(
    path: &Path,
    cap_bytes: u64,
) -> Option<impl Iterator<Item = String>> {
    use std::io::{BufRead, BufReader, Read};
    let file = fs::File::open(path).ok()?;
    let limited = file.take(cap_bytes);
    let reader = BufReader::new(limited);
    Some(reader.lines().filter_map(|r| r.ok()))
}

fn truncate_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n { return s.to_string(); }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ─── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmp_root(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let id = format!("wta-history-test-{}-{:?}-{:?}",
            label,
            std::process::id(),
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos(),
        );
        p.push(id);
        let _ = fs::create_dir_all(&p);
        p
    }

    fn write_file(p: &Path, contents: &str) {
        if let Some(parent) = p.parent() { let _ = fs::create_dir_all(parent); }
        let mut f = fs::File::create(p).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn yaml_only_matches_full_keys_not_substrings() {
        // Robustness: a line `summary_count: 0` must not match key `summary`.
        let text = "summary: hello\nsummary_count: 0\n";
        assert_eq!(parse_simple_yaml(text, "summary").as_deref(),       Some("hello"));
        assert_eq!(parse_simple_yaml(text, "summary_count").as_deref(), Some("0"));
        // Querying a nonexistent prefix must not partial-match a longer key.
        assert_eq!(parse_simple_yaml(text, "summa"), None);
    }

    #[test]
    fn codex_cwd_from_rollout_reads_session_meta() {
        let dir = tmp_root("codex-cwd");
        let path = dir.join("rollout-x.jsonl");
        write_file(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\",\"cwd\":\"C:\\\\Users\\\\user\"}}\n\
             {\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        );
        assert_eq!(
            codex_cwd_from_rollout(&path),
            Some(PathBuf::from("C:\\Users\\user"))
        );
    }

    #[test]
    fn codex_cwd_from_rollout_none_when_absent() {
        let dir = tmp_root("codex-cwd-none");
        let path = dir.join("rollout-y.jsonl");
        write_file(&path, "{\"type\":\"session_meta\",\"payload\":{\"id\":\"abc\"}}\n");
        assert_eq!(codex_cwd_from_rollout(&path), None);
    }

    #[test]
    fn yaml_block_scalar_literal_strip_returns_joined_content() {
        // Copilot writes long `summary:` fields as `|-` block scalars when
        // the prose contains line breaks. Before the parser learned about
        // block scalars, this regressed to a literal `|-` title.
        let text = "id: x\nsummary: |-\n  A command failed.\n  Diagnose the error.\nname: short\n";
        assert_eq!(
            parse_simple_yaml(text, "summary").as_deref(),
            Some("A command failed.\nDiagnose the error.")
        );
        // Adjacent keys after the block scalar are still discoverable.
        assert_eq!(parse_simple_yaml(text, "name").as_deref(), Some("short"));
    }

    #[test]
    fn yaml_block_scalar_literal_default_clip_strips_trailing_blank() {
        // `|` (no chomp indicator) is clip — keep a single trailing newline
        // for the raw value, but title-extraction trims trailing whitespace
        // so the visible string ends at the last non-blank char.
        let text = "summary: |\n  one\n  two\n\nname: x\n";
        assert_eq!(parse_simple_yaml(text, "summary").as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn yaml_block_scalar_folded_collapses_lines_to_spaces() {
        // `>` folds line breaks within a paragraph into single spaces.
        let text = "summary: >\n  hello there\n  world\nname: x\n";
        assert_eq!(
            parse_simple_yaml(text, "summary").as_deref(),
            Some("hello there world")
        );
    }

    #[test]
    fn yaml_block_scalar_terminates_at_dedent() {
        // The block must end at the first line that returns to the parent
        // indent level — otherwise we would consume the next mapping key
        // (`name`) as part of the block.
        let text = "summary: |-\n  body line\nname: tail\n";
        assert_eq!(parse_simple_yaml(text, "summary").as_deref(), Some("body line"));
        assert_eq!(parse_simple_yaml(text, "name").as_deref(),    Some("tail"));
    }

    #[test]
    fn yaml_block_scalar_handles_blank_line_inside_block() {
        // Blank lines belong to the block (folded styles use them as
        // paragraph breaks; literal styles preserve them verbatim).
        let text = "summary: |-\n  first paragraph\n\n  second paragraph\nname: x\n";
        let v = parse_simple_yaml(text, "summary").unwrap();
        assert!(v.contains("first paragraph"));
        assert!(v.contains("second paragraph"));
    }

    #[test]
    fn yaml_block_scalar_indicator_does_not_leak_for_inline_values() {
        // Sanity: a value that *contains* `|` but isn't a bare block
        // indicator must still parse as a plain scalar.
        let text = "summary: a | b\n";
        assert_eq!(parse_simple_yaml(text, "summary").as_deref(), Some("a | b"));
    }

    #[test]
    fn yaml_extraction_handles_unquoted_and_quoted_values() {
        let text = "id: abc-123\ncwd: C:\\Users\\foo\nname: 'My session'\nsummary: \"Bug fix #42\"\n";
        assert_eq!(parse_simple_yaml(text, "id").as_deref(),      Some("abc-123"));
        assert_eq!(parse_simple_yaml(text, "cwd").as_deref(),     Some("C:\\Users\\foo"));
        assert_eq!(parse_simple_yaml(text, "name").as_deref(),    Some("My session"));
        assert_eq!(parse_simple_yaml(text, "summary").as_deref(), Some("Bug fix #42"));
        assert_eq!(parse_simple_yaml(text, "missing"),            None);
    }

    #[test]
    fn claude_cwd_decoding_unix_root() {
        assert_eq!(
            decode_claude_cwd("-home-user-repo"),
            PathBuf::from("/home/user/repo")
        );
    }

    #[test]
    fn gemini_meta_first_line_yields_session_id() {
        // Gemini layout: JSONL file whose first line is the session header.
        let root = tmp_root("gemini-meta");
        let f = root.join("session-2026-01-01-abc.jsonl");
        write_file(&f,
            "{\"sessionId\":\"abcd-1234\",\"projectHash\":\"x\",\"startTime\":\"2026-01-01T00:00:00Z\",\"kind\":\"main\"}\n\
             {\"type\":\"user\",\"content\":[{\"text\":\"hello\"}]}\n");
        let (sid, _ts) = parse_gemini_meta(&f);
        assert_eq!(sid.as_deref(), Some("abcd-1234"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn gemini_meta_skips_non_session_first_line() {
        // Defensive: if a hook record lands first, we should give up rather
        // than mistake `type:"user"` for a session header.
        let root = tmp_root("gemini-meta-skip");
        let f = root.join("session-x.jsonl");
        write_file(&f,
            "{\"type\":\"user\",\"content\":[{\"text\":\"hi\"}]}\n");
        let (sid, _) = parse_gemini_meta(&f);
        assert!(sid.is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copilot_loader_picks_up_session_dir() {
        let home = tmp_root("copilot-home");
        let sid = "11111111-2222-3333-4444-555555555555";
        let dir = home.join(".copilot").join("session-state").join(sid);
        fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join("workspace.yaml"),
            "id: 11111111-2222-3333-4444-555555555555\n\
             cwd: C:\\Users\\me\\proj\n\
             summary: Refactor parser\n\
             summary_count: 1\n");
        write_file(&dir.join("events.jsonl"),
            "{\"type\":\"session.start\",\"data\":{}}\n");

        let v = load_copilot(&home);
        assert_eq!(v.len(), 1);
        let s = &v[0];
        assert_eq!(s.key, sid);
        assert_eq!(s.cli_source, CliSource::Copilot);
        assert_eq!(s.title, "Refactor parser");
        assert_eq!(s.cwd, PathBuf::from("C:\\Users\\me\\proj"));
        assert_eq!(s.status, AgentStatus::Historical);
        // `load_copilot` is the index-free test shim
        // (`load_copilot_indexed(.., &empty_index)`): the loader never
        // consults the agent-pane index itself. The real scan threads the
        // index through `select_top_candidates`, which *skips* Class A
        // candidates up front rather than stamping origin afterward. So
        // scanner output here always defaults to Unknown regardless of any
        // index that may exist in the host's real %LOCALAPPDATA%.
        assert_eq!(s.origin, crate::agent_sessions::SessionOrigin::Unknown);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn copilot_loader_falls_back_to_short_id_when_no_summary() {
        let home = tmp_root("copilot-noname");
        let sid = "abcdef01-aaaa-bbbb-cccc-dddddddddddd";
        let dir = home.join(".copilot").join("session-state").join(sid);
        fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join("workspace.yaml"),
            "id: abcdef01-aaaa-bbbb-cccc-dddddddddddd\n\
             cwd: D:\\x\n\
             user_named: false\n\
             summary_count: 0\n");
        // events.jsonl must exist (and be non-empty) for the loader to
        // accept the entry — see `copilot_loader_skips_ephemeral_session_with_no_events`.
        write_file(&dir.join("events.jsonl"), "{\"type\":\"session.start\"}\n");

        let v = load_copilot(&home);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "copilot abcdef01");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn copilot_loader_skips_ephemeral_session_with_no_events() {
        // Reproduces the "ghost session at top of session management view" bug: every time WT
        // (or wta itself) spawns a Copilot CLI process — e.g. as the
        // back-end for an agent pane or for a `?prompt` delegate — that
        // process eagerly creates `~/.copilot/session-state/<UUID>/workspace.yaml`
        // (171 bytes of stub metadata) before the user types anything.
        // If the user never interacts, no `events.jsonl` is ever written.
        // These dirs would otherwise dominate the top of session management view (most-recent
        // last_activity) on the next WT restart. Loader must skip them.
        let home = tmp_root("copilot-ghost");
        let base = home.join(".copilot").join("session-state");

        // Real session — has events.jsonl with content.
        let real = "11111111-1111-1111-1111-111111111111";
        let dir_real = base.join(real);
        fs::create_dir_all(&dir_real).unwrap();
        write_file(&dir_real.join("workspace.yaml"),
            "id: 11111111-1111-1111-1111-111111111111\ncwd: C:\\proj\nsummary: Real Work\n");
        write_file(&dir_real.join("events.jsonl"),
            "{\"type\":\"session.start\"}\n");

        // Ghost session — workspace.yaml only, no events.jsonl.
        let ghost = "22222222-2222-2222-2222-222222222222";
        let dir_ghost = base.join(ghost);
        fs::create_dir_all(&dir_ghost).unwrap();
        write_file(&dir_ghost.join("workspace.yaml"),
            "id: 22222222-2222-2222-2222-222222222222\ncwd: C:\\Users\\me\n");

        // Ghost session — empty events.jsonl (touched but never written).
        let ghost_empty = "33333333-3333-3333-3333-333333333333";
        let dir_ghost_empty = base.join(ghost_empty);
        fs::create_dir_all(&dir_ghost_empty).unwrap();
        write_file(&dir_ghost_empty.join("workspace.yaml"),
            "id: 33333333-3333-3333-3333-333333333333\ncwd: C:\\Users\\me\n");
        write_file(&dir_ghost_empty.join("events.jsonl"), "");

        let v = load_copilot(&home);
        assert_eq!(v.len(), 1, "only the real session should be loaded");
        assert_eq!(v[0].key, real);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_loader_picks_up_jsonl_files_and_skips_memory() {
        let home = tmp_root("claude-home");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-myproj");
        fs::create_dir_all(&proj).unwrap();
        write_file(&proj.join("aaaa-bbbb-cccc.jsonl"),
            "{\"type\":\"user\",\"message\":{\"content\":\"Hello there\"}}\n\
             {\"type\":\"assistant\",\"message\":{\"content\":\"Hi!\"}}\n");

        // memory project must be skipped
        let mem = projects.join("memory");
        fs::create_dir_all(&mem).unwrap();
        write_file(&mem.join("xxx.jsonl"), "{\"type\":\"user\"}\n");

        let v = load_claude(&home);
        assert_eq!(v.len(), 1);
        let s = &v[0];
        assert_eq!(s.key, "aaaa-bbbb-cccc");
        assert_eq!(s.cli_source, CliSource::Claude);
        assert_eq!(s.cwd, PathBuf::from("C:\\Users\\me\\myproj"));
        assert_eq!(s.title, "Hello there");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_loader_prefers_in_file_cwd_over_lossy_dirname() {
        // Real-world: a project whose final segment contains a `-`
        // (e.g. `agentic-terminal`) round-trips to the same encoded
        // dirname as `agentic\terminal`, so the dirname alone can't
        // recover the original path. The JSONL records carry the true
        // cwd verbatim.
        let home = tmp_root("claude-cwd-from-jsonl");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-codes-agentic-terminal");
        fs::create_dir_all(&proj).unwrap();
        write_file(&proj.join("ssss-tttt.jsonl"),
            "{\"type\":\"permission-mode\",\"sessionId\":\"ssss-tttt\"}\n\
             {\"type\":\"user\",\"cwd\":\"C:\\\\Users\\\\me\\\\codes\\\\agentic-terminal\",\"message\":{\"content\":\"hi\"}}\n");

        let v = load_claude(&home);
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0].cwd,
            PathBuf::from("C:\\Users\\me\\codes\\agentic-terminal"),
            "cwd from JSONL must beat lossy dirname decoding",
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_loader_falls_back_to_dirname_when_jsonl_has_no_cwd() {
        // When records carry no `cwd` field the loader still works,
        // landing on the lossy decoded dirname. Acceptable because no
        // better source of truth is available.
        let home = tmp_root("claude-cwd-fallback");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-myproj");
        fs::create_dir_all(&proj).unwrap();
        write_file(&proj.join("oooo-pppp.jsonl"),
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n");

        let v = load_claude(&home);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cwd, PathBuf::from("C:\\Users\\me\\myproj"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_loader_skips_phantom_session_with_only_meta_records() {
        // Reproduces the "ghost Claude session" bug: launching `claude`
        // and exiting (e.g. after just running `/model`, or with no
        // input at all) leaves a JSONL on disk that contains only meta
        // records — permission-mode, file-history-snapshot, isMeta
        // caveat, the slash-command echo + its captured stdout, and a
        // last-prompt footer. `claude --resume <id>` rejects these with
        // `No conversation found with session ID: <id>`, so the row
        // would dead-end on Enter in the session management view.
        // Loader must skip them; only the real session should appear.
        let home = tmp_root("claude-phantom");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-proj");
        fs::create_dir_all(&proj).unwrap();

        // Real session — has a non-meta user message Claude can resume.
        let real = "aaaaaaaa-1111-2222-3333-444444444444";
        write_file(&proj.join(format!("{}.jsonl", real)),
            "{\"type\":\"permission-mode\",\"sessionId\":\"aaaaaaaa-1111-2222-3333-444444444444\"}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello world\"},\
               \"sessionId\":\"aaaaaaaa-1111-2222-3333-444444444444\"}\n");

        // Phantom session — exactly the shape Claude writes when the
        // user opens the CLI, runs `/model`, and exits without typing
        // a real prompt. Has user records, but they're all meta or
        // slash-command echoes.
        let phantom = "bbbbbbbb-1111-2222-3333-444444444444";
        write_file(&proj.join(format!("{}.jsonl", phantom)),
            "{\"type\":\"permission-mode\",\"sessionId\":\"bbbbbbbb-1111-2222-3333-444444444444\"}\n\
             {\"type\":\"file-history-snapshot\",\"messageId\":\"x\",\"snapshot\":{\"trackedFileBackups\":{}}}\n\
             {\"type\":\"user\",\"isMeta\":true,\"message\":{\"role\":\"user\",\"content\":\"<local-command-caveat>Caveat: ...</local-command-caveat>\"}}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<command-name>/model</command-name>\"}}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<local-command-stdout>Set model to Opus</local-command-stdout>\"}}\n\
             {\"type\":\"last-prompt\",\"sessionId\":\"bbbbbbbb-1111-2222-3333-444444444444\"}\n");

        // Phantom session — totally empty JSONL (file touched but
        // nothing written before exit).
        let phantom_empty = "cccccccc-1111-2222-3333-444444444444";
        write_file(&proj.join(format!("{}.jsonl", phantom_empty)), "");

        let v = load_claude(&home);
        assert_eq!(v.len(), 1, "only the real session should survive; got {:?}",
            v.iter().map(|s| s.key.clone()).collect::<Vec<_>>());
        assert_eq!(v[0].key, real);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_loader_keeps_session_with_only_assistant_reply() {
        // Defensive: a session whose only conversational record is an
        // `assistant` line (e.g. user closed Claude mid-stream before
        // the user-message flush completed, but the assistant chunk
        // had already landed) is still resumable by Claude — keep it.
        let home = tmp_root("claude-assistant-only");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-proj");
        fs::create_dir_all(&proj).unwrap();
        let sid = "dddddddd-1111-2222-3333-444444444444";
        write_file(&proj.join(format!("{}.jsonl", sid)),
            "{\"type\":\"permission-mode\",\"sessionId\":\"dddddddd-1111-2222-3333-444444444444\"}\n\
             {\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"hi back\"}}\n");

        let v = load_claude(&home);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].key, sid);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_session_real_content_scans_past_large_early_meta_record() {
        // Regression: Claude's early meta records (notably
        // `file-history-snapshot` for a large project) can
        // individually exceed 64 KB. The previous fixed-window read
        // (TITLE_TAIL_BYTES = 64 KB) could be entirely consumed by a
        // single such record, never reaching the first real
        // user/assistant message — misclassifying a genuinely
        // resumable session as a phantom and pruning it from session management view.
        //
        // The streaming refactor (`stream_jsonl_lines` capped at
        // `CLASSIFY_SCAN_BYTES_CAP`) reads line-by-line and
        // short-circuits on first hit, so a huge meta record on
        // line 2 doesn't hide a real user record on line 3.
        let home = tmp_root("claude-large-meta-then-real");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-proj");
        fs::create_dir_all(&proj).unwrap();
        let sid = "ffffffff-1111-2222-3333-444444444444";

        // Build a `file-history-snapshot` whose JSON line is ~128 KB
        // — comfortably larger than the old 64 KB read window. Pad
        // with a synthetic field of repeated `x` characters that
        // serde_json will parse fine.
        let big_pad: String = "x".repeat(128 * 1024);
        let big_meta = format!(
            "{{\"type\":\"file-history-snapshot\",\"messageId\":\"m\",\"pad\":\"{}\"}}",
            big_pad
        );
        let real_user = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello world\"}}";
        let contents = format!(
            "{{\"type\":\"permission-mode\",\"sessionId\":\"{sid}\"}}\n\
             {big_meta}\n\
             {real_user}\n"
        );
        write_file(&proj.join(format!("{}.jsonl", sid)), &contents);

        let v = load_claude(&home);
        assert_eq!(
            v.len(), 1,
            "session must NOT be misclassified as phantom when real \
             content lives past a 64 KB early meta record; got {:?}",
            v.iter().map(|s| s.key.clone()).collect::<Vec<_>>()
        );
        assert_eq!(v[0].key, sid);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn classifiers_treat_io_error_as_has_content() {
        // Regression: when `stream_jsonl_lines` can't open the file
        // (locked by AV, transient permission error, file deleted
        // between `read_dir` and the classify scan), the classifier
        // must return `true` so the caller keeps the row. Returning
        // `false` would let transient I/O failures silently drop
        // real Claude / Gemini sessions out of session management view.
        //
        // We exercise the I/O-error branch by pointing at paths
        // that don't exist — `fs::File::open` fails the same way it
        // would for a real lock or permission denial as far as the
        // classifier is concerned (None from `stream_jsonl_lines`).
        let home = tmp_root("classifier-io-error");
        let nowhere_claude = home.join(".claude").join("projects").join("no").join("no.jsonl");
        let nowhere_gemini = home.join(".gemini").join("tmp").join("no").join("chats").join("session-no.jsonl");
        assert!(
            claude_session_has_real_content(&nowhere_claude),
            "Claude classifier must be conservative (true) when the file can't be opened",
        );
        assert!(
            gemini_jsonl_has_real_content(&nowhere_gemini),
            "Gemini classifier must be conservative (true) when the file can't be opened",
        );
        let _ = fs::remove_dir_all(&home);
    }

    // ─── Per-CLI resumability probe ─────────────────────────────────

    #[test]
    fn key_resumable_returns_true_when_artefact_missing_for_all_clis() {
        // Missing on-disk artefact → "defer to CLI" (true) so fresh
        // in-memory rows / test fixtures aren't blocked preemptively.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("resumable-missing-all-clis");
        for cli in [CliSource::Claude, CliSource::Codex, CliSource::Copilot, CliSource::Gemini] {
            assert!(
                key_is_resumable_on_disk_in(&home, &cli, "no-such-id"),
                "{:?} should defer to CLI when on-disk artefact is missing",
                cli
            );
        }
        // Unknown CLI: always true (we don't know how to check it).
        assert!(key_is_resumable_on_disk_in(&home, &CliSource::Unknown("codex".into()), "x"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_key_resumable_returns_false_for_phantom_jsonl_with_only_meta() {
        // Tight repro of the Claude "phantom session" bug.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("claude-resumable-phantom");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-proj");
        fs::create_dir_all(&proj).unwrap();
        let key = "ffffffff-2222-3333-4444-555555555555";
        write_file(&proj.join(format!("{}.jsonl", key)),
            "{\"type\":\"permission-mode\",\"sessionId\":\"ffffffff-2222-3333-4444-555555555555\"}\n\
             {\"type\":\"file-history-snapshot\",\"messageId\":\"x\",\"snapshot\":{\"trackedFileBackups\":{}}}\n\
             {\"type\":\"user\",\"isMeta\":true,\"message\":{\"role\":\"user\",\"content\":\"<local-command-caveat>...</local-command-caveat>\"}}\n\
             {\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"<command-name>/model</command-name>\"}}\n\
             {\"type\":\"last-prompt\",\"sessionId\":\"ffffffff-2222-3333-4444-555555555555\"}\n");
        assert!(!key_is_resumable_on_disk_in(&home, &CliSource::Claude, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn claude_key_resumable_returns_true_for_real_jsonl() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("claude-resumable-real");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-proj");
        fs::create_dir_all(&proj).unwrap();
        let key = "eeeeeeee-1111-2222-3333-444444444444";
        write_file(&proj.join(format!("{}.jsonl", key)),
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"}}\n\
             {\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"hi\"}}\n");
        assert!(key_is_resumable_on_disk_in(&home, &CliSource::Claude, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn copilot_key_resumable_returns_false_when_events_jsonl_missing() {
        // Tight repro of the Copilot "phantom session" bug: opening
        // `copilot` and exiting immediately writes a workspace.yaml
        // (171 bytes of stub) but no events.jsonl. Pressing Enter on
        // the resulting Ended row would launch `copilot --resume=<id>`
        // and dead-end on
        // `Error: No session, task, or name matched '<id>'`.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("copilot-resumable-phantom");
        let key = "55ce9f84-3a48-40d5-91d7-983e74dbe29c";
        let dir = home.join(".copilot").join("session-state").join(key);
        fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join("workspace.yaml"),
            "id: 55ce9f84-3a48-40d5-91d7-983e74dbe29c\ncwd: C:\\Users\\me\nsummary_count: 0\n");
        // No events.jsonl.
        assert!(!key_is_resumable_on_disk_in(&home, &CliSource::Copilot, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn copilot_key_resumable_returns_false_when_events_jsonl_empty() {
        // Variant: events.jsonl exists but is zero-bytes (touched but
        // never written). Same UX failure as the missing-file case.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("copilot-resumable-empty-events");
        let key = "00000000-0000-0000-0000-000000000abc";
        let dir = home.join(".copilot").join("session-state").join(key);
        fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join("workspace.yaml"), "id: x\ncwd: C:\\x\n");
        write_file(&dir.join("events.jsonl"), "");
        assert!(!key_is_resumable_on_disk_in(&home, &CliSource::Copilot, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn copilot_key_resumable_returns_true_when_events_jsonl_has_content() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("copilot-resumable-real");
        let key = "11111111-1111-1111-1111-111111111111";
        let dir = home.join(".copilot").join("session-state").join(key);
        fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join("workspace.yaml"), "id: x\ncwd: C:\\x\nsummary: Real Work\n");
        write_file(&dir.join("events.jsonl"), "{\"type\":\"session.start\"}\n");
        assert!(key_is_resumable_on_disk_in(&home, &CliSource::Copilot, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_key_resumable_returns_false_for_header_only_jsonl() {
        // Tight repro of the Gemini "phantom session" bug: opening
        // `gemini` and exiting immediately writes only the session
        // header line — no user/tool exchange. Real on-disk evidence
        // from the bug report: a 228-byte file containing just the
        // sessionId / startTime header.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("gemini-resumable-phantom");
        let chats = home.join(".gemini").join("tmp").join("p").join("chats");
        fs::create_dir_all(&chats).unwrap();
        let key = "aaaaaaaa-24c2-4d75-9f4b-57017e7e6cc0";
        write_file(&chats.join("session-2026-05-24T09-01-phantom.jsonl"),
            "{\"sessionId\":\"aaaaaaaa-24c2-4d75-9f4b-57017e7e6cc0\",\"projectHash\":\"x\",\"startTime\":\"2026-05-24T09:01:40.254Z\",\"kind\":\"main\"}\n");
        assert!(!key_is_resumable_on_disk_in(&home, &CliSource::Gemini, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_key_resumable_returns_true_when_jsonl_has_user_record() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("gemini-resumable-real");
        let chats = home.join(".gemini").join("tmp").join("p").join("chats");
        fs::create_dir_all(&chats).unwrap();
        let key = "abcd1234-1111-2222-3333-444444444444";
        write_file(&chats.join("session-2026-05-24T10-00-abcd.jsonl"),
            "{\"sessionId\":\"abcd1234-1111-2222-3333-444444444444\",\"projectHash\":\"x\",\"startTime\":\"2026-05-24T10:00:00Z\",\"kind\":\"main\"}\n\
             {\"type\":\"user\",\"content\":[{\"text\":\"hi\"}]}\n");
        assert!(key_is_resumable_on_disk_in(&home, &CliSource::Gemini, key));
        let _ = fs::remove_dir_all(&home);
    }

    // ─── Strict probe (used by the live-registry prune) ─────────────

    #[test]
    fn strict_probe_returns_false_when_artefact_missing_for_managed_clis() {
        // The strict probe is the one the post-`SessionStopped` /
        // post-`PaneClosed` prune uses. Its contract differs from
        // `key_is_resumable_on_disk_in` precisely on the
        // missing-artefact case: a live-tracked row whose CLI never
        // wrote anything to disk is conclusively a phantom (the most
        // common shape is ACP-launched `claude` that the user exits
        // without typing — Claude writes no JSONL at all). This is
        // exactly the path the lenient probe gets wrong, leaving the
        // row stuck Ended in session management view.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("strict-probe-missing");
        for cli in [CliSource::Claude, CliSource::Codex, CliSource::Copilot, CliSource::Gemini] {
            assert!(
                !key_has_definite_resumable_content_in(&home, &cli, "no-such-id"),
                "{:?} strict probe must report phantom when artefact is missing",
                cli
            );
        }
        // Unknown CLI: still true (we have no way to verify).
        assert!(key_has_definite_resumable_content_in(
            &home,
            &CliSource::Unknown("codex".into()),
            "x"
        ));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn strict_probe_returns_true_for_real_claude_jsonl() {
        // Symmetric check: when the JSONL exists and has real
        // content, the strict probe agrees with the lenient one
        // (resumable). This is the no-false-positive guard for the
        // prune.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("strict-probe-real-claude");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-proj");
        fs::create_dir_all(&proj).unwrap();
        let key = "real-claude-1111-2222-3333-444444444444";
        write_file(&proj.join(format!("{}.jsonl", key)),
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n");
        assert!(key_has_definite_resumable_content_in(&home, &CliSource::Claude, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn strict_probe_returns_false_for_phantom_claude_jsonl() {
        // The phantom case the loader already filters at startup —
        // strict probe must agree at prune time so live-tracked rows
        // are removed consistently.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("strict-probe-phantom-claude");
        let projects = home.join(".claude").join("projects");
        let proj = projects.join("C--Users-me-proj");
        fs::create_dir_all(&proj).unwrap();
        let key = "phantom-1111-2222-3333-444444444444";
        write_file(&proj.join(format!("{}.jsonl", key)),
            "{\"type\":\"permission-mode\",\"sessionId\":\"phantom\"}\n\
             {\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"<local-command-caveat>...\"}}\n");
        assert!(!key_has_definite_resumable_content_in(&home, &CliSource::Claude, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn strict_probe_returns_false_for_copilot_dir_with_empty_events() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("strict-probe-copilot-empty");
        let key = "11111111-2222-3333-4444-555555555555";
        let dir = home.join(".copilot").join("session-state").join(key);
        fs::create_dir_all(&dir).unwrap();
        write_file(&dir.join("workspace.yaml"), "id: x\n");
        write_file(&dir.join("events.jsonl"), "");
        assert!(!key_has_definite_resumable_content_in(&home, &CliSource::Copilot, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn strict_probe_returns_false_for_gemini_header_only_jsonl() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("strict-probe-gemini-header");
        let chats = home.join(".gemini").join("tmp").join("p").join("chats");
        fs::create_dir_all(&chats).unwrap();
        let key = "abcd1234-1111-2222-3333-444444444444";
        write_file(&chats.join("session-2026-05-24T10-00-abcd.jsonl"),
            "{\"sessionId\":\"abcd1234-1111-2222-3333-444444444444\",\"projectHash\":\"x\",\"startTime\":\"2026-05-24T10:00:00Z\",\"kind\":\"main\"}\n");
        assert!(!key_has_definite_resumable_content_in(&home, &CliSource::Gemini, key));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_loader_picks_up_session_files_and_resolves_cwd() {
        let home = tmp_root("gemini-home");
        write_file(&home.join(".gemini").join("projects.json"),
            r#"{"projects":{"C:\\Users\\me\\proj":"meproj"}}"#);
        let chats = home.join(".gemini").join("tmp").join("meproj").join("chats");
        fs::create_dir_all(&chats).unwrap();
        // Gemini JSONL: first line is the session header, subsequent lines
        // are individual messages.
        write_file(&chats.join("session-2026-05-03T10-47-abcd.jsonl"),
            "{\"sessionId\":\"abcd-1234\",\"projectHash\":\"x\",\"startTime\":\"2026-05-03T10:47:50Z\",\"kind\":\"main\"}\n\
             {\"type\":\"info\",\"content\":\"version up\"}\n\
             {\"type\":\"user\",\"content\":[{\"text\":\"explain build system\"}]}\n");
        // Per-subagent files in nested subdirectories must NOT be picked up.
        let subdir = chats.join("aaaa-bbbb");
        fs::create_dir_all(&subdir).unwrap();
        write_file(&subdir.join("inner.jsonl"), "{}\n");

        let v = load_gemini(&home);
        assert_eq!(v.len(), 1, "expected exactly one Gemini session, got {:?}",
            v.iter().map(|s| (s.key.clone(), s.title.clone())).collect::<Vec<_>>());
        let s = &v[0];
        assert_eq!(s.key, "abcd-1234");
        assert_eq!(s.cli_source, CliSource::Gemini);
        assert_eq!(s.cwd, PathBuf::from("C:\\Users\\me\\proj"));
        assert_eq!(s.title, "explain build system");
        assert_eq!(s.status, AgentStatus::Historical);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_loader_rejects_legacy_dot_json_files() {
        // Single-object `.json` was a transient layout. Latest Gemini went
        // back to `.jsonl`, so loader must NOT pick up `.json` files (they
        // would parse as one massive JSON line and confuse `parse_gemini_meta`).
        let home = tmp_root("gemini-home-rejects-json");
        write_file(&home.join(".gemini").join("projects.json"),
            r#"{"projects":{"C:\\proj":"p"}}"#);
        let chats = home.join(".gemini").join("tmp").join("p").join("chats");
        fs::create_dir_all(&chats).unwrap();
        write_file(&chats.join("session-2026-05-03T10-47-abcd.json"),
            "{\"sessionId\":\"json-id\",\"messages\":[]}");
        let v = load_gemini(&home);
        assert!(v.is_empty(), "`.json` files must be ignored: got {:?}",
            v.iter().map(|s| s.key.clone()).collect::<Vec<_>>());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_loader_skips_files_not_starting_with_session_prefix() {
        let home = tmp_root("gemini-home-skip");
        let chats = home.join(".gemini").join("tmp").join("p").join("chats");
        fs::create_dir_all(&chats).unwrap();
        write_file(&chats.join("notes.jsonl"),
            "{\"sessionId\":\"x\"}\n");

        let v = load_gemini(&home);
        assert!(v.is_empty(), "non-session-prefixed files must be ignored");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_loader_skips_phantom_header_only_sessions() {
        // Reproduces the in-the-wild Gemini phantom-session bug:
        // opening `gemini` and exiting immediately leaves a JSONL
        // on disk containing only the session header line (228 bytes)
        // — or sometimes two duplicate header lines (456 bytes). The
        // loader used to surface these in session management view with the synthetic title
        // `gemini <8-char>` (because `first_user_text_jsonl` returned
        // None), and Enter on them would launch
        // `gemini --resume <id>` and dead-end.
        let home = tmp_root("gemini-phantom-header-only");
        write_file(&home.join(".gemini").join("projects.json"),
            r#"{"projects":{"C:\\proj":"p"}}"#);
        let chats = home.join(".gemini").join("tmp").join("p").join("chats");
        fs::create_dir_all(&chats).unwrap();

        // Phantom: single header line, no `type` field anywhere.
        write_file(&chats.join("session-2026-05-24T09-01-phantom.jsonl"),
            "{\"sessionId\":\"aaaaaaaa-24c2-4d75-9f4b-57017e7e6cc0\",\"projectHash\":\"x\",\"startTime\":\"2026-05-24T09:01:40.254Z\",\"kind\":\"main\"}\n");

        // Phantom: two duplicate header lines (the 456-byte shape).
        write_file(&chats.join("session-2026-05-24T09-01-a5e06b8a.jsonl"),
            "{\"sessionId\":\"a5e06b8a-28a1-4e64-9802-f8b4572e832d\",\"projectHash\":\"x\",\"startTime\":\"2026-05-24T09:01:43.027Z\",\"kind\":\"main\"}\n\
             {\"sessionId\":\"a5e06b8a-28a1-4e64-9802-f8b4572e832d\",\"projectHash\":\"x\",\"startTime\":\"2026-05-24T09:01:43.039Z\",\"kind\":\"main\"}\n");

        // Real: header + at least one record carrying a `type` field.
        write_file(&chats.join("session-2026-05-24T10-00-real.jsonl"),
            "{\"sessionId\":\"eeeeeeee-2222-3333-4444-555555555555\",\"projectHash\":\"x\",\"startTime\":\"2026-05-24T10:00:00Z\",\"kind\":\"main\"}\n\
             {\"type\":\"user\",\"content\":[{\"text\":\"hello\"}]}\n");

        let v = load_gemini(&home);
        assert_eq!(v.len(), 1,
            "only the real session should survive; got {:?}",
            v.iter().map(|s| s.key.clone()).collect::<Vec<_>>());
        assert_eq!(v[0].key, "eeeeeeee-2222-3333-4444-555555555555");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_loader_keeps_session_with_info_record_only() {
        // Defensive: a session that has a `type:"info"` line but no
        // `type:"user"` (e.g. Gemini emitted a startup notice and the
        // user exited before typing) is still listed — the title
        // falls back to `gemini <8-char>` but the row at least has
        // *some* real content beyond the header, and Gemini's own
        // `--resume` may still load it. Don't over-filter.
        let home = tmp_root("gemini-info-only");
        write_file(&home.join(".gemini").join("projects.json"),
            r#"{"projects":{"C:\\proj":"p"}}"#);
        let chats = home.join(".gemini").join("tmp").join("p").join("chats");
        fs::create_dir_all(&chats).unwrap();
        write_file(&chats.join("session-2026-05-24T10-00-info.jsonl"),
            "{\"sessionId\":\"cccccccc-1111-2222-3333-444444444444\",\"projectHash\":\"x\",\"startTime\":\"2026-05-24T10:00:00Z\",\"kind\":\"main\"}\n\
             {\"type\":\"info\",\"content\":\"Update successful!\"}\n");
        let v = load_gemini(&home);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].key, "cccccccc-1111-2222-3333-444444444444");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn loaders_are_ok_when_directory_missing() {
        let nowhere = std::env::temp_dir().join("definitely-not-here-zzzzzz");
        // Should not panic; should return empty.
        assert!(load_copilot(&nowhere).is_empty());
        assert!(load_claude(&nowhere).is_empty());
        assert!(load_gemini(&nowhere).is_empty());
    }

    #[test]
    fn copilot_sessions_sorted_newest_first() {
        let home = tmp_root("copilot-sort");
        let base = home.join(".copilot").join("session-state");

        for (i, sid) in ["s-1", "s-2", "s-3"].iter().enumerate() {
            let d = base.join(sid);
            fs::create_dir_all(&d).unwrap();
            write_file(&d.join("workspace.yaml"),
                &format!("id: {}\ncwd: C:\\proj\nsummary: title-{}\n", sid, i));
            write_file(&d.join("events.jsonl"), "{}\n");
            // Stagger mtimes by overwriting the events file with a slight delay
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        let v = load_copilot(&home);
        assert_eq!(v.len(), 3);
        assert!(v[0].last_activity_at >= v[1].last_activity_at);
        assert!(v[1].last_activity_at >= v[2].last_activity_at);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn select_top_candidates_skips_class_a_sorts_and_truncates() {
        use std::time::Duration;
        let at = |secs: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
        let mk = |id: &str, secs: u64| Candidate {
            id: id.to_string(),
            sort_time: at(secs),
            path: PathBuf::from(id),
            cwd: None,
        };
        let mut index = HashSet::new();
        index.insert("class-a".to_string());

        let candidates = vec![
            mk("old", 100),
            mk("class-a", 999), // newest, but Class A → must be dropped
            mk("new", 300),
            mk("mid", 200),
        ];
        let top = select_top_candidates(candidates, &index, 2);
        // Class A dropped, remaining sorted newest-first, truncated to 2.
        assert_eq!(
            top.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            ["new", "mid"]
        );
    }

    #[test]
    fn cli_scan_flags_selects_one_loader_per_known_cli() {
        assert_eq!(cli_scan_flags(Some(&CliSource::Copilot)), (true, false, false, false));
        assert_eq!(cli_scan_flags(Some(&CliSource::Claude)), (false, true, false, false));
        assert_eq!(cli_scan_flags(Some(&CliSource::Gemini)), (false, false, true, false));
        assert_eq!(cli_scan_flags(Some(&CliSource::Codex)), (false, false, false, true));
    }

    #[test]
    fn cli_scan_flags_scans_all_for_none_or_custom_agent() {
        // None (no resolvable CLI) and a custom/unknown agent both scan all
        // four — matching the view, which shows every CLI when the
        // current_cli_filter() is None.
        assert_eq!(cli_scan_flags(None), (true, true, true, true));
        assert_eq!(
            cli_scan_flags(Some(&CliSource::Unknown("custom:my-agent".to_string()))),
            (true, true, true, true)
        );
    }

    #[test]
    fn load_copilot_indexed_skips_agent_pane_sessions() {
        let home = tmp_root("copilot-class-a");
        let base = home.join(".copilot").join("session-state");
        for sid in ["shell-1", "agent-pane-1"] {
            let d = base.join(sid);
            fs::create_dir_all(&d).unwrap();
            write_file(&d.join("workspace.yaml"), &format!("id: {sid}\ncwd: C:\\proj\nsummary: t\n"));
            write_file(&d.join("events.jsonl"), "{}\n");
        }
        let mut index = HashSet::new();
        index.insert("agent-pane-1".to_string());

        let v = load_copilot_indexed(&home, &index);
        assert_eq!(v.len(), 1, "the agent-pane (Class A) session must be skipped");
        assert_eq!(v[0].key, "shell-1");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn read_first_line_returns_first_non_empty_line() {
        let home = tmp_root("first-line");
        let p = home.join("f.jsonl");
        write_file(&p, "\n\n  \n{\"a\":1}\n{\"b\":2}\n");
        assert_eq!(read_first_line(&p).as_deref().map(str::trim), Some("{\"a\":1}"));
        let empty = home.join("empty.jsonl");
        write_file(&empty, "");
        assert!(read_first_line(&empty).is_none());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn read_first_line_caps_oversize_unterminated_line() {
        // A corrupt / non-JSONL file that is one giant line with no newline
        // must not be slurped whole: the read is bounded at
        // HEADER_LINE_BYTES_CAP, so we get back at most the cap (the
        // truncated text then fails the downstream JSON header parse).
        let home = tmp_root("first-line-cap");
        let p = home.join("giant.jsonl");
        let giant = "x".repeat(HEADER_LINE_BYTES_CAP as usize + 4096);
        write_file(&p, &giant);
        let got = read_first_line(&p).expect("returns the bounded prefix");
        assert!(
            got.len() <= HEADER_LINE_BYTES_CAP as usize,
            "read_first_line must cap the read, got {} bytes",
            got.len()
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn gemini_session_id_from_header_reads_header_only() {
        let home = tmp_root("gem-header");
        // Header (sessionId, no `type`) on line 1 → returns the id without
        // reading the rest of the (potentially huge) transcript.
        let p = home.join("session-x.jsonl");
        write_file(
            &p,
            "{\"sessionId\":\"abc-123\",\"projectHash\":\"h\",\"kind\":\"main\"}\n\
             {\"id\":\"m\",\"type\":\"user\",\"content\":\"hi\"}\n",
        );
        assert_eq!(gemini_session_id_from_header(&p).as_deref(), Some("abc-123"));
        // First line is a `type` record (no header) → None (matches parse_gemini_meta).
        let p2 = home.join("session-y.jsonl");
        write_file(&p2, "{\"id\":\"m\",\"type\":\"user\",\"content\":\"hi\"}\n");
        assert!(gemini_session_id_from_header(&p2).is_none());
        let _ = fs::remove_dir_all(&home);
    }

    // ─── Codex tests ────────────────────────────────────────────────────

    fn codex_session_path(home: &Path, yyyy: &str, mm: &str, dd: &str, iso: &str, id: &str) -> PathBuf {
        let dir = home.join(".codex").join("sessions").join(yyyy).join(mm).join(dd);
        fs::create_dir_all(&dir).unwrap();
        dir.join(format!("rollout-{}-{}.jsonl", iso, id))
    }

    fn codex_meta_line(id: &str, ts: &str, cwd: &str) -> String {
        format!(
            "{{\"timestamp\":\"{ts}\",\"type\":\"session_meta\",\
\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"{ts}\",\"cwd\":\"{cwd}\",\
\"originator\":\"codex-tui\",\"cli_version\":\"0.1.0\",\"source\":\"cli\"}}}}\n")
    }

    fn codex_subagent_meta_line(id: &str, parent: &str, ts: &str, cwd: &str) -> String {
        format!(
            "{{\"timestamp\":\"{ts}\",\"type\":\"session_meta\",\
\"payload\":{{\"id\":\"{id}\",\"forked_from_id\":\"{parent}\",\"timestamp\":\"{ts}\",\"cwd\":\"{cwd}\",\
\"originator\":\"codex-tui\",\"cli_version\":\"0.1.0\",\
\"source\":{{\"subagent\":{{\"thread_spawn\":{{\"parent_thread_id\":\"{parent}\",\"depth\":1}}}}}}}}}}\n")
    }

    fn codex_user_msg_line(ts: &str, text: &str) -> String {
        format!(
            "{{\"timestamp\":\"{ts}\",\"type\":\"event_msg\",\
\"payload\":{{\"type\":\"user_message\",\"message\":\"{text}\"}}}}\n")
    }

    #[test]
    fn load_codex_returns_one_row_per_real_rollout_file() {
        let home = tmp_root("load-codex-basic");
        let id = "11111111-2222-3333-4444-555555555555";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T10-30-00", id);
        let body = codex_meta_line(id, "2026-05-28T10:30:00Z", "C:/work/proj")
            + &codex_user_msg_line("2026-05-28T10:30:05Z", "summarize this repo");
        write_file(&path, &body);
        let rows = load_codex(&home);
        assert_eq!(rows.len(), 1, "expected one row, got {:?}", rows);
        let row = &rows[0];
        assert_eq!(row.cli_source, crate::agent_sessions::CliSource::Codex);
        assert_eq!(row.key, id, "key must be the rollout UUID");
        assert_eq!(row.cwd, PathBuf::from("C:/work/proj"));
        assert!(row.title.contains("summarize this repo"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn load_codex_skips_phantom_meta_only_files() {
        let home = tmp_root("load-codex-phantom");
        let id = "deadbeef-2222-3333-4444-555555555555";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T11-00-00", id);
        write_file(&path, &codex_meta_line(id, "2026-05-28T11:00:00Z", "C:/x"));
        assert_eq!(load_codex(&home).len(), 0, "phantom (meta-only) must be filtered out");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn load_codex_skips_subagent_fork() {
        // Codex's multi_agent_v1/spawn_agent forks a child thread that gets its
        // own rollout (source.subagent) and inherits the parent's first user
        // message — so it has an identical title. It is a codex-internal worker,
        // not a user session, and must not appear as a (duplicate) row.
        let home = tmp_root("load-codex-subagent");
        let parent = "11111111-2222-3333-4444-555555555555";
        let child  = "99999999-2222-3333-4444-555555555555";
        let pp = codex_session_path(&home, "2026", "06", "10", "2026-06-10T13-14-32", parent);
        write_file(
            &pp,
            &(codex_meta_line(parent, "2026-06-10T13:14:32Z", "C:/w")
                + &codex_user_msg_line("2026-06-10T13:14:43Z", "start new tab agent pane session")),
        );
        let cp = codex_session_path(&home, "2026", "06", "10", "2026-06-10T13-15-12", child);
        write_file(
            &cp,
            &(codex_subagent_meta_line(child, parent, "2026-06-10T13:15:12Z", "C:/w")
                + &codex_user_msg_line("2026-06-10T13:15:12Z", "start new tab agent pane session")),
        );

        let rows = load_codex(&home);
        assert_eq!(rows.len(), 1, "subagent fork must be filtered, got {:?}", rows);
        assert_eq!(rows[0].key, parent, "only the top-level session should survive");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_payload_is_subagent_discriminates_source() {
        let cli = serde_json::json!({ "source": "cli" });
        assert!(!codex_payload_is_subagent(&cli), "top-level source=\"cli\" is not a subagent");
        let sub = serde_json::json!({ "source": { "subagent": { "thread_spawn": { "depth": 1 } } } });
        assert!(codex_payload_is_subagent(&sub), "source.subagent must be detected");
    }

    #[test]
    fn load_codex_skips_phantom_meta_plus_env_context_only() {
        let home = tmp_root("load-codex-env-only");
        let id = "deadbeef-3333-3333-3333-333333333333";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T11-30-00", id);
        let env_line = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"<environment_context>cwd=C:/x</environment_context>\"}}]}}}}\n");
        write_file(&path, &(codex_meta_line(id, "2026-05-28T11:30:00Z", "C:/x") + &env_line));
        assert_eq!(load_codex(&home).len(), 0,
                   "meta + environment_context wrapper alone must be classified phantom");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn load_codex_orders_newest_first_by_payload_timestamp() {
        let home = tmp_root("load-codex-order");
        for (i, ts) in [
            (0u32, "2026-05-28T10:00:00Z"),
            (1u32, "2026-05-28T10:05:00Z"),
            (2u32, "2026-05-28T10:10:00Z"),
        ] {
            let id = format!("aaaaaaaa-{:04}-3333-4444-555555555555", i);
            let iso = ts.replace(':', "-").trim_end_matches('Z').to_string();
            let path = codex_session_path(&home, "2026", "05", "28", &iso, &id);
            write_file(&path,
                &(codex_meta_line(&id, ts, "C:/x")
                  + &codex_user_msg_line(ts, &format!("prompt {i}"))));
        }
        let rows = load_codex(&home);
        assert_eq!(rows.len(), 3);
        assert!(rows[0].title.contains("prompt 2"),
                "newest first; got titles {:?}",
                rows.iter().map(|r| &r.title).collect::<Vec<_>>());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_session_has_real_content_is_conservative_on_io_error() {
        let nowhere = PathBuf::from("Z:/definitely/does/not/exist.jsonl");
        assert!(codex_session_has_real_content(&nowhere),
                "must default to true when the file can't be opened");
    }

    #[test]
    fn codex_session_has_real_content_detects_user_message() {
        let home = tmp_root("codex-scan-user");
        let id = "abcd0001-2222-3333-4444-555555555555";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T12-00-00", id);
        write_file(&path,
            &(codex_meta_line(id, "2026-05-28T12:00:00Z", "C:/x")
              + &codex_user_msg_line("2026-05-28T12:00:05Z", "hi")));
        assert!(codex_session_has_real_content(&path));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_session_has_real_content_detects_agent_message() {
        let home = tmp_root("codex-scan-agent");
        let id = "abcd0002-2222-3333-4444-555555555555";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T12-30-00", id);
        let agent_line = "{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"ok\"}}\n";
        write_file(&path,
            &(codex_meta_line(id, "2026-05-28T12:30:00Z", "C:/x") + agent_line));
        assert!(codex_session_has_real_content(&path));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_title_falls_back_to_response_item_user_skipping_env_context() {
        let home = tmp_root("codex-title-fallback");
        let id = "abcdef00-3333-3333-3333-333333333333";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T13-00-00", id);
        let env = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"<environment_context>cwd=C:/x</environment_context>\"}}]}}}}\n");
        let real = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"refactor the parser\"}}]}}}}\n");
        write_file(&path, &(codex_meta_line(id, "2026-05-28T13:00:00Z", "C:/x") + &env + &real));
        let rows = load_codex(&home);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].title.contains("refactor the parser"),
                "got title: {:?}", rows[0].title);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_title_skips_injected_agents_md_instructions() {
        // codex auto-loads AGENTS.md when the cwd has one and prepends it as a
        // synthetic user-role response_item ("# AGENTS.md instructions for …")
        // BEFORE the real prompt. It must not become the session title — the
        // real prompt must win. Regression for the 69-char AGENTS.md-heading
        // title seen on sessions run inside the intelligent-terminal repo.
        let home = tmp_root("codex-title-agents-md");
        let id = "abcdef00-4444-4444-4444-444444444444";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T14-00-00", id);
        let agents = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"# AGENTS.md instructions for C:/proj\\n\\n<INSTRUCTIONS>\\n be concise \\n</INSTRUCTIONS>\"}}]}}}}\n");
        let real = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"friday is wonderful\"}}]}}}}\n");
        write_file(&path, &(codex_meta_line(id, "2026-05-28T14:00:00Z", "C:/proj") + &agents + &real));
        let rows = load_codex(&home);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "friday is wonderful",
                   "AGENTS.md injection must be skipped; got: {:?}", rows[0].title);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_session_with_only_injected_context_is_phantom() {
        // meta + <environment_context> + AGENTS.md injection, but no real user
        // turn → phantom (must not surface as a resumable session titled with a
        // doc heading). Guards the shared `codex_user_text_is_synthetic` used by
        // `codex_session_has_real_content`.
        let home = tmp_root("codex-phantom-agents-md");
        let id = "abcdef00-5555-5555-5555-555555555555";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T15-00-00", id);
        let env = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"<environment_context>cwd=C:/proj</environment_context>\"}}]}}}}\n");
        let agents = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"# AGENTS.md instructions for C:/proj\"}}]}}}}\n");
        write_file(&path, &(codex_meta_line(id, "2026-05-28T15:00:00Z", "C:/proj") + &env + &agents));
        assert_eq!(load_codex(&home).len(), 0,
                   "meta + env_context + AGENTS.md injection alone must be phantom");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_user_text_is_synthetic_recognizes_bare_and_dir_headings() {
        // Project AGENTS.md (directory=Some) -> "# AGENTS.md instructions for <dir>".
        assert!(codex_user_text_is_synthetic(
            "# AGENTS.md instructions for C:/proj\n\n<INSTRUCTIONS>\n be concise \n</INSTRUCTIONS>"
        ));
        // Global ~/.codex/AGENTS.md (directory=None) -> bare "# AGENTS.md instructions".
        assert!(codex_user_text_is_synthetic(
            "# AGENTS.md instructions\n\n<INSTRUCTIONS>\n be concise \n</INSTRUCTIONS>"
        ));
        // Bare heading with nothing after it, and with tolerated leading space.
        assert!(codex_user_text_is_synthetic("# AGENTS.md instructions"));
        assert!(codex_user_text_is_synthetic("  # AGENTS.md instructions\n"));
        // XML-ish wrapper block.
        assert!(codex_user_text_is_synthetic(
            "<environment_context>cwd=C:/x</environment_context>"
        ));
        // A real prompt that merely opens with the phrase mid-line is NOT synthetic.
        assert!(!codex_user_text_is_synthetic(
            "# AGENTS.md instructions are unclear, please rewrite them"
        ));
        // An ordinary prompt is not synthetic.
        assert!(!codex_user_text_is_synthetic("fix the build"));
    }

    #[test]
    fn codex_title_skips_injected_bare_agents_md_instructions() {
        // A GLOBAL ~/.codex/AGENTS.md makes codex emit the *bare* heading
        // "# AGENTS.md instructions" (directory=None) before the real prompt —
        // issue #339's still-leaking variant (the "for <dir>" form was already
        // skipped). The real prompt must still win the title.
        let home = tmp_root("codex-title-bare-agents-md");
        let id = "abcdef00-6666-6666-6666-666666666666";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T16-00-00", id);
        let agents = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"# AGENTS.md instructions\\n\\n<INSTRUCTIONS>\\n be concise \\n</INSTRUCTIONS>\"}}]}}}}\n");
        let real = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"fix the build\"}}]}}}}\n");
        write_file(&path, &(codex_meta_line(id, "2026-05-28T16:00:00Z", "C:/proj") + &agents + &real));
        let rows = load_codex(&home);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "fix the build",
                   "bare AGENTS.md injection must be skipped; got: {:?}", rows[0].title);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_session_with_only_bare_agents_md_is_phantom() {
        // meta + <environment_context> + *bare* AGENTS.md injection, no real user
        // turn → phantom. Guards codex_user_text_is_synthetic on the
        // global-AGENTS.md form via codex_session_has_real_content.
        let home = tmp_root("codex-phantom-bare-agents-md");
        let id = "abcdef00-7777-7777-7777-777777777777";
        let path = codex_session_path(&home, "2026", "05", "28", "2026-05-28T17-00-00", id);
        let env = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"<environment_context>cwd=C:/proj</environment_context>\"}}]}}}}\n");
        let agents = format!(
            "{{\"type\":\"response_item\",\"payload\":{{\"role\":\"user\",\
\"content\":[{{\"text\":\"# AGENTS.md instructions\\n\\n<INSTRUCTIONS>\\n be concise \\n</INSTRUCTIONS>\"}}]}}}}\n");
        write_file(&path, &(codex_meta_line(id, "2026-05-28T17:00:00Z", "C:/proj") + &env + &agents));
        assert_eq!(load_codex(&home).len(), 0,
                   "meta + env_context + bare AGENTS.md injection alone must be phantom");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_key_resumable_returns_true_when_artefact_missing() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("codex-resumable-missing");
        // Lenient probe: missing on-disk artefact defers to CLI (true)
        // so fresh in-memory rows aren't blocked preemptively.
        assert!(key_is_resumable_on_disk_in(&home, &CliSource::Codex, "no-such-id"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_key_resumable_returns_false_for_meta_only_jsonl() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("codex-resumable-phantom");
        let id = "ffffffff-2222-3333-4444-555555555555";
        // Build the meta-only file inline. The path shape is:
        //   home/.codex/sessions/2026/05/28/rollout-2026-05-28T10-00-00-<id>.jsonl
        let dir = home.join(".codex").join("sessions").join("2026").join("05").join("28");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-05-28T10-00-00-{}.jsonl", id));
        let meta = format!("{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-05-28T10:00:00Z\",\"cwd\":\"C:/x\",\"originator\":\"codex-tui\",\"cli_version\":\"0.1.0\",\"source\":\"cli\"}}}}\n");
        fs::write(&path, meta).unwrap();
        assert!(!key_is_resumable_on_disk_in(&home, &CliSource::Codex, id));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_key_resumable_returns_true_for_jsonl_with_user_message() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("codex-resumable-real");
        let id = "abcdef00-2222-3333-4444-555555555555";
        let dir = home.join(".codex").join("sessions").join("2026").join("05").join("28");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-05-28T10-30-00-{}.jsonl", id));
        let content = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-05-28T10:30:00Z\",\"cwd\":\"C:/x\",\"originator\":\"codex-tui\",\"cli_version\":\"0.1.0\",\"source\":\"cli\"}}}}\n\
{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"hi\"}}}}\n");
        fs::write(&path, content).unwrap();
        assert!(key_is_resumable_on_disk_in(&home, &CliSource::Codex, id));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_strict_probe_returns_false_when_artefact_missing() {
        use crate::agent_sessions::CliSource;
        let home = tmp_root("codex-strict-missing");
        assert!(!key_has_definite_resumable_content_in(&home, &CliSource::Codex, "no-id"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn codex_title_for_key_finds_user_message() {
        let home = tmp_root("codex-title-by-key");
        let dir = home.join(".codex").join("sessions").join("2026").join("05").join("28");
        fs::create_dir_all(&dir).unwrap();
        let id = "cafebabe-1111-2222-3333-444444444444";
        let path = dir.join(format!("rollout-2026-05-28T12-00-00-{}.jsonl", id));
        write_file(&path,
            &format!("{{\"timestamp\":\"2026-05-28T12:00:00Z\",\"type\":\"session_meta\",\
\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-05-28T12:00:00Z\",\
\"cwd\":\"C:/x\",\"originator\":\"codex-tui\",\"cli_version\":\"0.1.0\",\"source\":\"cli\"}}}}\n\
{{\"timestamp\":\"2026-05-28T12:00:05Z\",\"type\":\"event_msg\",\
\"payload\":{{\"type\":\"user_message\",\"message\":\"refactor the parser\"}}}}\n"));
        assert_eq!(codex_title_for_key(&home, id).as_deref(), Some("refactor the parser"));
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn codex_title_for_key_returns_none_for_unknown_id() {
        let home = tmp_root("codex-title-missing");
        assert_eq!(codex_title_for_key(&home, "no-such-id"), None);
        fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn parse_iso_handles_positive_offset() {
        // 2026-05-27T10:53:09+08:00 is 2026-05-27T02:53:09Z
        let t1 = parse_iso_to_system_time("2026-05-27T10:53:09+08:00").unwrap();
        let t2 = parse_iso_to_system_time("2026-05-27T02:53:09Z").unwrap();
        assert_eq!(t1, t2);
    }

    #[test]
    fn parse_iso_handles_negative_offset() {
        // 2026-05-27T02:53:09-05:00 is 2026-05-27T07:53:09Z
        let t1 = parse_iso_to_system_time("2026-05-27T02:53:09-05:00").unwrap();
        let t2 = parse_iso_to_system_time("2026-05-27T07:53:09Z").unwrap();
        assert_eq!(t1, t2);
    }

    #[test]
    fn parse_iso_rejects_pre_1970_years() {
        assert!(parse_iso_to_system_time("1969-12-31T23:59:59Z").is_none());
    }

    #[test]
    fn parse_iso_rejects_invalid_month() {
        assert!(parse_iso_to_system_time("2026-13-01T00:00:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-00-01T00:00:00Z").is_none());
    }

    #[test]
    fn parse_iso_rejects_invalid_day_for_month() {
        assert!(parse_iso_to_system_time("2026-02-30T00:00:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-05-32T00:00:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-04-31T00:00:00Z").is_none()); // April has 30
    }

    #[test]
    fn parse_iso_rejects_invalid_time_components() {
        assert!(parse_iso_to_system_time("2026-05-28T25:30:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-05-28T10:60:00Z").is_none());
        assert!(parse_iso_to_system_time("2026-05-28T10:30:60Z").is_none());
    }

    #[test]
    fn parse_iso_accepts_february_29_leap_year() {
        // 2024 IS a leap year; 2023 is not.
        assert!(parse_iso_to_system_time("2024-02-29T00:00:00Z").is_some());
        assert!(parse_iso_to_system_time("2023-02-29T00:00:00Z").is_none());
    }

    static WSL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn wsl_gate_defaults_on_and_honors_env() {
        let _g = WSL_ENV_LOCK.lock().unwrap();
        std::env::remove_var("WTA_WSL_SESSIONS");
        assert!(wsl_sessions_enabled(), "default must be enabled");
        std::env::set_var("WTA_WSL_SESSIONS", "0");
        assert!(!wsl_sessions_enabled());
        std::env::set_var("WTA_WSL_SESSIONS", "false");
        assert!(!wsl_sessions_enabled());
        // Forgiving parsing: trimmed, case-insensitive, extra falsey spellings.
        std::env::set_var("WTA_WSL_SESSIONS", "  False ");
        assert!(!wsl_sessions_enabled(), "trimmed + case-insensitive False");
        std::env::set_var("WTA_WSL_SESSIONS", "NO");
        assert!(!wsl_sessions_enabled());
        std::env::set_var("WTA_WSL_SESSIONS", "off");
        assert!(!wsl_sessions_enabled());
        std::env::set_var("WTA_WSL_SESSIONS", "1");
        assert!(wsl_sessions_enabled());
        std::env::remove_var("WTA_WSL_SESSIONS");
    }
}
// tools/wta/src/history_loader.rs
//
// Discover historical CLI agent sessions by scanning each CLI's on-disk
// log/state layout. Used to seed the AgentSessionRegistry with `Historical`
// entries on App startup so users can resume past sessions from F2.
//
// Layouts (verified 2026-05):
//   Copilot:  ~/.copilot/session-state/<UUID>/{workspace.yaml,events.jsonl}
//             - session id   = directory name
//             - cwd          = workspace.yaml `cwd:` field
//             - title        = workspace.yaml `summary:` (fallback `name:`)
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
// (Note: per-subagent JSONL files may live in nested `<UUID>/` subdirs of
// `chats/`. Top-level Gemini sessions are flat files named `session-*.jsonl`.
// under `<UUID>/<name>.json`. We only pick up `session-*.json` at the
// top level.)
//
// Sort each list by last_activity desc; cap each CLI at MAX_PER_CLI.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::agent_sessions::{AgentSession, AgentStatus, CliSource, SessionOrigin};

const MAX_PER_CLI: usize = 50;
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

pub fn load_all() -> Vec<AgentSession> {
    let mut out = Vec::new();
    let Some(home) = home_dir() else { return out };
    out.extend(take_n(load_copilot(&home), MAX_PER_CLI));
    out.extend(take_n(load_claude(&home),  MAX_PER_CLI));
    out.extend(take_n(load_gemini(&home),  MAX_PER_CLI));
    // Stamp `origin: AgentPane` on rows whose session id was recorded in
    // the local agent-pane index. Loaded once and applied as a join so the
    // per-CLI scanners stay agnostic of how the index is shaped or where
    // it lives.
    let agent_pane_keys = crate::agent_pane_origin::load_default_set();
    if !agent_pane_keys.is_empty() {
        for s in out.iter_mut() {
            if agent_pane_keys.contains(&s.key) {
                s.origin = SessionOrigin::AgentPane;
            }
        }
    }
    out
}

/// Best-effort title lookup for a single live session. Reads the same
/// per-CLI on-disk artefacts that `load_all` scans, but only for the
/// specific `key`. Used to upgrade synthetic titles (cwd basename) into
/// real ones (workspace.yaml summary / first user prompt) once the CLI
/// has had a chance to write that data — typically a few seconds after
/// the first hook event arrives. Returns `None` if no usable title is
/// on disk (caller keeps whatever synthetic title it had).
pub fn lookup_title_for_session(cli: CliSource, key: &str) -> Option<String> {
    let home = home_dir()?;
    match cli {
        CliSource::Copilot => copilot_title_for_key(&home, key),
        CliSource::Claude  => claude_title_for_key(&home, key),
        CliSource::Gemini  => gemini_title_for_key(&home, key),
        _ => None,
    }
}

fn copilot_title_for_key(home: &Path, key: &str) -> Option<String> {
    let dir = home.join(".copilot").join("session-state").join(key);
    let workspace = dir.join("workspace.yaml");
    let yaml = fs::read_to_string(&workspace).ok()?;
    parse_simple_yaml(&yaml, "summary").filter(|s| !s.is_empty())
        .or_else(|| parse_simple_yaml(&yaml, "name").filter(|s| !s.is_empty()))
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

// ─── Copilot ────────────────────────────────────────────────────────────

fn load_copilot(home: &Path) -> Vec<AgentSession> {
    let base = home.join(".copilot").join("session-state");
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&base) else { return out };

    for entry in rd.flatten() {
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
        let dir = entry.path();
        let id = match dir.file_name().and_then(|n| n.to_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };

        let workspace = dir.join("workspace.yaml");
        let events    = dir.join("events.jsonl");

        // Skip ephemeral / never-used Copilot CLI sessions. Whenever WT (or
        // wta itself) spawns a Copilot CLI process — e.g. as the back-end
        // for an agent pane, a `?prompt` delegate, or a coordinator — that
        // process eagerly creates `~/.copilot/session-state/<UUID>/workspace.yaml`
        // even before the user types anything. If the user never interacts,
        // no `events.jsonl` is ever written. These dirs would otherwise
        // appear at the very top of F2 after each WT restart (most-recent
        // last_activity), masking real historical sessions. Treat the
        // existence of a non-empty `events.jsonl` as the marker for "user
        // actually did something here".
        let has_real_activity = events.metadata()
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if !has_real_activity { continue; }

        let last_activity = events.metadata()
            .and_then(|m| m.modified()).ok()
            .or_else(|| workspace.metadata().and_then(|m| m.modified()).ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let started_at = workspace.metadata()
            .and_then(|m| m.modified()).ok()
            .unwrap_or(last_activity);

        let yaml = fs::read_to_string(&workspace).unwrap_or_default();
        let cwd = parse_simple_yaml(&yaml, "cwd")
            .map(PathBuf::from)
            .unwrap_or_default();
        let title = parse_simple_yaml(&yaml, "summary")
            .filter(|s| !s.is_empty())
            .or_else(|| parse_simple_yaml(&yaml, "name").filter(|s| !s.is_empty()))
            .unwrap_or_else(|| short_id(&id, "copilot"));

        out.push(AgentSession {
            key:               id.clone(),
            cli_source:        CliSource::Copilot,
            pane_session_id:   None,
            window_id:         None,
            tab_id:            None,
            title,
            cwd,
            started_at,
            last_activity_at:  last_activity,
            status:            AgentStatus::Historical,
            last_error:        None,
            current_tool:      None,
            attention_reason:  None,
            log_path:          Some(events),
            origin:            crate::agent_sessions::SessionOrigin::default(),
        });
    }
    out.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
    out
}

// ─── Claude ─────────────────────────────────────────────────────────────

fn load_claude(home: &Path) -> Vec<AgentSession> {
    let base = home.join(".claude").join("projects");
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&base) else { return out };

    for proj_entry in rd.flatten() {
        let proj_dir = proj_entry.path();
        let proj_name = match proj_dir.file_name().and_then(|n| n.to_str()) {
            Some(s) if s != "memory" => s.to_string(),
            _ => continue,
        };
        // Claude's directory-name encoding (`\` -> `-`) is lossy: paths
        // whose segments contain `-` (e.g. `agentic-terminal`) can't be
        // recovered from the directory name alone. Use it only as a
        // fallback — prefer the per-record `cwd` field embedded in the
        // JSONL itself, which preserves the original path verbatim.
        let cwd_fallback = decode_claude_cwd(&proj_name);

        let Ok(files) = fs::read_dir(&proj_dir) else { continue };
        for f in files.flatten() {
            let path = f.path();
            if path.is_dir() { continue; }
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            let id = match path.file_stem().and_then(|n| n.to_str()) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            // Reproduces the "ghost Claude session" bug: launching `claude`
            // and exiting without typing a real prompt (e.g. just running
            // `/model` then Ctrl+D) still leaves a JSONL on disk, but
            // `claude --resume <id>` rejects it with
            // `No conversation found with session ID: <id>`. Mirror the
            // Copilot ghost-session filter so these rows never appear in
            // the F2 view, where Enter would dead-end with that error.
            if !claude_session_has_real_content(&path) { continue; }
            let last_activity = path.metadata().and_then(|m| m.modified()).ok()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let title = first_user_text_jsonl(&path, ClaudeOrGemini::Claude)
                .unwrap_or_else(|| short_id(&id, "claude"));
            let cwd = read_cwd_from_claude_jsonl(&path).unwrap_or_else(|| cwd_fallback.clone());

            out.push(AgentSession {
                key:               id.clone(),
                cli_source:        CliSource::Claude,
                pane_session_id:   None,
                window_id:         None,
                tab_id:            None,
                title,
                cwd,
                started_at:        last_activity,
                last_activity_at:  last_activity,
                status:            AgentStatus::Historical,
                last_error:        None,
                current_tool:      None,
                attention_reason:  None,
                log_path:          Some(path),
                origin:            crate::agent_sessions::SessionOrigin::default(),
            });
        }
    }
    out.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
    out
}

// ─── Gemini ─────────────────────────────────────────────────────────────

fn load_gemini(home: &Path) -> Vec<AgentSession> {
    let tmp = home.join(".gemini").join("tmp");
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(&tmp) else { return out };

    let projects_json = home.join(".gemini").join("projects.json");
    let cwd_lookup    = parse_gemini_projects(&projects_json);

    for proj_entry in rd.flatten() {
        if !proj_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
        let proj_name = match proj_entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let chats = proj_entry.path().join("chats");
        let Ok(files) = fs::read_dir(&chats) else { continue };
        let cwd = cwd_lookup.get(&proj_name).cloned().unwrap_or_default();

        for f in files.flatten() {
            let path = f.path();
            if !is_gemini_session_file(&path) { continue; }

            // Drop phantom Gemini sessions: opening `gemini` and
            // exiting without exchanging a turn leaves a JSONL on
            // disk containing only the session header line(s) —
            // pressing Enter on the row in F2 would launch
            // `gemini --resume <id>` which Gemini rejects (and the
            // synthetic title `gemini <8-char>` from `short_id`
            // exposes the lack of content anyway). Mirrors the
            // Claude and Copilot loader-side filters.
            if !gemini_jsonl_has_real_content(&path) { continue; }

            let (sid, last_updated) = parse_gemini_meta(&path);
            // A JSONL with non-header content must also have a
            // resolvable `sessionId` in its header — otherwise
            // Gemini wouldn't have been able to write the rest. If
            // we can't parse it here, skip the entry rather than
            // synthesise an un-resumable `gemini:<filename>` key
            // (Enter on such rows used to silently no-op).
            let Some(key) = sid else { continue; };
            let last_activity = last_updated
                .or_else(|| path.metadata().and_then(|m| m.modified()).ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let title = first_user_text_jsonl(&path, ClaudeOrGemini::Gemini)
                .unwrap_or_else(|| short_id(&key, "gemini"));

            out.push(AgentSession {
                key:               key.clone(),
                cli_source:        CliSource::Gemini,
                pane_session_id:   None,
                window_id:         None,
                tab_id:            None,
                title,
                cwd:               cwd.clone(),
                started_at:        last_activity,
                last_activity_at:  last_activity,
                status:            AgentStatus::Historical,
                last_error:        None,
                current_tool:      None,
                attention_reason:  None,
                log_path:          Some(path),
                origin:            crate::agent_sessions::SessionOrigin::default(),
            });
        }
    }
    out.sort_by(|a, b| b.last_activity_at.cmp(&a.last_activity_at));
    out
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
/// "phantom" JSONL files out of the loader prevents Enter on an F2 row
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
        // Querying a non-existent prefix must not partial-match a longer key.
        assert_eq!(parse_simple_yaml(text, "summa"), None);
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
        // `load_copilot` itself never consults the agent-pane index — the
        // join is layered on top by `load_all`. So scanner output should
        // always default to Unknown regardless of any index that may exist
        // in the host's real %LOCALAPPDATA%.
        assert_eq!(s.origin, SessionOrigin::Unknown);
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
        // Reproduces the "ghost session at top of F2" bug: every time WT
        // (or wta itself) spawns a Copilot CLI process — e.g. as the
        // back-end for an agent pane or for a `?prompt` delegate — that
        // process eagerly creates `~/.copilot/session-state/<UUID>/workspace.yaml`
        // (171 bytes of stub metadata) before the user types anything.
        // If the user never interacts, no `events.jsonl` is ever written.
        // These dirs would otherwise dominate the top of F2 (most-recent
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
        // would dead-end on Enter in the F2 session-management view.
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
        // resumable session as a phantom and pruning it from F2.
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
        // real Claude / Gemini sessions out of F2.
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
        for cli in [CliSource::Claude, CliSource::Copilot, CliSource::Gemini] {
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
        // row stuck Ended in F2.
        use crate::agent_sessions::CliSource;
        let home = tmp_root("strict-probe-missing");
        for cli in [CliSource::Claude, CliSource::Copilot, CliSource::Gemini] {
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
        // loader used to surface these in F2 with the synthetic title
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
}

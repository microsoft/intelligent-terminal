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
//
//   Gemini:   ~/.gemini/tmp/<project-slug>/chats/session-*.jsonl
//             - session id   = first JSONL line `sessionId` field
//             - cwd          = ~/.gemini/projects.json reverse lookup
//             - title        = first JSONL line whose `type:"user"` carries
//                              a content[0].text (best-effort)
//             - last_activity= file mtime
//
//   Codex:    ~/.codex/sessions/YYYY/MM/DD/rollout-<iso-ts>-<UUID>.jsonl
//             - session id   = first JSONL line `session_meta` payload.id
//             - cwd          = `session_meta` payload.cwd
//             - title        = first `event_msg` payload.user_message,
//                              else first `response_item` role=user content
//                              (skipping codex's synthetic injections)
//             - last_activity= `session_meta` payload.timestamp (fallback file mtime)
//
// (Note: per-subagent JSONL files may live in nested `<UUID>/` subdirs of
// `chats/`. Top-level Gemini sessions are flat files named `session-*.jsonl`.
// under `<UUID>/<name>.json`. We only pick up `session-*.json` at the
// top level.)
//
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[allow(dead_code)]
const TITLE_TAIL_BYTES: u64 = 64 * 1024;

/// Upper bound on bytes read when streaming a JSONL line-by-line.
const CLASSIFY_SCAN_BYTES_CAP: u64 = 8 * 1024 * 1024;

/// Whether to scan WSL distros for historical sessions. Single
/// choke point + env opt-in (`WTA_WSL_SESSIONS=1|true|yes|on`).
/// Defaults to **disabled**: WSL sessions are intentionally hidden from
/// the session view until the mixed host/WSL TUI is designed. Flip the
/// `Err(_)` default back to `true` (or wire up the future `wslSessions`
/// setting through this same function) to re-enable — no other call site
/// changes.
pub(crate) fn wsl_sessions_enabled() -> bool {
    // Trim + case-fold so the opt-in is forgiving in scripts / CI
    // (` True `, `YES`, `on`, …), mirroring the env-bool parsing in
    // `resolve_sessions_origin_filter`.
    match std::env::var("WTA_WSL_SESSIONS") {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

// ─── Copilot per-key helpers ────────────────────────────────────────────

/// Resolve the Copilot session-state directory for `key`.
/// Always returns a path (no I/O); callers must `is_dir`/`exists` it.
pub(crate) fn copilot_session_dir_for_key(home: &Path, key: &str) -> PathBuf {
    home.join(".copilot").join("session-state").join(key)
}

// ─── Codex per-key helpers ──────────────────────────────────────────────

struct CodexSessionMeta {
    id: String,
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
    Some(CodexSessionMeta {
        id: payload.get("id")?.as_str()?.to_string(),
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
pub(crate) fn parse_iso_to_system_time(s: &str) -> Option<SystemTime> {
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

pub(crate) fn short_id(id: &str, cli: &str) -> String {
    let head: String = id.chars().take(8).collect();
    format!("{} {}", cli, head)
}

/// Extract a value from a flat key:value YAML file. Strings may be unquoted
/// (Copilot's workspace.yaml) or quoted. Supports block scalars (`|`, `|-`,
/// `|+`, `>`, `>-`, `>+`) for multi-line values — Copilot writes long
/// `summary:` fields this way, and a naive line read would otherwise
/// surface the literal `|-` indicator instead of the prose. Does NOT
/// support nested mapping structures.
#[allow(dead_code)]
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
#[allow(dead_code)]
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

#[allow(dead_code)]
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
#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[derive(Copy, Clone)]
#[allow(dead_code)]
enum ClaudeOrGemini { Claude, Gemini }

/// Best-effort: scan first chunk of JSONL for a user-message line and
/// return its text content, truncated to 60 chars.
#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
fn extract_gemini_user_text(v: &serde_json::Value) -> Option<String> {
    let arr = v.get("content")?.as_array()?;
    for part in arr {
        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
            return Some(t.to_string());
        }
    }
    None
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

#[allow(dead_code)]
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


























    // ─── Codex tests ────────────────────────────────────────────────────



    #[test]
    fn codex_payload_is_subagent_discriminates_source() {
        let cli = serde_json::json!({ "source": "cli" });
        assert!(!codex_payload_is_subagent(&cli), "top-level source=\"cli\" is not a subagent");
        let sub = serde_json::json!({ "source": { "subagent": { "thread_spawn": { "depth": 1 } } } });
        assert!(codex_payload_is_subagent(&sub), "source.subagent must be detected");
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
    fn wsl_gate_defaults_off_and_honors_env() {
        let _g = WSL_ENV_LOCK.lock().unwrap();
        std::env::remove_var("WTA_WSL_SESSIONS");
        assert!(!wsl_sessions_enabled(), "default must be disabled");
        std::env::set_var("WTA_WSL_SESSIONS", "1");
        assert!(wsl_sessions_enabled());
        std::env::set_var("WTA_WSL_SESSIONS", "true");
        assert!(wsl_sessions_enabled());
        // Forgiving parsing: trimmed, case-insensitive, extra truthy spellings.
        std::env::set_var("WTA_WSL_SESSIONS", "  True ");
        assert!(wsl_sessions_enabled(), "trimmed + case-insensitive True");
        std::env::set_var("WTA_WSL_SESSIONS", "YES");
        assert!(wsl_sessions_enabled());
        std::env::set_var("WTA_WSL_SESSIONS", "on");
        assert!(wsl_sessions_enabled());
        // Anything else (incl. the old falsey spellings) stays disabled.
        std::env::set_var("WTA_WSL_SESSIONS", "0");
        assert!(!wsl_sessions_enabled());
        std::env::set_var("WTA_WSL_SESSIONS", "false");
        assert!(!wsl_sessions_enabled());
        std::env::remove_var("WTA_WSL_SESSIONS");
    }
}

//! WSL historical agent-session discovery.
//!
//! Enumerates *running* WSL distros and, per distro, fetches the newest
//! agent-CLI session files (Copilot/Claude/Codex/Gemini) with a single
//! `wsl … bash` spawn that ranks + `tar`s them. The byte stream is
//! extracted on the Windows side with the in-box `tar.exe` into a temp
//! `$HOME`-mirror, over which the existing `history_loader` parsers run
//! verbatim. Rows are stamped `SessionLocation::Wsl { distro }`.
//!
//! Running distros only: touching a *stopped* distro's filesystem
//! auto-boots its VM (multi-second stall, GH#9541), so we never do it.

use crate::agent_sessions::{AgentSession, CliSource, SessionLocation};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Timeout for the per-distro fetch spawn. Bounds a wedged distro / 9P
/// stall so the history scan can't hang.
const WSL_FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// Timeout for the (cheap) `wsl -l --running -q` enumeration spawn.
const WSL_LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// Scan one distro into rows, with the spawn + extract boundaries
/// injected so unit tests run without `wsl.exe` / `tar.exe`.
///
/// `fetch_tar(distro) -> Option<tar bytes>`; `extract(bytes, dest)`
/// materializes a `$HOME`-mirror at `dest`.
pub(crate) fn scan_distro_with<F, E>(
    distro: &str,
    fetch_tar: F,
    extract: E,
    cli_filter: Option<&CliSource>,
) -> Vec<AgentSession>
where
    F: FnOnce(&str) -> Option<Vec<u8>>,
    E: FnOnce(&[u8], &Path) -> std::io::Result<()>,
{
    let Some(tar_bytes) = fetch_tar(distro) else {
        return Vec::new();
    };
    if tar_bytes.is_empty() {
        return Vec::new();
    }
    let tmp = match ScopedTempDir::new() {
        Ok(t) => t,
        Err(err) => {
            tracing::warn!(target: "wsl", distro, %err, "temp dir create failed");
            return Vec::new();
        }
    };
    if let Err(err) = extract(&tar_bytes, tmp.path()) {
        tracing::warn!(target: "wsl", distro, %err, "tar extract failed");
        return Vec::new();
    }
    let mut rows = crate::history_loader::load_all_in(tmp.path(), cli_filter);
    let loc = SessionLocation::Wsl {
        distro: distro.to_string(),
    };
    for r in &mut rows {
        r.location = loc.clone();
    }
    rows
}

/// Enumerate running distros and scan each, using the real spawn/extract.
/// Fast (no VM boot) — used for the initial session list.
pub fn scan_running_distros(cli_filter: Option<&CliSource>) -> Vec<AgentSession> {
    scan_distros(running_distros(), cli_filter)
}

/// Scan a specific set of distros, using the real spawn/extract.
fn scan_distros(distros: Vec<String>, cli_filter: Option<&CliSource>) -> Vec<AgentSession> {
    let mut out = Vec::new();
    for distro in distros {
        let rows = scan_distro_with(&distro, fetch_distro_tar, extract_tar_stream, cli_filter);
        tracing::info!(target: "wsl", distro = %distro, rows = rows.len(), "scanned distro");
        out.extend(rows);
    }
    out
}

/// `wsl -l --running -q` -> running distro names. Empty on any failure
/// (no WSL, nothing running, timeout).
pub(crate) fn running_distros() -> Vec<String> {
    list_distros(&["-l", "--running", "-q"])
}

/// Run `wsl.exe <args>` and parse its UTF-16LE distro-name list. Empty on
/// any failure (no WSL, timeout).
fn list_distros(args: &[&str]) -> Vec<String> {
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.args(args);
    match run_capture_with_timeout(cmd, WSL_LIST_TIMEOUT) {
        Some(bytes) => parse_running_distros(&bytes),
        None => Vec::new(),
    }
}

/// Production fetch: run the bash pipeline inside `distro`, capture the
/// tar stream from stdout. Base64-wraps the script so neither wsl.exe's
/// command-line re-parse nor bash quoting can mangle it.
fn fetch_distro_tar(distro: &str) -> Option<Vec<u8>> {
    let script = build_fetch_script(crate::history_loader::MAX_PER_CLI);
    let b64 = crate::osc52::base64_encode(script.as_bytes());
    let inner = format!("echo {b64} | base64 -d | bash");
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.args(["-d", distro, "--", "bash", "-c", &inner]);
    let bytes = run_capture_with_timeout(cmd, WSL_FETCH_TIMEOUT)?;
    (!bytes.is_empty()).then_some(bytes)
}

/// Production extract: pipe the tar stream into the in-box Windows
/// `tar.exe` (libarchive) extracting into `dest`. mtimes are preserved.
fn extract_tar_stream(tar_bytes: &[u8], dest: &Path) -> std::io::Result<()> {
    use std::io::{Read, Write};
    let tar_exe = std::env::var_os("SystemRoot")
        .map(|r| Path::new(&r).join("System32").join("tar.exe"))
        .unwrap_or_else(|| PathBuf::from("tar.exe"));
    let mut child = std::process::Command::new(tar_exe)
        .args(["-xf", "-", "-C"])
        .arg(dest)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    // Drain stderr on a thread so a chatty tar can't deadlock the stdin write
    // by filling the stderr pipe while we're still feeding the archive in.
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("tar.exe: no stderr"))?;
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("tar.exe: no stdin"))?
        .write_all(tar_bytes)?;
    let status = child.wait()?;
    let stderr_bytes = stderr_reader.join().unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        // Surface tar's own diagnostic (corrupt stream, unsupported feature,
        // path error) instead of a bare exit code.
        let detail = String::from_utf8_lossy(&stderr_bytes);
        let detail = detail.trim();
        Err(std::io::Error::other(if detail.is_empty() {
            format!("tar.exe exited with {status}")
        } else {
            format!("tar.exe exited with {status}: {detail}")
        }))
    }
}

/// Spawn `cmd`, capture stdout, but give up (kill the child) after
/// `timeout`. std-only: a reader thread drains stdout while the main
/// thread waits on an mpsc with a deadline.
fn run_capture_with_timeout(mut cmd: std::process::Command, timeout: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::sync::mpsc;
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    match rx.recv_timeout(timeout) {
        Ok(buf) => {
            let _ = child.wait();
            let _ = reader.join();
            Some(buf)
        }
        Err(_) => {
            // Timed out: kill the child so the reader hits EOF and exits.
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            None
        }
    }
}

/// A temp directory removed on drop. Used as the extraction target for a
/// distro's `$HOME`-mirror. Names are made unique with a v4 UUID.
struct ScopedTempDir(PathBuf);

impl ScopedTempDir {
    fn new() -> std::io::Result<Self> {
        let p = std::env::temp_dir().join(format!("wta-wsl-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p)?;
        Ok(Self(p))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScopedTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Strips a UTF-16 BOM and any NUL / CR bytes, trims, drops the `*` default
/// marker and blank lines.
pub(crate) fn parse_running_distros(utf16le: &[u8]) -> Vec<String> {
    let u16s: Vec<u16> = utf16le
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
        .lines()
        // `str::trim` won't drop a BOM (U+FEFF) or a NUL — neither is ASCII
        // whitespace — and a stray BOM/NUL carried into a distro name breaks
        // later `wsl -d <name>` lookups, so remove them explicitly.
        .map(|l| l.replace(|c: char| c == '\u{feff}' || c == '\0', ""))
        .map(|l| l.trim().trim_start_matches('*').trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Build the bash pipeline (run inside the distro) that ranks the newest
/// `cap` sessions per CLI by mtime and streams exactly their files as a
/// `tar` archive on stdout, relative to `$HOME`.
///
/// Assumes GNU coreutils / GNU find (`find -printf`) and GNU `tar`
/// (`--transform`) — the default on Ubuntu/Debian/Fedora/etc. BusyBox-only
/// distros (Alpine) are a known MVP gap (their `find` lacks `-printf`); they
/// simply yield no rows.
///
/// **Copilot only** is also searched under its snap-confined copy at
/// `~/snap/copilot-cli/common/.copilot/...`: in WSL/Ubuntu a missing
/// `copilot` prompts `sudo snap install copilot-cli` (the command-not-found
/// handler), so snap is a common install path for it. The `tar --transform`
/// strips the `snap/<app>/common/` prefix so those land at a
/// top-level `.copilot/...` in the archive, which the host parser then reads
/// unchanged. Claude/Codex/Gemini ship via npm and are searched only at their
/// standard `~/.<cli>` root (no snap).
///
/// Per CLI:
/// * Copilot — rank session dirs by `events.jsonl` mtime, emit each dir's
///   `events.jsonl` + `workspace.yaml`.
/// * Claude/Codex/Gemini — rank the session `.jsonl` files by mtime.
fn build_fetch_script(cap: usize) -> String {
    format!(
        r#"set -eu
cd "$HOME" 2>/dev/null || exit 0
shopt -s nullglob
CAP={cap}
list="$(mktemp)"
trap 'rm -f "$list"' EXIT INT TERM
{{
  # Copilot: rank session dirs by events.jsonl mtime; emit dir's two files.
  find .copilot/session-state \
       snap/*/common/.copilot/session-state \
       -mindepth 2 -maxdepth 2 -name events.jsonl -printf '%T@\t%h\n' 2>/dev/null \
    | sort -rn | head -n "$CAP" | cut -f2- \
    | while IFS= read -r d; do
        [ -f "$d/events.jsonl" ] && printf '%s\n' "$d/events.jsonl"
        [ -f "$d/workspace.yaml" ] && printf '%s\n' "$d/workspace.yaml"
      done
  # Claude: project .jsonl files (npm install; standard root only).
  find .claude/projects \
       -type f -name '*.jsonl' -printf '%T@\t%p\n' 2>/dev/null \
    | sort -rn | head -n "$CAP" | cut -f2-
  # Codex: rollout .jsonl files (nested YYYY/MM/DD; npm install).
  find .codex/sessions \
       -type f -name 'rollout-*.jsonl' -printf '%T@\t%p\n' 2>/dev/null \
    | sort -rn | head -n "$CAP" | cut -f2-
  # Gemini: chat session .jsonl files (npm install).
  find .gemini/tmp \
       -type f -name 'session-*.jsonl' -printf '%T@\t%p\n' 2>/dev/null \
    | sort -rn | head -n "$CAP" | cut -f2-
}} > "$list"
[ -s "$list" ] || exit 0
# Normalize snap-confined paths (snap/<app>/common/.copilot/...) to a
# top-level `.copilot/...` so the host parsers find them after extraction.
tar -cf - -C "$HOME" --transform='s|^snap/[^/]*/[^/]*/||' -T "$list" 2>/dev/null
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `&str` as UTF-16LE bytes (what wsl.exe emits).
    fn utf16le(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn parse_running_distros_handles_names_marker_and_blanks() {
        let bytes = utf16le("Ubuntu\r\n*Debian\r\n\r\n");
        assert_eq!(parse_running_distros(&bytes), vec!["Ubuntu", "Debian"]);
    }

    #[test]
    fn parse_running_distros_empty_when_nothing_running() {
        assert!(parse_running_distros(&utf16le("\r\n")).is_empty());
        assert!(parse_running_distros(&[]).is_empty());
    }

    #[test]
    fn parse_running_distros_strips_bom_and_nul() {
        // A real `wsl.exe -l --running -q` capture can carry a leading UTF-16
        // BOM and trailing NUL padding; neither must leak into a distro name
        // (it would break the later `wsl -d <name>` lookup).
        let bytes = utf16le("\u{feff}Ubuntu\u{0}\r\n*Debian\u{0}\r\n");
        assert_eq!(parse_running_distros(&bytes), vec!["Ubuntu", "Debian"]);
    }

    #[test]
    fn fetch_script_ranks_four_clis_and_tars_relative_to_home() {
        let script = build_fetch_script(50);
        // cd into $HOME and bail cleanly if it doesn't exist
        assert!(script.contains("cd \"$HOME\""));
        // all four CLI roots are scanned
        assert!(script.contains(".copilot/session-state"));
        assert!(script.contains(".claude/projects"));
        assert!(script.contains(".codex/sessions"));
        assert!(script.contains(".gemini/tmp"));
        // cap is threaded into the head limit
        assert!(script.contains("CAP=50"));
        // archive is produced relative to $HOME on stdout
        assert!(script.contains("tar -cf - -C \"$HOME\""));
        // Copilot is also searched under its snap-confined copy (`common`
        // only) — a missing `copilot` in WSL/Ubuntu prompts `snap install
        // copilot-cli` — and the tar --transform normalizes it back to a
        // top-level `.copilot/...` for the host parser.
        assert!(script.contains("snap/*/common/.copilot/session-state"));
        assert!(script.contains("shopt -s nullglob"));
        assert!(script.contains("--transform='s|^snap/[^/]*/[^/]*/||'"));
    }

    #[test]
    fn scan_distro_stamps_rows_with_wsl_location() {
        // Fake fetch: any non-empty bytes (real bytes are a tar; the fake
        // extractor ignores them and writes a known layout instead).
        let fetch = |_distro: &str| Some(vec![1u8, 2, 3]);
        // Fake extract: materialize one Copilot session under <dest>.
        let extract = |_bytes: &[u8], dest: &std::path::Path| -> std::io::Result<()> {
            let dir = dest
                .join(".copilot")
                .join("session-state")
                .join("11111111-2222-3333-4444-555555555555");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join("workspace.yaml"),
                "id: 11111111-2222-3333-4444-555555555555\ncwd: /home/u/proj\nsummary: hello wsl\n",
            )?;
            std::fs::write(dir.join("events.jsonl"), "{\"type\":\"user\"}\n")?;
            Ok(())
        };

        let rows = scan_distro_with("Ubuntu", fetch, extract, None);
        assert_eq!(rows.len(), 1, "expected one Copilot row");
        assert_eq!(
            rows[0].location,
            SessionLocation::Wsl {
                distro: "Ubuntu".to_string()
            }
        );
        assert_eq!(rows[0].title, "hello wsl");
    }

    #[test]
    fn scan_distro_empty_fetch_yields_no_rows() {
        let fetch = |_d: &str| None;
        let extract = |_b: &[u8], _d: &std::path::Path| Ok(());
        assert!(scan_distro_with("Ubuntu", fetch, extract, None).is_empty());
    }

    #[test]
    fn scan_distro_with_cli_filter_parses_only_selected_cli() {
        // Same Copilot-only fixture as scan_distro_stamps_rows_with_wsl_location,
        // but as named fns so the same fixture can drive two scans.
        fn fetch(_distro: &str) -> Option<Vec<u8>> {
            Some(vec![1u8, 2, 3])
        }
        fn extract(_bytes: &[u8], dest: &std::path::Path) -> std::io::Result<()> {
            let dir = dest
                .join(".copilot")
                .join("session-state")
                .join("11111111-2222-3333-4444-555555555555");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(
                dir.join("workspace.yaml"),
                "id: 11111111-2222-3333-4444-555555555555\ncwd: /home/u/proj\nsummary: hello wsl\n",
            )?;
            std::fs::write(dir.join("events.jsonl"), "{\"type\":\"user\"}\n")?;
            Ok(())
        }

        // Filtering to the CLI that's present parses + returns it.
        let cop = scan_distro_with("Ubuntu", fetch, extract, Some(&CliSource::Copilot));
        assert_eq!(cop.len(), 1, "selected CLI is parsed");
        assert_eq!(cop[0].cli_source, CliSource::Copilot);

        // Filtering to a different CLI skips Copilot's parse entirely, even
        // though its files sit in the extracted mirror — this is the
        // parse-time filter that avoids reading other CLIs' transcripts.
        let cla = scan_distro_with("Ubuntu", fetch, extract, Some(&CliSource::Claude));
        assert!(cla.is_empty(), "unselected CLI's transcripts are never parsed");
    }
}

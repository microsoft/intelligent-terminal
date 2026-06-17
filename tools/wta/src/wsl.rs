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

/// Decode the UTF-16LE output of `wsl -l --running -q` into distro names.
/// Strips NULs/CR, trims, drops the `*` default marker and blank lines.
#[allow(dead_code)] // Called by `running_distros()` in the scan-orchestration task.
pub(crate) fn parse_running_distros(utf16le: &[u8]) -> Vec<String> {
    let u16s: Vec<u16> = utf16le
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&u16s)
        .lines()
        .map(|l| l.trim().trim_start_matches('*').trim())
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Build the bash pipeline (run inside the distro) that ranks the newest
/// `cap` sessions per CLI by mtime and streams exactly their files as a
/// `tar` archive on stdout, relative to `$HOME`.
///
/// Assumes GNU coreutils/findutils (`find -printf`) — the default on
/// Ubuntu/Debian/Fedora/etc. BusyBox-only distros (Alpine) are a known
/// MVP gap (their `find` lacks `-printf`); they simply yield no rows.
///
/// Per CLI:
/// * Copilot — rank session dirs by `events.jsonl` mtime, emit each dir's
///   `events.jsonl` + `workspace.yaml`.
/// * Claude/Codex/Gemini — rank the session `.jsonl` files by mtime.
#[allow(dead_code)] // Invoked by the production fetch in the scan-orchestration task.
fn build_fetch_script(cap: usize) -> String {
    format!(
        r#"set -eu
cd "$HOME" 2>/dev/null || exit 0
CAP={cap}
list="$(mktemp)"
{{
  # Copilot: rank session dirs by events.jsonl mtime; emit dir's two files.
  find .copilot/session-state -mindepth 2 -maxdepth 2 -name events.jsonl \
       -printf '%T@\t%h\n' 2>/dev/null \
    | sort -rn | head -n "$CAP" | cut -f2- \
    | while IFS= read -r d; do
        [ -f "$d/events.jsonl" ] && printf '%s\n' "$d/events.jsonl"
        [ -f "$d/workspace.yaml" ] && printf '%s\n' "$d/workspace.yaml"
      done
  # Claude: project JSONLs.
  find .claude/projects -type f -name '*.jsonl' -printf '%T@\t%p\n' 2>/dev/null \
    | sort -rn | head -n "$CAP" | cut -f2-
  # Codex: rollout JSONLs (nested YYYY/MM/DD).
  find .codex/sessions -type f -name 'rollout-*.jsonl' -printf '%T@\t%p\n' 2>/dev/null \
    | sort -rn | head -n "$CAP" | cut -f2-
  # Gemini: chat session JSONLs.
  find .gemini/tmp -type f -name 'session-*.jsonl' -printf '%T@\t%p\n' 2>/dev/null \
    | sort -rn | head -n "$CAP" | cut -f2-
}} > "$list"
[ -s "$list" ] || exit 0
tar -cf - -C "$HOME" -T "$list" 2>/dev/null
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
    }
}

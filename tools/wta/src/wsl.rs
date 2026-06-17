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
}

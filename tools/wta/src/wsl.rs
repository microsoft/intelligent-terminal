//! Running WSL distro enumeration.
//!
//! Lists the **running** WSL distros (`wsl -l --running -q`) so the ACP
//! session scan ([`crate::wsl_acp`]) knows which distros to query. Only
//! running distros are returned: touching a *stopped* distro's filesystem
//! (or launching a CLI in it) auto-boots its VM (GH#9541), which is too
//! costly to do just to build a session list.
//!
//! This module used to also tar a distro's agent-CLI transcripts to the
//! host and parse them; that file-reading path was replaced by ACP
//! `session/list` — see `doc/specs/wsl-acp-session-list-feasibility.md`.

use std::time::Duration;

/// Timeout for the (cheap) `wsl -l --running -q` enumeration spawn.
const WSL_LIST_TIMEOUT: Duration = Duration::from_secs(10);

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
}

//! Bind a discovered session to its hosting WT pane.
//!
//! Strategy per the spec's finalized Decision #3:
//!   * Copilot → `inuse.<pid>.lock` in the session dir (exact).
//!   * Codex   → Restart Manager owner of the rollout file (exact).
//!   * Claude/Gemini → cwd correlation: among live CLI processes, pick the
//!     one whose working directory matches the session's cwd; ties (same cwd)
//!     are left unresolved (returns None) to avoid a wrong bind.
//!
//! Once a pid is chosen, the pane GUID comes from `proc_bind::wt_session_for_pid`.

use crate::agent_sessions::CliSource;
use crate::proc_bind;
use std::path::{Path, PathBuf};

/// A candidate live CLI process for correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub pid: u32,
    pub cwd: PathBuf,
}

/// Pure core: pick the unique candidate whose cwd matches `target`. Returns
/// `None` when there is no match OR more than one match (ambiguous — never
/// guess). Comparison is case-insensitive with trailing separators ignored
/// (Windows paths).
pub fn correlate_by_cwd(candidates: &[Candidate], target: &Path) -> Option<u32> {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    let want = norm(target);
    let mut hits = candidates.iter().filter(|c| norm(&c.cwd) == want);
    let first = hits.next()?;
    if hits.next().is_some() {
        None // ambiguous: two same-cwd candidates
    } else {
        Some(first.pid)
    }
}

/// Resolve the `(pane GUID, owner pid)` of a Copilot session via its lock file,
/// then the pane GUID from that pid's PEB. The pid is returned for liveness
/// polling; the pane may be `None` if the PEB read fails even when the pid is
/// known.
pub fn bind_copilot(session_dir: &Path) -> (Option<String>, Option<u32>) {
    let Some(pid) = proc_bind::copilot_pid_from_lock(session_dir) else {
        return (None, None);
    };
    (proc_bind::wt_session_for_pid(pid), Some(pid))
}

/// Resolve the `(pane GUID, owner pid)` of a Codex session via Restart Manager,
/// then the pane GUID from that pid's PEB. See [`bind_copilot`] for the
/// pane-vs-pid contract.
pub fn bind_codex(rollout_path: &Path) -> (Option<String>, Option<u32>) {
    let Some(pid) = proc_bind::file_owner_pid(rollout_path) else {
        return (None, None);
    };
    (proc_bind::wt_session_for_pid(pid), Some(pid))
}

/// Process exe names to enumerate per CLI when correlating by cwd. Copilot
/// (lock) and Codex (Restart Manager) bind exactly and need no enumeration.
fn exe_names(cli: &CliSource) -> &'static [&'static str] {
    match cli {
        CliSource::Claude => &["claude.exe"],
        CliSource::Gemini => &["node.exe"], // gemini runs as a node bundle
        _ => &[],
    }
}

/// Gather live candidate processes for a cwd-correlated CLI: `(pid, cwd)` for
/// every matching exe that has a readable working directory.
pub fn gather_candidates(cli: &CliSource) -> Vec<Candidate> {
    let mut out = Vec::new();
    for name in exe_names(cli) {
        for pid in proc_bind::pids_for_exe(name) {
            if let Some(cwd) = proc_bind::cwd_for_pid(pid) {
                out.push(Candidate { pid, cwd });
            }
        }
    }
    out
}

/// Resolve the `(pane GUID, owner pid)` hosting `cli`'s session, given the
/// session's cwd (path-encoded; available for Claude). Returns `(None, None)`
/// when there is no unique cwd match. For Copilot/Codex use
/// [`bind_copilot`]/[`bind_codex`].
pub fn bind_by_cwd(cli: &CliSource, session_cwd: &Path) -> (Option<String>, Option<u32>) {
    let candidates = gather_candidates(cli);
    let Some(pid) = correlate_by_cwd(&candidates, session_cwd) else {
        return (None, None);
    };
    (proc_bind::wt_session_for_pid(pid), Some(pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(pid: u32, cwd: &str) -> Candidate {
        Candidate {
            pid,
            cwd: PathBuf::from(cwd),
        }
    }

    #[test]
    fn unique_cwd_match_binds() {
        let candidates = vec![cand(10, r"C:\Users\u\proj"), cand(20, r"C:\Users\u\other")];
        assert_eq!(
            correlate_by_cwd(&candidates, Path::new(r"C:\Users\u\proj")),
            Some(10)
        );
    }

    #[test]
    fn case_and_trailing_sep_insensitive() {
        let candidates = vec![cand(10, r"c:\users\u\proj\")];
        assert_eq!(
            correlate_by_cwd(&candidates, Path::new(r"C:\Users\U\Proj")),
            Some(10)
        );
    }

    #[test]
    fn ambiguous_same_cwd_returns_none() {
        let candidates = vec![cand(10, r"C:\p"), cand(20, r"C:\p")];
        assert_eq!(correlate_by_cwd(&candidates, Path::new(r"C:\p")), None);
    }

    #[test]
    fn no_match_returns_none() {
        let candidates = vec![cand(10, r"C:\a")];
        assert_eq!(correlate_by_cwd(&candidates, Path::new(r"C:\b")), None);
    }

    #[test]
    fn gather_candidates_empty_for_cli_without_exe_names() {
        // Copilot/Codex bind by lock/RM, not cwd — no exe names to enumerate,
        // so this is deterministic regardless of what's running.
        assert!(gather_candidates(&CliSource::Copilot).is_empty());
        assert!(gather_candidates(&CliSource::Codex).is_empty());
    }

    #[test]
    fn bind_by_cwd_none_without_candidates() {
        // No exe_names -> no candidates -> never binds, regardless of cwd.
        assert_eq!(
            bind_by_cwd(&CliSource::Copilot, Path::new(r"C:\whatever")),
            (None, None)
        );
    }
}

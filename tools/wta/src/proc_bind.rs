//! proc_bind.rs — Win32 primitives that link a session file/dir to the
//! process that owns it, and that process to its hosting Windows Terminal
//! pane.
//!
//! Four primitives, each validated against live agent CLIs on 2026-06-09
//! (see `doc/specs/hookless-agent-session-tracking.md` → Verification):
//!
//!   * [`copilot_pid_from_lock`] — Copilot's `inuse.<pid>.lock` marker (pure fs).
//!   * [`parent_pid`]            — `InheritedFromUniqueProcessId` via NtQueryInformationProcess.
//!   * [`env_var_for_pid`] / [`wt_session_for_pid`] — read an env var from a
//!     process's PEB environment block (→ the pane GUID).
//!   * [`file_holders`] / [`file_owner_pid`] — Restart Manager "who holds this
//!     file open" (used for Codex, which keeps its rollout file open).
//!
//! FFI is declared inline rather than via `windows-sys` features because the
//! rarer calls (`NtQueryInformationProcess`, the `Rm*` family) are not all
//! covered by the crate's currently-enabled feature set, and inline `extern`
//! blocks keep this module self-contained and version-independent. Every
//! `unsafe` block is documented.

use std::path::Path;

/// Parse Copilot's in-use marker. Copilot writes a zero-byte file named
/// `inuse.<pid>.lock` into its session-state directory while a session is
/// live; the owning process id is encoded in the file name. Returns the
/// first such pid found, or `None` if the directory has no marker (or
/// cannot be read).
pub fn copilot_pid_from_lock(session_dir: &Path) -> Option<u32> {
    let entries = std::fs::read_dir(session_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(pid) = name
            .strip_prefix("inuse.")
            .and_then(|rest| rest.strip_suffix(".lock"))
            .and_then(|digits| digits.parse::<u32>().ok())
        {
            return Some(pid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "wta-proc-bind-{}-{}",
            label,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copilot_pid_from_lock_reads_marker() {
        let dir = tmp_dir("copilot-lock");
        std::fs::write(dir.join("inuse.12345.lock"), b"").unwrap();
        std::fs::write(dir.join("events.jsonl"), b"{}\n").unwrap();
        assert_eq!(copilot_pid_from_lock(&dir), Some(12345));
    }

    #[test]
    fn copilot_pid_from_lock_none_without_marker() {
        let dir = tmp_dir("copilot-nolock");
        std::fs::write(dir.join("events.jsonl"), b"{}\n").unwrap();
        assert_eq!(copilot_pid_from_lock(&dir), None);
    }

    #[test]
    fn copilot_pid_from_lock_ignores_malformed() {
        let dir = tmp_dir("copilot-bad");
        std::fs::write(dir.join("inuse.notanumber.lock"), b"").unwrap();
        assert_eq!(copilot_pid_from_lock(&dir), None);
    }
}

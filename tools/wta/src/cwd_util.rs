//! Validation helper for "starting directory" values that wta hands to
//! external launchers (`wtcli new-tab -d <cwd>`, the `resume_in_new_agent_tab`
//! protocol event consumed by WT to spawn a new tab, the boot-time
//! `--initial-load-cwd` flag, etc.).
//!
//! Agent session metadata (cwd recorded by Claude/Copilot/Gemini in their
//! per-session JSONL files) can easily go stale: the user moves or deletes
//! the project directory, mounts a different drive, etc. Passing a stale
//! cwd downstream causes `CreateProcessW` to fail with
//! `ERROR_DIRECTORY` (0x10b), which surfaces as a new tab/pane that opens
//! but is immediately broken — the connection prints
//! `Could not find ... working directory` and the user can't type
//! anything useful.
//!
//! Validating BEFORE we hand the value off lets us fall back cleanly: by
//! omitting the directory argument entirely, the consumer uses its own
//! default chain (profile `startingDirectory` → `%USERPROFILE%`), which
//! mirrors what plain `wtcli new-tab` (no `-d`) already does. We
//! deliberately do NOT pick a substitute directory ourselves — letting
//! the consumer's normal default kick in keeps behaviour consistent with
//! a vanilla "open new tab" action.

use std::path::Path;

/// Returns `Some(string)` if `path` is non-empty and refers to an
/// existing directory. Otherwise returns `None`, signalling the caller
/// should drop the cwd argument entirely so the launcher falls back to
/// its own default.
///
/// The check is a single `fs::metadata` syscall. On local NTFS this is
/// effectively free; on slow / unreachable network or WSL paths it can
/// block briefly. We accept that trade-off because:
/// * the cost is paid on a low-frequency dispatch path (Enter on a
///   historical agent session row, boot-time resume), not anywhere
///   latency-sensitive;
/// * the failure mode without the check (silently-broken pane) is worse
///   than a one-off stall;
/// * if `metadata` errors out (timeout, ACCESS_DENIED, anything), we
///   treat the path as invalid and fall back — which is the safe
///   direction.
pub fn validate_starting_directory<P: AsRef<Path>>(path: P) -> Option<String> {
    let p = path.as_ref();
    if p.as_os_str().is_empty() {
        return None;
    }
    match std::fs::metadata(p) {
        Ok(meta) if meta.is_dir() => Some(p.to_string_lossy().into_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("wta-cwd-util-{tag}-{pid}-{nanos}"));
        p
    }

    #[test]
    fn empty_path_returns_none() {
        assert_eq!(validate_starting_directory(""), None);
        assert_eq!(validate_starting_directory(PathBuf::new()), None);
    }

    #[test]
    fn nonexistent_path_returns_none() {
        let p = unique_temp_dir("nope");
        // Make sure it really doesn't exist.
        let _ = fs::remove_dir_all(&p);
        assert!(!p.exists());
        assert_eq!(validate_starting_directory(&p), None);
    }

    #[test]
    fn file_path_returns_none() {
        let dir = unique_temp_dir("file");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        fs::write(&file, b"x").unwrap();
        assert_eq!(validate_starting_directory(&file), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_directory_returns_canonicalish_string() {
        let dir = unique_temp_dir("ok");
        fs::create_dir_all(&dir).unwrap();
        let got = validate_starting_directory(&dir);
        assert_eq!(got, Some(dir.to_string_lossy().into_owned()));
        let _ = fs::remove_dir_all(&dir);
    }
}

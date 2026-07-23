//! Resolve the working directory for ACP `session/new` / `session/load`
//! in the **agent's own path namespace**.
//!
//! ## Why this exists
//!
//! The ACP `cwd` field must be a valid absolute path *in the agent
//! process's namespace*. An agent launched inside WSL validates against the
//! Linux filesystem, so a Windows path like `C:\WINDOWS\system32` (what
//! `std::env::current_dir()` returns for the packaged helper) is rejected
//! with `Directory path must be absolute`. A Windows-native agent, by
//! contrast, happily accepts that same Windows path — which is why the bug
//! only ever reproduced with WSL agents.
//!
//! ## Approach (no launcher/profile parsing — wrapper-proof)
//!
//! 1. **Target format** — which namespace the agent expects — is learned
//!    from the agent itself via `session/list`: each prior session reports
//!    its `cwd`, and a leading `/` means POSIX, a drive-letter means
//!    Windows. This is authoritative regardless of how the agent was
//!    launched (`wsl.exe …`, a `.cmd` wrapper, `cmd /c …`, etc.). When the
//!    list is empty or unsupported the target is unknown and the caller
//!    tries both formats.
//!
//! 2. **Source value** — the cwd we start from ([`pick_value`]) — drops
//!    "junk" launcher dirs (`System32`, `Windows`) and empty values down to
//!    `%USERPROFILE%`, so we never seed a session in System32.
//!
//! 3. **Conversion** is done by two *idempotent* converters,
//!    [`to_windows_format`] / [`to_linux_format`]: passing a path that is
//!    already in the requested format is a no-op, so the caller just calls
//!    the one matching the target and never has to reason about the source
//!    format.

use std::path::{Path, PathBuf};

/// A path's namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathFormat {
    Windows,
    Posix,
}

/// Classify a path's namespace: POSIX if it starts with `/`, Windows
/// otherwise (drive-letter `C:\…`, UNC `\\server\share`, extended-length
/// `\\?\C:\…`, etc.). It's a strict binary — there are only two namespaces
/// an agent can want, and callers always start from a real cwd, so there's
/// no third "indeterminate" case to reason about.
pub fn classify(path: &Path) -> PathFormat {
    if path.to_string_lossy().trim_start().starts_with('/') {
        PathFormat::Posix
    } else {
        PathFormat::Windows
    }
}

/// Learn the agent's namespace from the cwd values it reports in
/// `session/list`. Returns the first non-empty entry's format, or `None`
/// when the list is empty (caller then tries both formats).
pub fn detect_format<'a>(
    session_cwd_values: impl IntoIterator<Item = &'a str>,
) -> Option<PathFormat> {
    session_cwd_values
        .into_iter()
        .find(|c| !c.trim().is_empty())
        .map(|c| classify(Path::new(c)))
}

/// Choose the source cwd value, dropping junk launcher dirs (`System32`,
/// `Windows`) and empty values down to [`user_profile_dir`] (USERPROFILE →
/// Windows-only HOME → `%SystemDrive%\`). The result may itself be Windows
/// or POSIX — a WSL-integrated pane reports a POSIX `$PWD` — which is fine:
/// the converters are idempotent.
pub fn pick_value(candidate: Option<&Path>) -> PathBuf {
    if let Some(p) = candidate {
        if !p.as_os_str().is_empty() && !is_junk(p) {
            return p.to_path_buf();
        }
    }
    user_profile_dir()
}

/// Idempotent conversion to a Windows path:
/// * already Windows → unchanged;
/// * `/mnt/<drive>/…` → `<Drive>:\…`;
/// * any other POSIX path (e.g. `/home/user`) → `%USERPROFILE%` (a faithful
///   conversion would need the source distro's `\\wsl$` root, which we
///   don't know here — this is the rare WSL-pane→native-agent corner).
pub fn to_windows_format(path: &Path) -> PathBuf {
    match classify(path) {
        PathFormat::Windows => path.to_path_buf(),
        PathFormat::Posix => mnt_to_windows(&path.to_string_lossy()).unwrap_or_else(user_profile_dir),
    }
}

/// Idempotent conversion to a POSIX path:
/// * already POSIX → unchanged;
/// * Windows drive path `C:\a\b` → `/mnt/c/a/b` (standard WSL auto-mount,
///   distro-independent — no shell-out needed);
/// * non-drive Windows path (true UNC) → `/tmp` (via `windows_to_mnt`).
pub fn to_linux_format(path: &Path) -> PathBuf {
    match classify(path) {
        PathFormat::Posix => path.to_path_buf(),
        PathFormat::Windows => PathBuf::from(windows_to_mnt(&path.to_string_lossy())),
    }
}

/// Ordered list of cwd values to try against `session/new`, given the source
/// `value` and the (possibly unknown) agent `target` format. Normally one
/// entry; the extra rungs only matter on the rare empty-`session/list` /
/// wrong-guess path. De-duplicated, order-preserving.
pub fn build_attempts(value: &Path, target: Option<PathFormat>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    match target {
        Some(PathFormat::Windows) => push(to_windows_format(value)),
        Some(PathFormat::Posix) => {
            push(to_linux_format(value));
            push(PathBuf::from("/tmp"));
        }
        None => {
            // Target unknown: try the value in its own format first, then the
            // opposite, then a safe floor for each namespace.
            match classify(value) {
                PathFormat::Posix => {
                    push(to_linux_format(value));
                    push(to_windows_format(value));
                }
                PathFormat::Windows => {
                    push(to_windows_format(value));
                    push(to_linux_format(value));
                }
            }
            push(PathBuf::from("/tmp"));
        }
    }
    out
}

/// True when an error string looks like a cwd rejection (bad namespace /
/// nonexistent dir) — the signal to retry with the next candidate cwd.
/// Matches agents' own wording, e.g. copilot's "Directory path must be
/// absolute" / "Directory does not exist or cannot be accessed".
///
/// A rejection phrase alone isn't enough: words like "absolute" or "does not
/// exist" also appear in unrelated agent errors (missing model, resource
/// lookups), and retrying those down the cwd ladder is wasted work. So we
/// additionally require a directory/path/cwd context before treating the error
/// as retryable.
pub fn looks_like_cwd_error(haystack: &str) -> bool {
    let h = haystack.to_ascii_lowercase();
    let has_path_context = h.contains("directory")
        || h.contains("path")
        || h.contains("cwd")
        || h.contains("working dir");
    if !has_path_context {
        return false;
    }
    h.contains("absolute")
        || h.contains("does not exist")
        || h.contains("cannot be accessed")
        || h.contains("not a directory")
}

// --- internals ---------------------------------------------------------

/// `C:\Users\me` → `/mnt/c/Users/me`; bare `C:` → `/mnt/c`. Verbatim/device
/// prefixes (`\\?\C:\…`, `\\.\C:\…`) are stripped first. A non-drive Windows
/// path (true UNC like `\\server\share`) has no `/mnt` equivalent, so it
/// defers to the safe POSIX floor `/tmp` rather than emitting a nonsense
/// path like `/?/C:/foo`.
fn windows_to_mnt(win: &str) -> String {
    let win = win.trim();
    // Strip extended-length / device prefixes before drive parsing.
    let win = win
        .strip_prefix(r"\\?\")
        .or_else(|| win.strip_prefix(r"\\.\"))
        .unwrap_or(win);
    let bytes = win.as_bytes();
    if bytes.len() < 2 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' {
        // True UNC / non-drive Windows path — no drive to map onto /mnt.
        return "/tmp".to_string();
    }
    let drive = (bytes[0] as char).to_ascii_lowercase();
    let rest = &win[2..]; // after `C:`
    let rest = rest.replace('\\', "/");
    let rest = rest.trim_start_matches('/');
    if rest.is_empty() {
        format!("/mnt/{drive}")
    } else {
        format!("/mnt/{drive}/{rest}")
    }
}

/// `/mnt/c/Users/me` → `Some(C:\Users\me)`; non-`/mnt` POSIX → `None`.
fn mnt_to_windows(posix: &str) -> Option<PathBuf> {
    let posix = posix.trim();
    let rest = posix.strip_prefix("/mnt/")?;
    let mut chars = rest.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() {
        return None;
    }
    // After the drive letter we require a mountpoint boundary: either
    // end-of-string (`/mnt/c`) or a `/` (`/mnt/c/...`). Reject things like
    // `/mnt/cUsers`, which is an unrelated POSIX path, not a WSL mountpoint.
    let after = &rest[1..];
    if !after.is_empty() && !after.starts_with('/') {
        return None;
    }
    let after = after.strip_prefix('/').unwrap_or(after);
    let drive_up = drive.to_ascii_uppercase();
    if after.is_empty() {
        Some(PathBuf::from(format!("{drive_up}:\\")))
    } else {
        Some(PathBuf::from(format!(
            "{drive_up}:\\{}",
            after.replace('/', "\\")
        )))
    }
}

/// Junk launcher dirs WT/Windows hand back when there's no real cwd:
/// `C:\Windows\System32` and `C:\Windows`. Deliberately small — drive roots
/// and `%USERPROFILE%` are legitimate and must not be treated as junk.
fn is_junk(path: &Path) -> bool {
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let system32 = system_root.join("System32");
    path_eq_ci(path, &system_root) || path_eq_ci(path, &system32)
}

fn user_profile_dir() -> PathBuf {
    // This helper is the *Windows-namespace* fallback, so it must always
    // return a Windows path and never the junk launcher dir. Resolution:
    //   1. USERPROFILE, but only if it's Windows-looking AND not a junk
    //      launcher dir — some MSYS/Git-Bash setups export a POSIX-style
    //      USERPROFILE (e.g. `/c/Users/u`), and a misconfigured one could even
    //      point at `C:\Windows\System32` (the very junk `pick_value` avoids),
    //      so both are skipped.
    //   2. HOME, with the same Windows-looking + non-junk guard — a POSIX HOME
    //      (e.g. Git Bash's `/home/u`) or a junk HOME is likewise skipped.
    //   3. %SystemDrive%\ (e.g. `C:\`) — a guaranteed-valid Windows dir.
    //      Deliberately NOT `current_dir()`, which can be C:\WINDOWS\system32
    //      for the packaged helper (the very junk we're avoiding).
    if let Some(p) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
        let profile = PathBuf::from(p);
        if classify(&profile) == PathFormat::Windows && !is_junk(&profile) {
            return profile;
        }
    }
    if let Some(h) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        let home = PathBuf::from(h);
        if classify(&home) == PathFormat::Windows && !is_junk(&home) {
            return home;
        }
    }
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    PathBuf::from(format!("{drive}\\"))
}

fn path_eq_ci(a: &Path, b: &Path) -> bool {
    fn norm(p: &Path) -> String {
        let s = p.to_string_lossy();
        // Strip verbatim / device prefixes so `\\?\C:\Windows\System32`
        // normalizes the same as `C:\Windows\System32` — otherwise a
        // verbatim junk path would slip past `is_junk`.
        let s: &str = s
            .strip_prefix(r"\\?\")
            .or_else(|| s.strip_prefix(r"\\.\"))
            .unwrap_or(&s);
        s.trim_end_matches(['\\', '/'])
            .to_ascii_lowercase()
            .replace('/', "\\")
    }
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    // Serializes + restores process-wide env mutations so parallel tests
    // don't clobber each other's USERPROFILE/SystemRoot. Uses the CRATE-WIDE
    // env lock (`test_support::lock_env`) so these tests serialize against
    // every other env-mutating test in the crate, not just this module's — a
    // module-local lock would still race `std::env` (a process global) against
    // tests elsewhere. The guard restores prior values on drop (incl. during
    // panic-unwind from a failed assert), while still holding the lock.
    struct EnvGuard {
        saved: Vec<(String, Option<OsString>)>,
        _lock: crate::test_support::EnvGuard,
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, old) in &self.saved {
                match old {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
    fn scoped_env(vars: &[(&str, &str)]) -> EnvGuard {
        let lock = crate::test_support::lock_env();
        let saved = vars
            .iter()
            .map(|(k, _)| (k.to_string(), std::env::var_os(k)))
            .collect();
        for (k, v) in vars {
            std::env::set_var(k, v);
        }
        EnvGuard { saved, _lock: lock }
    }

    #[test]
    fn classify_basic() {
        // POSIX = leading '/' (after trimming leading whitespace).
        assert_eq!(classify(Path::new("/home/u")), PathFormat::Posix);
        assert_eq!(classify(Path::new("/mnt/c/foo")), PathFormat::Posix);
        assert_eq!(classify(Path::new("  /leading/space")), PathFormat::Posix);
        // Everything else is Windows: drive (back- or forward-slash), bare
        // drive, UNC, extended-length, even a bare relative fragment.
        assert_eq!(classify(Path::new(r"C:\foo")), PathFormat::Windows);
        assert_eq!(classify(Path::new("C:/foo")), PathFormat::Windows);
        assert_eq!(classify(Path::new("C:")), PathFormat::Windows);
        assert_eq!(classify(Path::new(r"\\server\share")), PathFormat::Windows);
        assert_eq!(classify(Path::new(r"\\?\C:\foo")), PathFormat::Windows);
        assert_eq!(classify(Path::new(r"\\wsl$\Ubuntu\home\u")), PathFormat::Windows);
        assert_eq!(classify(Path::new(r"relative\path")), PathFormat::Windows);
    }

    #[test]
    fn detect_format_from_session_cwd_values() {
        assert_eq!(
            detect_format(["/home/yeelam", "/mnt/c/x"]),
            Some(PathFormat::Posix)
        );
        assert_eq!(
            detect_format([r"Q:\official", r"C:\Users\me"]),
            Some(PathFormat::Windows)
        );
        // Leading empty/blank entries are skipped; first real one decides.
        assert_eq!(detect_format(["", "   ", "/home/u"]), Some(PathFormat::Posix));
        // Empty list / all-blank → unknown.
        assert_eq!(detect_format(Vec::<&str>::new()), None);
        assert_eq!(detect_format(["", "  "]), None);
    }

    #[test]
    fn windows_linux_round_trips() {
        // A real drive path survives a round trip through both converters.
        let win = Path::new(r"C:\Users\me");
        let posix = to_linux_format(win);
        assert_eq!(posix, PathBuf::from("/mnt/c/Users/me"));
        assert_eq!(to_windows_format(&posix), PathBuf::from(r"C:\Users\me"));
        // bare drive
        assert_eq!(to_linux_format(Path::new("C:")), PathBuf::from("/mnt/c"));
    }

    #[test]
    fn to_linux_is_idempotent_and_converts() {
        // already posix → unchanged
        assert_eq!(to_linux_format(Path::new("/home/u")), PathBuf::from("/home/u"));
        // windows drive → /mnt
        assert_eq!(
            to_linux_format(Path::new(r"C:\Users\me")),
            PathBuf::from("/mnt/c/Users/me")
        );
        assert_eq!(
            to_linux_format(Path::new(r"Q:\official\repo")),
            PathBuf::from("/mnt/q/official/repo")
        );
        assert_eq!(to_linux_format(Path::new(r"C:\")), PathBuf::from("/mnt/c"));
        // extended-length \\?\C:\foo → /mnt/c/foo (prefix stripped)
        assert_eq!(
            to_linux_format(Path::new(r"\\?\C:\foo")),
            PathBuf::from("/mnt/c/foo")
        );
        // true UNC has no /mnt mapping → safe POSIX floor
        assert_eq!(
            to_linux_format(Path::new(r"\\server\share")),
            PathBuf::from("/tmp")
        );
    }

    #[test]
    fn to_windows_is_idempotent_and_converts() {
        let _g = scoped_env(&[("USERPROFILE", r"C:\Users\tester")]);
        // already windows → unchanged
        assert_eq!(
            to_windows_format(Path::new(r"Q:\official")),
            PathBuf::from(r"Q:\official")
        );
        // /mnt → drive
        assert_eq!(
            to_windows_format(Path::new("/mnt/c/Users/me")),
            PathBuf::from(r"C:\Users\me")
        );
        assert_eq!(
            to_windows_format(Path::new("/mnt/q")),
            PathBuf::from(r"Q:\")
        );
        // non-/mnt posix → %USERPROFILE%
        assert_eq!(
            to_windows_format(Path::new("/home/yeelam")),
            PathBuf::from(r"C:\Users\tester")
        );
        // malformed /mnt (no boundary after drive) is NOT a mountpoint → %USERPROFILE%
        assert_eq!(
            to_windows_format(Path::new("/mnt/cUsers")),
            PathBuf::from(r"C:\Users\tester")
        );
    }

    #[test]
    fn pick_value_drops_junk() {
        let _g = scoped_env(&[("SystemRoot", r"C:\Windows"), ("USERPROFILE", r"C:\Users\tester")]);
        assert_eq!(
            pick_value(Some(Path::new(r"C:\WINDOWS\system32"))),
            PathBuf::from(r"C:\Users\tester")
        );
        assert_eq!(
            pick_value(Some(Path::new(r"C:\Windows"))),
            PathBuf::from(r"C:\Users\tester")
        );
        assert_eq!(pick_value(None), PathBuf::from(r"C:\Users\tester"));
        // verbatim/extended-length junk is also detected
        assert_eq!(
            pick_value(Some(Path::new(r"\\?\C:\WINDOWS\system32"))),
            PathBuf::from(r"C:\Users\tester")
        );
        // real paths pass through (windows or posix)
        assert_eq!(
            pick_value(Some(Path::new(r"Q:\repo"))),
            PathBuf::from(r"Q:\repo")
        );
        assert_eq!(
            pick_value(Some(Path::new("/home/yeelam"))),
            PathBuf::from("/home/yeelam")
        );
    }

    #[test]
    fn user_profile_dir_always_returns_windows_path() {
        // USERPROFILE empty + a POSIX HOME must NOT yield the POSIX HOME or a
        // junk current_dir; it falls back to %SystemDrive%\ (a Windows path).
        let _g = scoped_env(&[
            ("USERPROFILE", ""),
            ("HOME", "/home/u"),
            ("SystemDrive", "C:"),
        ]);
        let got = user_profile_dir();
        assert_eq!(classify(&got), PathFormat::Windows);
        assert_eq!(got, PathBuf::from(r"C:\"));
    }

    #[test]
    fn user_profile_dir_skips_posix_userprofile() {
        // A POSIX-style USERPROFILE (some MSYS/Git-Bash setups) must be
        // skipped, not returned verbatim, so the Windows-namespace contract
        // holds. With a Windows HOME available, that HOME wins.
        let _g = scoped_env(&[
            ("USERPROFILE", "/c/Users/u"),
            ("HOME", r"D:\home\u"),
            ("SystemDrive", "C:"),
        ]);
        let got = user_profile_dir();
        assert_eq!(classify(&got), PathFormat::Windows);
        assert_eq!(got, PathBuf::from(r"D:\home\u"));
    }

    #[test]
    fn user_profile_dir_posix_userprofile_and_home_falls_back_to_drive() {
        // Both USERPROFILE and HOME POSIX-style → neither is usable, so we
        // land on %SystemDrive%\ rather than emitting a POSIX path.
        let _g = scoped_env(&[
            ("USERPROFILE", "/c/Users/u"),
            ("HOME", "/home/u"),
            ("SystemDrive", "C:"),
        ]);
        let got = user_profile_dir();
        assert_eq!(classify(&got), PathFormat::Windows);
        assert_eq!(got, PathBuf::from(r"C:\"));
    }

    #[test]
    fn user_profile_dir_skips_junk_userprofile() {
        // A misconfigured USERPROFILE pointing at the junk launcher dir must be
        // skipped (honoring the "never return junk" contract); with no usable
        // HOME it falls back to %SystemDrive%\.
        let _g = scoped_env(&[
            ("USERPROFILE", r"C:\Windows\System32"),
            ("HOME", ""),
            ("SystemRoot", r"C:\Windows"),
            ("SystemDrive", "C:"),
        ]);
        let got = user_profile_dir();
        assert_eq!(classify(&got), PathFormat::Windows);
        assert!(!is_junk(&got));
        assert_eq!(got, PathBuf::from(r"C:\"));
    }

    #[test]
    fn looks_like_cwd_error_requires_path_context() {
        // Real agent cwd rejections (carry a directory/path context) → retry.
        assert!(looks_like_cwd_error(
            "Directory path must be absolute: C:\\WINDOWS\\system32"
        ));
        assert!(looks_like_cwd_error(
            "Directory does not exist or cannot be accessed"
        ));
        assert!(looks_like_cwd_error("cwd is not a directory"));
        // Unrelated errors that merely mention a rejection word but have no
        // path/directory/cwd context → NOT retried.
        assert!(!looks_like_cwd_error("The requested model does not exist"));
        assert!(!looks_like_cwd_error(
            "absolute URL required for the resource endpoint"
        ));
        assert!(!looks_like_cwd_error("authentication required"));
    }

    #[test]
    fn build_attempts_linux_target() {
        // windows value, linux agent → /mnt then /tmp
        assert_eq!(
            build_attempts(Path::new(r"Q:\repo"), Some(PathFormat::Posix)),
            vec![PathBuf::from("/mnt/q/repo"), PathBuf::from("/tmp")]
        );
        // posix value, linux agent → as-is then /tmp
        assert_eq!(
            build_attempts(Path::new("/home/u"), Some(PathFormat::Posix)),
            vec![PathBuf::from("/home/u"), PathBuf::from("/tmp")]
        );
    }

    #[test]
    fn build_attempts_windows_target() {
        let _g = scoped_env(&[("USERPROFILE", r"C:\Users\tester")]);
        assert_eq!(
            build_attempts(Path::new(r"Q:\repo"), Some(PathFormat::Windows)),
            vec![PathBuf::from(r"Q:\repo")]
        );
        // posix value, windows agent → converts (/mnt) or USERPROFILE
        assert_eq!(
            build_attempts(Path::new("/mnt/c/x"), Some(PathFormat::Windows)),
            vec![PathBuf::from(r"C:\x")]
        );
    }

    #[test]
    fn build_attempts_unknown_target_tries_both() {
        let _g = scoped_env(&[("USERPROFILE", r"C:\Users\tester")]);
        // windows value, unknown → windows, then linux, then /tmp
        let got = build_attempts(Path::new(r"Q:\repo"), None);
        assert_eq!(
            got,
            vec![
                PathBuf::from(r"Q:\repo"),
                PathBuf::from("/mnt/q/repo"),
                PathBuf::from("/tmp"),
            ]
        );
        // posix value, unknown → linux first, then windows, then /tmp
        let got2 = build_attempts(Path::new("/home/u"), None);
        assert_eq!(
            got2,
            vec![
                PathBuf::from("/home/u"),
                PathBuf::from(r"C:\Users\tester"),
                PathBuf::from("/tmp"),
            ]
        );
    }
}

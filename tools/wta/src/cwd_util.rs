//! Drops stale cwd values before wta hands them to a launcher (`wtcli new-tab -d`,
//! `resume_in_new_agent_tab`, `--initial-load-cwd`). A missing cwd would
//! otherwise turn into a broken pane via `CreateProcessW` failing with
//! `ERROR_DIRECTORY`. On failure we omit the arg so the launcher uses its
//! own default chain (profile `startingDirectory` → `%USERPROFILE%`).
//!
//! Only *local* Windows paths are existence-checked. Unix-style, WSL UNC
//! and network UNC paths pass through unchanged: they can't be checked
//! against the Windows fs without false-rejecting valid WSL paths, and
//! WSL/network UNC can stall `fs::metadata` for seconds (see GH#9541).
//!
//! Launchers that hand the value to a **Windows** process (`wtcli new-tab
//! -d`, a WT tab's `startingDirectory`) must use
//! [`validate_windows_starting_directory`] instead: a POSIX path is a valid
//! cwd for an in-distro agent but not for `CreateProcessW`.

use std::path::{Component, Path, Prefix};

/// `None` if `path` is empty, or if it's a local Windows path that
/// doesn't exist / isn't a directory. `Some(s)` otherwise — including
/// non-local paths, which are passed through unvalidated.
pub fn validate_starting_directory<P: AsRef<Path>>(path: P) -> Option<String> {
    let p = path.as_ref();
    let s = p.to_string_lossy();
    if s.is_empty() {
        return None;
    }
    if !is_local_windows_path(p) {
        return Some(s.into_owned());
    }
    match std::fs::metadata(p) {
        Ok(meta) if meta.is_dir() => Some(s.into_owned()),
        _ => None,
    }
}

/// [`validate_starting_directory`] plus a POSIX-path rejection, for
/// launchers whose target is a **Windows** process.
///
/// A session's recorded cwd can be POSIX (`/home/me`) even when the row is
/// stamped `SessionLocation::Host` — an agent CLI that syncs its sessions
/// from a backend reports the cwd of the machine that created them, so a
/// Linux-created session shows up in a Windows CLI's `session/list`.
/// Handing that path to `wtcli new-tab -d` (or to a tab's
/// `startingDirectory`) fails the launch outright with
/// `ERROR_DIRECTORY (0x8007010b)` — WT deliberately passes "linux-y" paths
/// through verbatim (`Utils::EvaluateStartingDirectory`, GH#592) so only a
/// WSL profile can mangle them. Dropping the cwd lets the launcher fall
/// back to the profile default and the CLI report its own resume result.
pub fn validate_windows_starting_directory<P: AsRef<Path>>(path: P) -> Option<String> {
    let p = path.as_ref();
    if is_posix_style_path(p) {
        return None;
    }
    validate_starting_directory(p)
}

/// `true` for paths that only resolve on a Unix host (a WSL distro): POSIX
/// absolute (`/`, `/home/me`) and home-relative (`~`, `~/proj`) forms.
/// These are exactly the shapes WT treats as "linux-y" and refuses to
/// resolve against the Windows filesystem. Forward-slash UNC
/// (`//server/share`) is NOT POSIX-style — Windows opens it fine.
fn is_posix_style_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    match bytes.first() {
        Some(b'/') => bytes.get(1) != Some(&b'/'),
        Some(b'~') => matches!(bytes.get(1), None | Some(b'/') | Some(b'\\')),
        _ => false,
    }
}

/// `true` for drive-letter (`C:\…`) and verbatim-drive (`\\?\C:\…`)
/// paths only. UNC, device-namespace, Unix-style, and relative paths
/// return `false` and skip the existence check.
fn is_local_windows_path(path: &Path) -> bool {
    matches!(
        path.components().next(),
        Some(Component::Prefix(p))
            if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
    )
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
    fn nonexistent_local_path_returns_none() {
        let p = unique_temp_dir("nope");
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
    fn existing_directory_returns_path_string() {
        let dir = unique_temp_dir("ok");
        fs::create_dir_all(&dir).unwrap();
        let got = validate_starting_directory(&dir);
        assert_eq!(got, Some(dir.to_string_lossy().into_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    /// Unix-style paths (typical for WSL profiles) must pass through
    /// unchanged. They can't be validated against the Windows filesystem
    /// without false-rejecting every WSL session — that bug would show
    /// up as "all my WSL agent panes boot in %USERPROFILE% instead of
    /// the project root".
    #[test]
    fn unix_style_paths_pass_through_unchanged() {
        for s in ["/home/user/proj", "/", "/tmp", "~/work", "~"] {
            assert_eq!(
                validate_starting_directory(s),
                Some(s.to_string()),
                "unix-style path `{}` was filtered",
                s
            );
        }
    }

    /// WSL UNC paths can stall `fs::metadata` for seconds when the
    /// distro is stopped — the exact failure GH#9541 fixed in WT. We
    /// avoid the syscall entirely and trust WT/wtcli to surface a real
    /// failure inline if the path is unreachable.
    #[test]
    fn wsl_unc_paths_pass_through_unchanged() {
        for s in [
            r"\\wsl$\Ubuntu\home\user\proj",
            r"\\wsl.localhost\Ubuntu\home\user\proj",
            r"\\?\UNC\wsl$\Ubuntu\home\user\proj",
        ] {
            assert_eq!(
                validate_starting_directory(s),
                Some(s.to_string()),
                "WSL UNC path `{}` was filtered",
                s
            );
        }
    }

    /// Generic SMB / network UNC paths are also pass-through: the
    /// remote host may be unreachable and we don't want to gate a
    /// launch on a network round-trip.
    #[test]
    fn network_unc_paths_pass_through_unchanged() {
        for s in [r"\\server\share\proj", r"\\10.0.0.1\share\dir"] {
            assert_eq!(
                validate_starting_directory(s),
                Some(s.to_string()),
                "UNC path `{}` was filtered",
                s
            );
        }
    }

    /// Relative paths have no anchor we can resolve from here. Pass
    /// through and let the launcher interpret them in its own context.
    #[test]
    fn relative_paths_pass_through_unchanged() {
        for s in ["foo", r"foo\bar", "./foo", r".\foo"] {
            assert_eq!(
                validate_starting_directory(s),
                Some(s.to_string()),
                "relative path `{}` was filtered",
                s
            );
        }
    }

    /// Extended-length drive-letter form (`\\?\C:\...`) is a local
    /// Windows path and SHOULD be validated like a regular `C:\...`.
    #[test]
    fn extended_length_drive_letter_is_validated() {
        let dir = unique_temp_dir("extended_len");
        fs::create_dir_all(&dir).unwrap();
        let ext = format!(r"\\?\{}", dir.to_string_lossy());
        let got = validate_starting_directory(&ext);
        assert_eq!(got, Some(ext.clone()));
        // Missing extended-length path → None.
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(validate_starting_directory(&ext), None);
    }

    /// The Windows-strict variant must drop POSIX cwds. A cloud-synced
    /// agent session created inside WSL is listed by the *Windows* CLI with
    /// its Linux cwd, so the row is `SessionLocation::Host` but its cwd
    /// only exists in the distro. Passing it to `wtcli new-tab -d` fails
    /// the whole launch with `ERROR_DIRECTORY (0x8007010b)`.
    #[test]
    fn windows_variant_rejects_posix_style_paths() {
        for s in ["/home/user/proj", "/", "/tmp", "~", "~/work", r"~\work"] {
            assert_eq!(
                validate_windows_starting_directory(s),
                None,
                "POSIX-style path `{}` must not be used as a Windows starting directory",
                s
            );
            assert_eq!(
                validate_starting_directory(s),
                Some(s.to_string()),
                "the lenient variant must still pass `{}` through for in-distro use",
                s
            );
        }
    }

    /// Everything the lenient variant accepts and that Windows can actually
    /// open must survive the strict variant unchanged.
    #[test]
    fn windows_variant_keeps_windows_resolvable_paths() {
        let dir = unique_temp_dir("win_ok");
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            validate_windows_starting_directory(&dir),
            Some(dir.to_string_lossy().into_owned())
        );
        let _ = fs::remove_dir_all(&dir);

        for s in [
            r"\\wsl$\Ubuntu\home\user\proj",
            r"\\wsl.localhost\Ubuntu\home\user\proj",
            r"\\server\share\proj",
            "//server/share/proj",
            r"foo\bar",
            "~tilde-prefixed-name",
        ] {
            assert_eq!(
                validate_windows_starting_directory(s),
                Some(s.to_string()),
                "`{}` is Windows-resolvable and must be kept",
                s
            );
        }

        assert_eq!(validate_windows_starting_directory(""), None);
    }

    #[test]
    fn posix_style_path_classification() {
        for s in ["/", "/home/user", "/mnt/c/src", "~", "~/proj", r"~\proj"] {
            assert!(is_posix_style_path(Path::new(s)), "should be POSIX: {}", s);
        }
        for s in [
            "",
            r"C:\foo",
            "C:/foo",
            r"\\wsl$\Ubuntu\home",
            r"\\server\share",
            "//server/share",
            r"\foo",
            "foo/bar",
            "~tilde-prefixed-name",
        ] {
            assert!(
                !is_posix_style_path(Path::new(s)),
                "should NOT be POSIX: {}",
                s
            );
        }
    }

    #[test]
    fn is_local_windows_path_classification() {
        // Local Windows: drive-letter forms.
        for s in [
            r"C:\",
            r"C:\foo",
            "C:/foo",
            "C:",
            r"d:\users\me",
            r"\\?\C:\foo",
            r"\\?\D:\bar",
        ] {
            assert!(
                is_local_windows_path(Path::new(s)),
                "should be local: {}",
                s
            );
        }
        // NOT local: unix, UNC (including WSL & extended-length UNC), relative.
        for s in [
            "",
            "/",
            "/home/user",
            "~/proj",
            "~",
            r"\\server\share",
            r"\\wsl$\Ubuntu\home",
            r"\\wsl.localhost\Ubuntu\home",
            r"\\?\UNC\server\share",
            r"\\?\UNC\wsl$\Ubuntu",
            "foo",
            r"foo\bar",
            "./foo",
        ] {
            assert!(
                !is_local_windows_path(Path::new(s)),
                "should NOT be local: {}",
                s
            );
        }
    }
}


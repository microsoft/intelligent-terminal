use std::path::PathBuf;
use std::sync::OnceLock;

// WTA runtime data lives under two package-private roots, split by lifetime:
//
//   * **State** (`intelligent_terminal_root`) — persistent data that must
//     survive and stay private to the package: the prompt-override directory,
//     the agent-pane session index (`agent-pane-sessions.jsonl`), and the
//     master-pipe rendezvous file. Stored in the package's `LocalState`,
//     alongside the WT app's own `settings.json` / `state.json`.
//
//   * **Local / cache** (`intelligent_terminal_local_root`) — transient,
//     regenerable diagnostics: log files and hook-bundle staging copies.
//     Stored in the package's `LocalCache\Local`, the cache store that
//     doesn't roam / back up.
//
// Both roots are package-private (cleaned up on uninstall, isolated between
// the dev-sideload family `IntelligentTerminal_rd9vj3e6a2mbr` and the store
// family `Microsoft.IntelligentTerminal_8wekyb3d8bbwe`). Both fall back to
// the same legacy bare `%LOCALAPPDATA%\IntelligentTerminal\` when the process
// has no package identity (dev builds run straight out of the Cargo target
// dir, tests) — such processes already fail COM activation (0x80073D54), so
// the fallback exists only so logging / tests keep working out of package.

/// Persistent, package-private **state** root: `…\LocalState\IntelligentTerminal\`
/// (or bare `%LOCALAPPDATA%\IntelligentTerminal\` when unpackaged). Backs
/// prompts, the agent-pane session index, and the master-pipe file.
///
/// Cached: queried on the hot path and package-identity lookup is a syscall.
pub fn intelligent_terminal_root() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| resolve_root(&["LocalState"])).clone()
}

/// Transient, package-private **cache** root:
/// `…\LocalCache\Local\IntelligentTerminal\` (or bare
/// `%LOCALAPPDATA%\IntelligentTerminal\` when unpackaged). Backs log files
/// and hook-bundle staging.
///
/// Cached for the same reason as [`intelligent_terminal_root`].
pub fn intelligent_terminal_local_root() -> Option<PathBuf> {
    static ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    ROOT.get_or_init(|| resolve_root(&["LocalCache", "Local"]))
        .clone()
}

/// Resolve a root under `%LOCALAPPDATA%`. When the process is packaged the
/// data lands under `Packages\<PackageFamilyName>\<package_subdir…>\IntelligentTerminal`;
/// otherwise it falls back to the bare `%LOCALAPPDATA%\IntelligentTerminal`
/// (the `package_subdir` is ignored, since there is no package store to
/// nest under).
fn resolve_root(package_subdir: &[&str]) -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)?;

    match current_package_family_name() {
        Some(family) => {
            let mut path = local.join("Packages").join(family);
            for segment in package_subdir {
                path.push(segment);
            }
            Some(path.join("IntelligentTerminal"))
        }
        None => Some(local.join("IntelligentTerminal")),
    }
}

/// Returns the current process's package family name (e.g.
/// `IntelligentTerminal_rd9vj3e6a2mbr`), or `None` when the process has no
/// package identity (unpackaged) or the OS call fails for any other reason.
fn current_package_family_name() -> Option<std::ffi::OsString> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;

    // First call with a null buffer queries the required length. A packaged
    // process returns ERROR_INSUFFICIENT_BUFFER and fills `len`; an
    // unpackaged process returns APPMODEL_ERROR_NO_PACKAGE (any non-122 rc
    // means "no usable identity" for our purposes).
    let mut len: u32 = 0;
    let rc = unsafe { GetCurrentPackageFamilyName(&mut len, std::ptr::null_mut()) };
    if rc != ERROR_INSUFFICIENT_BUFFER || len == 0 {
        return None;
    }

    // `len` includes the trailing NUL; allocate exactly that and call again.
    let mut buf = vec![0u16; len as usize];
    let rc = unsafe { GetCurrentPackageFamilyName(&mut len, buf.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }

    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    if end == 0 {
        return None;
    }
    Some(std::ffi::OsString::from_wide(&buf[..end]))
}

pub fn runtime_prompt_root() -> Option<PathBuf> {
    intelligent_terminal_root().map(|root| root.join("prompts"))
}

pub fn runtime_log_path(file_name: &str) -> PathBuf {
    if let Some(root) = intelligent_terminal_local_root() {
        let log_dir = root.join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        return log_dir.join(file_name);
    }

    PathBuf::from(file_name)
}

pub fn master_pipe_file_path() -> Option<PathBuf> {
    intelligent_terminal_root().map(|root| root.join("master-pipe.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpackaged_process_has_no_package_family_name() {
        // `cargo test` runs the test binary without package identity, so the
        // OS call must report "no package" and we must fall back gracefully
        // rather than panicking or returning a bogus name.
        assert_eq!(current_package_family_name(), None);
    }

    #[test]
    fn unpackaged_roots_fall_back_to_bare_intelligent_terminal() {
        // With no package identity (test context), BOTH roots collapse to the
        // legacy bare `…\IntelligentTerminal` — there is no package store to
        // nest the LocalState / LocalCache split under. Guard that neither
        // leaks a package-relative segment, so a regression that always emits
        // the packaged layout is caught.
        for root in [
            resolve_root(&["LocalState"]),
            resolve_root(&["LocalCache", "Local"]),
        ] {
            let root = root.expect("LOCALAPPDATA/APPDATA set in CI/dev");
            assert!(
                root.ends_with("IntelligentTerminal"),
                "unexpected root: {}",
                root.display(),
            );
            let s = root.to_string_lossy();
            assert!(
                !s.contains("LocalState") && !s.contains("LocalCache"),
                "unpackaged root must not point into a package store: {}",
                root.display(),
            );
        }
    }

    #[test]
    fn state_and_local_roots_agree_when_unpackaged() {
        // The state (LocalState) and local (LocalCache\Local) roots only
        // diverge under package identity; unpackaged they must resolve to the
        // same bare directory so dev runs keep a single on-disk root.
        assert_eq!(
            intelligent_terminal_root(),
            intelligent_terminal_local_root(),
        );
    }
}

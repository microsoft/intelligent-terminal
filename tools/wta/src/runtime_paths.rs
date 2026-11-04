use std::path::PathBuf;
use std::sync::OnceLock;

// WTA runtime data lives under two package-private roots, split by lifetime:
//
//   * **State** (`intelligent_terminal_root`) — persistent data that must
//     survive and stay private to the package: the prompt-override directory
//     and agent-pane session index (`agent-pane-sessions.jsonl`).
//     Stored in the package's `LocalState`,
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
/// prompts and the agent-pane session index.
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

/// Short, package-private scratch root for process-owned ACP runtimes.
pub(crate) fn acp_runtime_root() -> anyhow::Result<PathBuf> {
    use anyhow::{ensure, Context};

    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .context("LOCALAPPDATA is required for ACP runtime storage")?;
    ensure!(
        local.is_absolute(),
        "ACP runtime storage requires an absolute local path"
    );
    let family = query_package_family_name()?;
    Ok(acp_runtime_root_for(&local, family.as_deref()))
}

fn acp_runtime_root_for(local: &std::path::Path, family: Option<&std::ffi::OsStr>) -> PathBuf {
    match family {
        Some(family) => local
            .join("Packages")
            .join(family)
            .join("LocalCache")
            .join("acp"),
        None => local.join("IntelligentTerminal").join("acp"),
    }
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
/// `IntelligentTerminal_rd9vj3e6a2mbr` for dev-sideload, or
/// `Microsoft.IntelligentTerminal_8wekyb3d8bbwe` for the store family), or
/// `None` when the process has no package identity (unpackaged) or the OS
/// call fails for any other reason.
pub(crate) fn current_package_family_name() -> Option<std::ffi::OsString> {
    query_package_family_name().ok().flatten()
}

fn query_package_family_name() -> anyhow::Result<Option<std::ffi::OsString>> {
    use anyhow::{ensure, Context};
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::Foundation::{APPMODEL_ERROR_NO_PACKAGE, ERROR_INSUFFICIENT_BUFFER};
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;

    // SAFETY: the first call queries the size; the second owns that many UTF-16 units.
    let mut len: u32 = 0;
    let rc = unsafe { GetCurrentPackageFamilyName(&mut len, std::ptr::null_mut()) };
    if rc == APPMODEL_ERROR_NO_PACKAGE {
        return Ok(None);
    }
    if rc != ERROR_INSUFFICIENT_BUFFER {
        return Err(std::io::Error::from_raw_os_error(rc as i32))
            .context("query current package identity");
    }
    ensure!(len > 0, "package identity length is empty");

    // `len` includes the trailing NUL; allocate exactly that and call again.
    let mut buf = vec![0u16; len as usize];
    let rc = unsafe { GetCurrentPackageFamilyName(&mut len, buf.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::from_raw_os_error(rc as i32))
            .context("read current package identity");
    }

    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    ensure!(end > 0, "package identity is empty");
    Ok(Some(std::ffi::OsString::from_wide(&buf[..end])))
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

/// Resolve the C++ Windows Terminal app's `state.json`.
///
/// Resolution order:
///
/// 1. If the *current* process has package identity (the production case —
///    `wta.exe` deployed inside CascadiaPackage), use
///    [`current_package_family_name`] so we read the state.json belonging to
///    our own install (dev-sideload **or** store) rather than guessing.
///
/// 2. When the process is unpackaged (dev tree: unpackaged `wta.exe`
///    launched by a packaged WT via `TerminalPage::_DetectWtaPath`), scan
///    the `Packages` subdirectory under `%LOCALAPPDATA%` (or `%APPDATA%`
///    when `%LOCALAPPDATA%` is unset — mirrors `resolve_root`'s env-var
///    fallback) for any directory whose name matches a *known* WT
///    package family: `IntelligentTerminal_*` (dev sideload — suffix
///    varies per signing cert) or `Microsoft.IntelligentTerminal_*`
///    (store). Return the first one whose `state.json` exists.
///
/// Returns `None` when neither `%LOCALAPPDATA%` nor `%APPDATA%` is set or
/// no candidate `state.json` exists on disk.
pub fn wt_state_json_path() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)?;

    if let Some(family) = current_package_family_name() {
        let candidate = local
            .join("Packages")
            .join(family)
            .join("LocalState")
            .join("state.json");
        return candidate.exists().then_some(candidate);
    }

    let packages = local.join("Packages");
    // Collect matches, then pick deterministically by family priority:
    // dev-sideload first (`IntelligentTerminal_*`), then store
    // (`Microsoft.IntelligentTerminal_*`). `read_dir` enumeration order is
    // unspecified, so without an explicit priority the choice between
    // two installed families would non-deterministically vary across
    // machines and runs. Dev wins because unpackaged wta is itself a
    // dev-only scenario — devs running out-of-package overwhelmingly have
    // the dev-sideload install.
    let mut dev_match: Option<PathBuf> = None;
    let mut store_match: Option<PathBuf> = None;
    for entry in std::fs::read_dir(&packages).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let candidate = entry.path().join("LocalState").join("state.json");
        if !candidate.exists() {
            continue;
        }
        if dev_match.is_none() && name.starts_with("IntelligentTerminal_") {
            dev_match = Some(candidate);
        } else if store_match.is_none() && name.starts_with("Microsoft.IntelligentTerminal_") {
            store_match = Some(candidate);
        }
        if dev_match.is_some() && store_match.is_some() {
            break;
        }
    }
    dev_match.or(store_match)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_runtime_root_is_short_and_isolated_by_package() {
        let local = std::path::Path::new(r"C:\Users\Example\AppData\Local");
        let dev = acp_runtime_root_for(local, Some(std::ffi::OsStr::new("Dev_family")));
        let store = acp_runtime_root_for(local, Some(std::ffi::OsStr::new("Store_family")));
        assert_eq!(dev, local.join(r"Packages\Dev_family\LocalCache\acp"));
        assert_eq!(store, local.join(r"Packages\Store_family\LocalCache\acp"));
        assert_eq!(
            acp_runtime_root_for(local, None),
            local.join(r"IntelligentTerminal\acp")
        );
    }

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

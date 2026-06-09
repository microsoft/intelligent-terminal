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

// ── Win32 FFI (inline; see module docs for why not windows-sys) ──────────

/// Subset of `PROCESS_BASIC_INFORMATION` we read. Layout matches the OS
/// struct on x64; we only touch `peb_base_address` and
/// `inherited_from_unique_process_id`.
#[allow(non_snake_case)]
#[repr(C)]
struct ProcessBasicInformation {
    exit_status: i32,
    peb_base_address: usize,
    affinity_mask: usize,
    base_priority: i32,
    unique_process_id: usize,
    inherited_from_unique_process_id: usize,
}

// PROCESS_QUERY_INFORMATION (0x0400) | PROCESS_VM_READ (0x0010). A superset
// of QUERY_LIMITED_INFORMATION, sufficient for NtQueryInformationProcess and
// ReadProcessMemory on same-user processes.
const PROCESS_ACCESS: u32 = 0x0410;

#[link(name = "ntdll")]
extern "system" {
    fn NtQueryInformationProcess(
        handle: isize,
        info_class: i32,
        process_info: *mut core::ffi::c_void,
        process_info_len: u32,
        return_len: *mut u32,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(desired_access: u32, inherit: i32, pid: u32) -> isize;
    fn CloseHandle(handle: isize) -> i32;
    fn ReadProcessMemory(
        handle: isize,
        base_address: usize,
        buffer: *mut core::ffi::c_void,
        size: usize,
        bytes_read: *mut usize,
    ) -> i32;
}

/// RAII wrapper so we never leak a process handle across an early return.
struct ProcHandle(isize);

impl ProcHandle {
    /// Open `pid` for query + VM read. Returns `None` if the process is
    /// gone or access is denied (e.g. a different user / elevation).
    fn open(pid: u32) -> Option<Self> {
        // SAFETY: OpenProcess with a valid access mask; returns 0 on failure.
        let h = unsafe { OpenProcess(PROCESS_ACCESS, 0, pid) };
        if h == 0 {
            None
        } else {
            Some(ProcHandle(h))
        }
    }
}

impl Drop for ProcHandle {
    fn drop(&mut self) {
        // SAFETY: self.0 is a handle from OpenProcess, closed exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// Query `PROCESS_BASIC_INFORMATION` for an open handle.
fn basic_information(handle: isize) -> Option<ProcessBasicInformation> {
    // SAFETY: zeroed POD is a valid initial value; the struct is repr(C) and
    // sized to match what NtQueryInformationProcess(0, ...) writes.
    let mut pbi: ProcessBasicInformation = unsafe { std::mem::zeroed() };
    let mut ret_len: u32 = 0;
    let size = std::mem::size_of::<ProcessBasicInformation>() as u32;
    // SAFETY: handle is valid; pbi/ret_len are valid out-params of the
    // declared sizes; info_class 0 == ProcessBasicInformation.
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            0,
            &mut pbi as *mut _ as *mut core::ffi::c_void,
            size,
            &mut ret_len,
        )
    };
    if status == 0 {
        Some(pbi)
    } else {
        None
    }
}

/// The parent process id of `pid` (`InheritedFromUniqueProcessId`), or
/// `None` if the process is gone / inaccessible. Note: Windows reuses pids,
/// so a returned parent may have exited and been replaced — callers that
/// walk the chain should bound their iterations.
pub fn parent_pid(pid: u32) -> Option<u32> {
    let handle = ProcHandle::open(pid)?;
    let pbi = basic_information(handle.0)?;
    Some(pbi.inherited_from_unique_process_id as u32)
}

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

    /// Spawn a long-lived child we can probe, inheriting `envs` and `cwd`.
    /// `ping -n 30 127.0.0.1` sleeps ~29 s with no stdin needed; the test
    /// kills it as soon as the assertion is done.
    fn spawn_probe_child(
        envs: &[(&str, &str)],
        cwd: Option<&std::path::Path>,
    ) -> std::process::Child {
        let mut cmd = std::process::Command::new("cmd.exe");
        cmd.args(["/c", "ping", "-n", "30", "127.0.0.1"]);
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        for (k, v) in envs {
            cmd.env(k, v);
        }
        if let Some(d) = cwd {
            cmd.current_dir(d);
        }
        cmd.spawn().expect("spawn probe child")
    }

    #[test]
    fn parent_pid_of_child_is_current_process() {
        let mut child = spawn_probe_child(&[], None);
        let child_pid = child.id();
        let got = parent_pid(child_pid);
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(got, Some(std::process::id()));
    }
}

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

use std::os::windows::ffi::OsStrExt;
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
    fn GetExitCodeProcess(handle: isize, exit_code: *mut u32) -> i32;
    fn GetLastError() -> u32;
    fn ReadProcessMemory(
        handle: isize,
        base_address: usize,
        buffer: *mut core::ffi::c_void,
        size: usize,
        bytes_read: *mut usize,
    ) -> i32;
}

// PROCESS_QUERY_LIMITED_INFORMATION — the lightest right that still lets us
// read a process's exit code; works on same-user processes we can't open with
// the heavier PROCESS_ACCESS mask.
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const STILL_ACTIVE: u32 = 259;
const ERROR_ACCESS_DENIED: u32 = 5;

/// Whether `pid` refers to a still-running process. Returns `false` only when
/// the process has exited or never existed; a process that exists but can't be
/// opened (access denied, e.g. elevation) is reported **alive** so the caller
/// never reaps a live session by mistake. Used by master's Class-B liveness
/// poll to demote shell-pane sessions whose CLI was `Ctrl+C`'d — those CLIs
/// write no "session ended" record, so process death is the only signal.
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: query-only access right; OpenProcess returns 0 (NULL) on failure.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle == 0 {
        // SAFETY: GetLastError only reads the thread-local last-error code.
        let err = unsafe { GetLastError() };
        // ACCESS_DENIED means the process exists but is inaccessible → alive.
        // Any other error (typically ERROR_INVALID_PARAMETER) → no such pid.
        return err == ERROR_ACCESS_DENIED;
    }
    let mut code: u32 = 0;
    // SAFETY: `handle` is a valid process handle; `code` is a valid out-param.
    let ok = unsafe { GetExitCodeProcess(handle, &mut code) };
    // SAFETY: `handle` came from OpenProcess and is closed exactly once here.
    unsafe { CloseHandle(handle) };
    ok != 0 && code == STILL_ACTIVE
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

// x64 PEB offsets, validated 2026-06-09 against live agent CLIs.
//   PEB + 0x20                      -> ProcessParameters pointer
//   RTL_USER_PROCESS_PARAMETERS + 0x80   -> Environment pointer
//   RTL_USER_PROCESS_PARAMETERS + 0x3F0  -> EnvironmentSize (bytes)
const PEB_OFFSET_PROCESS_PARAMETERS: usize = 0x20;
const RUPP_OFFSET_ENVIRONMENT: usize = 0x80;
const RUPP_OFFSET_ENVIRONMENT_SIZE: usize = 0x3F0;
// Cap an implausible EnvironmentSize so a corrupt read can't allocate wildly.
const MAX_ENV_BYTES: usize = 1 << 20;

/// Read a pointer-sized value (`usize`) from another process's address space.
fn read_remote_ptr(handle: isize, address: usize) -> Option<usize> {
    let mut buf = [0u8; std::mem::size_of::<usize>()];
    let mut read: usize = 0;
    // SAFETY: handle is valid; buf is sized exactly size_of::<usize>().
    let ok = unsafe {
        ReadProcessMemory(
            handle,
            address,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            buf.len(),
            &mut read,
        )
    };
    if ok != 0 && read == buf.len() {
        Some(usize::from_ne_bytes(buf))
    } else {
        None
    }
}

/// Read `len` bytes from another process's address space.
fn read_remote_bytes(handle: isize, address: usize, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; len];
    let mut read: usize = 0;
    // SAFETY: handle is valid; buf has capacity `len`.
    let ok = unsafe {
        ReadProcessMemory(
            handle,
            address,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            len,
            &mut read,
        )
    };
    if ok != 0 {
        buf.truncate(read);
        Some(buf)
    } else {
        None
    }
}

// RTL_USER_PROCESS_PARAMETERS + 0x38 -> CurrentDirectory.DosPath (UNICODE_STRING)
//   +0x38: Length (u16, bytes)   +0x40: Buffer (ptr to UTF-16)
const RUPP_OFFSET_CURDIR_LENGTH: usize = 0x38;
const RUPP_OFFSET_CURDIR_BUFFER: usize = 0x40;

/// Read a process's current working directory from its PEB. `None` if the
/// process is inaccessible or the path is empty.
// Consumed by Plan C (master wiring); unused within Plan B.
#[allow(dead_code)]
pub fn cwd_for_pid(pid: u32) -> Option<std::path::PathBuf> {
    // The PEB / RTL_USER_PROCESS_PARAMETERS offsets below are x64-specific. On
    // another Windows arch (e.g. ARM64) they would point into the wrong remote
    // memory, so bail rather than return a garbage path.
    if !cfg!(target_arch = "x86_64") {
        return None;
    }
    let handle = ProcHandle::open(pid)?;
    let pbi = basic_information(handle.0)?;
    let pp = read_remote_ptr(
        handle.0,
        pbi.peb_base_address + PEB_OFFSET_PROCESS_PARAMETERS,
    )?;

    // Length is the low u16 of the pointer-sized read at the UNICODE_STRING base.
    let len_word = read_remote_ptr(handle.0, pp + RUPP_OFFSET_CURDIR_LENGTH)?;
    let len_bytes = len_word & 0xFFFF;
    if len_bytes == 0 || len_bytes > 0x8000 {
        return None;
    }
    let buf_ptr = read_remote_ptr(handle.0, pp + RUPP_OFFSET_CURDIR_BUFFER)?;
    let raw = read_remote_bytes(handle.0, buf_ptr, len_bytes)?;
    let utf16: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let s = String::from_utf16_lossy(&utf16);
    let trimmed = s.trim_end_matches(['\\', '\0']);
    if trimmed.is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(trimmed))
    }
}

/// Read and decode the full environment block of `pid` as a single string
/// with embedded NUL separators (one `NAME=VALUE` entry per NUL-delimited
/// segment). Returns `None` if the process can't be opened or the PEB walk
/// fails.
fn read_process_env_block(pid: u32) -> Option<String> {
    // x64-specific PEB / RTL_USER_PROCESS_PARAMETERS offsets (see `cwd_for_pid`);
    // return None on other arches rather than read the wrong remote memory. This
    // gates `env_var_for_pid` and `wt_session_for_pid`, which both route here.
    if !cfg!(target_arch = "x86_64") {
        return None;
    }
    let handle = ProcHandle::open(pid)?;
    let pbi = basic_information(handle.0)?;
    let pp = read_remote_ptr(
        handle.0,
        pbi.peb_base_address + PEB_OFFSET_PROCESS_PARAMETERS,
    )?;
    let env_addr = read_remote_ptr(handle.0, pp + RUPP_OFFSET_ENVIRONMENT)?;
    let mut env_size = read_remote_ptr(handle.0, pp + RUPP_OFFSET_ENVIRONMENT_SIZE)?;
    if env_size == 0 || env_size > MAX_ENV_BYTES {
        env_size = 1 << 16;
    }
    let raw = read_remote_bytes(handle.0, env_addr, env_size)?;
    // The block is UTF-16LE; build a u16 vec from byte pairs, then decode.
    let utf16: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Some(String::from_utf16_lossy(&utf16))
}

/// Find the value of env var `name` (ASCII-case-insensitive on the name) in a
/// decoded environment block — a string of `NAME=VALUE` entries separated by
/// NUL. Pure + panic-free: matches the `name=` prefix by bytes so a non-ASCII
/// character straddling the boundary can never trigger a char-boundary slice
/// panic. Extracted from `env_var_for_pid` so it can be unit-tested directly.
fn find_env_value(block: &str, name: &str) -> Option<String> {
    let prefix_len = name.len() + 1; // "name="
    for entry in block.split('\0') {
        let bytes = entry.as_bytes();
        if bytes.len() < prefix_len {
            continue;
        }
        // Compare the "name=" prefix by bytes (ASCII-case-insensitive on the
        // name; the trailing '=' matches itself). Never slices a &str.
        let (head, sep) = bytes.split_at(name.len());
        if sep.first() == Some(&b'=') && head.eq_ignore_ascii_case(name.as_bytes()) {
            // The value starts right after the ASCII '=', a valid boundary.
            return Some(entry[prefix_len..].to_string());
        }
    }
    None
}

/// Read environment variable `name` (case-insensitive) from `pid`'s PEB.
/// Returns `None` if the process is inaccessible or the variable is unset.
pub fn env_var_for_pid(pid: u32, name: &str) -> Option<String> {
    let block = read_process_env_block(pid)?;
    find_env_value(&block, name)
}

/// Convenience wrapper: read `WT_SESSION` (the hosting pane's GUID) from a
/// process's PEB. Every CLI process WT launches inherits this, so it is the
/// cheapest path from a bound pid to its pane.
pub fn wt_session_for_pid(pid: u32) -> Option<String> {
    env_var_for_pid(pid, "WT_SESSION")
}

// ── Restart Manager FFI (rstrtmgr.dll) ───────────────────────────────────

const ERROR_MORE_DATA: u32 = 234;
const CCH_RM_MAX_APP_NAME: usize = 255;
const CCH_RM_MAX_SVC_NAME: usize = 63;

#[repr(C)]
#[derive(Clone, Copy)]
struct Filetime {
    low: u32,
    high: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RmUniqueProcess {
    process_id: u32,
    process_start_time: Filetime,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RmProcessInfo {
    process: RmUniqueProcess,
    app_name: [u16; CCH_RM_MAX_APP_NAME + 1],
    service_short_name: [u16; CCH_RM_MAX_SVC_NAME + 1],
    application_type: i32,
    app_status: u32,
    ts_session_id: u32,
    restartable: i32,
}

#[link(name = "rstrtmgr")]
extern "system" {
    fn RmStartSession(session_handle: *mut u32, flags: u32, session_key: *mut u16) -> u32;
    fn RmRegisterResources(
        session_handle: u32,
        num_files: u32,
        files: *const *const u16,
        num_apps: u32,
        apps: *const core::ffi::c_void,
        num_services: u32,
        service_names: *const *const u16,
    ) -> u32;
    fn RmGetList(
        session_handle: u32,
        proc_info_needed: *mut u32,
        proc_info: *mut u32,
        affected_apps: *mut RmProcessInfo,
        reboot_reasons: *mut u32,
    ) -> u32;
    fn RmEndSession(session_handle: u32) -> u32;
}

/// Restart Manager: every process id currently holding `path` open. Empty
/// vec if nothing holds it (the common case for append-then-close writers
/// like Copilot/Claude/Gemini) or on any RM error. Used for Codex, which
/// keeps its rollout file open for the whole session.
pub fn file_holders(path: &Path) -> Vec<u32> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut session: u32 = 0;
    // Session key buffer must be >= CCH_RM_SESSION_KEY+1 (33) wide chars.
    let mut key = [0u16; 64];
    // SAFETY: session out-param and key buffer are valid for the declared sizes.
    let rc = unsafe { RmStartSession(&mut session, 0, key.as_mut_ptr()) };
    if rc != 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    let files = [wide.as_ptr()];
    // SAFETY: one file resource; no apps/services (null + 0 counts).
    let reg = unsafe {
        RmRegisterResources(
            session,
            1,
            files.as_ptr(),
            0,
            std::ptr::null(),
            0,
            std::ptr::null(),
        )
    };
    if reg == 0 {
        let mut needed: u32 = 0;
        let mut count: u32 = 0;
        let mut reason: u32 = 0;
        // First call probes the required array length.
        // SAFETY: null array with count 0 is the documented probe form.
        let probe = unsafe {
            RmGetList(
                session,
                &mut needed,
                &mut count,
                std::ptr::null_mut(),
                &mut reason,
            )
        };
        if (probe == 0 || probe == ERROR_MORE_DATA) && needed > 0 {
            // SAFETY: zeroed RmProcessInfo is valid POD; vec has `needed` slots.
            let mut arr: Vec<RmProcessInfo> = vec![unsafe { std::mem::zeroed() }; needed as usize];
            count = needed;
            // SAFETY: arr has capacity `count`; out-params valid.
            let got = unsafe {
                RmGetList(
                    session,
                    &mut needed,
                    &mut count,
                    arr.as_mut_ptr(),
                    &mut reason,
                )
            };
            if got == 0 {
                for info in arr.iter().take(count as usize) {
                    out.push(info.process.process_id);
                }
            }
        }
    }

    // SAFETY: session is a handle returned by RmStartSession.
    unsafe {
        RmEndSession(session);
    }
    out
}

/// First process id holding `path` open, or `None`. Thin wrapper over
/// [`file_holders`] for the common single-holder case (Codex).
#[allow(dead_code)] // consumed by Plan B's session binder, not yet wired here
pub fn file_owner_pid(path: &Path) -> Option<u32> {
    file_holders(path).into_iter().next()
}

// ── Toolhelp32 process enumeration (kernel32) ────────────────────────────

const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const MAX_PATH: usize = 260;
const INVALID_HANDLE_VALUE: isize = -1;

#[repr(C)]
struct ProcessEntry32W {
    dw_size: u32,
    cnt_usage: u32,
    th32_process_id: u32,
    th32_default_heap_id: usize,
    th32_module_id: u32,
    cnt_threads: u32,
    th32_parent_process_id: u32,
    pc_pri_class_base: i32,
    dw_flags: u32,
    sz_exe_file: [u16; MAX_PATH],
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> isize;
    fn Process32FirstW(snapshot: isize, entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: isize, entry: *mut ProcessEntry32W) -> i32;
}

/// Every running process whose executable file name equals `exe_name`
/// (case-insensitive, e.g. `"copilot.exe"` / `"node.exe"`). Empty on error.
pub fn pids_for_exe(exe_name: &str) -> Vec<u32> {
    let mut out = Vec::new();
    // SAFETY: snapshot flag + pid 0 (all processes); returns INVALID_HANDLE_VALUE on failure.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return out;
    }
    // SAFETY: zeroed POD; dw_size must be set before Process32FirstW.
    let mut entry: ProcessEntry32W = unsafe { std::mem::zeroed() };
    entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
    let want = exe_name.to_ascii_lowercase();

    // SAFETY: snapshot + entry valid; iterate until Process32NextW returns 0.
    let mut ok = unsafe { Process32FirstW(snapshot, &mut entry) };
    while ok != 0 {
        let end = entry.sz_exe_file.iter().position(|&c| c == 0).unwrap_or(MAX_PATH);
        let name = String::from_utf16_lossy(&entry.sz_exe_file[..end]);
        if name.to_ascii_lowercase() == want {
            out.push(entry.th32_process_id);
        }
        ok = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    // SAFETY: snapshot is a handle from CreateToolhelp32Snapshot.
    unsafe { CloseHandle(snapshot) };
    out
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
        let dir =
            std::env::temp_dir().join(format!("wta-proc-bind-{}-{}", label, std::process::id()));
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
        std::fs::write(dir.join("inuse.bad.lock"), b"").unwrap();
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

    #[test]
    fn env_var_for_pid_reads_child_environment() {
        let mut child = spawn_probe_child(&[("WTA_TEST_BIND", "marker-value-42")], None);
        let pid = child.id();
        // Give the child a moment to finish initializing its PEB.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let got = env_var_for_pid(pid, "WTA_TEST_BIND");
        let got_ci = env_var_for_pid(pid, "wta_test_bind"); // case-insensitive name
        let missing = env_var_for_pid(pid, "WTA_NOT_SET_XYZ");
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(got.as_deref(), Some("marker-value-42"));
        assert_eq!(got_ci.as_deref(), Some("marker-value-42"));
        assert_eq!(missing, None);
    }

    #[test]
    fn file_holders_includes_current_process() {
        let dir = tmp_dir("rm-hold");
        let path = dir.join("held.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();
        // Keep a read handle open for the duration of the query.
        let _held = std::fs::File::open(&path).unwrap();
        let holders = file_holders(&path);
        assert!(
            holders.contains(&std::process::id()),
            "expected current pid {} among holders {:?}",
            std::process::id(),
            holders
        );
    }

    #[test]
    fn find_env_value_basic_and_case_insensitive() {
        let block = "FOO=1\0WT_SESSION=abc-123\0BAR=2\0";
        assert_eq!(
            find_env_value(block, "WT_SESSION").as_deref(),
            Some("abc-123")
        );
        assert_eq!(
            find_env_value(block, "wt_session").as_deref(),
            Some("abc-123")
        );
        assert_eq!(find_env_value(block, "MISSING"), None);
    }

    #[test]
    fn find_env_value_non_ascii_entry_does_not_panic() {
        // An entry whose name has a multi-byte char straddling byte index
        // name.len() must NOT panic (regression for the char-boundary bug).
        // "123456789€" is 9 ASCII bytes + a 3-byte '€' (bytes 9..12).
        let block = "123456789\u{20ac}=value\0WT_SESSION=guid-42\0";
        // The query length 10 (WT_SESSION) lands inside '€' of the first entry.
        assert_eq!(
            find_env_value(block, "WT_SESSION").as_deref(),
            Some("guid-42")
        );
    }

    #[test]
    fn find_env_value_empty_value() {
        let block = "EMPTY=\0X=1\0";
        assert_eq!(find_env_value(block, "EMPTY").as_deref(), Some(""));
    }

    #[test]
    fn cwd_for_pid_reads_child_working_dir() {
        let dir = tmp_dir("cwd-child");
        // Canonicalize so the comparison is robust to short/long path forms.
        let canonical = std::fs::canonicalize(&dir).unwrap();
        let mut child = spawn_probe_child(&[], Some(&canonical));
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let got = cwd_for_pid(pid);
        let _ = child.kill();
        let _ = child.wait();
        let got = got.expect("cwd_for_pid returned None");
        // Compare case-insensitively on the final component to avoid
        // \\?\ prefix / drive-letter-case differences.
        assert!(
            got.to_string_lossy()
                .to_lowercase()
                .contains(&dir.file_name().unwrap().to_string_lossy().to_lowercase()),
            "expected cwd containing {:?}, got {:?}",
            dir.file_name().unwrap(),
            got
        );
    }

    #[test]
    fn pids_for_exe_finds_spawned_cmd() {
        let mut child = spawn_probe_child(&[], None);
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let pids = pids_for_exe("cmd.exe");
        let _ = child.kill();
        let _ = child.wait();
        assert!(pids.contains(&pid), "expected {} in {:?}", pid, pids);
    }

    #[test]
    fn file_holders_empty_for_unheld_file() {
        let dir = tmp_dir("rm-free");
        let path = dir.join("free.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();
        // No handle held -> Restart Manager reports no holders.
        let holders = file_holders(&path);
        assert!(holders.is_empty(), "expected no holders, got {:?}", holders);
    }

    #[test]
    fn pid_alive_true_for_self_false_for_dead() {
        // Our own process is alive.
        assert!(pid_alive(std::process::id()));
        // Spawn a child, kill it, confirm it reports dead.
        let mut child = spawn_probe_child(&[], None);
        let pid = child.id();
        assert!(pid_alive(pid), "freshly spawned child should be alive");
        let _ = child.kill();
        let _ = child.wait();
        // Give the OS a moment to tear the process down.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!pid_alive(pid), "killed child should report dead");
    }
}

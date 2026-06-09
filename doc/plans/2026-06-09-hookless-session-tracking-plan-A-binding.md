# Hookless Session Tracking — Plan A: Binding Primitives (`proc_bind`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `tools/wta/src/proc_bind.rs` — the four Win32 primitives that link a session file/dir to its owning process, and that process to its hosting WT pane — with full unit tests and a `wta bind-probe` debug subcommand.

**Architecture:** A self-contained, leaf module with no dependencies on the rest of the crate. It exposes four pure-ish functions over `unsafe` Win32 FFI (declared inline, not via `windows-sys` features, so the module is version-independent and self-contained). Each primitive was validated against live CLIs on 2026-06-09 (see `doc/specs/hookless-agent-session-tracking.md` → Verification). Later plans (B: watcher+classifiers, C: master wiring + hook removal) consume this module.

**Tech Stack:** Rust (edition 2021, toolchain `ms-prod-1.93`), inline `extern "system"` FFI to `kernel32`/`ntdll`/`rstrtmgr`, `std::os::windows`. No new Cargo dependencies (the only new crate in the feature, `notify`, lands in Plan B).

---

## Scope

**In scope (Plan A):**
- `copilot_pid_from_lock(session_dir)` — parse Copilot's `inuse.<pid>.lock` marker (pure fs).
- `parent_pid(pid)` — `InheritedFromUniqueProcessId` via `NtQueryInformationProcess`.
- `env_var_for_pid(pid, name)` + `wt_session_for_pid(pid)` — read a process's PEB environment block.
- `file_holders(path)` / `file_owner_pid(path)` — Restart Manager "who holds this file open" (Codex).
- `wta bind-probe` debug subcommand exercising all four.

**Out of scope (later plans):**
- The filesystem watcher, per-CLI record classifiers, cwd-correlation logic (Plan B).
- Wiring into `master/mod.rs`, the registry feed, window scoping, hook removal (Plan C).
- `cwd_for_pid` (PEB `CurrentDirectory` offset was not validated in the probes; the cwd-correlation path in Plan B will use `wt_session_for_pid` + per-pane cwd from WT instead, or validate that offset there).

## File Structure

| File | Responsibility | Action |
|---|---|---|
| `tools/wta/src/proc_bind.rs` | The four binding primitives + inline Win32 FFI + unit tests | **Create** |
| `tools/wta/src/main.rs` | Register `mod proc_bind;`; add `Command::BindProbe`; dispatch it | **Modify** (`mod` list ~line 4-31; `enum Command` ~line 270; dispatch `match cli.command` ~line 624) |

## Conventions (from `.github/instructions/rust*.instructions.md`)

- No `unwrap()`/`expect()` in non-test code — return `Option`/`Result`.
- Every `unsafe` block carries a `// SAFETY:` comment.
- `cargo fmt` + `cargo clippy` clean before the final commit.
- Tests live in a `#[cfg(test)] mod tests` block in the same file.
- The whole crate is Windows-only, so tests may freely use Win32 / `cmd.exe`.

## Test commands (run from repo root)

```bash
# Unit tests for this module (no need to kill wta.exe — tests build a separate binary):
cargo test --manifest-path tools/wta/Cargo.toml proc_bind:: -- --nocapture

# Manual probe (builds wta.exe — kill any running instance first, per AGENTS.md):
powershell -NoProfile -Command "Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force"
cargo run --manifest-path tools/wta/Cargo.toml -- bind-probe --pid <PID>
```

---

## Task 1: Module skeleton + `copilot_pid_from_lock`

Start with the pure-filesystem primitive (no FFI) to establish the module and its test harness.

**Files:**
- Create: `tools/wta/src/proc_bind.rs`
- Modify: `tools/wta/src/main.rs` (add `mod proc_bind;` to the module list)

- [ ] **Step 1: Register the module**

In `tools/wta/src/main.rs`, the `mod` declarations are an alphabetical-ish list (lines ~4-31). Add `proc_bind` after `pane_context` (line ~19) to keep order:

```rust
mod pane_context;
mod proc_bind;
mod protocol;
```

- [ ] **Step 2: Create the module with the file header and the first failing test**

Create `tools/wta/src/proc_bind.rs`:

```rust
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
```

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::copilot -- --nocapture`
Expected: 3 tests PASS (`copilot_pid_from_lock_reads_marker`, `_none_without_marker`, `_ignores_malformed`).

- [ ] **Step 4: Commit**

```bash
git add tools/wta/src/proc_bind.rs tools/wta/src/main.rs
git commit -m "feat(wta/proc_bind): module skeleton + copilot_pid_from_lock"
```

---

## Task 2: `parent_pid` via `NtQueryInformationProcess`

**Files:**
- Modify: `tools/wta/src/proc_bind.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `proc_bind.rs`. The test spawns a child and asserts its parent is the current process:

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::parent_pid_of_child -- --nocapture`
Expected: FAIL to compile with "cannot find function `parent_pid` in this scope".

- [ ] **Step 3: Add the FFI block and implement `parent_pid`**

Add near the top of `proc_bind.rs` (below the `use` line), the shared FFI surface plus the `parent_pid` function:

```rust
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
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::parent_pid_of_child -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/proc_bind.rs
git commit -m "feat(wta/proc_bind): parent_pid via NtQueryInformationProcess"
```

---

## Task 3: `env_var_for_pid` + `wt_session_for_pid` (PEB environment read)

**Files:**
- Modify: `tools/wta/src/proc_bind.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module. The child inherits a known env var; we read it back out of its PEB:

```rust
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
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::env_var_for_pid -- --nocapture`
Expected: FAIL to compile with "cannot find function `env_var_for_pid`".

- [ ] **Step 3: Implement the PEB environment reader**

Add to `proc_bind.rs` (below `parent_pid`). The x64 PEB offsets were validated on 2026-06-09:

```rust
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

/// Read and decode the full environment block of `pid` as a single string
/// with embedded NUL separators (one `NAME=VALUE` entry per NUL-delimited
/// segment). Returns `None` if the process can't be opened or the PEB walk
/// fails.
fn read_process_env_block(pid: u32) -> Option<String> {
    let handle = ProcHandle::open(pid)?;
    let pbi = basic_information(handle.0)?;
    let pp = read_remote_ptr(handle.0, pbi.peb_base_address + PEB_OFFSET_PROCESS_PARAMETERS)?;
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

/// Read environment variable `name` (case-insensitive) from `pid`'s PEB.
/// Returns `None` if the process is inaccessible or the variable is unset.
pub fn env_var_for_pid(pid: u32, name: &str) -> Option<String> {
    let block = read_process_env_block(pid)?;
    let prefix = format!("{}=", name).to_ascii_lowercase();
    for entry in block.split('\0') {
        if entry.len() <= name.len() {
            continue;
        }
        if entry[..name.len() + 1].to_ascii_lowercase() == prefix {
            return Some(entry[name.len() + 1..].to_string());
        }
    }
    None
}

/// Convenience wrapper: read `WT_SESSION` (the hosting pane's GUID) from a
/// process's PEB. Every CLI process WT launches inherits this, so it is the
/// cheapest path from a bound pid to its pane.
pub fn wt_session_for_pid(pid: u32) -> Option<String> {
    env_var_for_pid(pid, "WT_SESSION")
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::env_var_for_pid -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/proc_bind.rs
git commit -m "feat(wta/proc_bind): env_var_for_pid + wt_session_for_pid via PEB read"
```

---

## Task 4: `file_holders` / `file_owner_pid` via Restart Manager

**Files:**
- Modify: `tools/wta/src/proc_bind.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module. The test process itself holds a temp file open and asserts Restart Manager reports the current pid among the holders:

```rust
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
    fn file_holders_empty_for_unheld_file() {
        let dir = tmp_dir("rm-free");
        let path = dir.join("free.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();
        // No handle held -> Restart Manager reports no holders.
        let holders = file_holders(&path);
        assert!(holders.is_empty(), "expected no holders, got {:?}", holders);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::file_holders -- --nocapture`
Expected: FAIL to compile with "cannot find function `file_holders`".

- [ ] **Step 3: Implement the Restart Manager FFI + functions**

Add to `proc_bind.rs`. Append the `rstrtmgr` FFI block and the two functions:

```rust
use std::os::windows::ffi::OsStrExt;

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
            RmGetList(session, &mut needed, &mut count, std::ptr::null_mut(), &mut reason)
        };
        if (probe == 0 || probe == ERROR_MORE_DATA) && needed > 0 {
            // SAFETY: zeroed RmProcessInfo is valid POD; vec has `needed` slots.
            let mut arr: Vec<RmProcessInfo> =
                vec![unsafe { std::mem::zeroed() }; needed as usize];
            count = needed;
            // SAFETY: arr has capacity `count`; out-params valid.
            let got = unsafe {
                RmGetList(session, &mut needed, &mut count, arr.as_mut_ptr(), &mut reason)
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
pub fn file_owner_pid(path: &Path) -> Option<u32> {
    file_holders(path).into_iter().next()
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::file_holders -- --nocapture`
Expected: both PASS. (If `file_holders_includes_current_process` fails because RM excludes the calling process on this build, switch the test to spawn a holder child: `powershell -NoProfile -Command "$f=[IO.File]::Open('<path>','Open','Read','None'); Start-Sleep 30"` and assert the returned vec contains that child's pid. The production code is unaffected — Codex is always a separate process.)

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/proc_bind.rs
git commit -m "feat(wta/proc_bind): file_holders/file_owner_pid via Restart Manager"
```

---

## Task 5: `wta bind-probe` debug subcommand

Wire the primitives into a CLI subcommand so the whole module can be exercised end-to-end against live processes.

**Files:**
- Modify: `tools/wta/src/main.rs` (`enum Command` ~line 270; dispatch `match cli.command` ~line 624)

- [ ] **Step 1: Add the `BindProbe` command variant**

In `tools/wta/src/main.rs`, inside `enum Command { ... }` (starts ~line 270), add a new variant (place it near the other diagnostic commands; exact position doesn't matter):

```rust
    /// Diagnostics: exercise the proc_bind binding primitives.
    BindProbe {
        /// PID to read parent pid + WT_SESSION env from.
        #[arg(long)]
        pid: Option<u32>,
        /// File path to query Restart Manager holders of.
        #[arg(long)]
        file: Option<String>,
        /// Copilot session-state dir to read `inuse.<pid>.lock` from.
        #[arg(long)]
        lock_dir: Option<String>,
    },
```

- [ ] **Step 2: Add the dispatch arm**

In the `let result = match cli.command { ... }` block (starts ~line 624), add an arm. It is synchronous and infallible, so it returns `Ok(())`:

```rust
        Some(Command::BindProbe { pid, file, lock_dir }) => {
            run_bind_probe(pid, file, lock_dir);
            Ok(())
        }
```

- [ ] **Step 3: Add the handler function**

Add this free function in `main.rs` (anywhere among the other `run_*` helpers, e.g. near `run_info_mode`):

```rust
/// Print the result of each `proc_bind` primitive for the given inputs.
/// Pure diagnostics — used to validate binding against live agent CLIs.
fn run_bind_probe(pid: Option<u32>, file: Option<String>, lock_dir: Option<String>) {
    if let Some(pid) = pid {
        println!("parent_pid({pid})        = {:?}", proc_bind::parent_pid(pid));
        println!("wt_session_for_pid({pid}) = {:?}", proc_bind::wt_session_for_pid(pid));
    }
    if let Some(file) = file {
        let p = std::path::Path::new(&file);
        println!("file_holders({file})     = {:?}", proc_bind::file_holders(p));
    }
    if let Some(dir) = lock_dir {
        let p = std::path::Path::new(&dir);
        println!("copilot_pid_from_lock({dir}) = {:?}", proc_bind::copilot_pid_from_lock(p));
    }
}
```

- [ ] **Step 4: Build and verify the command compiles + runs**

```bash
powershell -NoProfile -Command "Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force"
cargo build --manifest-path tools/wta/Cargo.toml
```
Expected: builds clean. Then run against the current shell's own pid (PowerShell):
```bash
cargo run --manifest-path tools/wta/Cargo.toml -- bind-probe --pid $PID
```
Expected: prints a `parent_pid(...) = Some(...)` line and, if launched inside a WT pane, `wt_session_for_pid(...) = Some("<guid>")`. (Outside WT, `WT_SESSION` is `None` — that's fine.)

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/main.rs
git commit -m "feat(wta): add bind-probe diagnostic subcommand for proc_bind"
```

---

## Task 6: Lint, format, and finalize

**Files:**
- Modify: `tools/wta/src/proc_bind.rs`, `tools/wta/src/main.rs` (only if fmt/clippy require)

- [ ] **Step 1: Format**

Run: `cargo fmt --manifest-path tools/wta/Cargo.toml`
Expected: no diff, or only whitespace fixes.

- [ ] **Step 2: Clippy (treat warnings as errors for the new module)**

Run: `cargo clippy --manifest-path tools/wta/Cargo.toml -- -D warnings`
Expected: no warnings. Common fixes if any appear:
- `clippy::missing_safety_doc` — ensure each `unsafe fn`/block has a `// SAFETY:` note (already present).
- unused-import on `OsStrExt` — keep it (used by `encode_wide`).

- [ ] **Step 3: Run the full module test suite once more**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind:: -- --nocapture`
Expected: all Task 1-4 tests PASS (8 tests total).

- [ ] **Step 4: Commit any fmt/clippy fixes**

```bash
git add -A tools/wta/src/proc_bind.rs tools/wta/src/main.rs
git commit -m "chore(wta/proc_bind): fmt + clippy clean"
```

---

## Self-Review Checklist (completed during planning)

- **Spec coverage:** Plan A implements the binding primitives named in the spec's Pillar 2 / Verification for Copilot (lock), Codex (Restart Manager), and the pane GUID lookup (`WT_SESSION` from PEB) plus parent-pid walking. The cwd-correlation step for Claude/Gemini and all watcher/registry wiring are explicitly deferred to Plans B/C.
- **Placeholder scan:** No TBD/TODO; every code step contains complete code.
- **Type consistency:** `parent_pid`/`env_var_for_pid`/`wt_session_for_pid`/`file_holders`/`file_owner_pid`/`copilot_pid_from_lock` signatures are referenced identically in the tests and the `run_bind_probe` handler. `ProcHandle`, `ProcessBasicInformation`, `RmProcessInfo` are each defined once.
- **No new deps:** Confirmed — only inline FFI; `notify` is deferred to Plan B, so `cgmanifest.json` / `NOTICE.md` regeneration is **not** required for Plan A.

## What Plan B will build (preview, not part of this plan)

- Add `notify` to `Cargo.toml` (→ regenerate `cgmanifest.json` + `NOTICE.md`).
- `session_watcher/mod.rs` + `classify_{copilot,claude,codex,gemini}.rs` mapping appended records to `SessionEvent` (`SessionStarted` / `ToolStarting` / `ToolCompleted` / `Notification` / `SessionStopped`), reusing `history_loader` path/parse helpers.
- The Claude/Gemini cwd-correlation that pairs a discovered session file to a candidate process, then uses `proc_bind::wt_session_for_pid` (from Plan A) to reach the pane.

//! PID-based fallback pane scanner.
//!
//! ## Why this exists
//!
//! WTA already learns about agent-CLI panes through three mechanisms:
//!
//! 1. **Agent panes (Class A)** — the wta-helper TUI owns the pane and
//!    speaks ACP directly. The agent's SessionId is bound to the pane
//!    GUID at `new_session` / `load_session` time.
//! 2. **Resumed-into-shell panes** — `wtcli resume <sid> --pane`
//!    returns the new pane GUID synchronously.
//! 3. **PowerShell shell-integration hooks (Class B)** — when the user
//!    runs `copilot` / `claude` / `gemini` directly in a hooked pwsh
//!    pane, our hook emits an `intellterm.session_started` ext-event
//!    carrying both the pane GUID (`WT_SESSION`) and the agent's real
//!    session id (scraped from the CLI's stdout). Master replays it
//!    into the helper as a `SessionStarted` reducer event.
//!
//! If the user disables / never installs the PowerShell hooks
//! (default for unpackaged shells, or any non-pwsh shell), Class B
//! disappears entirely. The session then never shows in the session
//! management view even
//! though the CLI is alive and the user obviously *can* see its
//! output — they're staring at it in the pane.
//!
//! ## What this module does (Phase A)
//!
//! Periodically (~3 s tick) the helper:
//!
//! 1. Calls `wt_list_panes(my_tab_id)` to enumerate the shell PIDs of
//!    every pane in the helper's owner tab.
//! 2. For each pane that is **not** already bound to a session in the
//!    helper's `AgentSessionRegistry`, walks the shell's child
//!    processes BFS (≤3 levels) looking for an exe basename matching
//!    a tracked CLI (`copilot.exe`, `copilot-cli.exe`, `claude.exe`,
//!    `gemini.exe`).
//! 3. Diffs the result against the last scan and emits
//!    [`SessionEvent::PidScannerDetected`] /
//!    [`SessionEvent::PidScannerLost`] for each change.
//!
//! The reducer in `agent_sessions::AgentSessionRegistry::apply` then
//! creates / removes a synthetic row (key `pane:<lowercase guid>`,
//! `origin = Unknown`, `synthetic = true`). Synthetic rows are removed
//! entirely — never demoted to `Ended` — when the binding goes away or
//! when an authoritative hook event lands on the same pane.
//!
//! ## Known limitations (Phase A)
//!
//! * **No `node.exe` matching.** CLIs installed via `npm i -g
//!   @anthropic-ai/claude-code` and friends run under `node.exe`, which
//!   we deliberately don't match because that name is shared with
//!   every other Node.js tool. Detecting them requires inspecting the
//!   process command line for the script path, which is deferred to
//!   Phase A.1.
//! * **Owner-tab scoped.** Each helper scans only its own tab. The
//!   session management view in tab N
//!   tab N sees PID-detected rows from tab N; cross-tab visibility
//!   would require master-side scanning, which is out of scope.
//! * **No resumability.** Synthetic rows can be focused (`active_by_pane`
//!   maps the pane GUID to the synthetic key) but the row carries no
//!   real ACP session id, so Resume is not offered.
//! * **Class A wins.** Agent-pane GUIDs are always in
//!   `active_by_pane` and the reducer no-ops the Detected event on
//!   them, so the scanner can never duplicate WTA's own panes.

use std::collections::HashMap;

use crate::agent_sessions::{CliSource, SessionEvent};
use crate::shell::ShellManager;

/// One pane → CLI binding observed in a single scan tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneCliBinding {
    /// WT pane GUID (always lowercased so it matches `active_by_pane`).
    pub pane_guid:  String,
    /// Which tracked CLI was found under the shell's process tree.
    pub cli_source: CliSource,
    /// PID of the detected CLI process (so the reducer can distinguish
    /// "still the same `copilot` invocation" from "user `/exit`'d and
    /// re-ran it in the same pane" — same `(pane, cli)`, different PID).
    pub cli_pid:    u32,
}

/// Pure diff: turns the previous confirmed snapshot + the current
/// scan into the events that should be applied to the registry.
///
/// The returned `HashMap` is the new confirmed snapshot. Callers
/// should store it and pass it as `last` on the next tick.
///
/// Diff rules:
///
/// | last has pane | current has pane | (cli, pid) match | emit |
/// |---------------|------------------|------------------|------|
/// | no            | yes              | n/a              | Detected |
/// | yes           | no               | n/a              | Lost     |
/// | yes           | yes              | yes              | none     |
/// | yes           | yes              | no               | Lost + Detected |
///
/// The Lost-before-Detected ordering matters: the reducer's
/// `PidScannerDetected` arm short-circuits if the pane is already
/// bound (synthetic with same PID/CLI = no-op, different = update
/// in place), but emitting Lost first matches what the reducer
/// *would* do for a clean rebind and keeps `active_by_pane` invariants
/// crisp if the reducer ever changes shape.
pub fn diff_snapshots(
    last: &HashMap<String, (CliSource, u32)>,
    current: &[PaneCliBinding],
) -> (Vec<SessionEvent>, HashMap<String, (CliSource, u32)>) {
    let mut events = Vec::new();
    let mut new_state: HashMap<String, (CliSource, u32)> =
        HashMap::with_capacity(current.len());

    // Build a lookup for current observations for the symmetric pass.
    let mut current_map: HashMap<&str, &PaneCliBinding> =
        HashMap::with_capacity(current.len());
    for b in current {
        current_map.insert(b.pane_guid.as_str(), b);
    }

    // Pass 1: panes that vanished or rebinding.
    for (pane, (prev_cli, prev_pid)) in last {
        match current_map.get(pane.as_str()) {
            None => {
                events.push(SessionEvent::PidScannerLost { pane_guid: pane.clone() });
            }
            Some(b) if &b.cli_source == prev_cli && b.cli_pid == *prev_pid => {
                // Unchanged — carry forward; no event.
                new_state.insert(pane.clone(), (prev_cli.clone(), *prev_pid));
            }
            Some(b) => {
                // Same pane, different PID or different CLI.
                events.push(SessionEvent::PidScannerLost { pane_guid: pane.clone() });
                events.push(SessionEvent::PidScannerDetected {
                    pane_guid:  b.pane_guid.clone(),
                    cli_source: b.cli_source.clone(),
                    cli_pid:    b.cli_pid,
                });
                new_state.insert(b.pane_guid.clone(), (b.cli_source.clone(), b.cli_pid));
            }
        }
    }

    // Pass 2: new panes (in current but not in last).
    for b in current {
        if !last.contains_key(&b.pane_guid) {
            events.push(SessionEvent::PidScannerDetected {
                pane_guid:  b.pane_guid.clone(),
                cli_source: b.cli_source.clone(),
                cli_pid:    b.cli_pid,
            });
            new_state.insert(b.pane_guid.clone(), (b.cli_source.clone(), b.cli_pid));
        }
    }

    (events, new_state)
}

/// Match an exe basename (case-insensitive) against the Phase A
/// allow-list of tracked CLIs.
///
/// Returns the canonical [`CliSource`] when matched. Note: the
/// `node.exe` / `npm.cmd` / `npx.cmd` variants intentionally don't
/// match — see the module-level "Known limitations" section.
pub fn classify_exe(exe_name: &str) -> Option<CliSource> {
    let lower = exe_name.to_ascii_lowercase();
    match lower.as_str() {
        "copilot.exe" | "copilot-cli.exe" => Some(CliSource::Copilot),
        "claude.exe"                      => Some(CliSource::Claude),
        "gemini.exe"                      => Some(CliSource::Gemini),
        _                                 => None,
    }
}

/// Scan the panes in `tab_id` for tracked CLIs running under their
/// shell-pid process tree. `skip_pane_if` returns `true` for panes
/// the caller has already bound to a session (real or synthetic);
/// the scanner skips them to avoid duplicate work and to leave
/// authoritative rows untouched.
///
/// Returns one [`PaneCliBinding`] per pane where a tracked CLI was
/// found. Panes whose shell pid is unknown or whose tree contains
/// no tracked CLI are simply absent from the result.
///
/// Async because `wt_list_panes` is an IPC call to wtcli's COM
/// channel. The Win32 child-enum is synchronous and blocking; the
/// caller wraps each per-pane scan in [`tokio::task::spawn_blocking`]
/// so a slow / hung snapshot can't stall the event loop.
pub async fn scan_tab(
    shell_mgr: &ShellManager,
    tab_id: &str,
    skip_pane_if: impl Fn(&str) -> bool,
) -> anyhow::Result<Vec<PaneCliBinding>> {
    let panes_resp = shell_mgr.wt_list_panes(tab_id).await?;
    let panes_arr = panes_resp
        .get("panes")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);

    let mut bindings = Vec::new();
    for pane in panes_arr {
        let pane_guid_raw = match pane.get("session_id") {
            Some(serde_json::Value::String(s)) => s.clone(),
            // `list_panes` returns lowercase strings already, but
            // numeric ids would never round-trip as GUIDs; skip them.
            _ => continue,
        };
        let pane_guid = pane_guid_raw.to_ascii_lowercase();
        if skip_pane_if(&pane_guid) {
            continue;
        }

        let shell_pid = match pane.get("pid").and_then(|v| v.as_u64()) {
            Some(p) if p > 0 && p <= u32::MAX as u64 => p as u32,
            _ => continue, // pane has no live shell — nothing to walk.
        };

        // Win32 process enumeration is purely synchronous. Push it
        // onto the blocking pool so the runtime can keep servicing
        // ACP / UI events while the snapshot completes.
        let maybe_match = tokio::task::spawn_blocking(move || {
            child_enum::matching_cli_under(shell_pid)
        })
        .await
        .unwrap_or_else(|join_err| {
            tracing::warn!(
                target: "pid_pane_scanner",
                error = %join_err,
                shell_pid,
                "matching_cli_under spawn_blocking failed (likely runtime shutdown)",
            );
            None
        });

        if let Some((cli_source, cli_pid)) = maybe_match {
            bindings.push(PaneCliBinding { pane_guid, cli_source, cli_pid });
        }
    }
    Ok(bindings)
}

#[cfg(windows)]
mod child_enum {
    //! Win32 child-process enumeration. BFS from a root PID, depth ≤ 3,
    //! looking for an exe basename that matches the Phase A allow-list.
    //!
    //! Depth limit rationale:
    //!   * Direct child of the shell (`pwsh -> copilot`):              depth 1
    //!   * `pwsh -> cmd -> copilot.exe` (user wrapper):                 depth 2
    //!   * `pwsh -> npm.cmd -> node.exe -> copilot.exe` (theoretical):  depth 3
    //!
    //! Going deeper risks crawling the entire Toolhelp32 snapshot for
    //! long-running shells that have spawned dozens of child trees.

    use std::collections::{HashMap, HashSet, VecDeque};

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW,
        PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    use crate::agent_sessions::CliSource;
    use super::classify_exe;

    const MAX_DEPTH: u32 = 3;

    /// Walk the Toolhelp32 process snapshot starting at `root_pid`.
    /// Returns the first descendant whose exe matches the CLI
    /// allow-list, paired with that descendant's PID.
    pub fn matching_cli_under(root_pid: u32) -> Option<(CliSource, u32)> {
        // Snapshot the global process list once. Toolhelp32 is a
        // copy-on-snapshot kernel API so this is consistent for
        // the duration of our walk.
        let snap = Snapshot::new()?;
        let entries = snap.collect();

        // Group children by parent PID for O(1) BFS expansion.
        let mut children_of: HashMap<u32, Vec<&Entry>> = HashMap::new();
        for e in &entries {
            children_of.entry(e.parent_pid).or_default().push(e);
        }

        let mut queue: VecDeque<(u32, u32)> = VecDeque::new(); // (pid, depth)
        let mut visited: HashSet<u32> = HashSet::new();
        queue.push_back((root_pid, 0));
        visited.insert(root_pid);

        while let Some((pid, depth)) = queue.pop_front() {
            if depth >= MAX_DEPTH {
                continue;
            }
            let Some(kids) = children_of.get(&pid) else { continue; };
            for child in kids {
                if !visited.insert(child.pid) {
                    continue;
                }
                if let Some(cli) = classify_exe(&child.exe) {
                    return Some((cli, child.pid));
                }
                queue.push_back((child.pid, depth + 1));
            }
        }
        None
    }

    /// Owned exe-name + pid + parent-pid triple, decoupled from the
    /// raw PROCESSENTRY32W struct so the snapshot handle can close.
    struct Entry {
        pid:        u32,
        parent_pid: u32,
        exe:        String,
    }

    /// RAII handle for a Toolhelp32 snapshot. Closing the handle in
    /// `Drop` keeps the cleanup path obvious even on early returns.
    struct Snapshot(windows_sys::Win32::Foundation::HANDLE);

    impl Snapshot {
        fn new() -> Option<Self> {
            // SAFETY: `CreateToolhelp32Snapshot` returns either a valid
            // kernel object handle or INVALID_HANDLE_VALUE (-1 as
            // isize). We check both.
            let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
            if handle.is_null() || handle as isize == -1 {
                tracing::debug!(
                    target: "pid_pane_scanner",
                    "CreateToolhelp32Snapshot failed (err={})",
                    std::io::Error::last_os_error(),
                );
                return None;
            }
            Some(Self(handle))
        }

        fn collect(&self) -> Vec<Entry> {
            let mut out = Vec::new();
            // PROCESSENTRY32W requires the caller to set dwSize before
            // calling Process32FirstW — otherwise the API rejects the
            // struct as invalid and returns false.
            let mut pe: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
            pe.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

            // SAFETY: handle is valid by construction (checked in
            // `new`); pe is a properly sized zeroed struct with the
            // required size field set.
            if unsafe { Process32FirstW(self.0, &mut pe) } == 0 {
                return out;
            }
            loop {
                let exe = utf16_until_nul(&pe.szExeFile);
                out.push(Entry {
                    pid:        pe.th32ProcessID,
                    parent_pid: pe.th32ParentProcessID,
                    exe,
                });
                if unsafe { Process32NextW(self.0, &mut pe) } == 0 {
                    break;
                }
            }
            out
        }
    }

    impl Drop for Snapshot {
        fn drop(&mut self) {
            // SAFETY: handle was a valid kernel object on construction
            // and we close it exactly once.
            unsafe { CloseHandle(self.0) };
        }
    }

    /// Decode a NUL-terminated UTF-16 buffer into a Rust `String`.
    /// PROCESSENTRY32W::szExeFile is a fixed-size MAX_PATH array
    /// padded with zeros, so we stop at the first zero codepoint.
    fn utf16_until_nul(buf: &[u16]) -> String {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }
}

#[cfg(not(windows))]
mod child_enum {
    //! Non-Windows stub. The scanner has no useful behavior off
    //! Windows (WT itself is Windows-only) but compiling the rest of
    //! the module on Linux makes `cargo check --target …` workflows
    //! and CI matrices simpler.

    use crate::agent_sessions::CliSource;

    pub fn matching_cli_under(_root_pid: u32) -> Option<(CliSource, u32)> {
        None
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests cover the pure `diff_snapshots` and `classify_exe`
    //! functions. The async `scan_tab` and Win32 `child_enum` are
    //! exercised by the integration smoke test described in the spec
    //! (manual: run `copilot` in a hookless pwsh pane, watch the
    //! session management view).

    use super::*;
    use std::collections::HashMap;

    fn b(pane: &str, cli: CliSource, pid: u32) -> PaneCliBinding {
        PaneCliBinding { pane_guid: pane.to_string(), cli_source: cli, cli_pid: pid }
    }

    #[test]
    fn diff_snapshots_empty_to_empty_yields_no_events() {
        let last = HashMap::new();
        let (events, new) = diff_snapshots(&last, &[]);
        assert!(events.is_empty());
        assert!(new.is_empty());
    }

    #[test]
    fn diff_snapshots_emits_detected_for_new_panes() {
        let last = HashMap::new();
        let current = vec![b("p1", CliSource::Copilot, 100)];
        let (events, new) = diff_snapshots(&last, &current);
        assert_eq!(events.len(), 1);
        match &events[0] {
            SessionEvent::PidScannerDetected { pane_guid, cli_source, cli_pid } => {
                assert_eq!(pane_guid, "p1");
                assert_eq!(*cli_source, CliSource::Copilot);
                assert_eq!(*cli_pid, 100);
            }
            other => panic!("expected Detected, got {other:?}"),
        }
        assert_eq!(new.get("p1"), Some(&(CliSource::Copilot, 100)));
    }

    #[test]
    fn diff_snapshots_emits_lost_for_vanished_panes() {
        let mut last = HashMap::new();
        last.insert("p1".to_string(), (CliSource::Claude, 9));
        let (events, new) = diff_snapshots(&last, &[]);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], SessionEvent::PidScannerLost { pane_guid } if pane_guid == "p1"));
        assert!(new.is_empty(), "vanished pane must drop from carry-over state");
    }

    #[test]
    fn diff_snapshots_no_change_emits_nothing() {
        let mut last = HashMap::new();
        last.insert("p1".to_string(), (CliSource::Gemini, 42));
        let current = vec![b("p1", CliSource::Gemini, 42)];
        let (events, new) = diff_snapshots(&last, &current);
        assert!(events.is_empty(), "identical snapshot must be idle");
        assert_eq!(new.get("p1"), Some(&(CliSource::Gemini, 42)));
    }

    #[test]
    fn diff_snapshots_pid_change_emits_lost_then_detected() {
        let mut last = HashMap::new();
        last.insert("p1".to_string(), (CliSource::Copilot, 100));
        let current = vec![b("p1", CliSource::Copilot, 200)];
        let (events, new) = diff_snapshots(&last, &current);
        assert_eq!(events.len(), 2, "rebind = Lost + Detected");
        assert!(matches!(events[0], SessionEvent::PidScannerLost { .. }));
        assert!(matches!(events[1], SessionEvent::PidScannerDetected { cli_pid: 200, .. }));
        assert_eq!(new.get("p1"), Some(&(CliSource::Copilot, 200)));
    }

    #[test]
    fn diff_snapshots_cli_change_emits_lost_then_detected() {
        // Edge case: same pane, same PID, but the CLI binary reported
        // a different identity. In practice this can happen if the
        // user kills `copilot` and starts `claude` so fast that the
        // PID is reused. The reducer's "update in place" path also
        // handles this, but the differ must still surface the change.
        let mut last = HashMap::new();
        last.insert("p1".to_string(), (CliSource::Copilot, 100));
        let current = vec![b("p1", CliSource::Claude, 100)];
        let (events, _new) = diff_snapshots(&last, &current);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], SessionEvent::PidScannerLost { .. }));
        assert!(matches!(&events[1],
            SessionEvent::PidScannerDetected { cli_source, cli_pid: 100, .. }
            if *cli_source == CliSource::Claude
        ));
    }

    #[test]
    fn diff_snapshots_handles_multiple_panes_mixed_change() {
        let mut last = HashMap::new();
        last.insert("alive".to_string(),  (CliSource::Copilot, 10));
        last.insert("gone".to_string(),   (CliSource::Claude,  20));
        last.insert("rebind".to_string(), (CliSource::Gemini,  30));
        let current = vec![
            b("alive",  CliSource::Copilot, 10),  // unchanged
            b("rebind", CliSource::Gemini,  31),  // pid bump
            b("new",    CliSource::Claude,  40),  // appearance
        ];
        let (events, new) = diff_snapshots(&last, &current);

        let lost: Vec<&str> = events.iter().filter_map(|e| match e {
            SessionEvent::PidScannerLost { pane_guid } => Some(pane_guid.as_str()),
            _ => None,
        }).collect();
        let detected: Vec<(&str, u32)> = events.iter().filter_map(|e| match e {
            SessionEvent::PidScannerDetected { pane_guid, cli_pid, .. } =>
                Some((pane_guid.as_str(), *cli_pid)),
            _ => None,
        }).collect();

        assert!(lost.contains(&"gone"));
        assert!(lost.contains(&"rebind"));
        assert!(detected.contains(&("rebind", 31)));
        assert!(detected.contains(&("new", 40)));
        assert!(!lost.contains(&"alive"));
        assert!(!detected.iter().any(|(p, _)| *p == "alive"));

        assert_eq!(new.len(), 3);
        assert_eq!(new.get("alive"),  Some(&(CliSource::Copilot, 10)));
        assert_eq!(new.get("rebind"), Some(&(CliSource::Gemini,  31)));
        assert_eq!(new.get("new"),    Some(&(CliSource::Claude,  40)));
    }

    #[test]
    fn classify_exe_matches_phase_a_allow_list() {
        assert_eq!(classify_exe("copilot.exe"),     Some(CliSource::Copilot));
        assert_eq!(classify_exe("COPILOT.EXE"),     Some(CliSource::Copilot));
        assert_eq!(classify_exe("copilot-cli.exe"), Some(CliSource::Copilot));
        assert_eq!(classify_exe("claude.exe"),      Some(CliSource::Claude));
        assert_eq!(classify_exe("Claude.Exe"),      Some(CliSource::Claude));
        assert_eq!(classify_exe("gemini.exe"),      Some(CliSource::Gemini));
    }

    #[test]
    fn classify_exe_rejects_node_and_unrelated_exes() {
        assert_eq!(classify_exe("node.exe"),    None);
        assert_eq!(classify_exe("npm.cmd"),     None);
        assert_eq!(classify_exe("npx.cmd"),     None);
        assert_eq!(classify_exe("pwsh.exe"),    None);
        assert_eq!(classify_exe("cmd.exe"),     None);
        assert_eq!(classify_exe("copilot.dll"), None);
        assert_eq!(classify_exe(""),            None);
    }
}

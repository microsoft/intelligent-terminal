# Hookless Session Tracking — Plan C: Master Wiring + Hook Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Plan B watcher the live producer of Class-B agent sessions by feeding `wta-master`'s registry in-process, then remove the old hook apparatus (Rust bundle/installer/subcommand/env + the one C++ env injection).

**Architecture:** A background thread in `run_master_loop` runs `session_watcher::watch`; emitted `SessionEvent`s are bridged into the master's tokio loop and applied via the **existing** `registry.apply_event(...)` + `broadcast_ext_to_helpers(sessions/changed)` reducer (the same path `handle_session_hook` uses). On first sight of a session key the master synthesizes a `SessionStarted` and best-effort binds its pane GUID via Plan A's `proc_bind`. Once this producer is live, the PowerShell-hook producer is deleted.

**Tech Stack:** Rust (master = tokio `LocalSet` / `spawn_local`), Plan A `proc_bind`, Plan B `session_watcher`, the existing `session_registry::SessionRegistry` trait + `broadcast_ext_to_helpers`. One C++ edit (MSBuild/razzle).

---

## Prerequisites

- Plan A (`proc_bind`) and Plan B (`session_watcher`) merged/available.

## Verified integration points (from investigation)

- Master state: `MasterStateInner.registry: Arc<dyn session_registry::SessionRegistry>` (`master/mod.rs:152`). The session-management view renders from **master's** snapshot, not the helper's local registry (`master/mod.rs:2049`).
- Reducer template: `handle_session_hook` (`master/mod.rs:2055-2099`): `registry.apply_event(event)` → if applied, `broadcast_ext_to_helpers(build_sessions_changed_notification())`.
- `registry.apply_event(event: agent_sessions::SessionEvent) -> bool` (returns whether it changed state) — exactly what Plan B emits.
- `registry.lookup(&SessionId) -> Option<SessionInfo>` / `registry.upsert(SessionInfo)` (`master/mod.rs:804-824`, `955-964`).
- Spawn site for background tasks: `run_master_loop` (`master/mod.rs:1344`), alongside the history-scan `spawn_local` (`:1528`) and the WT-event subscriber (`:1565`).
- `broadcast_ext_to_helpers(&MasterStateInner, acp::ExtNotification)` (`master/mod.rs:1994`), `build_sessions_changed_notification()` (`session_registry.rs:223`).
- `agent_session_to_session_info(&AgentSession) -> SessionInfo` (`session_registry.rs:735`) and `SessionInfo::new(SessionId, PathBuf)` for constructing rows.

## Removal surface (verified)

| Item | Location |
|---|---|
| Rust installer module | `tools/wta/src/agent_hooks_installer.rs` + `mod agent_hooks_installer;` (`main.rs:5`) |
| `Hooks` subcommand | `main.rs:436-442` (variant), `:512-535` (`HooksAction`), dispatch + handlers (`:960-1030`) |
| Call sites | `main.rs:973,978,1005,1019,2474`; `master/mod.rs:1226-1252` (`upgrade_installed_hooks`) |
| Hook env (Rust) | `protocol/acp/spawn.rs:94-103` (`WTA_HOOK_LOG_DIR`) |
| Bundle | `tools/wta/wt-agent-hooks/**` |
| Dormant ingest | `app.rs:5140-5173` (`agent_event` WtEvent), `app.rs:2736-2745` (`publish_session_hook`) |
| Hook env (C++) | `src/cascadia/TerminalConnection/ConptyConnection.cpp:77-93` (`WTA_HOOK_LOG_DIR`) + comment `src/cascadia/inc/IntelligentTerminalPaths.h:61-66` |
| Settings/FRE hook UI (C++) | `TerminalSettingsEditor/AIAgents.xaml`, `AIAgentsViewModel.h`, `ut_app/AgentHooksStatusTests.cpp` |

**Keep (shared, do NOT remove):** the COM `SendEvent`/`Broadcast` bus (`TerminalProtocolComServer.cpp:707-765`), the autofix OSC-133 path, the `AgentEvent;` VT bridge, and the `session_hook`/`handle_session_hook` ext-request mechanism (left dormant).

---

## PHASE 1 — Producer (master watcher → registry)

## Task 1: Bridge the watcher into master + apply loop

**Files:**
- Modify: `tools/wta/src/master/mod.rs`

- [ ] **Step 1: Add the apply helper (mirrors `handle_session_hook`'s tail)**

In `master/mod.rs`, add near `handle_session_hook` (~line 2099):

```rust
/// Apply one watcher-emitted session event to master's registry and, if it
/// changed state, broadcast `sessions/changed` so helpers refetch. Mirrors
/// `handle_session_hook` but for the in-process file watcher (no ext-request
/// round-trip). `SessionStarted` synthesis + pane binding happens in
/// `ensure_watched_session_row` before the activity event is applied.
async fn apply_watcher_event(state: &MasterStateInner, emitted: crate::session_watcher::Emitted) {
    ensure_watched_session_row(state, &emitted).await;
    let applied = state.registry.apply_event(emitted.event).await;
    if applied {
        broadcast_ext_to_helpers(
            state,
            crate::session_registry::build_sessions_changed_notification(),
        )
        .await;
    }
}
```

- [ ] **Step 2: Add a temporary stub for `ensure_watched_session_row` (filled in Task 3)**

So Task 1 compiles independently, add a stub (replaced in Task 3):

```rust
/// Ensure master's registry has a row for the event's session key, creating a
/// minimal one on first sight. Pane binding is added in Plan C Task 3.
async fn ensure_watched_session_row(
    state: &MasterStateInner,
    emitted: &crate::session_watcher::Emitted,
) {
    let sid = acp::SessionId::new(emitted.key.clone());
    if state.registry.lookup(&sid).await.is_none() {
        let mut info = crate::session_registry::SessionInfo::new(sid, std::path::PathBuf::new());
        info.cli_source = Some(emitted.cli.clone());
        info.status = Some(crate::agent_sessions::AgentStatus::Idle);
        info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
        state.registry.upsert(info).await;
    }
}
```

> Verify `SessionOrigin::Unknown` is the correct variant for shell-pane (Class-B) sessions by checking `agent_sessions::SessionOrigin` (the spec's "Class B" = `SessionOrigin::Unknown`). If the field or variant name differs, adjust.

- [ ] **Step 3: Spawn the watcher in `run_master_loop`**

In `run_master_loop` (`master/mod.rs:1344`), after the history-scan `spawn_local` block (ends ~`:1563`), add:

```rust
    // ── Hookless Class-B session watcher ──────────────────────────────
    // A blocking `notify` watcher runs on its own OS thread; a bridge thread
    // forwards emitted events into this LocalSet via a tokio channel, where
    // they're applied to master's registry (same reducer as session_hook).
    {
        let (sync_tx, sync_rx) = std::sync::mpsc::channel::<crate::session_watcher::Emitted>();
        std::thread::Builder::new()
            .name("wta-session-watch".into())
            .spawn(move || {
                if let Err(err) = crate::session_watcher::watch(sync_tx) {
                    tracing::warn!(target: "session_watcher", error = %err, "watcher exited");
                }
            })
            .ok();

        let (async_tx, mut async_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::session_watcher::Emitted>();
        std::thread::Builder::new()
            .name("wta-session-watch-bridge".into())
            .spawn(move || {
                for emitted in sync_rx {
                    if async_tx.send(emitted).is_err() {
                        break;
                    }
                }
            })
            .ok();

        let inner_for_watch = Arc::clone(&inner);
        tokio::task::spawn_local(async move {
            while let Some(emitted) = async_rx.recv().await {
                apply_watcher_event(&inner_for_watch, emitted).await;
            }
        });
    }
```

- [ ] **Step 4: Build**

```bash
powershell -NoProfile -Command "Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force"
cargo build --manifest-path tools/wta/Cargo.toml
```
Expected: clean build. Fix any `SessionInfo` field-name mismatches the compiler reports (the field set is in `session_registry.rs`).

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/master/mod.rs
git commit -m "feat(wta/master): bridge session_watcher into registry apply loop"
```

---

## Task 2: `proc_bind::pids_for_exe` (process enumeration)

Needed for Claude/Gemini cwd-correlation (gather candidate CLI processes).

**Files:**
- Modify: `tools/wta/src/proc_bind.rs`

- [ ] **Step 1: Write the failing test**

Add to `proc_bind.rs` `tests`:

```rust
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
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::pids_for_exe -- --nocapture`
Expected: FAIL ("cannot find function `pids_for_exe`").

- [ ] **Step 3: Implement via Toolhelp32**

Add the Toolhelp FFI + function to `proc_bind.rs`:

```rust
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
```

- [ ] **Step 4: Run the test**

Run: `cargo test --manifest-path tools/wta/Cargo.toml proc_bind::tests::pids_for_exe -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/proc_bind.rs
git commit -m "feat(wta/proc_bind): pids_for_exe via Toolhelp32 process enum"
```

---

## Task 3: Pane binding on first-seen session

Replace the Task 1 stub with real pane binding per the spec's Decision #3.

**Files:**
- Modify: `tools/wta/src/session_watcher/bind.rs` (add the per-CLI resolver)
- Modify: `tools/wta/src/master/mod.rs` (`ensure_watched_session_row`)

- [ ] **Step 1: Add the candidate-gathering + per-CLI resolver to `bind.rs`**

Add to `tools/wta/src/session_watcher/bind.rs`:

```rust
use crate::agent_sessions::CliSource;

/// Process exe names to enumerate per CLI when correlating by cwd.
fn exe_names(cli: &CliSource) -> &'static [&'static str] {
    match cli {
        CliSource::Claude => &["claude.exe"],
        CliSource::Gemini => &["node.exe"], // gemini runs as node bundle/gemini.js
        _ => &[],
    }
}

/// Gather live candidate processes for a cwd-correlated CLI: (pid, cwd) for
/// every matching exe that has a readable working directory.
pub fn gather_candidates(cli: &CliSource) -> Vec<Candidate> {
    let mut out = Vec::new();
    for name in exe_names(cli) {
        for pid in crate::proc_bind::pids_for_exe(name) {
            if let Some(cwd) = crate::proc_bind::cwd_for_pid(pid) {
                out.push(Candidate { pid, cwd });
            }
        }
    }
    out
}

/// Resolve the pane GUID hosting `cli`'s session, given the session's cwd
/// (path-encoded; available for Claude/Gemini). Returns `None` if no unique
/// match. For Copilot/Codex use [`bind_copilot`]/[`bind_codex`] instead.
pub fn bind_by_cwd(cli: &CliSource, session_cwd: &Path) -> Option<String> {
    let candidates = gather_candidates(cli);
    let pid = correlate_by_cwd(&candidates, session_cwd)?;
    crate::proc_bind::wt_session_for_pid(pid)
}
```

- [ ] **Step 2: Replace `ensure_watched_session_row` in `master/mod.rs` with real binding**

```rust
/// Ensure master's registry has a row for the event's session key, creating a
/// minimal one (with a best-effort pane binding) on first sight.
async fn ensure_watched_session_row(
    state: &MasterStateInner,
    emitted: &crate::session_watcher::Emitted,
) {
    use crate::agent_sessions::CliSource;
    let sid = acp::SessionId::new(emitted.key.clone());
    if state.registry.lookup(&sid).await.is_some() {
        return;
    }

    // Best-effort pane GUID + cwd resolution (never blocks row creation).
    let home = std::env::var("USERPROFILE").map(std::path::PathBuf::from).unwrap_or_default();
    let (pane, cwd): (Option<String>, std::path::PathBuf) = match &emitted.cli {
        CliSource::Copilot => {
            let dir = crate::history_loader::copilot_session_dir_for_key(&home, &emitted.key);
            (crate::session_watcher::bind::bind_copilot(&dir), dir)
        }
        CliSource::Codex => {
            match crate::history_loader::find_codex_rollout_by_id(&home, &emitted.key) {
                Some(path) => (crate::session_watcher::bind::bind_codex(&path), path.parent().map(Into::into).unwrap_or_default()),
                None => (None, std::path::PathBuf::new()),
            }
        }
        CliSource::Claude => {
            let cwd = crate::history_loader::claude_cwd_for_key(&home, &emitted.key).unwrap_or_default();
            (crate::session_watcher::bind::bind_by_cwd(&emitted.cli, &cwd), cwd)
        }
        CliSource::Gemini => {
            let cwd = crate::history_loader::gemini_cwd_for_key(&home, &emitted.key).unwrap_or_default();
            (crate::session_watcher::bind::bind_by_cwd(&emitted.cli, &cwd), cwd)
        }
        CliSource::Unknown(_) => (None, std::path::PathBuf::new()),
    };

    let mut info = crate::session_registry::SessionInfo::new(sid, cwd);
    info.cli_source = Some(emitted.cli.clone());
    info.status = Some(crate::agent_sessions::AgentStatus::Idle);
    info.origin = Some(crate::agent_sessions::SessionOrigin::Unknown);
    info.pane_session_id = pane;
    state.registry.upsert(info).await;
}
```

> **Helper functions to confirm/add:** `history_loader::find_codex_rollout_by_id` exists (`history_loader.rs:826`). `claude_cwd_for_key` / `gemini_cwd_for_key` may not exist as such — if absent, derive cwd from the discovery path the watcher already computed. **Better:** thread the `cwd: Option<PathBuf>` that `discover::identify` already returns (Plan B) through `Emitted` so master doesn't re-derive it. If `Emitted` lacks `cwd`, add an `Option<PathBuf>` field to `Emitted` in Plan B's `mod.rs` and populate it from `discover::identify` in `process_change` — adjust this task accordingly.

- [ ] **Step 3: Build**

```bash
cargo build --manifest-path tools/wta/Cargo.toml
```
Expected: clean. Resolve any missing-helper compile errors per the note above (prefer threading `cwd` through `Emitted`).

- [ ] **Step 4: Manual end-to-end verification (the real test)**

```bash
powershell -NoProfile -Command "Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force"
```
Build + deploy the package (F5 `CascadiaPackage` or the razzle build per AGENTS.md), open Windows Terminal, run `copilot` (or claude/codex/gemini) in a normal pane, issue a tool-using prompt, then open the session-management view (`/sessions` in an agent pane, or `wta sessions list`). Expected: the shell-pane session appears with live `Working`/`Idle` transitions, **with the hook bundle NOT installed**. Check `wta-main_master.log` for `target=session_watcher` apply lines.

- [ ] **Step 5: Commit**

```bash
git add tools/wta/src/session_watcher/bind.rs tools/wta/src/master/mod.rs
git commit -m "feat(wta/master): bind watched sessions to panes (lock/RM/cwd)"
```

---

## PHASE 2 — Remove the Rust hook producer

## Task 4: Delete installer, subcommand, call sites, env, bundle

**Files:**
- Delete: `tools/wta/src/agent_hooks_installer.rs`, `tools/wta/wt-agent-hooks/**`
- Modify: `tools/wta/src/main.rs`, `tools/wta/src/master/mod.rs`, `tools/wta/src/protocol/acp/spawn.rs`

- [ ] **Step 1: Remove the env injection in `spawn.rs`**

In `tools/wta/src/protocol/acp/spawn.rs` (~line 94-103), delete the `cmd.env("WTA_HOOK_LOG_DIR", ...)` block and its comment.

- [ ] **Step 2: Remove the master startup upgrade task**

In `tools/wta/src/master/mod.rs` (~line 1226-1252), delete the block that calls `crate::agent_hooks_installer::upgrade_installed_hooks()`.

- [ ] **Step 3: Remove the `Hooks` subcommand + handlers + module**

In `tools/wta/src/main.rs`:
- Delete `mod agent_hooks_installer;` (line ~5).
- Delete the `Hooks { ... }` variant (`enum Command`, ~436-442).
- Delete the `HooksAction` enum (~512-535).
- Delete the dispatch arm `Some(Command::Hooks { action }) => { ... }` and the handler code that calls `ensure_installed_scoped` / `status` / `uninstall` (~960-1030).
- Delete the startup `ensure_installed()` call (~line 2474).

- [ ] **Step 4: Delete the installer module + bundle**

```bash
git rm tools/wta/src/agent_hooks_installer.rs
git rm -r tools/wta/wt-agent-hooks
```

- [ ] **Step 5: Build and fix fallout**

```bash
powershell -NoProfile -Command "Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force"
cargo build --manifest-path tools/wta/Cargo.toml
```
Expected: compile errors only at the deleted call sites — remove each remaining reference the compiler flags (e.g. an `agent_hooks_installer::CliScope` import in `main.rs`). No `notify`/dep changes here, so **no cgmanifest/NOTICE regen** (the bundle was `include_str!`-embedded, not a crate). Verify with: `cargo metadata --manifest-path tools/wta/Cargo.toml --format-version 1 | findstr /i hooks` → no output.

- [ ] **Step 6: Run the suite**

Run: `cargo test --manifest-path tools/wta/Cargo.toml -- --nocapture`
Expected: PASS (any `agent_hooks_installer` unit tests are gone with the module; no other test should depend on it).

- [ ] **Step 7: Commit**

```bash
git add -A tools/wta/
git commit -m "refactor(wta): remove agent-hooks installer, subcommand, env, and bundle"
```

---

## Task 5: Remove the now-dormant Rust ingest path (cleanup)

The `agent_event` WtEvent never fires once C++ stops emitting it (Task 6) and the bundle is gone. Remove the dead helper-side ingest. **Keep** `session_hook`/`handle_session_hook` (harmless, and a future re-use point).

**Files:**
- Modify: `tools/wta/src/app.rs`

- [ ] **Step 1: Remove the `agent_event` branch**

In `tools/wta/src/app.rs` (~5140-5173), delete the `if method == "agent_event" { ... return; }` block inside the `AppEvent::WtEvent` handler. Leave the rest of the `WtEvent` handler (autofix / vt_sequence) intact.

- [ ] **Step 2: Remove `publish_session_hook` if now unused**

In `tools/wta/src/app.rs` (~2736-2745), if `publish_session_hook` has no remaining callers after Step 1, delete it and the `session_hook_tx` field + its wiring. If the compiler shows other callers, leave it.

- [ ] **Step 3: Build + test**

```bash
cargo build --manifest-path tools/wta/Cargo.toml
cargo test --manifest-path tools/wta/Cargo.toml -- --nocapture
```
Expected: clean. Fix any unused-import/field warnings the deletion surfaces.

- [ ] **Step 4: Commit**

```bash
git add tools/wta/src/app.rs
git commit -m "refactor(wta/app): remove dormant agent_event hook ingest"
```

---

## PHASE 3 — Remove the C++ hook surface

## Task 6: Remove the C++ env injection

**Files:**
- Modify: `src/cascadia/TerminalConnection/ConptyConnection.cpp`
- Modify: `src/cascadia/inc/IntelligentTerminalPaths.h` (comment only)

- [ ] **Step 1: Delete the `WTA_HOOK_LOG_DIR` injection**

In `src/cascadia/TerminalConnection/ConptyConnection.cpp` (~77-93), delete the block that does `environment.as_map().insert_or_assign(L"WTA_HOOK_LOG_DIR", ...)` and its comment. **Do not** touch the adjacent `WT_COM_CLSID` injection.

- [ ] **Step 2: Update the stale comment**

In `src/cascadia/inc/IntelligentTerminalPaths.h` (~61-66), remove/adjust the comment referencing `WTA_HOOK_LOG_DIR` so it no longer implies hook support.

- [ ] **Step 3: Build the C++ side (razzle)**

```bash
cmd.exe /c "tools\razzle.cmd && bcz no_clean"
```
Expected: builds clean. `WTA_HOOK_LOG_DIR` is no longer referenced in C++ (the Rust `spawn.rs` reference was removed in Task 4).

- [ ] **Step 4: Format + commit**

```bash
cmd.exe /c "tools\razzle.cmd && runformat"
git add src/cascadia/TerminalConnection/ConptyConnection.cpp src/cascadia/inc/IntelligentTerminalPaths.h
git commit -m "refactor(terminal): drop WTA_HOOK_LOG_DIR env injection (hooks removed)"
```

---

## Task 7: Remove the Settings/FRE hook UI (splittable)

> This task is **separable** — the hookless feature works without it, but leaving it ships a Settings/FRE "Install hooks" action whose installer is gone. Execute it here, or split into a dedicated UX PR. Coordinate the exact UX (remove the section vs. repurpose it as "session tracking is automatic") with the team.

**Files:**
- Modify: `src/cascadia/TerminalSettingsEditor/AIAgents.xaml`, `AIAgentsViewModel.h`/`.cpp`
- Delete/adjust: `src/cascadia/ut_app/AgentHooksStatusTests.cpp`

- [ ] **Step 1: Remove the hook install/status UI**

In `AIAgents.xaml` + `AIAgentsViewModel.{h,cpp}`, remove the hook-install/status controls and the view-model members/commands that invoked `wta hooks install/status` (or that surfaced `AgentHooksStatus`). Search the editor project for `hook` to find all bindings.

- [ ] **Step 2: Remove the dead C++ test**

```bash
git rm src/cascadia/ut_app/AgentHooksStatusTests.cpp
```
Then remove its `<ClCompile Include="AgentHooksStatusTests.cpp" />` entry from the `ut_app` vcxproj.

- [ ] **Step 3: Build + run the editor/app tests**

```bash
cmd.exe /c "tools\razzle.cmd && bcz no_clean"
cmd.exe /c "tools\razzle.cmd && runut *App*Tests.dll"
```
Expected: builds + unit tests pass; no dangling hook references.

- [ ] **Step 4: Commit**

```bash
git add -A src/cascadia/TerminalSettingsEditor/ src/cascadia/ut_app/
git commit -m "refactor(settings): remove agent-hooks install UI (tracking is now automatic)"
```

---

## Task 8: Finalize

- [ ] **Step 1: Rust fmt + clippy**

```bash
cargo fmt --manifest-path tools/wta/Cargo.toml
cargo clippy --manifest-path tools/wta/Cargo.toml -- -D warnings
```
Expected: clean (remove any now-unused imports left by the deletions).

- [ ] **Step 2: Full Rust test pass**

```bash
cargo test --manifest-path tools/wta/Cargo.toml -- --nocapture
```
Expected: PASS.

- [ ] **Step 3: End-to-end smoke (no hooks installed)**

Confirm: with `wt-agent-hooks` NOT installed in any CLI, a shell-pane `copilot`/`claude`/`codex`/`gemini` session shows up in the session-management view with live activity, and Enter focuses its pane (when binding succeeded). Check `wta-main_master.log` for `apply_watcher_event` activity and no `agent_hooks` / `upgrade decision` lines.

- [ ] **Step 4: Final commit (if fmt/clippy changed anything)**

```bash
git add -A tools/wta/
git commit -m "chore(wta): fmt + clippy clean after hook removal"
```

---

## Self-Review Checklist (completed during planning)

- **Spec coverage:** Implements Pillar-2 "Home = wta-master" wiring (watcher → `apply_event` → `broadcast sessions/changed`, the existing reducer), first-sight `SessionStarted` synthesis + pane binding (copilot lock / codex RM / claude+gemini cwd), and the full hook teardown (Rust installer/subcommand/env/bundle + the one C++ env injection + the Settings UI). Window scoping is satisfied implicitly: master is the single per-process owner and broadcasts to all helpers, which already filter — no per-window routing is added (consistent with the verified `master/mod.rs` having no `window_id`).
- **Placeholder scan:** No TBD/TODO. Deletion tasks cite exact files/lines from investigation; the one area flagged "confirm/adjust" (claude/gemini cwd plumbing through `Emitted`) is an explicit, bounded instruction, not a placeholder.
- **Type consistency:** `apply_watcher_event`/`ensure_watched_session_row` consume `session_watcher::Emitted { cli, key, event }` (Plan B) and call `registry.apply_event`/`lookup`/`upsert` + `broadcast_ext_to_helpers`/`build_sessions_changed_notification` (verified signatures). `bind_by_cwd`/`gather_candidates`/`bind_copilot`/`bind_codex` extend Plan B's `bind.rs`; `pids_for_exe`/`cwd_for_pid`/`wt_session_for_pid` are Plan A/Plan-C-Task-2 primitives.
- **Phasing:** Phase 1 (producer) is independently verifiable before any removal; Phase 2 (Rust teardown) and Phase 3 (C++ teardown) only run once the producer is confirmed live, so the feature is never broken mid-stream. Task 7 is explicitly splittable.

## Done = the whole feature

After Plans A + B + C: Class-B agent sessions (Copilot/Claude/Codex/Gemini) are tracked hook-free — discovery + live activity from the file watcher, pane binding from `proc_bind`, surfaced via master's existing registry/broadcast — with the entire PowerShell-hook apparatus removed. (Antigravity remains the documented, deferred follow-up per the spec.)

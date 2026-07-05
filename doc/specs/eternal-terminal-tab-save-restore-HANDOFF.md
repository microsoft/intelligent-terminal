# HANDOFF — Eternal Terminal: per-tab Save / Restore (step 1)

**Purpose of this file:** a self-contained handoff so a fresh agent (or human)
can pick up mid-execution and continue without the original chat context.
Read this top-to-bottom, then jump to **§8 "Exact next actions"**.

> ## ✅ STATUS: IMPLEMENTATION COMPLETE (2026-07-02)
> All 7 task groups (G1–G7) are implemented and reviewed (each spec + quality
> reviewed; plus a clean final cross-boundary integration review). The sections
> below are retained as **historical execution log + reusable reference**
> (build gotchas §5, research anchors §9); the per-group "next actions" in §8
> are superseded by this banner.
>
> **Branch HEAD:** `66d8e22f`. **Rust suite:** `1019 passed, 0 failed`.
> **C++ NOT built here** (vcpkg not bootstrapped — see §5); build/verify in VS.
>
> **What remains (yours):** build C++ in Visual Studio; F5 smoke per the plan's
> Phase F2 checklist; run the E2E test `test/e2e/tests/Feature.EternalTerminal.Tests.ps1`.
>
> **Non-blocking follow-ups** (from the final integration review):
> 1. `SavedTabDeleted` AppEvent is emitted but its handler is a no-op (dead code).
> 2. Cross-window focus: restore routes to `GetMostRecentWindow()` + same-window
>    `FocusTab` (not `FocusTabInAnyWindow`), so multi-window focus-if-open falls
>    back to a new-tab restore. Step-1 was scoped same-window; revisit for multi-window.
> 3. Picker view `saved_tabs_view.rs` uses hardcoded English ("Loading…", "No
>    saved tabs yet.", hint) and doesn't render `savedAt`/cwd — i18n + column polish.

**Original last-updated note (historical):** 2026-07-02, after Task group **G2**
(Rust Phase A) was implemented + passed spec-compliance review.

---

## 1. Mission

Implement **step 1** of the "Eternal Terminal" vision for the Intelligent
Terminal (Windows Terminal fork): let a user **snapshot a single tab** (layout +
scrollback content) under a user-typed title and **restore it later** — focusing
the original tab if it is still open, otherwise opening a new tab. Two new
agent-pane slash commands drive it, gated behind a global experimental setting.

- **`/save-ws <title>`** — snapshot this tab; re-saving the same tab overwrites.
- **`/restore-ws`** — picker (↑/↓, Enter to restore, `D` to delete, Esc close).
- Gate: global bool **`experimental.eternalTerminal.enabled`** (default `false`);
  when off, neither command appears in the `/` menu and both are inert.

**In scope (this branch):** layout + scrollback content, focus-if-open,
overwrite-on-re-save, gating, per-snapshot buffer folder, `D` delete.

**Deferred (NOT this branch):** restoring the tab's *agent pane*/conversation
(step 2), *workspaces* (multi-tab/tab-group/window layout), crash-recovery
integration, live shell reattach, restored-tab→origin rebinding.

**Source of truth for the vision:** `shench_microsoft/DFX_specs`, branch
`hamza/IT/spec-updates-0.1`, path `Intelligent Terminal/eternal-terminal/eternal-terminal.md`.
Private Microsoft-EMU repo — read it with the `yuazha_microsoft` gh account:
```
gh api "repos/shench_microsoft/DFX_specs/contents/Intelligent%20Terminal/eternal-terminal/eternal-terminal.md?ref=hamza/IT/spec-updates-0.1" --jq .content  # base64
```

## 2. Where the design + plan live (READ THESE — full detail)

Committed on this branch under `doc/specs/`:
- **Design doc:** `doc/specs/eternal-terminal-tab-save-restore.md` (commit `766e2df0d`)
- **Implementation plan:** `doc/specs/eternal-terminal-tab-save-restore-plan.md` (commit `f2fcda72c`) — phased TDD plan with real code for every task, plus a "verify, don't invent" note list.
- **This handoff:** `doc/specs/eternal-terminal-tab-save-restore-HANDOFF.md`

The plan is the executable artifact. This handoff tracks *execution state* and
environment; the plan holds the *code*.

## 3. Branch / worktree

- **Branch:** `dev/yuazha/eternal-terminal-save-restore` (off `main` @ `eeee65435`).
- **Worktree:** `C:\Users\yuazha\Git\intelligent-terminal\.worktree\eternal-terminal-save-restore`
  — **ALL edits, builds, commits happen here.** Do NOT edit the main tree.
- Convention in this repo: worktrees at `.worktree\<name>`, branches `dev/yuazha/<name>`.

**Commits so far (newest first):**
```
09427c9e2  wta: gate /save-ws + /restore-ws in popup/dispatcher; stub handlers   (A5)
8bc80ed9e  wta/commands: add gated /save-ws and /restore-ws kinds + matches_gated (A4)
e6aafd823  wta: receive --eternal-terminal flag into App.eternal_terminal_enabled   (A3)
f2fcda72c  doc: implementation plan …
766e2df0d  doc: design …
eeee65435  (main) wta: own telemetry provider … (#363)   <- base
```
(HEAD of branch = `09427c9e2`. Use `git -C <worktree> --no-pager log --oneline` for exact A3/A4 SHAs.)

## 4. Key design decisions (already made — do not relitigate)

Resolved with the user during brainstorming; full rationale in the design doc.
- **Content scope:** restore layout **and** scrollback content (not layout-only).
- **Gate:** one `settings.json` global bool `experimental.eternalTerminal.enabled`
  (mirror `autoFixEnabled`), passed to the helper as a CLI flag `--eternal-terminal`.
  Gates BOTH commands (hidden + inert when off).
- **Command names:** `/save-ws`, `/restore-ws` (tab-level; distinct from the
  existing `/sessions` agent-conversation picker).
- **Architecture:** Rust helper renders UI + issues **request/response COM calls**
  via `wtcli`; **C++ owns** storage, serialization, snapshot buffers, and the
  focus-vs-new-tab decision. (Chosen over "Rust reads disk" and "reuse
  PersistedWindowLayouts".)
- **Storage:** new `ApplicationState.SavedTabSessions` vector, parallel to (never
  consumed by) crash-recovery `PersistedWindowLayouts`. Record keyed by
  **`SourceStableId`** (the live tab's StableId) → re-save overwrites.
- **Snapshot buffers:** private folder **`SettingsDirectory\SavedTabSessions\{id}\buffer_{sessionId}.txt`**
  (NOT the settings root). This is immune to WT's crash-recovery buffer cleanup
  (that glob is root-only, non-recursive, prefix `buffer_*`/`elevated_*`), so we
  **do not touch the cleanup**. `D`-delete = remove record + `rmdir` the folder.
- **Save:** `Tab::BuildStartupActions(BuildStartupKind::Persist)` (embeds each
  pane's `connection.SessionId`) + `control.PersistTo()` each pane's buffer into
  the snapshot folder.
- **Restore:** if `_FindTabByStableId(SourceStableId)` finds a live tab →
  focus it (`_SelectTab` / `FocusTabInAnyWindow`). Else **stage** the snapshot's
  `buffer_{sid}.txt` files into the settings root, then replay the stored actions
  via **`ProcessStartupActions`** — the unmodified `_MakeTerminalPane`
  auto-restores content via `RestoreFromPath`. **Why stage-into-root and not a
  member-variable buffer-dir override:** `TerminalPage::ProcessStartupActions`
  (`TerminalPage.cpp:2948`) is a coroutine that **suspends between actions**
  (`co_await resume_foreground`) whenever the window already has tabs, so a
  set-before/clear-after member override cannot bracket the async pane creation.
- **Title:** inline `/save-ws <title>` (rest = title, like `/fix <hint>`).

## 5. Environment + build/test gotchas (CRITICAL)

- **Rust (`tools/wta`) build/test MUST run inside VS2022 *Enterprise* vcvars64.**
  Plain cargo / razzle fails with `LNK1104 cannot open file 'libcmt.lib'` because
  the VS18 install on this machine is partial. Kill stale `wta.exe` first. From
  the worktree root:
  ```powershell
  Get-Process wta -ErrorAction SilentlyContinue | Stop-Process -Force
  & $env:ComSpec /c 'call "C:\Program Files\Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat" >nul && cargo test --manifest-path tools/wta/Cargo.toml'
  ```
  (First build in a fresh worktree is cold → several minutes. The `vswhere.exe`
  warning from vcvars is benign; exit code 0 is what matters.)
- **C++ build (razzle + MSBuild):** from the worktree root:
  ```powershell
  cmd.exe /c "tools\razzle.cmd && bcz no_clean"
  ```
  (Release: `bcz rel no_clean`.) This is heavy (many minutes). None of the C++
  groups (G1,G3,G4,G5) have been built yet — see §7.
- **Never `Get-Process WindowsTerminal | Stop-Process`** — the IT fork's exe is
  also named `WindowsTerminal.exe` and that command kills the agent's own host
  terminal. Filter by path/PFN, or let ItE2E Start/Stop-Terminal manage it.
- **Commits:** append the trailer
  `Co-authored-by: Copilot <223556219+Copilot@users.noreply.github.com>`.
- **User preference (long-term):** @DDKinger normally builds/verifies before
  committing and does not want auto-commit. **For THIS execution the user
  explicitly opted into the subagent full-auto flow (subagents auto-commit; user
  reviews at the end).** That override is session-scoped — do not treat auto-commit
  as their standing preference on future work.
- **Respond to the user in Chinese** (never Japanese).

## 6. Execution model (how we're running this)

Using the **superpowers subagent-driven-development** skill:
- One **implementer** subagent per task-group (fresh context; given full task
  text — do NOT make it read the plan file), then two reviewers:
  1. **spec-compliance** review (verify exactly the spec, nothing more/less),
  2. **code-quality** review (only after spec passes).
- Reviewer→fix→re-review loop until both pass; then mark the group done and move
  to the next. Implementer subagents run **sequentially** (they share the
  worktree — never dispatch two in parallel). Reviewers are read-only.
- Prompt templates:
  `C:\Users\yuazha\.copilot\installed-plugins\superpowers-marketplace\superpowers\skills\subagent-driven-development\{implementer-prompt.md,spec-reviewer-prompt.md,code-quality-reviewer-prompt.md}`
- **Grouping:** to minimize heavy C++ rebuilds, tasks are executed in coherent
  commit-unit **groups** (G1–G7 below), each = one implementer dispatch. Each
  subagent must be told to work in the worktree (§3), use the §5 build commands,
  and commit at the plan's commit points.
- After all groups: one final whole-branch code review, then the
  **finishing-a-development-branch** skill.

## 7. Task groups (plan tasks → execution groups)

| Group | Plan tasks | Lang | Status |
|---|---|---|---|
| **G2** | A3 + A4 + A5 | Rust | ✅ DONE (e6aafd823/8bc80ed9e/09427c9e2) — spec + quality reviewed |
| **G1** | A1 + A2 | C++ | ✅ DONE (e02c0d665/4bcab0a5a) — static-verified; build in VS |
| **G3** | B1 + B2 + B3 | C++ | ✅ DONE (0b5b46b3) — spec + C++ correctness clean |
| **G4** | C1–C5 | C++ | ✅ DONE (d4f121a41) — spec + correctness clean; elevated-buffer refinement |
| **G5** | D1 + D2 + D3 | C++ | ✅ DONE (6e181a58f/004c5c6f4) — ABI clean; proxy IDL also updated |
| **G6** | E1 + E2 + E3 | Rust | ✅ DONE (68350d9a/734bcdd0/62a3bf47) — reviewed → overlay-exclusivity fix → re-review ✅ |
| **G7** | F1 | PowerShell | ✅ DONE (859143ca/66d8e22f) — authored + fixed; NOT run here (needs built Terminal) |
| — | F2 | manual | ⏳ YOURS — F5 smoke via the plan's Phase F2 checklist |

**Dependency order:** G2 (done) → G1 → G3 → G4 → G5 → G6 → G7.
(G2 Rust was run first only to validate the loop cheaply; G1 C++ is functionally
its pair. G4 depends on G3; G5 on G4; G6 on G5; G7 on G6.)

**G2 spec-review minor note (non-blocking, optional to address):** test
`gated_flag_marks_only_eternal_commands` (`commands.rs:373`) doesn't exhaustively
assert *no other* command is experimental. Hides no defect (registry confirms
only the two). Could strengthen when convenient.

## 8. Exact next actions (do this next)

1. **Finish G2:** dispatch a **code-quality** reviewer (use the
   requesting-code-review template) for commit range
   `f2fcda72cce831d18fcfdea45d0738009310d75d..09427c9e2e59f15f49c86fba16b2bd1e01f75d51`
   in the worktree. If it flags Critical/Important issues → dispatch the same
   implementer to fix → re-review. When approved, mark `impl-g2-rust-a` done.
2. **G1 (C++ Phase A):** dispatch an implementer with the full text of plan
   Tasks **A1 + A2** (paste from `doc/specs/…-plan.md`). It adds the
   `EternalTerminalEnabled` bool to `MTSMSettings.h` + `GlobalAppSettings.idl`
   and appends `--eternal-terminal` in `TerminalPage.cpp` right after the
   `--no-autofix` block (~`TerminalPage.cpp:1841-1845`). Verify with
   `bcz no_clean` (§5). Spec review → quality review → done.
3. Continue **G3 → G4 → G5 → G6 → G7** the same way, each with the plan's full
   task text and the §5 build commands.
4. After G7: final whole-branch code review, then **finishing-a-development-branch**.
5. Keep the user in Chinese; they will review the whole branch at the end.

**Session todo IDs** (SQLite `todos` table in the original session — recreate if
in a new session): `impl-g2-rust-a` (in_progress→done next), `impl-g1-cpp-a`,
`impl-g3-cpp-b`, `impl-g4-cpp-c`, `impl-g5-cpp-d`, `impl-g6-rust-e`,
`impl-g7-e2e`.

## 9. Research already done (reuse — don't re-derive)

Verified code anchors/patterns the plan's code is mirrored from (file:line may
drift slightly — locate by content):

- **Global bool setting:** `MTSMSettings.h:76-84` `MTSM_GLOBAL_SETTINGS(X)`
  X-macro (e.g. `X(bool, AutoFixEnabled, "autoFixEnabled", false)`);
  `GlobalAppSettings.idl:118-143` `INHERITABLE_SETTING(Boolean, …)`. No GPO/policy
  needed for the experimental flag.
- **Helper cmdline plumbing:** `TerminalPage.cpp:1792-1853` assembles the wta
  helper argv; `--no-autofix` appended at ~1841; call site sets
  `args.Commandline(helperCmd)` at ~1915. Rust receives it: `main.rs:181`
  `no_autofix: bool` → `main.rs:2792` `autofix_enabled = !cli.no_autofix` →
  `App::new`. Helper's own tab id = `app.owner_tab_id` (`app.rs:1944`, from
  `--owner-tab-id`).
- **ApplicationState persisted collection:** `ApplicationState.h:34-108`
  `MTSM_APPLICATION_STATE_FIELDS` X-macro + `WindowLayout` struct + `state_t`;
  `ApplicationState.cpp:24-94` `JsonUtils::ConversionTrait<WindowLayout>` +
  `WindowLayout::ToJson/FromJson`; `:305-315` `AppendPersistedWindowLayout`
  (`single_threaded_vector` + `_throttler()`). `IVector<ActionAndArgs>` serializes
  via `SetValueForKey/GetValueForKey` (same as `TabLayout`).
- **Save serialization source:** `TerminalPage::PersistState` (`TerminalPage.cpp:5625`)
  → `Tab::BuildStartupActions(BuildStartupKind::Persist)`; each pane's SessionId
  embedded in `TerminalPaneContent::GetNewTerminalArgs` Persist case
  (`TerminalPaneContent.cpp:135`).
- **Content persist/restore:** `WindowEmperor.cpp:1310-1370` `control.PersistTo()`
  → `buffer_{sessionId}.txt`; `:1372-1397` cleanup (root-only non-recursive glob).
  Restore auto: `TerminalPage.cpp:7300-7308` `control.RestoreFromPath(...)` when
  `hasSessionId`.
- **Replay actions into current window:** `TerminalPage::ProcessStartupActions`
  (`TerminalPage.cpp:2948`) — coroutine, `_actionDispatch->DoAction`, **suspends
  between actions** (the reason restore stages buffers into root instead of using
  a member override).
- **Focus a tab:** `_FindTabByStableId` (`TerminalPage.cpp:865`),
  `TerminalPage::FocusTab`, `WindowEmperor::FocusTabInAnyWindow`
  (`WindowEmperor.cpp:926`), `_SelectTab` (`TerminalPage.cpp:6098`).
- **COM protocol:** `TerminalProtocol.idl` `interface IProtocolServer`
  (CreateTab/FocusPane/…); impls in `TerminalProtocolComServer.cpp` — target page
  via `_getPage(AppHost*)` (`:164`) + `s_emperor->GetWindows()/GetMostRecentWindow()`;
  `CreateTab` at `:772`, `FocusPane` at `:927`; helpers `_hstr`, `_bstrFromJson`.
  Page-side coroutines in **`TerminalPage.Protocol.cpp`**: `CreateProtocolTab`
  (`:562`) is the exact pattern (`get_strong()` + `co_await wil::resume_foreground(Dispatcher())` + synchronous UI work + `co_return`).
- **wtcli subcommand:** `src/tools/wtcli/main.cpp` — `ConnectToTerminal` (`:71`,
  `WT_COM_CLSID` + `CoCreateInstance`), `new-tab` block (`:457`, CLI11
  `add_subcommand`/`add_option`/`callback` + `CallJson(...)` + `PrintJson`),
  `focus-pane` block (`:569`).
- **Rust wtcli shell-out:** `tools/wta/src/shell/wt_channel/cli_channel.rs` —
  `resolve_wtcli_path()` (`:26`), `spawn_wtcli_split_then_focus_with_callback`
  (`:217`, spawns thread, `.output()`, parses stdout JSON, invokes callback) is
  the pattern for the four new `spawn_wtcli_*` helpers. Deliver results back as
  new `AppEvent`s.
- **Rust picker to mirror:** `/sessions` view — `AgentsViewState` (`app.rs:2083`),
  `open_agents_view_for_tab` (`app.rs:3399`), key handling pattern in
  `handle_setup_key` (`app.rs:3860`, Up/Down/Enter/`selected_index`),
  render `ui/agents_view.rs` (ratatui `List`/`ListState`).

## 10. Relevant stored memories (already in Copilot memory)

- WT persists scrollback to `SettingsDirectory\buffer_{SessionId}.txt` via
  `PersistTo`; restore auto via `RestoreFromPath`; cleanup glob is root-only /
  non-recursive. (repository)
- Design specs live in `doc/specs/`, not `docs/superpowers/specs/`. (repository)
- Code changes for @DDKinger go in a new `.worktree` branch, not the main tree. (user)
- Rust `cargo` here must run under VS2022 Enterprise vcvars64 (VS18 partial →
  LNK1104). (user)
- IT fork exe is `WindowsTerminal.exe`; never `Get-Process WindowsTerminal |
  Stop-Process`. (repository)
- Respond to @DDKinger in Chinese, never Japanese. (user)
- @DDKinger normally verifies before committing / no auto-commit (standing
  preference — but see §5: this execution was an explicit one-off opt-in to
  full-auto).

---

*If anything here conflicts with the committed plan/design docs, those docs win
for design detail; this file wins for current execution state.*

# Terminal–Shell Integration for ACP Agents

> **Research Document** — synthesizes findings from 5 parallel researchers and 5 cross-domain reviewers.
> Covers 4 open issues in how Windows Terminal can provide structured context to ACP agents.

---

## Executive Summary

Windows Terminal's ACP (Agent Control Plane) agent currently relies on LLM-based "guessing" to infer shell state — reading buffer text to detect CWD, shell type, and command errors. This burns **3,000–8,000+ tokens per interaction** and is unreliable.

The terminal internally already tracks much of this data (exit codes in `ScrollbarData`, CWD in `Terminal::_workingDirectory`, marks in `TextBuffer`) but **does not expose it through events or enriched APIs**. The single highest-impact change is exposing the exit code field that already exists internally — approximately 5 lines of IDL change for ~5,000 tokens/session saved.

**Key decisions from this research:**

| Decision | Rationale |
|----------|-----------|
| **Add OSC 7 support** alongside existing OSC 9;9 | Industry standard; bash/zsh/fish emit it natively |
| **Expose exit codes in WinRT API** | Data exists internally but is stripped at the projection boundary |
| **Add `CommandCompleted` + `WorkingDirectoryChanged` events** | Eliminates all polling; event-driven is the only token-efficient architecture |
| **Detect shell type from profile commandline** at launch | Zero-cost, immediate, no shell cooperation needed |
| **Ship shell integration scripts** (bundled, manual opt-in for v1) | Auto-injection has too many shell-specific failure modes for v1 |
| **REJECT color/text heuristics for error detection** | 15-25% false positive rate is net-token-negative |
| **Design APIs with source provenance** | Let agents know how trustworthy each signal is |

### Token Impact Summary

| Signal | Current cost (guessing) | Proposed cost (structured) | Savings |
|--------|------------------------|---------------------------|---------|
| CWD | ~700–2,200 tokens | ~20 tokens (read property) | **~97%** |
| Shell type | ~650 tokens | ~10 tokens (enum) | **~98%** |
| Error detection | ~1,300–5,300 tokens | ~5 tokens (exit code int) | **~99%** |
| Full context | ~3,000–8,000 tokens | ~120 tokens (context packet) | **~96%** |

---

## Issue 1: CWD Detection Per Shell Per Pane

### Current State

Terminal tracks CWD exclusively via **OSC 9;9** (ConEmu's proprietary sequence). The data flow:

```
Shell emits: ESC]9;9;"C:\Users\dev\project"ESC\
  → OutputStateMachineEngine.cpp:878 (case ConEmuAction)
  → adaptDispatch.cpp:3525 (DoConEmuAction, subParam==9)
  → adaptDispatch.cpp:3580 (validates path, strips quotes)
  → TerminalApi.cpp:206 (Terminal::SetWorkingDirectory stores in _workingDirectory)
  → ControlCore.cpp:1508 (WorkingDirectory property exposes to WinRT)
```

**OSC 7 is NOT implemented.** The standard `ESC]7;file://hostname/path ST` sequence used by bash, zsh, fish, and most non-Windows terminals silently falls through to the `default:` case in `ActionOscDispatch()` and is discarded.

**No OS-level CWD detection exists.** However, `ConptyConnection.cpp:707-735` already reads the child process PEB to extract `CommandLine` — the infrastructure for reading `CurrentDirectory` from the same struct is already in place.

### Problem

1. Non-Windows shells (bash, zsh, fish — whether in WSL Ubuntu or Git Bash) emit OSC 7, not OSC 9;9 → CWD is lost
2. cmd.exe requires manual `PROMPT` modification for any CWD reporting
3. No change event → agents must poll or re-read the property
4. CWD is a single global field, not tracked per-command

### Recommended Solution: Layered CWD with Source Provenance

**Priority chain: OSC 7 > OSC 9;9 > PEB fallback > starting directory**

#### Phase 1: Add OSC 7 Parsing

Add `case 7` to `OscActionCodes` enum in `OutputStateMachineEngine.hpp:206-230` and route to a new handler in `adaptDispatch.cpp` that:
- Parses `file://hostname/url-encoded-path` format
- URL-decodes the path component (note: no existing URL-decode util in codebase — needs ~15 lines)
- Stores hostname separately for remote CWD awareness (SSH, WSL remoting)
- Calls existing `_api.SetWorkingDirectory(decoded_path)`

**Validated effort (V1 review): ~40-60 lines across 6 files** (R1's "~10 lines" was underestimated due to URL decoding, `ITermDispatch` layer, and hostname storage).

**Files to modify:**
- `src/terminal/parser/OutputStateMachineEngine.hpp` — add `SetWorkingDirectoryURI = 7` to enum
- `src/terminal/parser/OutputStateMachineEngine.cpp` — add `case 7` in `ActionOscDispatch()`
- `src/terminal/adapter/ITermDispatch.hpp` — add `DoOsc7Action()` virtual
- `src/terminal/adapter/adaptDispatch.hpp/.cpp` — implement URI parsing + decode
- `src/cascadia/UnitTests_TerminalCore/TerminalApiTest.cpp` — mirror OSC 9;9 tests

#### Phase 1: Add `WorkingDirectoryChanged` Event

`Terminal::SetWorkingDirectory()` at `TerminalApi.cpp:206` is a dumb setter — no event. Add a callback following the exact pattern of `_pfnTitleChanged` at `Terminal.hpp:325-337`:

```cpp
// Terminal.hpp — add member:
std::function<void()> _pfnWorkingDirectoryChanged;

// TerminalApi.cpp — in SetWorkingDirectory(), after _workingDirectory = uri:
if (_pfnWorkingDirectoryChanged) _pfnWorkingDirectoryChanged();

// ControlCore — wire event, same as TitleChanged pattern
```

**Validated effort: ~8-10 lines. Risk: VERY LOW.**

#### Phase 1: Per-Prompt CWD Snapshot

When `StartPrompt()` is called (OSC 133;A received), capture `_workingDirectory` into the prompt's `ScrollbarData`. This enables "what directory was command N run in?" queries.

```cpp
// Marks.hpp — add to ScrollbarData:
std::optional<std::wstring> workingDirectory;
```

**Note (V1 review):** Memory impact — every prompt mark carries a string copy. For long sessions with thousands of prompts, consider storing an interned index instead.

#### Phase 2: PEB CWD Reader (Fallback)

Extend `ConptyConnection.cpp:707-735` (which already reads `RTL_USER_PROCESS_PARAMETERS.CommandLine`) to also read `params.CurrentDirectory.DosPath`:

```cpp
// After existing ReadProcessMemory for params:
winrt::impl::hstring_builder cwd{ params.CurrentDirectory.DosPath.Length / 2u };
ReadProcessMemory(process, params.CurrentDirectory.DosPath.Buffer,
                  cwd.data(), params.CurrentDirectory.DosPath.Length, nullptr);
```

**Validated effort: ~15-20 lines.** Reuses exact privilege model (PROCESS_VM_READ already requested).

**Limitations (V2 review):**
- ⚠️ Reads root process CWD only — not the foreground/leaf shell
- 🔴 **Fails for WSL (Ubuntu/other distros)** — `NtQueryInformationProcess` can't read Linux process CWDs; the Windows-side `wsl.exe` process has a static CWD that doesn't reflect the actual Linux shell's working directory
- ✅ Works for cmd.exe, PowerShell, Git Bash (native Windows processes) without any configuration

#### CWD Source Provenance

Add `_cwdSource` enum to `Terminal` so consumers know trustworthiness:

```cpp
enum class CwdSource { None, StartingDirectory, PEB, OSC9, OSC7 };
```

### Shell-Specific CWD Compatibility Matrix

| Shell × Platform | OSC 7 native? | OSC 9;9 native? | PEB fallback? | Best method |
|-------|:---:|:---:|:---:|---|
| PowerShell 7+ | Via prompt func | Via built-in shell integration | ✅ | OSC 9;9 (existing) |
| PowerShell 5.1 | Via prompt func | Via prompt func | ✅ | OSC 9;9 with script |
| cmd.exe | ❌ | Via `PROMPT $e]9;9;$P$e\` | ✅ | **PEB fallback** (zero-config) |
| bash (Git Bash / MSYS2) | ✅ Native | Manual | ✅ (native Windows process) | OSC 7 |
| bash (WSL Ubuntu) | ✅ Native (Ubuntu default) | Manual | ❌ (PEB reads `wsl.exe`, not bash) | **OSC 7** (requires adding support) |
| zsh (WSL) | ✅ Via `chpwd` hook | Manual | ❌ (same WSL limitation) | OSC 7 |
| fish (WSL) | ✅ Automatic | Manual | ❌ (same WSL limitation) | OSC 7 |
| SSH session | Depends on remote shell | Depends on remote shell | ❌ (reads `ssh.exe` CWD) | OSC 7 from remote shell |

> **Note on WSL:** The default WSL distro is Ubuntu, which runs bash. Users may install other distros (Debian, Fedora) or shells (zsh, fish) inside WSL. The key limitation is platform-level: PEB fallback always fails for WSL because Terminal can only read the Windows-side `wsl.exe` process memory, not the Linux process inside the VM. OSC 7 is the only reliable CWD mechanism for any shell running inside WSL.

---

## Issue 2: Shell Identification Per Pane

### Current State

Terminal has **no runtime shell identification mechanism**. The only shell awareness:

1. **Profile `Commandline`** — the executable path configured in the profile (e.g., `powershell.exe`, `cmd.exe`, `wsl.exe`)
2. **`ConsoleShimPolicy`** — detects 3 shells (`cmd.exe`, `powershell.exe`, `pwsh.exe`) by process image name for compatibility shims
3. **`ConptyConnection._clientName`** — stores the spawned process image name

All detection is **launch-time only**. If the user types `bash` inside PowerShell, Terminal still thinks it's PowerShell. There is no VT standard for shell self-identification.

### Problem

1. Agent doesn't know which shell is running → can't tailor commands (`dir` vs `ls`, `$LASTEXITCODE` vs `$?`)
2. Shell changes mid-session are invisible
3. No capability advertisement → agent doesn't know if shell supports FTCS, completions, etc.
4. VS Code solves this with custom `OSC 633` scripts but Windows Terminal has no equivalent

### Recommended Solution: Profile Detection + OSC 9001 Self-Report

#### Phase 1: ShellType from Profile Commandline

Parse the profile's `commandline` at creation time. Map to enum:

```cpp
enum class ShellType { Unknown, Cmd, PowerShell5, Pwsh, Bash, Zsh, Fish, Nushell, Wsl, Python };
```

`Profile::NormalizeCommandLine()` at `Profile.cpp:385-470` already canonicalizes command lines. Add a companion `DetermineShellType()` that matches the executable name.

**Validated effort: ~20 lines. Risk: VERY LOW.**

Expose via `ICoreState.idl`:
```idl
enum ShellType { Unknown, Cmd, PowerShell, Pwsh, Bash, Zsh, Fish, Nushell, Wsl };
ShellType ShellType { get; };
```

#### Phase 2: OSC 9001;ShellType Self-Report

Using Terminal's own `OSC 9001` namespace (already handles `CmdNotFound` at `adaptDispatch.cpp:3800-3823`):

```
ESC ] 9001 ; ShellType ; pwsh ; CapFTCS,CapCompletions,CapOSC7 ST
```

Shell integration scripts emit this on startup. The handler in `DoWTAction()`:
```cpp
else if (action == L"ShellType") {
    auto shellName = parts.size() >= 2 ? til::at(parts, 1) : L"unknown";
    auto capabilities = parts.size() >= 3 ? til::at(parts, 2) : L"";
    _api.SetShellType(shellName, capabilities);
}
```

This overrides the profile-based guess with a definitive answer and declares capabilities.

#### NOT Recommended for v1: GetConsoleProcessList Polling

Process tree walking for leaf-process detection (R2's proposal P2B) is **medium-high effort** with fragility concerns:
- Processes can exit between enumeration and inspection
- Python REPL, node.js, etc. appear as "leaf" but aren't shell changes
- `GetConsoleProcessList` exists in the console host server side but is NOT exposed to the Terminal app layer

**Defer to Phase 3** where it can complement OSC 9001 for non-cooperating shells.

---

## Issue 3: Error/Command Status Detection

### Current State

**OSC 133;D with exit codes is fully implemented internally.** The complete pipeline:

```
Shell emits: ESC]133;D;1 ST
  → adaptDispatch.cpp:3688 (DoFinalTermAction, case 'D')
  → Parses exit code from string
  → textBuffer.cpp:3501 (EndCurrentCommand(error=1))
  → Row.cpp:1271 (EndOutput sets exitCode=1, category=Error)
  → ScrollbarData stored on the prompt row
```

Exit codes stored in `ScrollbarData.exitCode` (`Marks.hpp:44`). Category mapped: 0 = `Success`, non-0 = `Error`.

**THE GAP:** This data is **not exposed through WinRT**. The WinRT `ScrollMark` struct (`ICoreState.idl:14-19`) carries only `Row` and `Color` — the exit code is stripped during projection in `ControlCore::ScrollMarks()`.

**No `CommandCompleted` event exists.** The mark is stored but no notification fires. Agents must poll or read the entire buffer to discover command completion.

### Problem

1. Exit codes exist internally but agents can't access them → agent must scan output text for error patterns (~1,300-5,300 tokens/query)
2. No command completion event → agents must poll constantly → burns ~200 tokens/poll
3. No command duration tracking → can't detect stuck commands
4. cmd.exe has NO mechanism to emit exit codes (no post-command hook)

### Token-Inefficient Approaches (REJECTED)

**Color-based error detection:** 15-25% false positive rate. `git diff` uses red for deletions (not errors), PowerShell uses red for `Write-Error` (correctly), themes may use red decoratively. Each false positive triggers an unnecessary agent response costing ~300 tokens. At 20% FP rate with 50 triggers/session: 10 false × 300 = **3,000 tokens wasted**. **Net token-negative.**

**Text pattern heuristics** ("error:", "FAILED", "Exception"): Lower false positive rate (~10%) but still unreliable. Compiler warnings contain "error", help text contains "Error", log messages contain error counts. **Defer to Phase 3 as a low-confidence fallback only**, never as primary signal.

### Recommended Solution: Expose Existing Data + Events

#### Phase 1: Expose Exit Codes in WinRT ScrollMark API

**This is the highest-ROI change in the entire research.** The data exists — just add it to the IDL:

```idl
// ICoreState.idl — update ScrollMark struct:
struct ScrollMark {
    Int32 Row;
    Microsoft.Terminal.Core.OptionalColor Color;
    MarkCategory Category;                          // NEW — enum already exists at idl:6-12
    Windows.Foundation.IReference<UInt32> ExitCode;  // NEW
};
```

Update `ControlCore::ScrollMarks()` at `ControlCore.cpp:2553-2568` to populate the new fields from `ScrollbarData`.

**Validated effort: ~6-8 lines. Risk: VERY LOW.** Pure additive WinRT surface expansion.

#### Phase 1: CommandCompleted Event

Fire a new event from `EndCurrentCommand()`:

```
EndCurrentCommand() in textBuffer.cpp:3501
  → New callback: _pfnCommandCompleted(exitCode, commandText)
  → Bubbles through Terminal → ControlCore → TermControl
  → Agent subscribes to CommandCompleted event
```

**Validated effort: ~30-40 lines across ~8 files.** Follows pattern of existing events (TitleChanged, OutputIdle).

**Critical codebase note:** The comment at `Marks.hpp:45-46` says _"Future consideration: stick the literal command as a string on here, if we were given it with the 633;E sequence."_ — the team is already planning to enrich mark data. Align with this intent.

#### Phase 2: Command Duration Tracking

Add timestamps to mark lifecycle:

```cpp
// ScrollbarData — add:
std::optional<std::chrono::steady_clock::time_point> commandStartTime;
std::optional<std::chrono::milliseconds> commandDuration;

// In StartOutput(): record commandStartTime = now()
// In EndCurrentCommand(): duration = now() - commandStartTime
```

Enables "that command took 45 seconds" and stuck-command detection.

### Error Detection Confidence Model

The agent should use a tiered confidence model internally (never expose scores to users):

| Tier | Confidence | Signal Source | Agent Behavior |
|------|-----------|---------------|----------------|
| **Certain** | 95-100% | OSC 133;D exit code ≠ 0 | Act decisively: "That command failed." |
| **High** | 75-94% | Exit code + error text patterns | Act decisively with detail |
| **Medium** | 50-74% | autoMarkPrompts + text heuristics | "It looks like there might have been an issue." |
| **Low** | 25-49% | Text patterns only, no exit code | Don't mention proactively. Respond if asked. |
| **Blind** | 0-24% | TUI/SSH/no marks | Treat as raw text. Never guess. |

### Shell-Specific Error Detection Matrix

| Shell | Exit code via OSC 133;D | Exit code via API | autoMarkPrompts | Best error detection |
|-------|:---:|:---:|:---:|---|
| PowerShell 7+ | ✅ (with shell integration) | ❌ | ✅ (default ON) | OSC 133;D |
| PowerShell 5.1 | ✅ (with script) | ❌ | ✅ | OSC 133;D |
| cmd.exe | ❌ **Impossible** (no post-command hook) | ❌ | ✅ | **Blind** — cannot detect errors without shell cooperation |
| bash | ✅ (with script, `$?` in hook) | ❌ | ✅ | OSC 133;D |
| zsh | ✅ (with script, `$?` in precmd) | ❌ | ✅ | OSC 133;D |
| fish | ✅ (with script, `$status`) | ❌ | ✅ | OSC 133;D |
| SSH | Depends on remote | ❌ | ✅ | Depends on remote shell config |

---

## Issue 4: Better Terminal–Shell Integration for ACP Agents

### Current State

There is **no dedicated agent integration layer** in the codebase. An ACP agent has access to:

| API | What it provides | Limitation |
|-----|-----------------|------------|
| `ICoreState.WorkingDirectory` | CWD string | No change event, no source provenance |
| `ControlCore.CommandHistory()` | Command strings as `CommandHistoryContext` | No exit codes, no output, no CWD per command |
| `ControlCore.ReadEntireBuffer()` | Full buffer text | Expensive (write lock, full scan, ~2K-10K tokens) |
| `ControlCore.ScrollMarks` | Mark positions + colors | **No exit codes** despite being stored internally |
| `ControlCore.OutputIdle` event | 100ms debounce after output stops | Too generic — doesn't distinguish command completion from output pause |
| `ControlCore.SendInput()` | Inject keystrokes | Works for agent→shell commands |
| `SuggestionsControl` | UI for agent recommendations | Ready to use |

**Infrastructure maturity:**

| Capability | Maturity | Key Gap |
|-----------|:--------:|---------|
| CWD tracking | 🟡 | No change event; no OSC 7; no per-command history |
| Shell/process tracking | 🔴 | Root process only; no live detection; no shell type |
| Mark/error system | 🟢 internally, 🔴 externally | Rich data not projected through WinRT |
| Agent integration | 🔴 | No agent interfaces, context aggregator, or registration |
| Input injection | 🟢 | `SendInput()` and `PasteText()` work today |
| Suggestions UI | 🟢 | `SuggestionsControl` ready |

### Recommended Architecture: Event-Driven PaneContext

#### Design Principles

1. **Event-driven, not polling** — every signal the agent needs should have a change event
2. **Structured, not text** — expose typed properties, not buffer text
3. **Source-annotated** — every value carries its provenance (OSC7, PEB, profile, etc.)
4. **Tiered richness** — cheap context packet (~120 tokens) by default, expensive data (buffer text) on-demand only
5. **Graceful degradation** — works at reduced capability for unconfigured shells

#### Ideal Agent Context Packet (~120 tokens)

```json
{
  "pane": {
    "shellType": "pwsh",
    "shellTypeSource": "osc9001",
    "cwd": "D:\\projects\\myapp",
    "cwdSource": "osc7",
    "connectionState": "connected",
    "integrationLevel": 4
  },
  "lastCommand": {
    "text": "npm test",
    "exitCode": 1,
    "durationMs": 3400
  },
  "recentCommands": [
    { "text": "cd src", "exitCode": 0 },
    { "text": "git pull", "exitCode": 0 }
  ],
  "currentInput": "npm run b"
}
```

Compare to `ReadEntireBuffer()`: 2,000–10,000+ tokens for the same information.

#### Integration Levels (Auto-Detected Per Session)

```
Level 0: Nothing (SSH into raw telnet)
Level 1: OS-level only (PEB CWD + process list + autoMarkPrompts)
Level 2: Minimal OSC (9;9 CWD notification)
Level 3: FTCS prompt marks (133;A/B) — no exit codes
Level 4: Full integration (133;A/B/C/D + exit codes + CWD + shell type)
```

The level only goes UP within a session (once we see `133;D`, we know Level 4 forever). The agent adapts behavior based on detected level.

#### On-Demand Extension (Only When Needed)

Output text is the most expensive field (~500-5000 tokens). Only fetch reactively:

```json
// Only requested when exitCode != 0:
{
  "lastCommandOutput": {
    "text": "FAIL src/App.test.js\n  ● renders correctly\n    Expected: 1\n    Received: 2",
    "truncated": true,
    "totalLines": 47,
    "shownLines": 10
  }
}
```

**Key design rule:** Never include output text proactively. Include last N lines only, with a `truncated` flag.

### Shell Integration Scripts

**Ship scripts for: PowerShell 7+, PowerShell 5.1, bash, zsh, fish.**
**Manual opt-in for v1** (auto-injection deferred due to blockers from V2 review).

All scripts follow this template:
```
# Guard against double-injection
if WT_SHELL_INTEGRATION is set: return
export WT_SHELL_INTEGRATION=1

# Emit shell self-identification
emit OSC 9001;ShellType;{name};{capabilities}

# Hook prompt: emit FTCS A → prompt → FTCS B
# Hook pre-exec: emit FTCS C
# Hook post-exec: emit FTCS D;$?
# Hook CWD change: emit OSC 7 (or OSC 9;9 for PowerShell)
```

**Auto-injection blockers (V2 review):**
1. 🔴 `bash --init-file` **replaces** `.bashrc`, not appends → destroys user config
2. 🔴 `ZDOTDIR` override **replaces** `.zshrc` → destroys user config
3. 🔴 PowerShell 5.1 scripts using PS7-only `` `e `` escape syntax → must use `[char]0x1b`
4. 🔴 No conflict detection for existing shell integration (e.g., oh-my-posh already emitting FTCS)

These must be resolved before enabling auto-injection (Phase 2).

---

## Unified Architecture

```
┌────────────────────────────────────────────────────────────────┐
│                  EXTERNAL CONSUMERS                            │
│       ACP Agents  │  MCP Clients  │  Extensions               │
└────────┬──────────┴───────┬───────┴────────────────────────────┘
         │ WinRT API        │ MCP/JSON (v3)
┌────────▼──────────────────▼────────────────────────────────────┐
│                 INTEGRATION LAYER                              │
│  ┌──────────────┐  ┌──────────────┐                            │
│  │ PaneContext   │  │  MCP Server  │                            │
│  │ (WinRT, v2)   │  │  (v3)        │                            │
│  └──────┬───────┘  └──────┬───────┘                            │
│         └─────────────────┘                                    │
│                   │                                            │
│         ┌─────────▼────────┐                                   │
│         │ TerminalState    │ ← Reads from all sources          │
│         │ Aggregator       │                                   │
│         └─────────┬────────┘                                   │
└───────────────────┼────────────────────────────────────────────┘
                    │
┌───────────────────▼────────────────────────────────────────────┐
│                 CONTROL LAYER (ControlCore)                     │
│                                                                │
│  Properties:                    Events:                        │
│  - WorkingDirectory             - TitleChanged (exists)        │
│  - ShellType (NEW)              - OutputIdle (exists)          │
│  - CommandHistory (exists)      - ConnectionStateChanged       │
│  - CwdSource (NEW)             - CommandCompleted (NEW)        │
│                                 - WorkingDirectoryChanged (NEW)│
└───────────────────┬────────────────────────────────────────────┘
                    │
┌───────────────────▼────────────────────────────────────────────┐
│                  CORE LAYER (Terminal + TextBuffer)             │
│                                                                │
│  Terminal:                    TextBuffer / ScrollbarData:       │
│  - _workingDirectory          - category (exists)              │
│  - _cwdSource (NEW)           - exitCode (exists)              │
│  - _shellType (NEW)           - color (exists)                 │
│  - _shellCapabilities (NEW)   - cwd snapshot (NEW)             │
│                               - timestamp (NEW, v2)            │
└───────────────────┬────────────────────────────────────────────┘
                    │
┌───────────────────▼────────────────────────────────────────────┐
│                 VT PARSER LAYER                                │
│                                                                │
│  OSC 7   → SetWorkingDirectory(uri, Source::OSC7)     [NEW]    │
│  OSC 9;9 → SetWorkingDirectory(path, Source::OSC9)    [EXISTS] │
│  OSC 133 → FinalTerm marks A/B/C/D                   [EXISTS] │
│  OSC 9001;ShellType → SetShellType(name, caps)       [NEW]    │
│  OSC 9001;CmdNotFound → SearchMissingCommand         [EXISTS] │
└───────────────────┬────────────────────────────────────────────┘
                    │
┌───────────────────▼────────────────────────────────────────────┐
│              SHELL INTEGRATION SCRIPTS (bundled)               │
│                                                                │
│  PowerShell: FTCS A/B/C/D + OSC 9;9 + OSC 9001;ShellType     │
│  Bash/Zsh:   FTCS A/B/C/D + OSC 7   + OSC 9001;ShellType     │
│  Fish:       FTCS A/B/C/D + OSC 7   + OSC 9001;ShellType     │
│  Cmd.exe:    PEB fallback only (no integration possible)       │
└────────────────────────────────────────────────────────────────┘
```

### CWD Source Resolution (When Multiple Sources Disagree)

```
Priority: OSC 7 > OSC 9;9 > PEB > Starting Directory
           ↑ URL-encoded   ↑ Raw path  ↑ May be    ↑ Config
             + hostname      ConEmu      stale       only
```

If both OSC 7 and PEB report CWD, OSC 7 wins (it's from the shell itself, not a process memory read). PEB is a lazy fallback polled every ~2 seconds, but only when no OSC CWD has been set recently.

---

## Implementation Phases

### Phase 1 — Foundation (~80 lines, high value)

All items are parallelizable with no dependencies:

| # | Change | Effort | Files |
|---|--------|--------|-------|
| 1 | Add OSC 7 parsing | ~40-60 lines | OutputStateMachineEngine.hpp/cpp, adaptDispatch.hpp/cpp, ITermDispatch.hpp |
| 2 | ShellType from profile commandline | ~20 lines | Profile.h/cpp, ICoreState.idl |
| 3 | Expose exit codes in WinRT ScrollMark | ~8 lines | ICoreState.idl, ControlCore.cpp |
| 4 | `WorkingDirectoryChanged` event | ~10 lines | Terminal.hpp, TerminalApi.cpp, ControlCore.idl/cpp |
| 5 | Per-prompt CWD snapshot | ~10 lines | Marks.hpp, textBuffer.cpp |
| 6 | Ship shell integration scripts (manual) | New files | res/ or resources |

**Outcome:** Agent gets structured CWD, shell type, exit codes, and change events. Eliminates ~90% of buffer-scanning guesswork.

### Phase 2 — Intelligence (~300 lines)

Depends on Phase 1 scripts being available:

| # | Change | Effort | Depends On |
|---|--------|--------|------------|
| 7 | `CommandCompleted` event | ~40 lines | Phase 1 #4 pattern |
| 8 | Command duration tracking | ~30 lines | Phase 1 #5 |
| 9 | OSC 9001;ShellType handler | ~30 lines | Phase 1 #2, #6 |
| 10 | PaneContext aggregator (WinRT) | ~100 lines | All Phase 1 |
| 11 | Auto-injection logic | ~100 lines | Phase 1 #6 + blocker resolution |
| 12 | PEB CWD reader (fallback) | ~20 lines | Phase 1 #4 |

### Phase 3 — Ecosystem (complex features)

| # | Change | Effort | Notes |
|---|--------|--------|-------|
| 13 | MCP server | Very High | Separate process, JSON-RPC, reads PaneContext |
| 14 | Leaf process tracker | Medium-High | GetConsoleProcessList + image name |
| 15 | Rich CommandRecord API | High | Extends MarkExtents with full structured data |
| 16 | Text heuristics (low-confidence fallback) | Medium | Regex + confidence scoring, fallback only |
| 17 | Shell capability negotiation | Low | Extension to OSC 9001 response |
| 18 | OSC 9100 bidirectional agent protocol | High | Novel — no industry precedent |

---

## Degradation Matrix

What the agent knows at each integration level, per shell:

| Shell × Level | CWD | Shell Type | Exit Codes | Command Text |
|---|---|---|---|---|
| **cmd.exe, no integration** | PEB fallback | From profile | ❌ Impossible | Buffer scraping (~70% accurate) |
| **cmd.exe, autoMarkPrompts** | PEB fallback | From profile | ❌ Impossible | Better scraping (prompt boundaries) |
| **PowerShell, no integration** | PEB fallback | From profile | ❌ | Buffer scraping |
| **PowerShell, full scripts** | OSC 9;9 ✅ | OSC 9001 ✅ | OSC 133;D ✅ | MarkExtents ✅ |
| **bash/zsh (WSL Ubuntu), no integration** | ❌ (PEB reads `wsl.exe`, not Linux shell) | From profile ("wsl") | ❌ | Buffer scraping |
| **bash/zsh (WSL Ubuntu), full scripts** | OSC 7 ✅ | OSC 9001 ✅ | OSC 133;D ✅ | MarkExtents ✅ |
| **bash (Git Bash), no integration** | PEB fallback ✅ (native process) | From profile | ❌ | Buffer scraping |
| **bash (Git Bash), full scripts** | OSC 7 ✅ | OSC 9001 ✅ | OSC 133;D ✅ | MarkExtents ✅ |
| **SSH session** | ❌ (reads ssh.exe CWD) | "ssh" from profile | ❌ | Buffer scraping |
| **TUI app (vim, htop)** | PEB of TUI process | TUI name from leaf process | ❌ (alt buffer) | ❌ (TUI owns buffer) |

**Key insight (V4 review):** The degraded baseline is better than expected because `autoMarkPrompts` defaults to ON (`MTSMSettings.h:101`), providing prompt boundaries for ALL shells. The critical gap is **exit codes** — the single biggest differentiator between degraded and full integration, and cmd.exe may never provide them.

---

## Appendix A: Competitive Landscape

| Terminal | CWD Detection | Shell ID | Error Detection | AI Integration |
|---|---|---|---|---|
| **VS Code** | OSC 633 + shell scripts | Scripts self-identify | Exit codes + marks | Copilot in Terminal |
| **WezTerm** | OSC 7 + auto-inject | Process detection | FTCS marks | None |
| **iTerm2** | OSC 7 + OSC 1337 | Shell integration profiles | FTCS marks + alerts | AI features |
| **Ghostty** | OSC 7 + auto-inject | Process + auto-inject | FTCS marks | None |
| **Kitty** | OSC 7 + protocol extensions | Process detection | FTCS marks | None |
| **Warp** | Owns the shell | N/A (is the shell) | Owns exit codes | Built-in AI |
| **Windows Terminal** | OSC 9;9 only | Profile commandline only | FTCS internal only | ACP (proposed) |

**Industry verdict (R4 review):** Shell cooperation is not optional — every successful terminal has invested in shell integration scripts. The best (Ghostty, VS Code) auto-inject transparently. No terminal has solved error detection without shell cooperation. Terminal-native AI integration is a greenfield opportunity — no standard protocol exists yet.

---

## Appendix B: Key Codebase References

| Component | File | Lines | Notes |
|---|---|---|---|
| OSC dispatch | `src/terminal/parser/OutputStateMachineEngine.cpp` | 757-905 | Routes OSC to handlers |
| OSC 9;9 CWD handler | `src/terminal/adapter/adaptDispatch.cpp` | 3525-3610 | ConEmu CWD parsing |
| OSC 133 FTCS handler | `src/terminal/adapter/adaptDispatch.cpp` | 3651-3720 | FinalTerm marks A/B/C/D |
| OSC 9001 WT handler | `src/terminal/adapter/adaptDispatch.cpp` | 3800-3823 | CmdNotFound, extensible |
| CWD storage | `src/cascadia/TerminalCore/TerminalApi.cpp` | 206-224 | `SetWorkingDirectory()` |
| Mark storage | `src/buffer/out/Marks.hpp` | 21-83 | `ScrollbarData`, `MarkExtents` |
| Exit code storage | `src/buffer/out/Row.cpp` | 1250-1280 | `EndOutput()` sets exitCode |
| WinRT ScrollMark | `src/cascadia/TerminalControl/ICoreState.idl` | 14-19 | **Missing exit code!** |
| PEB reading pattern | `src/cascadia/TerminalConnection/ConptyConnection.cpp` | 707-735 | CommandLine from PEB |
| autoMarkPrompts | `src/cascadia/TerminalCore/Terminal.cpp` | 725-754 | Heuristic marks on Enter |
| Command history | `src/cascadia/TerminalControl/ControlCore.cpp` | 2375-2420 | `CommandHistoryContext` |
| Profile normalization | `src/cascadia/TerminalSettingsModel/Profile.cpp` | 385-470 | `NormalizeCommandLine()` |

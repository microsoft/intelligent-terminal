# WSL Agent Session Management (Historical MVP)

## Abstract

Intelligent Terminal (IT) surfaces a list of agent-CLI sessions (Copilot /
Claude / Codex / Gemini) in the `/sessions` view. Today every producer of that
list — the on-disk history scanner (`history_loader::load_all`) and the live
file/process watcher (`session_watcher`) — looks **only under the Windows user
profile** (`%USERPROFILE%\.copilot`, `.claude`, `.codex`, `.gemini`). A session
the user ran with the **Linux-native** CLI **inside a WSL distro** writes its
transcript to the distro's ext4 filesystem (`/home/<user>/.copilot/...`) and is
therefore completely invisible to IT.

This spec adds **historical** visibility and **resume** for those in-distro
sessions. It is deliberately an MVP:

> **List WSL sessions in `/sessions`, and resume a selected one back into its
> distro. No live status, no Attention/permission surfacing, no hooks inside
> WSL.**

The change is almost entirely a `wta` (Rust) addition. The C++ side is
unchanged except (later, deferred) a settings toggle; the MVP ships a `wta`-side
gate + environment kill-switch instead.

## Background: Class A / Class B, and what "WSL session" means here

IT classifies every session by `SessionOrigin` (`agent_sessions.rs:137`):

- **Class A — `AgentPane`**: an ACP session IT created for an agent pane. The
  agent CLI runs on the **Windows host** (spawned by `wta-master`); only its
  `cwd` may point into WSL (`\\wsl$\...`). `cwd_util.rs` already tolerates WSL
  UNC cwds. These are **not** the subject of this spec.

- **Class B — `Unknown`**: the user ran the CLI themselves in a normal shell
  pane. When that shell pane is a **WSL pane** and the user runs the distro's
  own Linux `copilot` / `claude` / `gemini` / `codex`, the transcript lands on
  **distro ext4** under the Linux `$HOME`. **This is the gap this spec fills.**

So "WSL session" throughout = a **Class B, in-distro** session whose on-disk
artefacts live inside a WSL distro, not on the Windows host.

## Scope

In scope (MVP):

1. Enumerate **running** WSL distros and scan each distro's Linux `$HOME` for
   the same four CLI layouts the host scanner already understands.
2. Merge the discovered rows into `/sessions`, tagged with their distro so the
   user can tell host rows from WSL rows.
3. **Resume** a selected WSL row back into that distro (new tab running
   `wsl -d <distro> -- <cli> --resume <key>`).

Explicitly **out of scope** (accepted MVP limitations, see "Known
limitations"):

- No live status for WSL rows (no Working / Idle / Attention; no watcher).
- No hooks installed inside WSL; no permission/Attention surfacing.
- **Stopped** distros are not scanned (we never auto-boot a VM).
- Only the distro's default-login `$HOME` is scanned (multi-Linux-user deferred).
- Shift+Enter "resume-in-agent-pane" (ACP `session/load`) is **not** offered for
  WSL rows (the helper/agent run on the host; loading a Linux session into a
  host agent pane is wrong). Shift+Enter == Enter for WSL rows.

## Read mechanism: Hybrid (in-distro fetch), running distros only

### Why not the UNC path (`\\wsl$\<distro>\...`)

WSL exposes distro ext4 to Windows over a **9P** file server bridged across the
WSL2 utility VM. Every `open`/`stat`/`readdir`/`read` is a round-trip across the
VM boundary (~9 ms each on the dev machine), and the existing scanner reads
**every** session's full transcript (for title + phantom classification) before
it sorts and caps to 50. That is hundreds–thousands of 9P round-trips per scan.

Measured on the dev machine (200 sessions / 400 small files, WSL2 Ubuntu):

| Approach | Cold | Warm |
|---|---|---|
| UNC read, all files, from Windows | 7.3 s | 3.6 s |
| In-distro read (one `wsl` spawn) | 3.6 s | ~1.1 s |
| First UNC touch of a **stopped** distro (auto-boots VM) | +5.7 s | — |

Two conclusions drive the design: **(a)** fetch bytes *inside* the distro
(local ext4) and stream them out in one spawn, rather than reading file-by-file
over 9P; **(b)** only ever touch **running** distros — the first access to a
stopped distro silently boots its VM and stalls for seconds (GH#9541).

### The hybrid: distro fetches bytes, host keeps all parsing logic

The existing per-CLI parsers (`load_copilot/claude/gemini/codex`,
title/phantom/mtime logic) are **unchanged**. We only change *where the bytes
come from*. Per running distro, a **single** `wsl -d <distro> -- bash` spawn runs
one pipeline that (1) ranks sessions cheaply by mtime and (2) streams exactly the
needed files out as a `tar`:

1. **Rank (cheap, no file reads):** `find` the four CLI roots under `$HOME`
   emitting `mtime \t path`, `sort -rn`, `head` to the per-CLI cap
   (`MAX_PER_CLI`, today 50). This selects the newest N sessions per CLI without
   reading any transcript.
2. **Bundle (one stream):** `tar -cf -` the files those top-N sessions need
   (Copilot: the session **directory** — `events.jsonl` + `workspace.yaml`;
   Claude/Gemini/Codex: the session **`.jsonl`**), relative to `$HOME`, to
   stdout. `tar` is a standard Linux tool; `-c` create, `-f -` write the archive
   to stdout (so the bytes flow back through the `wsl.exe` pipe), paths kept
   relative via `-C $HOME` so the extracted tree mirrors a real `$HOME`.

On the Windows side, `wta` extracts that stream (Rust `tar` crate) into a
**temporary home directory** that mirrors the distro layout
(`<tmp>/.copilot/session-state/...`, `<tmp>/.claude/projects/...`, …), then calls
the **existing** `load_copilot(<tmp>)` / `load_claude(<tmp>)` / … verbatim and
stamps `location = Wsl { distro }` on each returned row.

- **Zero parser changes:** because the parsers take a `home: &Path`, pointing
  them at the temp dir reuses *all* tested title/phantom/sort/cap logic.
- **mtime fidelity:** Gemini and Claude derive `last_activity` from **file
  mtime**, so the extraction MUST preserve mtimes. GNU `tar` records them and the
  Rust `tar` crate restores them on `unpack` by default — verify in tests.
- **Time-boxed:** the spawn is bounded by a timeout so a wedged distro/9P can
  never hang the scan; on timeout the distro contributes nothing (logged).

### Distro enumeration

A small `wsl` module exposes `running_distros() -> Vec<String>`, parsing
`wsl.exe -l --running -q`. Notes verified on the dev machine:

- Output is **UTF-16LE** with embedded NULs and a trailing `*` marker on the
  default distro — must be decoded/trimmed (mirror the existing pattern used for
  `wsl -l` parsing elsewhere if any; otherwise decode UTF-16LE explicitly).
- No WSL installed → the enumeration spawn fails fast (no `wsl.exe`) → empty
  list. WSL installed but **nothing running** → one cheap `wsl -l --running -q`
  spawn returns empty → **no per-distro fetch spawns, no rows**. The only cost on
  a machine with no running distro is that single enumeration spawn (and it is
  itself behind the gate in "Gating", so a disabled feature pays nothing).

## Data model

Add a `location` dimension so a row carries *where* it lives:

```rust
// agent_sessions.rs
pub enum SessionLocation {
    Host,                      // default — Windows %USERPROFILE%
    Wsl { distro: String },    // in-distro ext4 under the distro's $HOME
}
```

- New field `AgentSession::location` (defaults to `Host`; every existing
  construction site stays valid via `..Default::default()` or an explicit
  `Host`).
- `discover::Discovered` likewise gains an optional location (used only if/when
  the watcher is extended later; for the historical MVP only `load_all` stamps
  it).
- **Identity / dedup is `(location, key)`-keyed.** A host session and a WSL
  session can in principle share a UUID; they must not collide in the registry.
  Audit `AgentSessionRegistry` insert/dedup and the agent-pane-origin join to key
  on `(location, key)` (host-only callers keep `Host` and are unaffected).

## Display (the "where did this come from" prefix)

The `/sessions` renderer already reserves a **row prefix slot** between the
selection caret and the title (`agents_view.rs:498 origin_prefix_for`, budgeted
through `prefix_w`). WSL rows reuse exactly this slot:

- `origin_prefix_for` returns a short distro tag for `Wsl` rows, e.g.
  `Ubuntu ` (distro name + space). Host rows are unchanged.
- The existing `prefix_w` width accounting already shrinks the title cap to fit
  the prefix, so no new layout math is needed.
- Tag **style** (plain `Ubuntu ` vs bracketed `[Ubuntu] `) — pick plain to match
  the existing un-bracketed agent-pane id prefix; final wording is a 1-line
  change.

This satisfies the "session title should say where it came from" requirement
without a second column.

## Resume (back into the distro)

Resume routing already exists: `decide_enter_action` (`session_mgmt.rs`) →
`dispatch_resume` (`app.rs:3040`). The only changes are in `dispatch_resume`:

1. **Build a WSL command line** when `s.location` is `Wsl { distro }`:

   ```
   cmd /c echo <banner> && wsl -d <distro> [--cd <linux-cwd>] -- <cli> <resume_flag> <key>
   ```

   - `<cli> <resume_flag> <key>` is the **same** trio the host path builds
     (`format!("{} {} {}", cli_id, profile.resume_flag, key)`); it just runs
     **inside** the distro, so it invokes the distro's Linux CLI that actually
     owns the conversation.
   - `wsl.exe` is on the Windows PATH, so the existing `cmd /c echo banner && …`
     wrapper (loading banner, issue #135) and the `wtcli new-tab -c …` launch are
     reused unchanged.
   - `--cd <linux-cwd>` sets the **Linux** working directory so the CLI's
     cwd-keyed session store resolves (Claude/Copilot key their store by cwd).
     Use the row's `cwd` when it is a Linux path (Claude path-decoded; Copilot
     `workspace.yaml cwd:`); omit `--cd` when absent and let WSL default to the
     distro `$HOME`. The Windows `-d <cwd>` arg to `wtcli new-tab` is **not**
     used for WSL rows (the Linux cwd is set via `--cd`, not the Windows starting
     directory).

2. **Skip the host-disk phantom guard for WSL rows.** `dispatch_resume` calls
   `history_loader::key_is_resumable_on_disk(&s.cli_source, &s.key)`, which
   probes the **host** `~/.copilot|.claude|...`. For a WSL key that path doesn't
   exist, so the guard would wrongly prune the row. For `Wsl` rows, bypass this
   guard and defer to the Linux CLI's own `--resume` validation (the same lenient
   stance already taken for unknown CLIs). A WSL-aware on-disk probe is a
   possible future hardening (it would need another in-distro spawn) — out of
   scope for MVP.

3. **Shift+Enter == Enter for WSL rows.** The `ResumeInAgentPane` branch
   (ACP `session/load`) is suppressed for `Wsl` location (helper/agent are
   host-side); both Enter and Shift+Enter route to the CLI-flag resume above.

## Gating: `wta`-side chokepoint + env kill-switch (real setting deferred)

Following the repo's own MVP-gate convention
(`app.rs:62-78` — `MVP_SESSIONS_ORIGIN_FILTER` const + `WTA_SESSIONS_SHOW_AGENT_PANE`
env override), the WSL scan is gated at a **single chokepoint** in `load_all`:

- A function `wsl_sessions_enabled() -> bool` reads `WTA_WSL_SESSIONS`
  (`0`/`false` disables) and otherwise returns the build default (**enabled**).
- `load_all` only enumerates/scans distros when `wsl_sessions_enabled()` is true.

Rationale:

- **Gate at the scan, not the display.** Disabling means WSL rows never enter the
  registry → never rendered → and **no** `wsl.exe` spawn cost is paid. (Trade-off:
  re-enabling needs a re-scan to repopulate — acceptable for a kill-switch.)
- **Immediate safety, zero C++/settings-model/XAML work.** If WSL scanning
  misbehaves on a user's machine (slow distro, odd `wsl` build), they can set
  `WTA_WSL_SESSIONS=0` without a new build.
- **Future-proofed wiring.** When a real `wslSessions` setting is added later
  (`MTSMSettings.h` X-macro → `GlobalAppSettings` → C++ passes a `--wsl-sessions`
  flag to `wta`, same as `acpAgent`/`language`), it overrides the env default at
  the **same** chokepoint — a one-line change, no refactor. The settings UI
  toggle and the scan gate are the **same** boolean; no separate display filter
  is needed.

This is a deferred follow-up, not MVP work; the spec only commits to the
chokepoint + env override now.

## Where this plugs into the existing pipeline

`history_loader::load_all` is called from **both** the helper TUI process
(`app.rs:3741`, inside a `spawn_blocking`) and `wta-master`
(`master/mod.rs:1628`). The WSL branch lives **inside** `load_all`, so both
callers get WSL rows for free, and both already run the scan off the UI/critical
path. The per-distro spawn + timeout must stay on the blocking pool (never the
async reactor / UI thread).

## Testing

- **`wsl` module:** unit-test `running_distros()` parsing against captured
  UTF-16LE `wsl -l --running -q` bytes (default-marker `*`, NULs, CRLF, empty).
- **Hybrid extraction:** unit-test that a `tar` stream of a synthetic distro
  `$HOME` extracts into a temp dir on which the **existing** parsers produce the
  same rows as a native host scan of the same layout — and that **mtimes are
  preserved** (drives Gemini/Claude `last_activity` + sort order).
- **`location` plumbing:** registry dedup keeps a host `key` and a WSL `key` with
  the same UUID as **distinct** rows.
- **Display:** `origin_prefix_for` emits the distro tag for `Wsl` rows and the
  title-cap budgeting still right-aligns the timestamp (extend existing
  `agents_view` render tests).
- **Resume:** `dispatch_resume` for a `Wsl` row produces the expected
  `wsl -d <distro> [--cd …] -- <cli> <resume_flag> <key>` argv (assert via the
  existing `DispatchedCommand` test seam), and the host-disk phantom guard is
  bypassed for WSL keys.
- **Gate:** `wsl_sessions_enabled()` honors `WTA_WSL_SESSIONS=0/1` and the build
  default (guard env mutation with the existing serialization mutex pattern used
  by the `WTA_SESSIONS_SHOW_AGENT_PANE` tests).
- **No real `wsl.exe` in unit tests** — the spawn boundary is injected/mocked;
  `running_distros()` and the spawn helper take their raw bytes from a seam so
  CI (no WSL) stays deterministic.

## Known limitations (accepted for MVP)

1. **Running distros only.** Stopped distros are skipped to avoid the multi-second
   auto-boot stall; their history reappears only once the user starts the distro.
   A future "start & scan" affordance could opt in explicitly.
2. **No live status / no hooks in WSL.** WSL rows are historical-only. Live
   tracking would need either an in-distro watcher (inotify) or a cross-boundary
   transport for hooks (a distro bash hook can't `CoCreateInstance` the host COM
   server) — both are substantial and deferred.
3. **Single Linux user.** Only the default-login `$HOME` is scanned; a distro with
   multiple Linux users surfaces only one.
4. **Resume phantom safety is the CLI's job for WSL.** The host-disk phantom guard
   is bypassed; a phantom WSL key dead-ends in the Linux CLI's own error rather
   than being pruned pre-launch.

## Future follow-ups (not MVP)

- Real `wslSessions` setting + Settings UI toggle (wired to the existing
  chokepoint).
- WSL live status via an in-distro watcher.
- WSL-aware resumability probe (in-distro) to restore the phantom guard.
- Stopped-distro opt-in scan.
- Multi-Linux-user enumeration.

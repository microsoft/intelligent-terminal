# Durable Sessions — "Keep running in the background" spike (step 5)

*Status: investigation / design spike. No production code in this PR — this
documents the evaluation the [Durable Sessions](https://github.com/shench_microsoft/DFX_specs/blob/main/Intelligent%20Terminal/durable-sessions/durable-sessions.md)
spec explicitly calls for (P1: "keep a session running in the background after
the terminal is closed, tmux-style detach").*

## Goal

Steps 1–4 made shell + agent sessions **restorable** (save on close, replay on
demand) and extended workspaces with durable scrollback. That is *snapshot &
rebuild*: a command in flight when the terminal closes does **not** keep
running.

The remaining, hardest requirement is **keep-running**: a long build started
before closing the terminal is *still running* when the user reopens and
**reattaches to the live session**, rather than restoring a snapshot. This is
the tmux/screen "detach & reattach" model. Per the spec, this needs "a
persistent local session host … that owns the shell processes and that the UI
attaches to and detaches from."

This spike answers the two questions the task posed:

1. Can we just run **tmux** on Windows (and drive it via `tmux -CC` control
   mode, the way iTerm2 does)?
2. If not, is **[psmux](https://github.com/psmux/psmux)** a viable session host?

## Q1 — Native tmux on Windows: **No**

* tmux is a POSIX program (Unix PTYs, Unix-domain sockets, signals). There is
  **no native Windows port**; it only runs under **WSL2 / Cygwin / MSYS2**. A
  WSL-hosted tmux can only own *WSL* processes, not native Windows shells
  (PowerShell/cmd/pwsh), so it can't be the session host for our native panes.
* `tmux -CC` **control mode** (the machine-readable protocol iTerm2 renders
  natively) is only wired up end-to-end by **iTerm2 on macOS**. No Windows
  terminal (Windows Terminal, ConEmu, Alacritty, WezTerm) ships a `-CC` client.
* Conclusion: "just run tmux" forces a WSL dependency and still can't host
  native Windows shells. **Rejected** for the durable-sessions session host.

## Q2 — psmux: **Yes**, and it already speaks control mode

[psmux](https://github.com/psmux/psmux) ("the native Windows tmux, born in
PowerShell, made in Rust", MIT) is a **native Windows terminal multiplexer**
that uses **Windows ConPTY directly** — no WSL/Cygwin/MSYS2 — and implements the
tmux command language and control protocol.

### Verified on a dev machine (already installed via winget `marlocarlo.psmux`, exposed as `tmux.exe`)

| Capability | Probe | Result |
|---|---|---|
| Reports as tmux | `tmux -V` | `tmux 3.3.6` |
| **Detached session survives its creator** | `tmux new-session -d -s spike` then `tmux ls` | `spike: 1 windows (created …)` — server keeps it running |
| **Control mode** (`-CC`) | `tmux -CC` | emits the tmux control protocol: `%begin … %end`, `%window-add @1` |
| Teardown | `tmux kill-server` | clean |

Per its [compatibility matrix](https://github.com/psmux/psmux/blob/master/docs/compatibility.md):
83 tmux commands, reads `~/.tmux.conf`, **session persist (detach/reattach) ✅**,
**control mode `-C`/`-CC` ✅**, native Windows shells ✅, zero dependencies ✅,
server namespaces (`-L`) for isolated instances, MIT license. Requires
Windows 10/11 + PowerShell 7+.

**psmux *is* the "persistent local session host" the spec describes**: a
background server that owns ConPTY shell processes and outlives its client — and
it already exposes the exact `-CC` control transport we'd need to attach/detach.

## How psmux maps onto the durable-sessions design

The spec lists two engineering approaches; psmux collapses them into one that
already exists:

* **Process ownership & lifetime** → the psmux *server* owns the ConPTY child
  processes. Closing the client (detach) does not kill them; `kill-server`
  (or the last session ending) does. This is the tmux-style lifetime the spec
  wants.
* **Output buffering while detached** → the server keeps each pane's screen
  state so a reattach re-renders current contents (verified: a detached session
  keeps its window).
* **Reattach after clean relaunch and after a crash** → `attach`/`-CC` reattach
  by session name against the still-running server.

## Integration options for Intelligent Terminal

### Option A (recommended) — WT as a psmux `-CC` control-mode client

Model on iTerm2's tmux integration, which is well documented and battle-tested:

* When "keep running" is enabled, WT launches shells **inside** a psmux server
  and drives it over `psmux -CC` on a dedicated, per-window server namespace
  (`-L`). WT parses the control protocol (`%output`, `%window-add`,
  `%layout-change`, `%session-changed`, …) and maps psmux **windows/panes → WT
  tabs/panes**, rendering natively in our existing `TermControl`. The user never
  sees the raw tmux UI.
* Closing WT **detaches** (server + shells keep running). Relaunch **reattaches**
  to the same server and re-hydrates live panes — a build started before close
  is still going.
* This reuses our ConPTY/`TermControl` stack and the per-tab routing we already
  have for agent panes; the new surface is a control-protocol client (parser +
  window/pane reconciler + a local transport to the server).

### Option B (fallback / MVP) — transparent psmux session wrapping

Each durable shell pane is a named psmux session; WT `attach`es on restore. No
native control-mode rendering (the pane shows psmux's own UI), so tab/pane
fidelity and chrome are weaker, but it is far less work and proves the
keep-running lifetime end-to-end.

### Option C — build our own native session host

The spec's alternative. psmux already is a mature, MIT, ConPTY-native
implementation of exactly this, so building a parallel host is hard to justify
for an MVP; prefer to reuse (Option A) and contribute upstream if gaps appear.

## Risks / open questions

* **Third-party dependency.** psmux is MIT but young; adopting it means vetting,
  version-pinning, vendoring/attribution (same process as the Rust crates in
  `wta`), and a packaging decision (bundle vs. require install). It is Rust +
  ConPTY, which aligns with `wta`'s toolchain.
* **Control-protocol parity.** iTerm2 relies on specific `-CC` semantics; we
  must confirm psmux's control-mode output matches closely enough (layout
  notifications, `%output` framing, resize, `%exit`) or drive parity upstream.
* **Agent panes.** Our agent pane is a ConPTY hosting `wta-helper`. Under keep-
  running it would live as a psmux pane too; its ACP session already survives
  via `session/load` (steps 2–3), so a detached agent could keep working and
  reattach — but this interaction needs its own prototype.
* **Settings & honesty.** Ties into the P0 startup toggles (restore shell /
  restore agent / **continue running**). When keep-running is off we keep the
  step 1–4 snapshot behavior; when on, we attach to the live server.
* **Buffering limits, mouse/IME, security** (the server owns processes — scope
  it per-user, per-namespace) all need validation.

## Recommendation

1. **Adopt psmux as the durable-sessions "keep-running" session host.** Native
   tmux is not an option on Windows; psmux fills the exact gap and already
   provides session persistence + a `-CC` control transport (both verified).
2. **Prototype Option A** — a WT `psmux -CC` control-mode client — as the
   keep-running MVP, reusing our ConPTY/`TermControl` and per-tab routing, and
   gate it behind the "continue running commands" startup setting.
3. Start with **Option B** if a faster end-to-end proof of the detach/reattach
   lifetime is needed before investing in the control-mode client.

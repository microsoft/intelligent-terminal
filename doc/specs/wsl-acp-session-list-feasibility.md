# WSL Session Visibility via ACP `session/list` — Feasibility Study

## Abstract

[`wsl-session-management.md`](./wsl-session-management.md) (PR #323) made
**historical, in-distro WSL agent sessions** visible in `/sessions` by spawning
one `wsl … bash` per running distro, `tar`-ing the newest CLI transcripts to
stdout, extracting them host-side, and running the existing
`history_loader::classify_*` jsonl parsers over them.

This document records an empirical study of two follow-up questions:

1. **Can ACP `session/list` replace that file-reading approach** to obtain the
   historical WSL sessions directly from each CLI?
2. **Can ACP notify the session view when a *new* WSL session appears**, so the
   list updates mid-window instead of only at startup?

The study was run with a new throwaway diagnostic, `wta probe-sessions`
(`src/protocol/acp/probe.rs::probe_sessions`, `src/main.rs`), which spawns an
agent in ACP mode, runs `initialize`, then calls `list_sessions` and prints the
result.

**Bottom line:** ACP `session/list` is a real, well-supported capability that
*can* fetch WSL history. Every CLI Intelligent Terminal targets supports it
(copilot npm *and* snap, claude, codex). Gemini is excluded by choice — its CLI
is dropping ACP support upstream — so the ACP route needs **no tar/file
fallback at all**; it can be the sole WSL history source.
Question 2 is **not** an ACP problem at all — WTA already has a 5 s periodic
refetch; the WSL scan simply needs to participate in it.

## Background

### How `session/list` is used in WTA today

ACP 0.10 defines `list_sessions` as an **UNSTABLE** capability
(`agent-client-protocol-0.10.0/src/agent.rs:150`: *"Lists existing sessions
known to the agent … not part of the spec yet"*). WTA enables it
(`tools/wta/Cargo.toml`, feature `unstable_session_list`) but uses it only on
the **helper ↔ master** hop: `wta-master` answers `session/list` from its **own
live-session registry** and **never forwards it to the agent CLI**
(`src/master/mod.rs:1137`):

> *"Answer `session/list` from our own live-session registry instead of
> forwarding to the agent CLI … Forwarding to the agent CLI would conflate the
> two and re-introduce the cross-CLI variance."*

So WTA had never observed what an agent CLI's **own** `session/list` returns.
That is exactly what this study measured.

### The two transports

- **Host history (today)**: `history_loader::load_all` + per-CLI
  `session_watcher::classify_*` parse on-disk jsonl under the Windows profile.
- **WSL history (PR #323)**: `wsl.rs::scan_running_distros` tars the distro
  transcripts and runs the *same* host parsers over them.

Both are file-reading. The question is whether an ACP `session/list` round-trip
to the CLI is a better source.

## Method

`wta probe-sessions --agent "<cmdline>"` spawns `<cmdline>` exactly like an
agent pane would (`spawn_agent_process`), runs ACP `initialize`, dumps the
agent's advertised capabilities, then calls `list_sessions` and prints the
returned rows as JSON. The same binary was pointed at:

- Windows-side CLIs directly (`copilot --acp --stdio`, the npx adapters,
  `gemini --experimental-acp`).
- WSL CLIs through the `wsl.exe` stdio bridge
  (`wsl -d <distro> -- bash -lc <wrapper>`).

## Findings

### 1. `session/list` support is per-CLI (3 of 4)

| CLI | ACP launch (`agent_registry.rs`) | `sessionCapabilities.list` | `list_sessions` result |
|-----|----------------------------------|----------------------------|------------------------|
| **copilot** | native `--acp --stdio` (`:87`) | `Some` | OK — 50 host sessions |
| **claude** | `npx @agentclientprotocol/claude-agent-acp` (`:109`) | `Some` | OK — 21 host sessions |
| **codex** | `npx @zed-industries/codex-acp` (`:126`) | `Some` | OK — host sessions |
| **gemini** | native `--experimental-acp` (`:144`) | **`None`** | **`Method not found`** |

Versions observed: copilot 1.0.64–1.0.66, claude-agent-acp 0.52.0, codex-acp
0.16.0, gemini-cli 0.46.0. Each supporting CLI returns **structured** rows
(`session_id`, `cwd`, `title`, `updated_at`) parsed by the CLI itself — no
host-side jsonl parsing required. **Gemini does not implement the capability**
— and its CLI is dropping ACP upstream, so Intelligent Terminal treats it as
**out of scope** rather than a case to fall back to file-reading for.

### 2. ACP `session/list` *does* fetch WSL history (verified npm AND snap)

`wta probe-sessions --agent "wsl -d Debian -- bash -lc /home/<user>/acp.sh"`
(wrapper = `exec copilot --acp --stdio`) returned the Debian distro's historical
copilot sessions over the normal `wsl.exe` + **pipe stdin** bridge. So ACP over
WSL is viable end-to-end for both **npm- and snap-installed** copilot (snap
confirmed on Ubuntu — see Finding 4).

### 3. Two WSL traps that masquerade as "ACP doesn't work"

Both produced misleading `"server shut down unexpectedly"` failures until
`wta-probe.log` (which drains the child's stderr) revealed the real cause:

- **`wsl.exe -- <cmd>` runs under a NON-login `bash -c`.** A PATH-installed CLI
  (`~/.local/node22/bin/copilot`) is then `command not found`. **Fix:** wrap in
  `bash -lc`. Note `spawn_agent_process` (`spawn.rs:49`) splits the agent
  cmdline with `split_whitespace` and **cannot carry** `bash -lc "copilot --acp
  --stdio"` (the quoted script is torn apart) — a real implementation needs
  either `shell-words` parsing or a no-space wrapper script.
- **`/tmp` is tmpfs and is wiped when the distro auto-shuts-down**, so a wrapper
  written to `/tmp` vanishes between probe runs. Use a persistent path
  (`~/acp.sh`).

### 4. snap-installed copilot DOES work (earlier "snap is broken" was a measurement error)

Ubuntu's default copilot is a **snap** (`/snap/copilot-cli/50/bin/copilot`). It
prints a **non-fatal** `cannot preserve mount namespace … / unexpected eof from
helper process` warning on stderr, but `--acp` works:
`wta probe-sessions --agent "wsl -d Ubuntu -- bash -lc ~/acp.sh"` returned
`list_sessions: ok` with 4 Ubuntu sessions over normal **pipe stdin** — identical
to a manual TTY run.

An earlier draft of this study concluded snap was broken; **that was wrong.** The
failing runs were `(printf …; sleep N) | copilot` measured during snap's **cold
start** (first launch does `Package extraction took ~5.6 s`) and were never
cross-checked with `probe`. Once warm, a stdin-held ladder (2/6/12/20 s) **all**
returned the initialize response (554 B). **Lesson: cross-check a WSL CLI with
`probe` (pipe stdin, held open) before concluding it "can't do ACP"; budget for
snap cold-start in the initialize timeout.**

### 5. The "coverage gap" (2 vs 9) is not a gap

Debian's `~/.copilot/session-state` had 9 UUID dirs but `session/list` returned
2. Inspection: only those 2 have an `events.jsonl` (85 KB/18 lines, 44 KB/8
lines); the other 7 are empty scaffolds (`checkpoints/ files/ research/
workspace.yaml`, no transcript). `session/list` returns exactly the sessions
with **real content** — arguably **more** accurate than tar-ing every directory.

### 6. Question 2 (notify on new session) is orthogonal to ACP

ACP 0.10's Client trait has **no** "session-list-changed" push — only
`session_notification` (updates *within* an existing session, keyed by
`session_id`) and `ext_notification` (`client.rs`). WTA's own
`intellterm.wta/session_added|removed|sessions/changed` ride `ext_notification`
and only cover **master-managed** sessions, never user-run WSL ones.

But the session view does **not** depend on a push: a 5 s periodic tick already
fans out `AppEvent::SessionsChanged` → `schedule_agents_refetch_for_open_views`
(`client.rs:2549`, `app.rs:5380`). PR #323's WSL scan is **startup-only** and
does not participate. **Fix is file-based**: re-run the WSL scan on that tick
(throttled), independent of ACP.

## Conclusions

- **Q1 — ACP *can* replace file-reading for every targeted CLI.**
  `session/list` works for copilot (npm AND snap), claude, and codex over the WSL
  bridge — the three CLIs Intelligent Terminal supports. **Gemini** is the lone
  CLI that doesn't implement it (`list: None` → `Method not found`), but its CLI
  is dropping ACP upstream and is **out of scope**, so no tar/file fallback is
  required: ACP can be the sole WSL history source.
- **Q2 — ACP is not the answer.** Wire the existing WSL scan into the 5 s
  `SessionsChanged` refetch; no ACP push exists or is needed.

## Recommended design (if pursued): ACP-only, no tar fallback

A self-contained WSL history source built entirely on ACP `session/list` (every
targeted CLI supports it, so the PR #323 tar path is **not** needed for WSL
history once this lands):

1. **Probe + cache capability per (distro, CLI).** One `initialize` round-trip
   tells us `sessionCapabilities.list`. Cache it; re-probe on miss. (Defensive
   only — all three targeted CLIs report `Some`; a `None` row is simply skipped,
   not tar-fallen-back.)
2. **ACP path** (copilot npm+snap, claude, codex): one-shot
   `wsl -d <distro> -- bash -lc <wrapper>` → `initialize` + `list_sessions` →
   structured rows stamped `SessionLocation::Wsl{distro}`. Replaces the
   `history_loader::classify_*` parser **and** the PR #323 tar scan for WSL.
3. **snap cold-start tolerance.** snap copilot's first `--acp` launch pays a
   one-time `Package extraction` (~5–6 s); give the WSL `initialize` a generous
   timeout (≥ the npx-adapter budget) so a cold snap isn't misread as failure.
4. **login-shell carrier.** Fix `spawn_agent_process` to parse quotes
   (`shell-words`) or thread a wrapper, so `bash -lc "<cli> <acp-flags>"` works
   (wsl.exe's default `bash -c` is non-login and won't find a PATH-installed CLI).
5. **periodic refetch (Q2).** Run the source on the existing 5 s
   `SessionsChanged` tick (throttled, running-distros-only — never auto-boot a
   stopped distro, per PR #323 / GH#9541).

## Costs & open questions

- **Per-(distro, CLI) ACP spawn**; claude/codex add an `npx` download inside the
  distro on first use, and snap copilot pays its one-time extraction. Heavier
  per-call than one tar spawn per distro — but it's a single uniform code path
  (no parallel tar branch to maintain).
- **Auth.** A logged-out WSL CLI still answers `initialize`/`list_sessions`
  here, but this was not exhaustively tested across CLIs.
- **`session/load` resume.** copilot/claude/codex advertise `loadSession: true`;
  an ACP-resume into an agent pane for WSL rows (vs PR #323's CLI-flag resume) is
  a possible follow-up but inherits the same per-distro ACP-server cost.

## Appendix: reproduce

```text
# Windows host:
wta probe-sessions --agent "copilot --acp --stdio"
wta probe-sessions --agent "npx -y @agentclientprotocol/claude-agent-acp"
wta probe-sessions --agent "npx -y @zed-industries/codex-acp"
wta probe-sessions --agent "gemini --experimental-acp"        # list: None

# WSL (npm CLI; persistent login-shell wrapper required):
#   ~/acp.sh = #!/bin/bash\nexec copilot --acp --stdio
wta probe-sessions --agent "wsl -d Debian -- bash -lc /home/<user>/acp.sh"
```

Diagnostic log (drains the child CLI's stderr — decisive for the WSL traps):
`%LOCALAPPDATA%\IntelligentTerminal\logs\wta-probe.log`.

# Mock ACP Agent — deterministic E2E coverage for the agent stack

Author: kaitao@microsoft.com
Status: Proposal / in progress (Form A scaffolding next)
Related: `doc/release-check-list.md`, `doc/release-ut-plan.md`,
`doc/release-automation-plan.md`, `doc/specs/Multi-window-agent-pane.md`

## Problem

Most of `release-check-list.md` is marked `[E2E]`: agent-pane chat, permission
UI, insert/run, autofix, model selection, session management, multi-tab/window
routing. Unit tests cover the **logic cores** of these, but not the behavior a
user actually exercises, because that needs a real agent on the other end of
the ACP wire. Driving the **real** Copilot/Claude/Gemini for a release gate is
non-deterministic (LLM answers vary), slow, network-dependent, and flaky.

A **mock ACP agent** — a deterministic implementation of the agent side of the
Agent Client Protocol — slots in exactly where the real agent CLI does and
makes the whole stack reproducible. This spec defines what it is, the two forms
it runs in, what each form can and cannot test, and a phased plan.

## Where the mock plugs in

The agent call chain:

```
WindowsTerminal → ConptyConnection → wta-helper ─(ACP/pipe)→ wta-master ─(ACP/stdio)→ [agent CLI]
```

The mock replaces **[agent CLI]** — the only place an LLM lives. Everything to
its left (WT, helper, master, COM, conpty, the WTA TUI) stays real.

WTA implements the **client** side of ACP (`WtaClient: acp::Client`,
`tools/wta/src/protocol/acp/client.rs:1668`). The mock therefore implements the
**agent** side (`acp::Agent`, from `agent-client-protocol = 0.10`). Required
trait methods: `initialize`, `new_session`, `prompt`, `cancel`; the rest have
defaults (`load_session`, `set_session_model`, `authenticate`, …) which the
mock overrides per scenario.

The mock streams replies to WTA by calling, on its `AgentSideConnection`:

```rust
conn.session_notification(SessionNotification::new(
    session_id,
    SessionUpdate::AgentMessageChunk(ContentChunk::new("MOCK_OK".into())),
)).await?;
```

then returns a `PromptResponse { stop_reason: EndTurn }`. (Pattern proven by
the crate's own `src/rpc_tests.rs::create_connection_pair`.)

## Two forms — and why both

The mock can attach at two boundaries. They are **not** either/or; each reaches
a different depth, so we use both and split coverage by what each can reach.

| | Form A — in-process | Form B — out-of-process |
|---|---|---|
| Harness | `cargo test` (`#[tokio::test]`) | one-shot scenario script on a prepared box |
| Wire | `tokio::io::duplex` in memory | real stdio to `mock-acp-agent.exe` (custom agent) |
| What's real | ACP client, App reducer, TUI render | WT process, helper/master, conpty, COM, wtcli, UI |
| Determinism | total | high (no LLM) but desktop/UI timing exists |
| CI | yes | no (needs interactive desktop + UIA) |
| Speed | ms | seconds–minutes |
| Flakiness | none | some (UI/timing) |

**Principle: anything Form A can cover deterministically must NOT be pushed to
Form B.** Form B is reserved for what genuinely needs a live WT process / real
UI. A single all-in-one one-shot E2E run would be slow, desktop-bound, flaky,
and un-CI-able — a poor regression gate.

---

## Form A — in-process, as `cargo test`

The mock is an in-memory `acp::Agent` wired to WTA's real `ClientSideConnection`
over `tokio::io::duplex`, mirroring `create_connection_pair` from the crate
tests but substituting the real `WtaClient` for the test client.

```
tokio::io::duplex()
        ┌──────────────── client→agent ───────────────┐
WTA  ClientSideConnection(WtaClient)            AgentSideConnection(MockAgent)  mock
        └──────────────── agent→client ───────────────┘
```

### Depth: A1 (shallow) vs A2 (deep)

- **A1 — connection-level.** The test constructs `ClientSideConnection` with the
  real `WtaClient` + the mock peer, calls `conn.prompt(...)` directly, and
  asserts the `AppEvent`s `WtaClient` emits (and optionally feeds them into a
  real `App` + asserts state / TestBackend render). Covers the **ACP protocol
  handling + App reducer + render**. Cheap, lands the harness fast. Skips the
  big orchestration function `run_acp_client_over_pipe`.

- **A2 — orchestration-level.** Drive the real `run_acp_client_over_pipe`
  (`client.rs:2258`) against the mock so the test exercises the actual channel
  wiring, session map, lazy session creation, autofix prompt assembly, restart,
  etc. Today that function opens a **named pipe to master**, so A2 needs one of:
  1. a **small refactor** extracting the ACP-loop body to accept
     `impl AsyncRead + AsyncWrite`, so both the production named-pipe path and a
     test `duplex` can drive it (preferred — also clarifies the code), or
  2. a **named-pipe loopback** in the test (mock connects as the agent side of a
     real pipe) — heavier, no refactor.

Plan: **A1 first** (proves the harness + wire end-to-end), then introduce the
A2 stream-injection refactor to unlock the high-value orchestration tests.

### Rendering assertions

WTA renders via `ui::layout::render(frame, app)` (`tools/wta/src/ui/layout.rs`).
A render assertion builds a `ratatui::Terminal::new(TestBackend::new(w, h))`,
drives an `App` to the desired state, calls the render, and asserts on the
backing `Buffer` text. This covers "streaming output renders correctly", chat
view, session view, permission card, model picker — **without** a real terminal.

### The mock (Form A core)

A `MockAgent` struct implementing `acp::Agent`, configurable by a `MockScript`:

- `initialize` → returns a fixed capability set (toggle `loadSession`, fixed
  model list).
- `new_session` / `load_session` → deterministic `SessionId`; `load_session`
  replays a canned history.
- `prompt` → per-script: stream N `AgentMessageChunk`s (deterministic text such
  as `MOCK_OK:<echo>`), optionally a `ToolCall` / `request_permission`, then
  return a chosen `StopReason` (`EndTurn` / `MaxTokens` / `Refusal`).
- failure modes: `authenticate`/`prompt` return `acp::Error` with
  `AuthRequired` / a protocol code; "hang" delays past the watchdog; "disconnect"
  drops the connection mid-turn.
- **side-channel capture**: records every received `PromptRequest`
  (text + `_meta` pane context, model) into an `Arc<Mutex<Vec<…>>>` the test
  asserts on — e.g. "the autofix prompt carried the failing pane's context".

### What Form A can test (deterministic, CI)

- Chat round-trip: prompt → `AgentMessageChunk` → App message → rendered text.
- Streaming render: multiple chunks coalesce correctly.
- Permission flow: mock `request_permission` → App permission state → allow/reject reply.
- Tool call surfaced (insert/run **card** appears in the model — the *card*, not
  the real pane write, which is Form B).
- Autofix prompt **content**: failing-pane context + template reach the agent
  (side-channel), and the returned fix lands as a card.
- Model: advertised list → `/model` picker state → `set_session_model` round-trip.
- Session: `new_session` id, `load_session` replay, session-state derivation.
- Failure/degrade: auth → sign-in mode; transport loss → reconnect state;
  protocol error → error line; soft stop → system line; unresponsive → watchdog.
- Slash effects on a real ACP session (`/new` new id, `/stop` cancels in-flight,
  `/restart` …) — A2.
- Multi-tab routing: per-tab sessions, prompt/autofix land on the right tab — A2.

### What Form A cannot test

Real WT window, conpty, COM/wtcli, pane open/hide/focus, insert/run into a
**real** shell pane, real keyboard path through WT, multi-window drag, packaging
identity. → Form B.

---

## Form B — out-of-process, one-shot harness

The mock is compiled to `mock-acp-agent.exe` and configured as a **custom
agent** in settings:

```jsonc
{ "acpAgent": "custom:mock", "acpCustomCommand": "mock-acp-agent.exe --mode happy" }
```

Now real WT + helper/master run, with the mock as the agent CLI. A runnable
scenario script (PowerShell + UIA/WinAppDriver and/or `wtcli`) drives input and
asserts via observable side effects.

- **Drive:** `wtcli send-keys` / UIA to type prompts, press `Ctrl+Shift+.`, etc.
- **Observe:** `wtcli capture-pane` (read the agent-pane TUI text → `MOCK_OK`),
  `wtcli listen` (events), the mock's side-channel log, WTA/terminal logs.
- **Modes:** `--mode happy|auth-fail|hang|disconnect|protocol-error|soft-stop|slow`,
  `--model-list a,b`, deterministic outputs, and a permission/tool trigger phrase.

### What Form B adds over Form A

- Agent pane actually opens/hides/stashes/focuses (button + hotkeys, all positions).
- Insert into pane / Run in pane reach a **real** shell pane (verify with capture-pane).
- Autofix end-to-end from a real shell failure (`osc 133;D;1`) through to a card
  in the real pane.
- `wtcli`/COM activation, packaging identity, master/helper spawn + crash recovery.
- Multi-tab / split / multi-window routing with real tab drag.

### Still manual (neither form)

Real LLM answer quality, real install/auth UX (winget, `gh auth`), visual
polish, high-contrast / scaling / RTL rendering quality, screen-reader output.

---

## Checklist mapping (target)

| `release-check-list.md` area | Becomes |
|---|---|
| Agent pane chat round-trip, streaming, permission, tool-card | Form A |
| Autofix prompt content / suggestion card / dismiss / target routing | Form A |
| Model list + `/model` selection | Form A |
| Session state + Enter/Shift+Enter dispatch outcome | Form A (UT already covers `decide_enter_action`) |
| Failure/degrade (auth/transport/protocol/soft-stop/unresponsive) | Form A |
| Slash command effects on a live session | Form A (A2) |
| Multi-tab routing logic | Form A (A2) |
| Pane open/hide/focus, positions | Form B |
| Insert/run into a real pane | Form B |
| Real keyboard shortcuts through WT | Form B |
| Multi-window drag, COM/wtcli, packaging | Form B |
| Real agent quality, install/auth, visual/a11y/RTL | Manual |

## Where the code lives

- **Form A mock + tests:** in the WTA crate, in `protocol::acp` test modules (so
  they can reach the private `WtaClient`/`ClientState`). New file e.g.
  `tools/wta/src/protocol/acp/mock_agent.rs` (`#[cfg(test)]`), plus scenario
  tests alongside. Optionally promote the mock behind a `cfg(test)`-or-feature
  flag if Form B reuses it.
- **Form A render helpers:** a small `TestBackend` helper near the `ui` module.
- **Form B mock binary:** a `mock-acp-agent` bin target in the `tools/wta`
  workspace (reuses the same `agent-client-protocol` version — no drift) sharing
  the `MockAgent` core with Form A.
- **Form B harness:** `tools/release-validation/` scenario scripts (out of CI).

## Phased plan

1. **A1 harness + first scenario** — `MockAgent` (happy mode) + duplex wiring;
   one `#[tokio::test]`: prompt → assert WTA surfaces `MOCK_OK` (AppEvent and/or
   TestBackend render). Proves the wire end-to-end. ← next
2. **A1 breadth** — permission, tool-card, model list/select, failure modes,
   soft stop, session new/load. Side-channel assertions on prompt content.
3. **A2 refactor + orchestration tests** — extract the ACP loop to accept an
   injected stream; test `run_acp_client_over_pipe` against the mock for autofix
   assembly, slash effects, multi-tab routing.
4. **Form B mock binary** — `mock-acp-agent.exe` sharing the `MockAgent` core.
5. **Form B harness** — one-shot scenario scripts: open/hide pane, chat,
   insert/run, autofix, on a prepared box; emit a pass/fail report.

Steps 1–3 are CI-able and convert the bulk of the `[E2E]` behavior surface to
deterministic regression; steps 4–5 cover the residual true-UI surface.

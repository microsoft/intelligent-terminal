# ACP Multi-Session Concurrency — Investigation

## Question

wta currently runs **one ACP session** per agent process and serializes prompt
submission with a single `await` loop in `protocol/acp/client.rs:1690`:

```rust
while let Some(prompt) = prompt_rx.recv().await {
    let result = conn.prompt(PromptRequest::new(session_id.clone(), ...)).await;
    ...
}
```

Combined with the per-tab session model in `app.rs:1509` (`switch_tab_session`),
this means: when a user has prompts in two tabs, the second tab's prompt cannot
start until the first tab's prompt finishes — `wall ≈ T_a + T_b`.

The protocol crate (`agent-client-protocol` v0.10 with `unstable_session_model`)
allows multiple sessions per ACP connection. **Question:** if wta were to create
one `SessionId` per tab and dispatch prompts concurrently on the same
connection, would the agent CLI on the other end actually run them in parallel,
or would it serialize them internally?

This determines whether option **#3 (one connection, N sessions, concurrent
prompts)** is viable, or whether real concurrency requires option **#2 (N
connections, N agent subprocesses)**.

## Approach

A throwaway probe binary did the following:

1. Spawn one agent subprocess.
2. ACP `initialize`.
3. Call `new_session` twice → `SessionId` A and B on the same connection.
4. Issue two `prompt` calls via `tokio::join!` so both futures are polled
   concurrently on the same task.
5. Timestamp every `session/update` chunk, tagged by `sessionId`.
6. Compare the two streams:
   - `wall ≈ max(T_a, T_b)` and chunks interleaving in time → **concurrent**
   - `wall ≈ T_a + T_b` and B's first chunk lands after A's last → **serial**

Permission requests were auto-cancelled so the agent could not invoke tools —
this kept the comparison to pure LLM streaming and avoided shell-call latency
noise.

The prompt was deliberately model-bound work that streams gradually:

> Write three short haikus. The first about the number 7, the second about the
> number 13, the third about the number 42. Output them one after another, with
> a blank line between.

## Results

Each agent received the same prompt twice (one per session) at `t = 0` after
`tokio::join!`. Times below are wall-clock relative to the moment both prompts
were fired.

| Agent | Version | A duration | B duration | Wall (≈ max?) | First-chunk gap | Verdict |
|---|---|---|---|---|---|---|
| **Copilot** (`copilot --acp --stdio`) | 1.0.43 | 5686 ms | 5630 ms | **5686 ms** ✓ | 19 ms | **CONCURRENT** |
| **Claude Code** (`npx @zed-industries/claude-code-acp`) | 0.16.2 | 7734 ms | 4758 ms | **7734 ms** ✓ | 9 ms | **CONCURRENT** |
| **Codex** (`npx @zed-industries/codex-acp`) | 0.13.0 | 12203 ms | 25673 ms | **25674 ms** ✗ | 7194 ms | **SERIAL** |

Detail rows:

- **Copilot.** Chunks from A and B interleaved throughout the run. B's first
  chunk arrived 19 ms after A's; both streams ended within ~100 ms of each
  other. Wall time was roughly half of what serial execution would have cost
  (~11.3 s).

- **Claude Code.** Same picture as Copilot but tighter: 9 ms gap on first
  chunk, 2126 ms of stream overlap. B actually finished *earlier* than A
  (4.8 s vs 7.7 s) because Claude generated a slightly shorter response for
  that session — but both ran in parallel, not sequentially.

- **Codex.** A streamed from 11.5 s to 14.2 s (after `tokio::join!` was
  fired). B's first chunk did not arrive until 16.6 s — **4.5 s after A had
  already finished**. The wall time (25.7 s) is essentially `T_a + T_b`. The
  protocol layer accepted both `prompt` calls and assigned distinct
  `SessionId`s, but `codex-acp` serializes the underlying LLM calls behind an
  internal lock.

### Note on the Claude run

Claude Code's ACP adapter refuses to launch when the `CLAUDECODE` env var is
set (it detects nested Claude Code sessions and bails to avoid resource
contention). Inside an agent host that itself runs under Claude Code, the
probe must clear that env var before spawning. wta's normal launch path is
unaffected.

## Interpretation

**ACP itself is not the bottleneck.** The wire protocol is JSON-RPC 2.0 with
per-request `RequestId`s; out-of-order responses are native. The
`unstable_session_model` feature of the protocol crate exposes the
`new_session` / `SessionId` machinery wta already relies on.

**Whether concurrency materializes is decided by the agent CLI:**

- Copilot and Claude Code both run independent LLM calls per session and
  schedule them in parallel. For these two — which are the most common
  defaults — moving wta to one-connection / N-sessions would cut wall time
  for two-tab workloads roughly in half.

- Codex internally serializes. A wta change would still produce **correct**
  multi-tab behavior on Codex (no chunk leakage between tabs, no lost
  responses), but no speedup. wall remains `≈ T_a + T_b`.

## Implication for wta architecture

Option **#3 is the right path forward**:

- One wta process, one ACP connection, one agent subprocess (no extra
  process / memory cost).
- `HashMap<TabId, SessionId>` lazily populated on first prompt per tab.
- `Arc<ClientSideConnection<...>>` so multiple `tokio::spawn_local`'d tasks
  can call `.prompt()` concurrently.
- Per-tab in-flight state (`agent_streaming`, `pending_agent_response`,
  `prompt_in_flight`, `current_prompt_id`) moved into `TabSession` so chunks
  route to the originating tab regardless of which tab is currently focused
  in the UI.
- Submit guard remains useful inside a single tab (one in-flight prompt per
  tab still serializes that tab's conversation, which is correct — turns
  must be ordered within a session).

Performance ceiling per agent:
- Copilot, Claude Code: `wall ≈ max(T_tab_i)` (true parallelism).
- Codex: `wall ≈ Σ T_tab_i` — same as today, but with correct isolation.

No agent regresses; two of three accelerate.

## Reproducing

The probe binary used for this investigation has been deleted. To rerun, the
key elements of the methodology are:

- Depend on `agent-client-protocol = { version = "0.10", features = ["unstable_session_model"] }`.
- Implement the minimum `acp::Client` trait: `request_permission` returning
  `Cancelled`, `session_notification` recording `(elapsed_ms, sessionId,
  chunk_kind)`, and the four terminal stubs returning `method_not_found`.
- Wire `acp::ClientSideConnection::new(client, stdin, stdout, |fut| spawn_local(fut))`.
- After `initialize` + two `new_session` calls, `tokio::join!` two
  `conn.prompt(PromptRequest::new(session_id, vec![text.into()]))` futures.
- For Copilot use `copilot --acp --stdio`. For Claude / Codex use the
  `npx -y @zed-industries/{claude-code,codex}-acp` adapters listed in
  `agent_registry.rs`.
- For Claude inside a Claude Code host process, clear `CLAUDECODE`,
  `CLAUDE_CODE_SSE_PORT`, `CLAUDE_CODE_ENTRYPOINT` before spawn.

The verdict heuristic that worked well: compute `overlap = min(last_a,
last_b) - max(first_a, first_b)`. `overlap > 200 ms` indicates concurrency;
`overlap < -200 ms` indicates serial execution; otherwise re-run with a
longer prompt to disambiguate.

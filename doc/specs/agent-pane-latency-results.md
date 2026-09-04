# Agent Pane Latency Implementation and Results

**Date:** 2026-09-03  
**Baseline revision:** `41f9754e2865`  
**Branch:** `hamza-usmani-agent-pane-latency-analysis`

## Summary

This branch implements the highest-value changes from the agent-pane latency
investigation:

- tiered master/provider/helper pre-warming;
- one queued prompt while the provider connects;
- one consolidated Terminal Protocol prompt-context operation;
- non-blocking PowerShell command recall;
- agent-free master control clients;
- generation-safe agent availability caching;
- correlated startup and prompt timing;
- active-tab-only speculative helper materialization.

The main remaining startup cost is Copilot's ACP initialization and
`session/new`, not the helper/master named-pipe transport.

## Before/after benchmarks

| Metric | Before | After | Improvement |
| --- | ---: | ---: | ---: |
| Healthy pane pre-warm to Connected | 3,716 ms | 3,248 ms p50 | **468 ms / 12.6% faster** |
| Helper start to ACP session ready | 2,983 ms p50 | 2,265 ms p50 | **718 ms / 24.1% faster** |
| WTA overhead above direct Copilot ACP median | 744 ms | 276 ms | **62.9% less wrapper overhead** |
| Prompt terminal-context collection | 230.6 ms p50 | 76.8 ms p50 | **153.8 ms / 66.7% faster** |
| Processes per prompt-context capture | 3 | 1 | **66.7% fewer** |
| Blocking PowerShell recall on observed autofix | 5,820 ms | 0 ms awaited | **Removed from critical path** |
| Helper first editable frame | Not instrumented | 321 ms p50 | New user-visible milestone |
| Editable lead before ACP session ready | Not available | 1,928 ms p50 | Input is usable while connection finishes |
| Copilot children after 20 control-list calls | Fresh-start race could create 2 | Stayed at 1 | Duplicate spawn eliminated |
| Direct WTA children after visiting five tabs | Master + 5 helpers | Master + 1 helper | **6 to 2** |

### Startup trials

| Trial | Policy resolution | Master lease ready | Helper materialized | Connected |
| ---: | ---: | ---: | ---: | ---: |
| 1, first Debug launch after deployment | 18 ms | 4,319 ms | 5,589 ms | 8,464 ms |
| 2 | 18 ms | 177 ms | 631 ms | 3,369 ms |
| 3 | 17 ms | 170 ms | 611 ms | 3,209 ms |
| 4 | 17 ms | 170 ms | 627 ms | 3,248 ms |
| 5 | 17 ms | 171 ms | 617 ms | 3,170 ms |

The first trial paid a 4.3-second cold Debug image/process-start penalty. Later
trials stabilized at 3.17-3.37 seconds.

Five direct `copilot --acp --stdio` probes measured a 2,972 ms median through
`initialize` plus `session/new`. The measured WTA gap therefore fell from
744 ms to 276 ms.

## Implementation

### Tiered pre-warming

Terminal now holds a process-level `SharedWta` lease and starts the trusted,
policy-approved default provider without creating a session. Only the selected
eligible tab materializes a hidden helper/session.

Unused speculative helpers are evicted as tab selection changes. Explicitly
opened, restored, transferred, or conversation-bearing panes remain alive.

The lifecycle implementation also covers:

- latest-settings crash recovery;
- partial `AllowedAgents` changes;
- policy-safe explicit default command and ID;
- native/default provider pool-key convergence;
- pinned default custom/model-specific pool entries;
- attempt-safe asynchronous reaping;
- one-shot pane ownership transfer across drag;
- explicit-close suppression;
- settings recreation without losing speculative state.

### Editable while connecting

The helper renders before ACP initialization completes. While `Connecting`, it
accepts one visible queued prompt. The prompt is:

- cancellable locally;
- bound to an immutable connection generation;
- dispatched exactly once after the matching connection becomes ready;
- retained across transparent same-binding reconnect;
- cancelled visibly before an explicit rebind or terminal failure;
- restored if the prompt channel closes before accepting it;
- rekeyed safely across tab drag.

A delayed ACP fixture verified:

```text
helper_first_frame elapsed_ms=303
queued_prompt_accepted prompt_id=1
queued_prompt_dispatched prompt_id=1 elapsed_ms=4719 dispatched=true
context_ready context_build=0.063s
first_text since_prompt_sent=0.019s
```

### Consolidated prompt context

Terminal Protocol 2.3 adds `GetPromptContext` and
`wtcli prompt-context`.

One operation now resolves the explicit/effective source pane, returns its
metadata, captures the most recent completed prompt, and falls back to a bounded
tail when shell marks are unavailable.

Explicit missing pane IDs never substitute the focused pane. Protocol 2.2
retains an explicit logged compatibility path.

`ReadBufferTail` prevents fallback capture from copying the complete scrollback
on the UI thread.

Twenty-five paired deployed trials:

| Path | Processes | p50 | p95 | Mean |
| --- | ---: | ---: | ---: | ---: |
| Legacy active-pane + last-prompt + fallback | 3 | 230.6 ms | 251.0 ms | 234.3 ms |
| Protocol 2.3 prompt-context | 1 | 76.8 ms | 98.1 ms | 79.9 ms |

### Non-blocking command recall

PowerShell command enumeration no longer runs on prompt submission.

Prompt assembly performs memory-only lookup/ranking. Cold, stale, busy, or
failed cache entries schedule a single-flight background refresh and omit
near-match context for that turn.

The background refresh includes a PATH-existence gate and does not treat
degraded data as authoritative. It took approximately 9.4 seconds median on
the benchmark machine, but awaited prompt-path cost is now zero.

### Agent-free control clients

Master control clients identify themselves with:

```text
_meta.wta.connection_role = "master-control-v1"
```

They can access master-owned operations without selecting, binding, or spawning
an agent. Unsupported role versions fail explicitly.

With an explicit running-master pipe:

- 20/20 `sessions list` calls succeeded;
- p50 was 133.3 ms;
- p95 was 136.8 ms;
- Copilot children remained exactly 1 before and after.

### Async agent availability

Host-agent discovery is now asynchronous, generation-aware, and single-flight.
It does not gate master/helper pre-warming.

FRE and Settings update on their UI dispatchers, preserve valid selection,
handle policy-only non-Copilot defaults, and prevent empty-agent hook setup.

Failed or invalidated generations remain retryable and cannot publish a stale
valid result.

## Validation

### Code/build validation

| Validation | Result |
| --- | --- |
| Baseline WTA suite | 1,890 passed, 1 ignored |
| Final WTA suite | **1,949 passed, 1 ignored** |
| Explicit-target WTA build | Passed |
| Full Debug Terminal/package build | Passed |
| SharedWta unit tests | 43/43 passed |
| Settings/policy tests | 31/31 passed |
| ControlCore tests | 33/33 passed |
| TerminalApp tests | 215/215 passed |
| Terminal Protocol tests | 7/7 passed |
| Agent availability tests | 5/5 passed |
| Speculative-helper tests | 5/5 passed |
| Full LocalTests | 83 passed; 34 blocked by the pre-existing `0x8000ffff` TerminalPage test initialization failure |
| Whitespace check | Passed with repository CRLF handling |

### Deployed E2E

The validated Dev package was registered from this branch's Debug `AppX`
directory.

| Suite | Passed | Failed | Skipped |
| --- | ---: | ---: | ---: |
| Agent pane interaction | 15 | 0 | 0 |
| Session list/view switching | 13 | 0 | 1 documented identity-gated case |
| Shared agent lifecycle | 1 | 0 | 0 |
| Agent pane CWD/session routing | 1 | 0 | 0 |
| **Total** | **30** | **0** | **1** |

Checklist item `C288`, **Background tabs do not pre-warm unused agent
helpers**, is marked `[x]` in the generated release report.

Artifacts:

- `test/e2e/artifacts/latency-rerun/report.html`
- `test/e2e/artifacts/latency-rerun/results.xml`
- `test/e2e/artifacts/latency-rerun/release-report.md`

## Remaining limitations

1. Copilot/provider `initialize` and `session/new` remain the dominant startup
   cost.
2. The first Debug launch can pay a large Defender/image-loading penalty.
3. PowerShell command recall is asynchronous but still expensive; moving it to
   master/process scope would avoid repeated helper warming.
4. Per-tab WSL or agent overrides remain lazy; only the process-global host
   default is pre-warmed.
5. Typewriter/render coalescing is not included.
6. Master lifetime remains tied to the Terminal process.
7. External session CLI discovery remains package-private unless
   `--master <pipe>` is supplied.

## Recommended follow-up

Run a Release-build ETW benchmark separating:

- process creation and Defender;
- provider `initialize`;
- provider `session/new`;
- helper first frame;
- first visible text.

The current results do not justify replacing the helper/master named-pipe
transport. Further latency work should target provider/session startup and a
shared command-recall cache.

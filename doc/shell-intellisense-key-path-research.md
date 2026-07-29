# Shell / Terminal IntelliSense 实现 Key Path 调研

> 调研日期：2026-07-27  
> 目标：定位主流实现中从按键到候选展示的关键链路，重点分析算法、时间预算、数据源，以及 LLM 是否进入按键关键路径。

## 1. 结论先行

### 1.1 最重要的实现结论

1. **传统 completion 基本都不是 LLM。** PowerShell、fish、zsh、bash、Nushell、VS Code Terminal Suggest、inshellisense 和 Warp 的 Tab completion 核心均为 parser/spec/history/filesystem + fuzzy matching。
2. **Shell-owned 与 Terminal-owned 的关键差异不是算法，而是输入缓冲区所有权。**
   - PSReadLine、fish、Reedline 和 Warp 拥有结构化输入 buffer，可以直接修改文本。
   - VS Code 和 inshellisense 面对真实 PTY，只能通过 backspace/delete/insert 等字节序列模拟按键。
3. **PSReadLine 给出了最严格的 predictor 时间约束：20ms。**
4. **异步实现普遍采用“最新请求获胜”。**
   - zsh：杀掉旧 fork。
   - fish：覆盖 pending job；运行超过 500ms 后允许新 worker 绕过旧任务。
   - Nushell：generation counter + 单 worker 合并队列。
   - VS Code/inshellisense：provider timeout + AbortController。
   - Warp：generation counter + abortable future。
5. **LLM inline completion 的核心不是让模型足够快，而是减少真正发送的请求。**
   - debounce；
   - cancel previous；
   - confidence gate；
   - prefix/cache reuse；
   - context truncation；
   - hard timeout；
   - stale response suppression。
6. **LLM 不应替代本地 completion。** 最合理的设计是 deterministic fast lane + optional LLM slow lane。

## 2. 关键链路总览

| 实现 | 输入触发 | Context | 候选算法 | 异步/取消 | 时间常量 | LLM |
|---|---|---|---|---|---|---|
| PSReadLine | 每次按键/Tab | PowerShell AST + tokens | history prefix + native completer + predictor | Task + cancellation | predictor 20ms | 仅插件可能使用 |
| fish | 每次按键/Tab | buffer + parser/tokenizer | history prefix + completion rules + natural sort | debounced thread pool | 500ms abandon；250ms pre-exec highlight | 否 |
| zsh-autosuggestions | 每次 ZLE widget | `BUFFER` + history | 最近历史 prefix | fork + kill superseded | 无固定 timeout | 否 |
| zsh compsys | Tab | Shell completion context | completer functions + `compadd` | 同步阻塞 | 无 | 否 |
| bash/readline | Tab | word boundary + compspec | shell function/command + alphabetical sort | 同步阻塞 | 无 | 否 |
| Nushell/Reedline | Tab/menu/hint | Nu AST + EngineState | typed completer + prefix/substring/nucleo fuzzy | worker + generation | cache TTL 5s；poll 100ms | 否 |
| VS Code Terminal | 自动/trigger char/Ctrl+Space | shell integration prompt model | providers + Fig specs + editor fuzzy score | provider race + timeout | auto 5s；manual 30s | 否 |
| inshellisense | 每次相关按键 | shadow xterm buffer + shell integration | Fig specs + priorities | AbortController | generator 默认 5s | 否 |
| Warp completion | Tab/自动 | owned code-editor buffer | Rust/JS signatures + fuzzy match | abortable future + generation | 未发现固定 debounce | 否 |
| Copilot inline | 每次编辑 | editor context | specialized model + post-filter | cancel previous + adaptive debounce | 约 25–275ms debounce，非官方源码分析 | 是 |
| Fig/Amazon Q inline | zsh buffer change | buffer + 最多 49 条历史 | completion model + validation | last-writer-wins debounce | 300ms debounce；5s IPC timeout | 是 |
| Warp Next Command | session/history context | active session + history | 未公开 | 推断为 idle/command boundary | 未公开 | 是 |

## 3. PowerShell / PSReadLine

### 3.1 Key path

```text
ReadKeyThreadProc
  -> InputLoop()
  -> ProcessOneKey()
  -> Render()
  -> ParseInput()                  // 整个 buffer 重新 parse
  -> Prediction.QueryForSuggestion()
       |-- history StartsWith scan
       `-- CommandPrediction.PredictInputAsync(timeout=20ms)
             -> one Task per ICommandPredictor
             -> WhenAny(all predictors, Delay(20ms))
             -> cancel unfinished predictors
  -> PredictionInlineView/ListView render
```

Tab completion 是另一条同步路径：

```text
Tab
  -> CommandCompletion.CompleteInputImpl()
  -> CompletionAnalysis.GetResults()
  -> CompletionCompleters
  -> completion menu
```

### 3.2 最关键源码

| 责任 | Repo / path |
|---|---|
| 按键循环 | `PowerShell/PSReadLine: PSReadLine/ReadLine.cs`，`InputLoop()` |
| 每次 render 前 parse | `PowerShell/PSReadLine: PSReadLine/ReadLine.cs`，`ParseInput()` |
| completion context | `PowerShell/PowerShell: src/System.Management.Automation/engine/CommandCompletion/CompletionAnalysis.cs` |
| Tab completion 入口 | `PowerShell/PowerShell: .../CommandCompletion/CommandCompletion.cs`，`CompleteInputImpl()` |
| 各类 completer | `PowerShell/PowerShell: .../CommandCompletion/CompletionCompleters.cs` |
| predictor 接口 | `PowerShell/PowerShell: .../PredictionSubsystem/ICommandPredictor.cs` |
| 20ms 调度 | `PowerShell/PowerShell: .../PredictionSubsystem/CommandPrediction.cs` |
| inline/list 实现 | `PowerShell/PSReadLine: PSReadLine/Prediction.Views.cs` |
| 接受建议 | `PowerShell/PSReadLine: PSReadLine/Prediction.cs` |

### 3.3 算法

- 当前输入每次 render 都重新调用 PowerShell parser，生成 AST、tokens 和 parse errors。
- `PredictionContext` 包含 AST、tokens、cursor、当前 token 和 related ASTs。
- history prediction 是最近优先的 `StartsWith` 匹配。
- inline 模式中 plugin suggestion 优先于 history。
- Tab completion 由大量类型化 completer 处理命令、参数、路径、enum、type、native executable 等。

### 3.4 时间与并发

- predictor 默认硬截止时间：**20ms**。
- 每个 predictor 并行运行。
- 20ms 内未完成的 predictor 被取消，结果不展示。
- 当队列中超过 10 个按键且距上次 render 小于 50ms 时，PSReadLine 可跳过本次 render，降低 paste/快速输入时的重绘压力。
- displayed/accepted/executed feedback 放入 ThreadPool，不阻塞输入。

### 3.5 数据源

- session/persisted history；
- PowerShell parser 和 command discovery；
- filesystem；
- cmdlet/parameter/type/enum metadata；
- `ICommandPredictor` plugins。

### 3.6 是否 LLM-driven

内置逻辑不是 LLM。`ICommandPredictor` 可以由第三方实现为 LLM predictor，但仍受 PSReadLine 的 20ms client deadline 约束。远程 LLM 如果不做缓存或异步预取，通常无法在该路径中稳定返回。

## 4. fish

### 4.1 Key path

```text
handle_char_event()
  -> insert_char()/insert_string()
  -> color_suggest_repaint_now()
       |-- autosuggestion debouncer
       |     -> history prefix search
       |     `-- completion fallback
       |-- highlight debouncer
       `-- repaint cached/stale result

background result ready
  -> event_signaller
  -> service_debounced_results()
  -> merge result
  -> repaint
```

### 4.2 最关键源码

| 责任 | Path |
|---|---|
| 输入与调度 | `fish-shell/fish-shell: src/reader/reader.rs` |
| completion engine | `src/complete.rs` |
| parser/tokenizer | `src/parse_tree.rs`, `src/tokenizer.rs` |
| debounce | `src/threads/debounce.rs` |
| I/O worker | `src/reader/iothreads.rs` |
| highlighting | `src/highlight/highlight.rs` |

### 4.3 算法

- autosuggestion 先搜索 history prefix。
- history 无结果时，可复用完整 completion engine 生成 suggestion。
- completion 组合 command-specific rules、wrap chain、filesystem、variables 和 usernames。
- 排序先保留最佳 rank，去重，再做 natural sort。
- autosuggestion 另有同大小写优先、备份文件降权和重复参数降权。

### 4.4 时间与并发

- autosuggestion、highlight 和 history pager 各有独立 debounce channel。
- background thread pool 最少 1、最多 16 worker。
- 当前 job 运行超过 **500ms** 后可被标记为 abandoned；后续请求可创建新 worker，不再被慢任务堵塞。
- Enter 前最多等待 **250ms** 获取完整 highlighting；超时后使用无 I/O 的同步 fallback。
- UI 采用 stale-while-revalidate：先画缓存结果，后台完成后再 repaint。

### 4.5 数据源与 LLM

数据来自 history、completion definitions、autoload scripts 和 filesystem。core 中无 LLM。

## 5. zsh

### 5.1 zsh-autosuggestions

```text
KeyPress
  -> wrapped ZLE widget
  -> original widget updates BUFFER
  -> if reusable: retain existing suggestion
  -> otherwise cancel old fork
  -> fork subshell
       -> history strategy: most recent "$BUFFER*"
  -> zle -F pipe callback
  -> set POSTDISPLAY
```

关键文件：

- `zsh-users/zsh-autosuggestions: src/bind.zsh`
- `src/widgets.zsh`
- `src/fetch.zsh`
- `src/strategies/history.zsh`
- `src/async.zsh`

算法非常简单：对 history 做 prefix glob，选择最近匹配项。异步通过 fork + pipe 实现；新请求到达时关闭 fd 并 `kill -TERM` 旧进程。没有固定 timeout。

### 5.2 zsh compsys

```text
Tab
  -> expand-or-complete
  -> _main_complete
  -> completer chain
  -> compadd
  -> match list/menu
```

关键点：

- 整条链路是同步 Shell function。
- 慢 completer 会直接阻塞输入。
- `_store_cache` / `_retrieve_cache` 可将昂贵结果缓存到 `~/.zcompcache`。
- 没有 LLM、debounce 或通用 timeout。

## 6. bash / Readline / bash-completion

### 6.1 Key path

```text
Tab
  -> rl_complete
  -> rl_complete_internal
  -> gen_completion_matches
       -> rl_attempted_completion_function
            -> bash attempt_shell_completion
            -> programmable_completions
            -> compspec / complete -F / complete -C
       -> fallback filename completion
  -> remove_duplicate_matches
  -> alphabetical qsort
  -> insert single match or display list
```

首次遇到未知命令时，bash-completion 使用 lazy-load-then-retry：

```text
complete -D -F _comp_complete_load
  -> _comp_load searches completion directories
  -> source per-command script
  -> script registers real complete -F handler
  -> return 124
  -> bash retries completion
```

### 6.2 关键源码

- GNU Readline authoritative source: [Savannah Readline](https://git.savannah.gnu.org/cgit/readline.git/)
- `readline/complete.c`: `rl_complete_internal()`、`gen_completion_matches()`
- bash `bashline.c`: `attempt_shell_completion()`
- `scop/bash-completion: bash_completion`: `_comp_complete_load()`、`_comp_load()`

### 6.3 算法、时间与数据源

- 默认 alphabetical sort，无 fuzzy score。
- 候选超过默认 100 项时询问是否全部展示。
- 没有 async、debounce 或 timeout；只有 signal interruption。
- 数据源包括 filesystem、compspec、Shell functions、external command 和 per-command completion scripts。
- 无 LLM；若 completion function 调网络或模型，整个 Readline 会同步阻塞。

## 7. Nushell / Reedline

### 7.1 Key path

```text
Reedline read_line_helper()
  -> poll_completion() [Idle/Pending/Ready]
  -> process input
  -> request completion(query, generation)
       -> 5s cache lookup
       -> send latest query to worker
       -> return stale result or Pending

worker
  -> drain queue, keep newest query only
  -> Nu parser
  -> specialized completer
  -> Prefix/Substring/Fuzzy matching
  -> cache result
  -> publish generation

main loop
  -> poll sees Ready generation
  -> update menu and repaint
```

### 7.2 关键源码

| 责任 | Path |
|---|---|
| event loop | `nushell/reedline: src/engine.rs` |
| worker/cache | `nushell/nushell: crates/nu-cli/src/completions/completer.rs` |
| matching | `crates/nu-cli/src/completions/completion_options.rs` |
| history hint | `nushell/reedline: src/hinter/cwd_aware.rs` |
| menus | `nushell/reedline: src/menu/` |

### 7.3 算法与时间

- Nu parser 将输入路由到 command、flag、variable、cell path、file、operator、attribute 和 custom completer。
- matching 支持 prefix、substring 和 `nucleo_matcher` fuzzy。
- smart sort 按 fuzzy score 降序，再 alphabetical tie-break。
- background worker 通过 generation 丢弃 stale result。
- completion cache TTL：**5s**。
- async work pending 时 event loop 默认每 **100ms** poll。
- 非交互 blocking completion fallback 最长 **30s**。

### 7.4 数据源与 LLM

数据来自 Nu AST/EngineState、PATH、filesystem、history 和 user custom completion。无内置 LLM；用户自定义 completer 理论上可调用模型。

## 8. VS Code Terminal Suggest

### 8.1 Key path

```text
keystroke / shell integration update
  -> TerminalSuggestAddon._sync()
  -> TerminalCompletionService.provideCompletions()
       -> fan out providers
       -> race each provider against timeout
       |-- built-in providers
       |-- shell integration providers
       |-- Fig-spec extension
       `-- optional LSP-backed provider
  -> TerminalCompletionModel
       -> fuzzy score
       -> terminal-specific secondary sort
  -> SimpleSuggestWidget
  -> acceptSelectedSuggestion()
  -> sendText(backspaces/deletes/insertion/optional CR)
  -> real shell receives synthetic input
```

### 8.2 关键源码

| 责任 | Path |
|---|---|
| trigger/context | `microsoft/vscode: src/vs/workbench/contrib/terminalContrib/suggest/browser/terminalSuggestAddon.ts` |
| provider fan-out | `.../terminalCompletionService.ts` |
| ranking | `.../terminalCompletionModel.ts` |
| shared fuzzy matcher | `src/vs/workbench/services/suggest/browser/simpleCompletionModel.ts` |
| widget | `.../simpleSuggestWidget.ts` |
| Fig provider | `extensions/terminal-suggest/src/fig/` |
| generators | `extensions/terminal-suggest/src/fig/execute.ts` |
| PATH cache | `extensions/terminal-suggest/src/env/pathExecutableCache.ts` |

### 8.3 算法

- 复用编辑器 IntelliSense 的 `fuzzyScore` / `fuzzyScoreGracefulAggressive`。
- terminal-specific secondary sort 包括：
  - inline suggestion boost；
  - punctuation penalty；
  - Windows 下 `.ps1`/`.exe` boost；
  - git `main`/`master` boost；
  - directory depth ordering。
- VS Code 内置 Terminal Suggest vendor 了 Fig completion spec 格式。

### 8.4 时间、缓存与数据源

- 自动触发 provider timeout：**5s**。
- 显式手动触发 timeout：**30s**。
- Fig generator subprocess：Windows 最长 **20s**，其他系统 **5s**。
- `$PATH` executable 和 shell globals cache TTL：**7 天**，并提供手动清缓存。
- 数据源包括 shell integration、PATH、filesystem、Fig specs、dynamic generators、git/npm 等 project context。

这些 timeout 是错误隔离上限，不是理想 UI latency。首批本地候选仍应在 50–100ms 内出现。

### 8.5 是否 LLM-driven

Terminal completion 核心不是 LLM。Copilot/Agent 与 Terminal Suggest 是独立功能。

## 9. Microsoft inshellisense

### 9.1 Key path

```text
PTY output
  -> shadow @xterm/headless buffer
  -> CommandManager determines current real input vs shell ghost text

keypress
  -> SuggestionManager._loadSuggestions()
       -> abort previous AbortController
       -> runtime.getSuggestions()
            -> walk @withfig/autocomplete spec
            -> run subcommand/arg/option matcher
            -> optional dynamic generator subprocess
       -> filter/priority
  -> ANSI popup render
  -> applyReplacement()
  -> term.write()
  -> PTY receives inserted bytes
```

### 9.2 关键源码

- `microsoft/inshellisense: src/isterm/commandManager.ts`
- `src/ui/suggestionManager.ts`
- `src/runtime/runtime.ts`
- `src/runtime/generator.ts`
- `src/ui/ui-root.ts`

### 9.3 时间、数据源和 LLM

- 每次新输入创建新 `AbortController`，取消旧 request。
- dynamic generator 默认 **5s** timeout，超时后 kill subprocess。
- 数据直接来自 `@withfig/autocomplete` specs、filesystem 和 generator output。
- 无 LLM。

## 10. Warp

> Warp client 在 2026 年已开源；本节源码结论基于 `warpdotdev/warp`，调研时使用 commit `c20645b9b0425dd8fb49fe9f9d08280e1e725ce9`。

### 10.1 Completion key path

```text
Tab
  -> TuiInputAction::Complete
  -> RequestShellCompletion event
  -> request_shell_completion()
  -> warp_completer::suggestions()
       -> native Rust or JS/Fig-compatible signatures
       -> shell/custom generator
       -> CaseSensitive/CaseInsensitive/Fuzzy matcher
       -> generation/current-request check
  -> common-prefix insertion or menu
  -> TuiCompletionAcceptance
  -> model.user_insert()
```

### 10.2 关键源码

| 责任 | Path |
|---|---|
| completion API | `warpdotdev/warp: crates/warp_completer/src/completer/mod.rs` |
| matching | `crates/warp_completer/src/completer/matchers.rs` |
| fuzzy engine | `crates/fuzzy_match/src/lib.rs` |
| native/JS signatures | `crates/warp_completer/src/signatures/{legacy,v2}` |
| QuickJS conversion | `crates/warp_completer/src/signatures/v2/js.rs` |
| TUI request | `crates/warp_tui/src/terminal_session_view/completions.rs` |
| acceptance | `crates/warp_tui/src/input/view.rs` |
| GUI suggestions | `app/src/input_suggestions.rs` |
| LLM next-command | `app/src/ai/predict/next_command_model.rs` |

### 10.3 算法、异步和数据源

- Warp completion 使用 native Rust signatures 和 Fig-compatible JS signatures。
- JS 通过 embedded QuickJS 执行，不需要 Node subprocess。
- generator 可以是 Shell command 或 JS callback。
- matcher 支持 case-sensitive、case-insensitive 和 fuzzy。
- request 使用 monotonic generation + abortable future 取消 stale work。
- 未在 completion path 中发现固定 debounce。
- 因 Warp 拥有结构化 editor buffer，接受 suggestion 是直接 `model.user_insert()`，而不是向 PTY 模拟 backspace。

### 10.4 LLM 边界

- Tab completion：非 LLM。
- local history autosuggestion：非 LLM。
- `NextCommandModel`：LLM，使用 terminal history/context 调用服务端模型。
- agent/composer：独立于 completion。
- 已发现参数 generator validation 的 **150ms** timeout，但未发现 Next Command 的公开请求 deadline。

## 11. LLM-driven inline suggestion

### 11.1 GitHub Copilot

#### 已官方验证

- `github/copilot-language-server-release` 的 `textDocument/inlineCompletion` 使用 cancel-previous 语义。
- 新请求会取消旧请求；client 被明确建议尽快发送 `$/cancelRequest`。
- protocol 记录 displayed、partial accepted length 和 full acceptance。
- Next Edit Suggestions 使用独立 request method，与快速 inline completion 分离。
- GitHub 公布的 2025 模型更新结果：latency 降低 35%、throughput 提升 3 倍，并以 time-to-first-token、shown rate、accepted-and-retained characters 等指标评估。

#### 高可信但非官方的 shipped-extension 分析

公开的 extension bundle 逆向分析显示：

```text
adaptive debounce =
    25 + 250 / (1 + (score / 0.3475)^7)
```

- 高 confidence：接近 **25ms**。
- 低 confidence：接近 **275ms**。
- 没有 adaptive feature 时 fallback 约 **75ms**。
- 低于 contextual filter threshold 的 request 可在发送前直接取消。
- 保留少量 exploration traffic，避免模型只学习高置信度样本。

缓存包括：

1. last `(prefix, suffix)` exact cache；
2. 约 100 项 LRU；
3. **typing-as-suggested**：如果用户继续输入的是当前 ghost text 的 prefix，直接裁剪旧 suggestion，不发新网络请求。

取消检查分布在 debounce、prompt extraction、stream、post-processing 等多个 await boundary。

> 注意：具体 threshold 和公式来自非官方逆向分析，可能随实验变化，不应当作为稳定产品常量。

### 11.2 Fig / Amazon Q Developer CLI inline completion

这是目前最有价值的开源终端 LLM inline reference：

Repo：`aws/amazon-q-developer-cli-autocomplete`

```text
zsh buffer change
  -> inline-shell-completion subprocess
  -> figterm IPC
  -> shared LAST_RECEIVED timestamp
  -> 300ms debounce
  -> build context from buffer + history
  -> call completion model
  -> validate
  -> cache alternatives
  -> return insert_text
```

关键常量：

| 项目 | 数值 |
|---|---:|
| 默认 debounce | **300ms** |
| Shell CLI -> figterm IPC hard timeout | **5s** |
| history context | 最多 **49 条** |
| throttling retry | 最多 **3 次** |
| telemetry flush rate | 最多 **2 events/s** |

关键实现：

- `crates/fig_integrations/src/shell/inline_shell_completion/strategies/inline.zsh`
- `crates/q_cli/src/cli/internal/inline_shell_completion.rs`
- `crates/figterm/src/inline/mod.rs`

其 debounce 是 last-writer-wins：

1. 每次输入更新共享 timestamp。
2. task sleep 300ms。
3. timestamp 已变化则说明有更新输入，旧 task 返回空结果。
4. 只有最新且稳定 300ms 的 request 调模型。

cache 每个 prompt 清空，避免跨命令污染；model output 经过 validation 后才显示或缓存。

### 11.3 Warp Next Command

官方确认它基于 active terminal session 和 command history 使用 AI，并将结果插入 buffer，用户仍需按 Enter。公开资料没有：

- debounce；
- p50/p95；
- request timeout；
- model size；
- cache policy。

根据 UI 更可能是 idle 或 command-boundary trigger，而不是每个 keystroke 请求；这是推断，不是官方事实。

## 12. LLM response 时间控制方法

### 12.1 真正有效的控制点

| 控制点 | 作用 |
|---|---|
| Fast local lane | 网络未返回时仍有即时 suggestion |
| Debounce | 用户持续输入时不发无效请求 |
| Adaptive debounce | 高置信请求更快，低置信请求更容易被下一按键取消 |
| Cancel previous | 停止浪费带宽和模型计算 |
| Generation ID | 即使 cancel 失败，也拒绝 stale response |
| Confidence gate | 请求发送前淘汰低价值输入 |
| Exact/prefix cache | 高频编辑不访问网络 |
| Typing-as-suggested | 用户沿着旧建议输入时复用剩余文本 |
| Context truncation | 限制 prompt token 和 prefill latency |
| Specialized small model | 降低 TTFT 和生成时长 |
| Small output limit | Shell suggestion 通常只需要一行 |
| Streaming | 尽早展示稳定 prefix，但必须防止闪烁 |
| Hard timeout | 慢请求降级为无建议 |
| Validation | 防止低质量/危险输出进入 cache |

### 12.2 推荐预算

| 阶段 | 目标 |
|---|---:|
| 按键与终端回显 | <16ms，永不等待 provider |
| exact/prefix cache | <5ms |
| history/local predictor | <=20ms |
| deterministic popup 首批 | <50ms 理想，<100ms 上限 |
| LLM debounce | adaptive 30–150ms 起步 |
| LLM soft target | p50 <=300ms |
| LLM hard timeout | 2–5s |
| agent/Quick Fix | 不进入 keystroke path，可 streaming |

### 12.3 推荐状态机

```text
IDLE
  -> BUFFER_CHANGED
  -> LOCAL_FAST_LANE
       |-- local result -> render immediately
       `-- cache miss
  -> ADAPTIVE_DEBOUNCE
       |-- newer generation -> cancel
       `-- stable
  -> CONFIDENCE_GATE
       |-- low score -> suppress
       `-- high score
  -> LLM_REQUEST
       |-- newer generation -> cancel/drop
       |-- timeout/error -> keep local result
       `-- success
  -> VALIDATE
       |-- invalid/unsafe -> suppress
       `-- valid
  -> RENDER_LLM_GHOST_TEXT
       |-- user types matching prefix -> trim cached suggestion
       |-- user diverges -> clear and cancel
       |-- partial/full accept -> feedback
       `-- command executes -> retained/success feedback
```

## 13. 对 Intelligent Terminal 的关键建议

### 13.1 Completion coordinator

建议将 provider 分成两个 scheduler：

```text
Fast lane, deadline 20–100ms
  - history
  - PATH/builtins
  - filesystem
  - native shell completion
  - Fig-compatible spec
  - git/project cache

Slow lane
  - dynamic subprocess generators
  - remote/cloud resource lookup
  - LLM inline prediction
  - ACP agent
```

fast lane 每个 provider 都有 deadline，先完成先展示；slow lane 不影响输入和首屏候选。

### 13.2 Buffer ownership

Intelligent Terminal 当前与 VS Code/inshellisense 一样面对真实 PTY，并不拥有 Shell line editor buffer。因此第一版接受 suggestion 很可能需要：

- 通过 shell integration 获取可靠的 buffer/cursor；
- 或发送 backspace/delete/insert 序列；
- 对 PowerShell/bash/zsh/fish/cmd 分别处理 quoting、multiline 和 shell-native ghost text；
- 使用 request generation 防止 UI 所见 buffer 与 Shell 实际 buffer 不一致。

长期如果要达到 Warp 的可靠性，需要建立 Shell bridge，使 Terminal 能以结构化方式读取和修改 line buffer，而不是推断终端 cell。

### 13.3 Spec 选择

VS Code、inshellisense 和 Warp 都验证了 Fig-compatible spec 的价值。建议：

- 先兼容 Fig schema，不新建独占 schema；
- generator 默认 sandbox/timeout；
- description lazy resolve；
- provider 输出统一转换为内部 `CompletionItem`；
- 对 spec 来源建立版本、签名和信任策略。

### 13.4 LLM

- ACP agent 不放入每次按键路径。
- 若做 inline LLM，使用专用轻量模型/API，而非完整 agent session。
- 本地 suggestion 永远先显示。
- 默认 100–300ms 停顿后才发请求。
- 每次 buffer change 取消旧 request，并用 generation 丢弃迟到 response。
- 复用 last suggestion 和 alternate candidates。
- 输出限制为一条命令或短 suffix。
- 默认只插入，不执行。

## 14. 实现优先级

1. **先实现 deterministic coordinator、provider API、generation/cancellation 和 metrics。**
2. **复用 Fig specs，打通 PowerShell 与 WSL Shell context。**
3. **实现 Terminal-owned popup 和 safe PTY replacement。**
4. **增加 local history inline prediction，验证 20ms fast lane。**
5. **最后增加 LLM slow lane，先做 idle Next Command，再考虑 per-keystroke inline。**

## 15. 参考仓库

- [PowerShell/PSReadLine](https://github.com/PowerShell/PSReadLine)
- [PowerShell/PowerShell](https://github.com/PowerShell/PowerShell)
- [fish-shell/fish-shell](https://github.com/fish-shell/fish-shell)
- [zsh-users/zsh-autosuggestions](https://github.com/zsh-users/zsh-autosuggestions)
- [zsh-users/zsh](https://github.com/zsh-users/zsh)
- [GNU Readline](https://git.savannah.gnu.org/cgit/readline.git/)
- [scop/bash-completion](https://github.com/scop/bash-completion)
- [nushell/reedline](https://github.com/nushell/reedline)
- [nushell/nushell](https://github.com/nushell/nushell)
- [microsoft/vscode](https://github.com/microsoft/vscode)
- [microsoft/inshellisense](https://github.com/microsoft/inshellisense)
- [withfig/autocomplete](https://github.com/withfig/autocomplete)
- [warpdotdev/warp](https://github.com/warpdotdev/warp)
- [github/copilot-language-server-release](https://github.com/github/copilot-language-server-release)
- [aws/amazon-q-developer-cli-autocomplete](https://github.com/aws/amazon-q-developer-cli-autocomplete)
- [GitHub Copilot performance engineering](https://github.blog/ai-and-ml/github-copilot/the-road-to-better-completions-building-a-faster-smarter-github-copilot-with-a-new-custom-model/)

## 16. 证据强度与限制

- PSReadLine、fish、zsh、Nushell、VS Code、inshellisense、Warp 和 Fig/Amazon Q 的 key path 均来自源码。
- Bash/Readline 应以 Savannah 为权威来源；部分分析使用了内容一致的 GitHub mirror 进行检索。
- Copilot cancel-previous protocol 和相对性能改进来自官方资料。
- Copilot 25–275ms adaptive debounce、约 75ms fallback、contextual filter 和 LRU 细节来自 shipped extension 的非官方逆向分析，可信但不是稳定 API。
- Warp 未公开 Next Command 的绝对 latency、debounce、timeout 和模型细节。
- 除 PSReadLine 20ms 外，厂商普遍不公开 completion p50/p95/p99；本报告中的 Intelligent Terminal 预算是工程目标，不是市场统一标准。

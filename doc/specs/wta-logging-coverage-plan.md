# WTA 日志覆盖改进方案

状态:**已实现** · 分支:`dev/kaitao/logs` · 适用版本:wta `0.1.0`

> 实现落在:`tools/wta/src/logging.rs`(info 默认 + cli 日滚动 + `housekeeping`)、
> `tools/wta/src/main.rs`(`process_label` + `main()` 顶部唯一一次 init + 顶层
> 错误日志)、`tools/wta/src/master/mod.rs`(target=master 失败日志 + initialize
> 失败内联日志)、`tools/wta/src/protocol/acp/client.rs`(helper initialize 失败
> 内联日志)、`CLAUDE.md`(Logs 一节)。`cargo build` 通过,`logging::` 单测 5/5 通过。

## 背景与结论

排查「很多人的日志里只有 agent hook 的 trace」后确认:

- **不是权限问题。** 日志目录用 `std::fs::create_dir_all` 创建,根路径由
  `runtime_paths` 解析(packaged 进程落在 `LocalCache\Local\IntelligentTerminal\logs\`,
  unpackaged 退到 bare `%LOCALAPPDATA%`);拿不到 root 还会 fallback 到 `temp_dir`。
  两种身份的进程对该目录都可写。CLAUDE.md 提到的 `0x80073D54` 是 COM 激活
  (APPMODEL_ERROR_NO_PACKAGE),与写日志无关。
  > 注:#124(已并入 main)把日志根从 bare `%LOCALAPPDATA%` 改为包私有的
  > `LocalCache\Local`(`intelligent_terminal_local_root()`)。本方案与之**正交**,
  > `logging::init` 只是改调该函数,housekeeping 仍作用于解析出来的同一个 `logs\`。
- **是覆盖面 + 级别问题。** 真正写日志的只有 6 个入口,大量入口根本不
  初始化日志;且 release 默认级别 `warn` 会吞掉 info。

`hook-trace.log` 由 PowerShell 钩子 `send-event.ps1` 在每次 agent
prompt/stop/session 事件时写入,所以总是满的;而 Rust wta 自身的
`wta-*.log` 只在少数命令里才产生 —— 这就是「只剩 hook trace」的成因。

## 现状

### 日志文件与写入方

| 文件 | 写入方 | 来源 |
|---|---|---|
| `wta-main_master.log` | Rust wta | `logging::init("main_master")` (`master/mod.rs:1152`) |
| `wta-main_helper.log` | Rust wta | `logging::init("main_helper")` (`main.rs:1690`) |
| `wta-main.log` | Rust wta | `logging::init("main")` (`main.rs:1636`) |
| `wta-delegate.log` | Rust wta | `logging::init("delegate")` (`main.rs:1525`) |
| `wta-probe.log` | Rust wta | `logging::init("probe")` (`main.rs:863`) |
| `wta-install-hooks.log` | Rust wta | `logging::init("install-hooks")` (`main.rs:905`) |
| `hook-trace.log` | PowerShell 钩子 | `wt-agent-hooks/*/send-event.ps1:70`(非 Rust) |
| `wta-agent-pane.log` | C++ 侧 | `_AutoCreateHiddenAgentPane` 等(非 Rust) |

### 问题清单

| # | 问题 | 位置 | 影响 |
|---|---|---|---|
| A | `main()` 在 dispatch 之前(CLI 解析、locale、ETW 注册、legacy flag)无日志 | `main.rs:569`–600 | 最早期失败/panic 无痕,仅剩 stderr |
| B | 所有 `wtcli` 类短命命令不调用 `logging::init` | `main.rs:601` match 各分支(`Listen`/`List*`/`Sessions`/`capture-pane`/`split-pane`/`new-tab`…) | autofix 链路(`wtcli listen --json`)与 discovery 命令出错时 Rust 侧无日志 |
| C | release 默认级别 `warn`,且 CLAUDE.md 写的是 `info`(对不上) | `logging.rs:14` | 不设 `WTA_LOG` 时 info 全丢;文档误导 |
| D | 多个 helper(每 tab 一个)共写同一 `wta-main_helper.log` | `main.rs:1690` 固定文件名 | 多 tab 时行交错、无法归属;跨进程 append 可能撕裂行 |
| E | 版本升级后旧版本日志无人清理,持续累积 | 无对应代码 | 磁盘占用增长,旧版本噪声干扰排查 |

## 改进方案

### 决策(已确认)

- B:wtcli 类命令统一落到 **`wta-cli.log`**。
- C:release 默认级别 **抬到 `info`**。
- D:helper 日志 **按 PID 分文件**。
- E:版本升级时 **清理旧版本日志**(本方案新增设计)。
- 交付:**本轮只出方案文档**,暂不动代码。

### 改动 A + B:把日志初始化提到 `main()` 顶部,全程只初始化一次

`tracing_subscriber` 的全局 subscriber 只能 `.init()` 一次。正确做法:

```
Cli::parse()
  → process_label(&cli)          // 从 cli 推导进程标签
  → let _guard = logging::init(&label)   // 唯一一次,紧跟 parse
  → set_locale / telemetry::register / legacy flag / match dispatch
  → _guard 由 main() 持有到进程结束(确保 non-blocking appender flush)
```

随之删除散落在 6 个 handler 内部的 `logging::init` 调用
(`main.rs:863/905/1525/1636/1690`、`master/mod.rs:1152`)。

**`process_label` 推导表**(基于 `cli.master` / `cli.connect_master` / `cli.command`):

| 入口判定 | 标签 | 文件 |
|---|---|---|
| `cli.master.is_some()` → master 模式 | `main_master` | `wta-main_master.log` |
| `cli.connect_master.is_some()` → helper 模式 | `main_helper-{pid}` | `wta-main_helper-{pid}.log`(见 D) |
| 无 flag + 无 subcommand → default TUI | `main` | `wta-main.log` |
| `Command::Delegate` | `delegate` | `wta-delegate.log` |
| `Command::ProbeModels` | `probe` | `wta-probe.log` |
| `Command::Hooks(Install)` | `install-hooks` | `wta-install-hooks.log` |
| 其余全部(`Listen`/`List*`/`Sessions`/`Hooks(Status/Uninstall)`/`capture-pane`/`split-pane`/`new-tab`/`--info`/`--test-pipe`…) | `cli` | `wta-cli.log` |

> 注:`--info` / `--test-pipe` 在 match 之前处理(`main.rs:593-598`),
> 因此 init 必须先于它们。提前 init 后,这两条 legacy 路径也被覆盖。

**附带修掉的隐患**:短命命令现在若在 handler 内 drop `WorkerGuard`,
non-blocking appender 可能来不及 flush;改由 `main()` 持有 guard 直到退出即可。

### 改动 C:release 默认级别 `warn` → `info`

`logging.rs:14` 的 release 分支由 `"warn"` 改为 `"info"`:

```rust
pub(crate) fn default_filter_directive(debug_assertions: bool) -> &'static str {
    if debug_assertions { "debug" } else { "info" }   // was "warn"
}
```

同步更新:

- 单测 `release_build_default_is_warn` / `release_default_filter_rejects_info_and_below`
  改为断言 release 默认放行 info(`max_level_hint == INFO`)。
- CLAUDE.md「Logs」一节:把「default: info」更正为「debug 构建=`debug`,
  release 构建=`info`;`WTA_LOG`/`RUST_LOG` 可覆盖」,与代码一致。

`WTA_LOG` / `RUST_LOG` 覆盖逻辑不变。

### 改动 D:helper 日志按 PID 分文件

helper 标签改为 `format!("main_helper-{}", std::process::id())`,
产出 `wta-main_helper-{pid}.log`,彻底消除多 tab 交错 / 归属问题。
master 仍是单例,保持单文件 `wta-main_master.log`。

**保留 `wta-cli.log` 单文件的权衡**(B 的选择):wtcli 命令高频且短命,
多进程并发 append 同一文件在 Windows 上可能交错。缓解:

- 每行已带时间戳;确保每行带 `pid=` 字段便于过滤。
- 单次 wtcli 调用日志量小、写入快,撕裂概率低,作为可接受的取舍。

> 代价:per-PID helper 文件会随 tab 反复开关而无界增长 —— 由改动 E 的
> 保留策略统一回收。

### 改动 E:版本升级清理 + per-PID 文件保留策略

> **不自造日志库。** 日志栈全用现成 crate(`tracing` / `tracing-subscriber`
> / `tracing-appender` 0.2.5);按天滚动用 `Rotation::DAILY`,滚动文件保留用
> 原生 `Builder::max_log_files`(见改动 D 的 cli 配置)。`housekeeping()` 只做
> 滚动库覆盖不到的**跨文件名清理**(版本升级清库 + per-PID helper 回收),
> 即十余行 `read_dir` + `remove_file`,不涉及任何日志库逻辑。

新增 `logging::housekeeping()`,在 `logging::init` 内、构建 appender 之前
调用一次。两件事:

**(1) 版本升级清理**

- 版本号取 `env!("CARGO_PKG_VERSION")`。
- 在 logs 目录维护标记文件 `.wta-log-version`(内容=上次运行的版本)。
- 启动时比对:版本不同 ⇒ 删除上一版本残留的 `wta-*.log`,然后写入新版本号。
- **并发安全**:升级后多个新版本进程会同时启动(master + 多 helper + cli),
  需保证只清一次。用原子声明:
  - `OpenOptions::new().write(true).create_new(true)` 抢占一个
    `.wta-log-cleanup.lock`;抢到的进程执行清理并更新 `.wta-log-version`,
    其余进程跳过。
  - 清理用 `remove_file`,对被其他进程占用的文件(升级过渡期仍存活的旧进程)
    会失败 —— 忽略即可,安全。
- **清理方式(已定)**:**直接 `remove_file` 删除**,不归档、不保留旧版本。

**(2) per-PID helper 文件保留**(配合改动 D)

清理时一并回收陈旧的 `wta-main_helper-*.log`:

- 优先按「PID 已不存在」回收(进程已退出的文件可删)。
- 兜底按时间(已定):**删除 mtime 早于 3 天的 `wta-main_helper-*.log`**。

> 实现上 (1)(2) 共用同一把 `.wta-log-cleanup.lock`,在同一次 housekeeping
> 里顺序执行,避免多进程重复扫描。

### 改动 F:master / helper 两侧连接日志必须「全」(硬要求)

> 业务硬要求:master 侧和 helper 侧的日志要**完整**,**尤其是连接、连接失败**
> 这类事件 —— 排查 agent pane / autofix 不工作时,首要看的就是连接链路。

不能只在成功路径打日志,**失败/异常路径必须同等覆盖**。逐项确保:

**wta-master 侧**(`master/mod.rs`):

- 命名管道创建、accept loop 每次 accept(成功与失败都打)。
- 每个 helper 连接的建立 / 断开,带 `helper_id`、`window_id`、`live_helpers=` 计数。
- agent CLI 子进程 spawn:命令行、成功、**失败(含 errno/HRESULT、stderr)**。
- `session/new`、`session/load` 转发的成功与**失败**(超时、agent 拒绝、CLI 退出)。
- agent CLI 意外退出 / 崩溃的检测(`target=agent_stderr` 不能吞)。

**wta-helper 侧**(`main.rs:run_default_tui_over_pipe`、`protocol/acp/client.rs`):

- 管道连接到 master:尝试、成功、**失败与重试**(管道名、错误码、重试次数)。
- 「无 WT 协议连接」「无 wt_pipe_channel」这类降级路径(现在是 `WARN`,保留)。
- ACP `initialize` / `session/new` 的请求-响应,**含失败原因**。
- ACP 会话状态机迁移到 `Failed` / 断连时,打出原因而不仅仅是状态值。

失败路径统一用 `WARN`/`ERROR` 级别 + 结构化字段(`error=`、`code=`、
`elapsed_ms=` 等),保证即便默认级别下也可见(release 默认已抬到 `info`,
warn/error 必然落盘)。

### 改动 G:隐私与级别审计(全量 `tracing::` 复查)

原则:**用户实际内容(prompt / agent 回复 / 终端输出 / 输入 / 标题 / 键入)
最多只在 `trace`;`warn`/`error` 这类 critical 必须落盘。** info(release 默认)
和 debug(一个环境变量即开)都不放原文。

**统一约定**:新增 `acp_trace_content()`(`target=*.content`,trace 级)承载敏感
内容;各处保留「长度/计数/枚举/id」在 debug/info,原文挪到 trace。

**内容下沉到 trace(原 info/debug → trace 或改记长度):**

| 位置 | 原级别 | 内容 | 处理 |
|---|---|---|---|
| `app.rs` handle_key | info | 每次按键 `KeyCode`(可重建 prompt) | → trace |
| `client.rs` log_turn_trace | info | prompt body_head/body_tail | info 只留 `prompt_len`,原文 → trace |
| `client.rs` acp_log_built_prompt | debug | 完整拼装 prompt(含终端 buffer) | → trace |
| `client.rs` session_notification | debug | 完整 SessionUpdate(agent 消息/思考/计划) | debug 只留 `kind`,原文 → trace |
| `client.rs` request_permission / create_terminal | debug | 工具标题 / 命令行+args | debug 留面包屑+计数,原文 → trace |
| `client.rs` prompt_timing(prompt_received/first_tool_call/permission/complete) | debug | prompt/标题/描述 preview | timing 留 debug,preview → trace |
| `main.rs` run_delegate | info | `?prompt` 全文 | → `prompt_chars` 计数,原文 → trace |
| `main.rs` delegate_with_context / wt_event_rx | debug | commandline(含 prompt)/ 完整 WT 事件(含 `vt_sequence` 终端输出) | debug 留 method/cwd,原文 → trace |
| `coordinator.rs` send/open_and_send/send_input | debug | `input_preview` / commandline | 删 preview 留 `input_chars`;commandline → trace |
| `ui/agents_view.rs` | debug | 会话 `title`(由对话生成) | 从 tuple 删除,只留 key+status |
| `app.rs` copilot login std | debug | 登录子进程原文(含 device code) | → trace |
| `master.rs` create_terminal | info | agent 命令行 | info 留 `args_len`,原文 → trace |

**级别上修(critical 落盘):**

| 位置 | 原 | 新 |
|---|---|---|
| `client.rs` ACP I/O loop 失败(pipe + child 两路) | `*_probe.log`(debug)+ `eprintln!` | `tracing::warn`(去掉 eprintln) |
| `client.rs` agent kill 失败 / wait 失败 | debug | warn |
| `client.rs` agent 进程退出 | debug | info |
| `master.rs` agent_stderr | warn(噪声+可能含内容) | debug(真正的退出/崩溃另在 error) |

**保留(评估后认定可接受):** master 端 fs 读写的 `path`(操作元数据,非内容);
`agent_stderr` / probe 的 agent 自身 stderr 行(诊断必需,留 debug)。

审计覆盖 ~15k 行;`telemetry.rs`(ETW 只发字节数/id)、`osc52.rs`(剪贴板从不落日志)
经确认本就 privacy-clean。`cargo build` 通过,562 测试全绿。

## 改动文件清单

| 文件 | 改动 |
|---|---|
| `tools/wta/src/logging.rs` | `default_filter_directive` release→`info`;`cli` 标签用 `rolling::daily`,其余用 `rolling::never`;新增 `housekeeping()`(版本比对直删 + helper/cli 文件按 3 天 mtime 回收);`init` 调用 `housekeeping()`;更新单测 |
| `tools/wta/src/main.rs` | 新增 `process_label(&cli)`;`main()` 顶部 `Cli::parse()` 后唯一一次 `logging::init`,guard 持有到结束;删除 `863/905/1525/1636/1690` 的内部 init;helper 标签带 pid |
| `tools/wta/src/master/mod.rs` | 删除 `1152` 的内部 init(改由 `main()` 统一);**补全连接链路日志(成功+失败),见改动 F** |
| `tools/wta/src/protocol/acp/client.rs` | **补全 helper 侧 ACP 连接/initialize/session 失败日志,见改动 F** |
| `CLAUDE.md` | 「Logs」一节级别说明更正;补充 `wta-cli.log`、`wta-main_helper-{pid}.log`、版本清理行为 |

## 验收要点

- 直接跑 `wta list-panes` / `wta listen` 等命令后,`wta-cli.log` 有内容。
- 在 `Cli::parse()` 之后、`set_locale` 之前人为 panic,`wta-*.log` 能看到痕迹。
- 多 tab 各自产生独立 `wta-main_helper-{pid}.log`。
- release 构建不设 `WTA_LOG` 时 info 可见。
- 手动改写 `.wta-log-version` 为旧值后重启,旧 `wta-*.log` 被清理且只清一次。
- 开关 tab 多次后,陈旧 helper 文件按保留策略被回收。

## 决策记录

1. **E 的清理方式**:✅ 直接 `remove_file` 删除,不归档。
2. **per-PID 保留阈值**:✅ 按天数,删除 mtime 早于 **3 天**的 helper 文件。
3. **`wta-cli.log` 滚动**:✅ 按天滚动。

### `wta-cli.log` 按天滚动(已定)

> 背景:最初提议的「1KB 上限」不可行 —— `tracing_appender` 不支持按体积
> 滚动(只有 `minutely`/`hourly`/`daily`/`never` 时间粒度),且 1KB 远小于
> 单条 wtcli 调用的日志量。故改为按天滚动。

- `wta-cli.log` 用 `tracing_appender::rolling` 的 **`Builder`** 配置
  `rotation(DAILY)` + **`max_log_files(3)`**,产出 `wta-cli.log.YYYY-MM-DD`
  并由库**自动**删除超出 3 个的最旧轮转文件 —— **全部原生,无需自定义清理**:

  ```rust
  tracing_appender::rolling::Builder::new()
      .rotation(tracing_appender::rolling::Rotation::DAILY)
      .filename_prefix("wta-cli")
      .filename_suffix("log")
      .max_log_files(3)
      .build(&log_dir)?
  ```

- 其余进程日志(master / delegate / probe / install-hooks)保持
  `rolling::never`;helper 用 per-PID 文件(改动 D),其回收由改动 E 的
  3 天 mtime 策略覆盖。

> `max_log_files` 只能裁剪**同一个 appender 自己的轮转集**,因此只对
> `wta-cli.log.*` 生效;helper 的 per-PID 文件和版本升级清库属于跨文件名
> 操作,库管不到,见改动 E。

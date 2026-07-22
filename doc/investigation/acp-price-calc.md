# ACP Usage / Cost 调查与统一展示设计

- **状态**：设计草案，尚未实现
- **首次调查**：2026-07-17
- **最后核验**：2026-07-22
- **协议基线**：ACP protocol version 1
- **本仓库依赖**：`agent-client-protocol = 1.0.0`，`agent-client-protocol-schema = 1.1.0`
- **目标开发验证包（固定版本，尚未落入当前启动映射）**：
  - Claude ACP：`@agentclientprotocol/claude-agent-acp@0.59.0`
  - Codex ACP：`@agentclientprotocol/codex-acp@1.1.2`

## 结论

1. ACP v1 已经定义稳定的会话级 `SessionUpdate::UsageUpdate`：
   - `used`：当前上下文使用的 token 数；
   - `size`：上下文窗口总 token 数；
   - `cost`：可选的会话累计费用，使用 ISO 4217 货币码。
2. 单轮 input/output/cache/reasoning token 拆分仍受
   `unstable_end_turn_token_usage` feature 门控，不能作为当前稳定实现的基础。
3. ACP 只定义 contract，不强制 agent 一定发送 usage/cost；每个 agent 必须实测。
4. GitHub Copilot 的 **AI Units**（本机 CLI 1.0.71 用语；较早文档/changelog 使用
  **AI Credits**）是 provider 专有单位，不是 ISO 4217 货币，ACP 标准 `Cost` 无法直接
  表达。
5. 最终产品策略为：
   - **标准 ACP 优先**；
   - 允许显式白名单、由 agent/provider 直接报告的私有 usage 扩展；
  - 只展示 agent/provider 接口直接报告的数值与单位；
  - 不做 token→USD、multiplier→Credits 或其他本地计费计算；
   - 没有可信报告值就不显示。
6. UI 组件命名为 **Usage**，而不是 Cost。不同单位只展示，不换算、不比较。
7. 开发期间使用真实最新版 pinned adapter 构造 Claude/Codex mock 环境，验证 provider
  分流；mock 配置、脚本、抓包与凭据不提交。
8. 实现必须模块化并复用现有 ACP routing、agent registry、per-tab state projection 和
  Bottom Bar 状态更新路径，不建立平行架构。
9. 开发期 **fail fast**：adapter 识别、解析、归一化、合并与 UI contract 出错时立即让测试/
  debug 运行失败，不加兜底 `try/catch`。功能完成并验证后，只在最外层 Usage feature
  boundary 加一次 release 降级：失败则隐藏 Usage，不影响聊天主流程。
10. **本功能暂不实现 Gemini-specific provider 支持**。Google 已发布新的 agent tool
  Antigravity，产品方向是未来逐步从 Gemini 迁移到 Antigravity；因此不为 Gemini 新增私有
  usage adapter、provider API、专属 fixture/E2E 或兼容承诺。Antigravity 的协议、reporter
  identity 与 usage contract 需另行调查后再纳入。

> 本文中的“报告值”表示数值和单位来自 ACP agent、provider 扩展或 provider API，而不是
> WTA 根据 token、模型价格或倍率计算。WTA 可以做数字分组、小数位和单位名称等显示格式化，
> 但不能改变数值含义或换算单位。报告值不一定等于最终发票金额：agent/wrapper 自己也可能
> 发送估算值，ACP 当前没有字段声明费用是 provider 结算值还是本地估算值。

---

## 开发包版本决策（核验于 2026-07-22）

本节最初定义实现与 E2E 验证目标；Step 0 baseline 已将当前代码同步到下面的固定版本。

开发与 E2E mock 固定使用以下**已发布版本**，避免 `npx` 在不同开发时间自动拉取不同实现：

| 用途 | 固定 package | 备注 |
|---|---|---|
| Claude ACP agent | `@agentclientprotocol/claude-agent-acp@0.59.0` | npm `latest`；依赖 Claude Agent SDK 0.3.207 和 ACP SDK 1.2.1 |
| Codex ACP agent | `@agentclientprotocol/codex-acp@1.1.2` | npm `latest`；依赖 `@openai/codex ^0.144.0` 和 ACP SDK `^1.2.1` |
| 独立 Claude Code CLI（不参与本功能 pin） | npm `latest` 2.1.210，`stable` 2.1.206 | 直接 ACP harness 不需要；Settings 内置项的产品级 E2E 需要 `claude` 在 `PATH` |
| 独立 Codex CLI（不参与本功能 pin） | npm `latest` 当前解析为 Windows x64 package `0.145.0-alpha.9-win32-x64` | Settings 内置项的产品级 E2E 需要 `codex` 在 `PATH`；adapter 自带兼容 Codex dependency |

固定启动命令：

```text
npx -y @agentclientprotocol/claude-agent-acp@0.59.0
npx -y @agentclientprotocol/codex-acp@1.1.2
```

当前仓库的 Rust registry、C++ `_BuildAgentCommandLine()` 与 Settings model probe 已统一使用
上表的 pinned command。未固定命令、官方 Codex 1.1.0 和已废弃的
`@zed-industries/codex-acp@0.16.0` 仅作为历史命令识别 alias，不再用于新 session 启动。
Launch metadata 仍在 C++ 与 Rust 重复；后续应消除该重复并保留 drift tests。

上述 adapter 版本由 `npm view <package> version` 在 2026-07-22 实时核验；此前网页调查得到的
Claude `0.60.0` / Codex `1.1.5` 实际未发布，npm 返回 404，不能使用。“最新”是本次开发
基线，不表示永久自动跟随 npm latest。升级版本必须显式改代码、更新 compatibility
fixture/记录并重新跑 Claude/Codex E2E mock。

## 当前仓库核对（2026-07-22）

下表区分**当前已实现**和**本文目标**。后文出现 `建议`、`需要`、`应`、`首版` 的内容均是
待实现设计，不应反读为当前行为。

| Area | 当前仓库事实 | 本文目标 |
|---|---|---|
| ACP dependency | [tools/wta/Cargo.toml](../../tools/wta/Cargo.toml) 声明 `agent-client-protocol = "1.0"`；lock 为 protocol 1.0.0 / schema 1.1.0，未启用 `unstable_end_turn_token_usage` | 首版只消费 stable v1 `SessionUpdate::UsageUpdate` |
| Built-in family ID | [src/cascadia/inc/AgentRegistry.h](../../src/cascadia/inc/AgentRegistry.h) 定义五个 C++ ID（含 OpenCode）；Rust `KNOWN_AGENTS` 手工重复同一组值 | `build.rs` 从 C++ registry 生成 Rust constants，并做 drift check |
| Claude launch | C++ 与 Rust 都固定为 `npx -y @agentclientprotocol/claude-agent-acp@0.59.0` | 消除 launch metadata 重复 |
| Codex launch | C++ 与 Rust 都固定为官方 `npx -y @agentclientprotocol/codex-acp@1.1.2` | 消除 launch metadata 重复并保留历史识别 alias |
| Command ownership | C++ `_BuildAgentCommandLine()` 构造 host/default command；Rust `AgentProfile` 为 per-tab built-in selection 重建 command，因此目前确有两份映射 | 建立可生成/共享的 launch metadata；完成前用测试强制两处完全一致 |
| Custom selection | Settings 将 `npx ...` 保存为 `custom:npx`；master 对未知 helper ID 回退到 host 已信任的 default command，从不执行 pipe 上传来的 command | 分离 instance/family/reporter；custom 可识别 compatible family，但首版不能启用私有 usage extension |
| Usage receive | master 原样按 session 路由 `SessionNotification`；helper 的 `WtaClient::session_notification()` 目前把 `UsageUpdate` 落入 `_ => {}` | 在 helper 增加 standard normalizer、可靠 latest-value state 和 projection |
| Usage state/UI | `AppEvent`、`TabSession`、`AgentPaneContent` 与 XAML 当前均无 usage 字段/控件 | 增加 provider-neutral state；由 C++ Bottom Bar Column 2 渲染 `UsageGroup` |
| C++ event route | `agent_state_changed` 已按 `tab_id` 经 COM server 定向到 `TerminalPage::OnAgentStateChanged()`；`project_tab_state()` 已发送统一 per-tab snapshot | 在现有 snapshot 增加可选 `usage`，不新增 COM/IDL route |
| Rust codegen | [tools/wta/build.rs](../../tools/wta/build.rs) 当前只生成 ETW telemetry metadata | 增加 Agent registry codegen，但保留现有 ETW 生成 |
| Gemini / Antigravity | Gemini 仍是当前仓库的内置 ACP agent；Antigravity 尚无 registry/profile/usage 集成 | Usage feature 不做 Gemini-specific provider；迁移到 Antigravity 前另行调查协议与 identity，不预先复用 `gemini` family ID |

这张表是本次 repo audit 的基线。若实现期间代码先于文档变化，应同步更新“当前仓库事实”，
不能只修改目标章节。

---

## 术语

- **agent / agent CLI**：WTA 启动并通过 ACP 对话的程序或 adapter，例如 Copilot CLI、
  Gemini CLI、Claude ACP adapter、Codex ACP adapter。
- **ACP transport adapter**：把某个 agent SDK/CLI 转成 ACP 的独立进程，例如
  `claude-agent-acp`；它与下文解析私有 usage payload 的 **Usage extension adapter** 不是
  同一层。
- **`agent_instance_id`**：用户选择的具体配置实例，例如内置 `copilot` 或自定义
  `custom:npx`。用于设置项、生命周期和 per-tab routing。
- **`agent_family_id`**：该实例兼容哪一种 agent 语义。产品级 canonical family ID 定义在
  [src/cascadia/inc/AgentRegistry.h](../../src/cascadia/inc/AgentRegistry.h) 的
  `BuiltinAcpAgents` 中，例如 `claude`。Usage adapter 必须复用这些 ID，不能再写第三套
  `"copilot"` / `"claude"` / `"codex"` / `"gemini"` 字符串。
- **reporter**：实际生成 usage payload 的进程/协议实现，例如
  `@agentclientprotocol/claude-agent-acp`。
- **billing provider / issuer**：实际定义计费单位的一方。例如 Copilot 的 AI Units 由
  GitHub 定义。它不一定等同于启动的 executable 或 ACP adapter。
- **usage**：泛指费用、credits、token、上下文占用或配额。
- **cost**：仅指真实货币金额。`117.9 AI Units` 是 usage，不是 `$117.90` cost。

---

## 1. ACP 官方 contract

### 1.1 已稳定：会话级 `UsageUpdate`

上游 PR [#1371](https://github.com/agentclientprotocol/agent-client-protocol/pull/1371)
于 2026-06-05 合并并稳定了 session usage updates。当前依赖的 v1 schema 已包含：

```rust
pub struct UsageUpdate {
    /// Tokens currently in context.
    pub used: u64,
    /// Total context window size in tokens.
    pub size: u64,
    /// Cumulative session cost (optional).
    pub cost: Option<Cost>,
    pub meta: Option<Meta>,
}

pub struct Cost {
    /// Total cumulative cost for session.
    pub amount: f64,
    /// ISO 4217 currency code, e.g. "USD" or "EUR".
    pub currency: String,
    pub meta: Option<Meta>,
}
```

`UsageUpdate` 同时携带两个语义不同的指标：

| 字段 | 指标语义 | 更新方式 |
|---|---|---|
| `used / size` | 当前会话上下文占用 | gauge，新值替换旧值 |
| `cost` | 当前会话累计货币费用 | cumulative，新值替换旧值，不能再次累加 |

`cost` 是可选字段。Agent 可以只报告上下文占用，不报告费用。

### 1.2 仍不稳定：单轮 token 拆分

上游 PR [#1345](https://github.com/agentclientprotocol/agent-client-protocol/pull/1345)
将单轮 token usage 与 session usage 分开。目前 `PromptResponse.usage` 以及包含
`input_tokens`、`output_tokens`、`thought_tokens`、`cached_read_tokens`、
`cached_write_tokens` 的 `Usage` 仍受以下 feature 门控：

```rust
#[cfg(feature = "unstable_end_turn_token_usage")]
pub usage: Option<Usage>;
```

WTA 当前没有启用这个 feature。本设计可以预留输入通道，但首版不依赖它。

### 1.3 ACP 没有 Credits contract

在当前 v1/v2 schema 中没有标准的 `credit`、`premium request`、`multiplier` 或通用
quota unit。`Cost.currency` 明确要求 ISO 4217，因此不能把 `Credits` 当作 currency 填入。

ACP 提供 `_meta` 和自定义 extension notification，但字段名与语义由实现方负责，不具备
跨 provider 互操作性。

---

## 2. Agent 实现现状与证据强度

下面区分“已核验的协议/代码事实”和“尚待本项目抓包验证的第三方报告”。不能因为某个
GitHub issue 声称实现支持，就直接在产品中标记为准确费用。

| WTA 内置 `agent_family_id` | 当前启动路径 | 已知 usage 模型 | ACP 证据状态 | 产品处理 |
|---|---|---|---|---|
| `copilot` | 原生 `copilot --acp --stdio` | 当前本机 `/usage` 称为 AI Units，同时显示 token breakdown | **本项目已验证 CLI 1.0.71**：只发现模型静态 `_meta.copilotUsage` 倍率和 `/usage` 人类可读文本；未发送标准 `UsageUpdate`、结构化实际 AI Units、cost 或 token usage | 当前不显示动态 usage；不解析 `/usage` 文本，不用倍率推算 |
| `claude` | `@agentclientprotocol/claude-agent-acp@0.59.0` adapter | API 用户可看到 token 与估算 USD；订阅用户主要看到 plan usage | 第三方 issue 声称会发送标准 usage/cost；WTA 尚未独立验证 | 必须抓包；按实际收到的标准或白名单扩展处理 |
| `codex` | `@agentclientprotocol/codex-acp@1.1.2` adapter | API 通常按 token/货币；ChatGPT 订阅走计划限制 | 官方 adapter 声明支持 token usage events；WTA 尚未独立验证 | 必须抓包；注意报告者是 adapter，不一定是 OpenAI 账单 API |
| `gemini` | 原生 `gemini --experimental-acp` | `/stats` 可显示 token；API 计费与账户 quota 分开 | [Gemini issue #24280](https://github.com/google-gemini/gemini-cli/issues/24280) 曾记录 token 在 `_meta.quota.token_count`、没有 cost；后续版本可能变化 | **Out of scope**：不新增 Gemini-specific usage adapter/API/fixture/E2E；仅保留 provider-neutral 标准 ACP 行为，不作兼容承诺 |

### 2.1 GitHub Copilot AI Units / AI Credits

[Copilot CLI changelog](https://github.com/github/copilot-cli/blob/main/changelog.md) 中出现：

- `AI Credits` label；
- AI credit usage/budget/limit；
- ACP model info 中的 usage multiplier；
- 旧版本中的 PRU / premium request 相关描述。

本机 1.0.71 的 `/usage` 实际显示 `AI Units`。这说明产品术语本身会演进，也进一步证明
WTA 不应硬编码旧名称或根据历史说明推算。上述记录只能证明 Copilot 有专有 usage/计费
UX，但**不能证明**：

- AI Credits 与历史 PRU 完全等价；
- credits 的精确公式就是 `prompt count × model multiplier`；
- Copilot 会通过标准 ACP `Cost` 发送 credits；
- 只靠 model multiplier 能重建后端账单。

因此 WTA 不能本地推算 AI Units/Credits。只有 Copilot ACP 消息或官方 provider API 直接
报告数值与当前单位名称时，才能归一化为 provider credit unit。

### 2.2 本机 Copilot CLI 1.0.71 ACP 实测（2026-07-21）

本机安装来自 WinGet，执行文件版本为 GitHub Copilot CLI 1.0.71。使用
`copilot --acp --stdio` 完成 `initialize`、`session/new`、一个只返回 `OK` 的最小 prompt，
然后在同一 session 发送 `/usage`。没有读取或输出登录凭据。

实测结果：

| 阶段 | 实际 wire 数据 |
|---|---|
| `initialize` | 没有 usage/credit/cost/quota/token 字段 |
| `session/new` | 每个模型在 `_meta.copilotUsage` 中提供静态倍率（如 `0x`、`0.33x`、`1x`、`3x`、`7.5x`、`14x`、`15x`），并提供 `_meta.copilotPriceCategory` |
| 最小 prompt | 只收到 config option 更新、`agent_message_chunk`（`O`、`K`）和最终 `PromptResponse`；没有标准 `usage_update`，没有结构化 AI Units/cost/token 字段，也没有 usage extension notification |
| 随后的 `/usage` | 通过 `agent_message_chunk` 返回人类可读文本：`Requests: 1 AI Units ... Tokens: input 37.0k, output 4, cached 0`；最终 response 仍没有结构化 usage 字段 |
| 本地 `events.jsonl` | 对应 `assistant.message` 记录包含 `data.outputTokens = 4`；该次检查没有发现 AI Units、input/cached token 或 cost 字段 |

结论：**本机 Copilot 知道并能显示本次 session 的 AI Units 与 token breakdown，但当前
1.0.71 ACP 接口没有把这些实际用量作为机器可读 contract 提供给 WTA。**

WTA 不能采用以下方法：

- 解析 `/usage` 的自然语言文本：它是 UI 文案，格式与名称可随版本、本地化和 billing
  platform 改变；
- 使用 `_meta.copilotUsage` 倍率乘请求数：它是模型静态相对用量，不是后端报告的本次
  实际 AI Units；
- 只读取 `events.jsonl.outputTokens` 后补算其他 token 或费用：数据不完整且不是 ACP
  contract。

因此在 Copilot 1.0.71 上，当前产品规则的结果是：**动态 usage 隐藏**。模型选择器可以
单独展示 provider 已报告的静态倍率/价格类别，但不能把它标成“本次已使用”。未来如果
Copilot 增加结构化 ACP extension、标准 `UsageUpdate` 或官方机器接口，再新增白名单
adapter。

### 2.3 Claude 的 cost 也不自动等于发票

[Claude Code cost 文档](https://code.claude.com/docs/en/costs) 明确说明 `/usage` 的 session
dollar figure 是根据 token 在本地计算的估算值，权威账单应查看 Claude Console。即使
Claude ACP adapter 把该值放入标准 `Cost`，WTA 也只能称其为“agent-reported cost”，不能
在 UI 中承诺为最终结算金额。

### 2.4 Agent Maestro 作为 Claude ACP 测试桥（2026-07-22）

> 本节同时记录当前可做的 local connectivity experiment 和尚未实现的 identity/trust 目标。
> `agentFamilyId`、`acp_reporter_ids`、reporter→family resolver 目前都不存在于代码中；带这些
> 字段的 JSON 与优先级描述均是 proposal，不是当前 settings/wire contract。

本机安装了 Agent Maestro 2.10.0。它在 `127.0.0.1:23333` 提供 Anthropic/OpenAI/Gemini-
compatible HTTP API，但**不提供 ACP endpoint**。本机 `/api/v1/lm/chatModels` 返回 52 个
VS Code LM 模型，包括 Claude Sonnet 5 和 Claude Opus 4.6/4.7/4.8；Anthropic
`/v1/messages/count_tokens` 已实测返回结构化 `input_tokens`。

因此 Agent Maestro 不能直接填入 Intelligent Terminal 的 `+ Add New...`；需要
`@agentclientprotocol/claude-agent-acp` 将 ACP 转为 Claude Agent SDK/Anthropic API 请求：

```text
Intelligent Terminal
  -> ACP JSON-RPC over stdio
  -> @agentclientprotocol/claude-agent-acp
  -> Anthropic-compatible HTTP
  -> Agent Maestro (127.0.0.1:23333)
  -> VS Code Language Model API
  -> Copilot-provided Claude model
```

最新 `claude-agent-acp` 依赖 Claude Agent SDK，SDK 的平台 optional dependency 自带 native
Claude binary；不要求另行安装全局 Claude Code CLI。它也会读取 user/project
`.claude/settings.json`，因此能继承 Agent Maestro 写入的 `ANTHROPIC_BASE_URL`、
`ANTHROPIC_AUTH_TOKEN` 和 `ANTHROPIC_MODEL`。

#### 本机当前缺口

- Agent Maestro server 正常；
- Claude-compatible models 正常；
- Node v24.18.0、npm/npx 11.16.0 已安装并在 `PATH`；
- 已运行 `Agent Maestro: Configure Claude Code Settings`，user-level
  `~/.claude/settings.json` 指向 `http://127.0.0.1:23333/api/anthropic`，模型为
  `claude-sonnet-5[1m]`；
- `npx` 下载 pinned adapter 时仍出现间歇性 `ERR_SSL_SSL/TLS_ALERT_HANDSHAKE_FAILURE`，
  adapter 尚未成功缓存/启动；需要先恢复到 `registry.npmjs.org` 的稳定 TLS 连接。

#### 可行的实验步骤

1. 安装 Node.js 22+（只需要 Node/npm/npx，不需要安装 Claude CLI）。
2. 在 VS Code Command Palette 运行 `Agent Maestro: Configure Claude Code Settings`。
3. 推荐选择 **User Settings**，使 `~/.claude/settings.json` 对 IT 启动的 adapter 可见；选择
   一个 VS Code LM 中的 Claude 模型。
4. 确认 Agent Maestro API server 运行；如果启用了自定义 LLM API key，确保
   `.claude/settings.json.env.ANTHROPIC_AUTH_TOKEN` 与之匹配。默认关闭认证时，one-click
   写入的 placeholder token 即可。
5. 重启 Intelligent Terminal，让新安装的 `npx` 进入 `PATH`。
6. 在 Settings → Agents → `+ Add New...` 中填入并保存一个固定版本的 ACP command，例如：

   ```text
  npx -y @agentclientprotocol/claude-agent-acp@0.59.0
   ```

7. 打开 agent pane 做最小 prompt 测试，并抓 ACP wire 验证模型、tool call 和
   `UsageUpdate`。

因为没有全局 `claude` executable，内置 `Claude` 条目仍不会出现在 Settings 下拉框中；
这里使用的是 `custom:*` ACP agent。它可以支持基础 agent pane，但不自动获得四个内置
provider 的 hooks/session-management 特权。

#### 必须把 custom instance 与 effective family 分开建模

当前 Settings 的 `+ Add New...` 会用命令的第一个 token 派生 custom ID。对于：

```text
npx -y @agentclientprotocol/claude-agent-acp@0.59.0
```

当前保存结果是 `agent_instance_id = "custom:npx"`。Helper 请求该未知 ID 时，master 不会执行
pipe 上传来的 command，而是回退到 host 启动时已信任的 default command；如果全局设置就是
这个 custom agent，连接仍可使用该 pinned command，但 master 保留的 ID 仍是 `custom:npx`，
不会把它视为 canonical `claude`。`resolve_agent_id_from_cmd()` 也不能可靠补救，因为 pinned
npm spec 与 registry 中未带版本号的 command 不相等。

设计模型需要同时表达（下面不是当前 settings schema）：

```jsonc
{
  "agentInstanceId": "custom:npx",
  "agentFamilyId": "claude",
  "command": "npx -y @agentclientprotocol/claude-agent-acp@0.59.0"
}
```

- `agentInstanceId` 决定这是用户自定义 launch、遵守 custom-agent policy；
- `agentFamilyId` 使用 `AgentRegistry.h::BuiltinAcpAgents` 中的 canonical `claude`，表达
  protocol compatibility 并驱动 family-neutral routing/UX；它本身不授权私有 usage adapter；
- `command` 保持用户固定的 adapter 版本，master 不能因为 family 是 `claude` 而替换成
  内置 profile command；
- adapter artifact version 是 `0.59.0`，ACP handshake 报告的 usage `reporter_id` 是稳定
  package ID `@agentclientprotocol/claude-agent-acp`；
- billing issuer/source 是 Agent Maestro / VS Code LM / Copilot 路径，**不是 Anthropic**。

因此“把它当真正的 Claude agent”只适用于 agent protocol family 和 UX，不表示它使用
Anthropic 账户或 Anthropic 定价。Usage provider 层不得按 `agentFamilyId == "claude"` 套用
Claude 价格表；它仍然只消费 agent 实际报告的标准 `UsageUpdate`。

`claude-agent-acp` 的 `InitializeResponse.agentInfo.name` 当前是稳定 package ID
`@agentclientprotocol/claude-agent-acp`。因此 family resolution 可以按以下优先级自动完成：

1. 内置 agent selection 自带 canonical family；
2. custom 定义显式保存的 `agentFamilyId`；
3. ACP handshake 返回的 **exact allowlisted reporter ID**；
4. 否则为 Generic/unknown family。

建议给 Rust `AgentProfile` 增加 `acp_reporter_ids`，例如 Claude profile 声明：

```rust
acp_reporter_ids: &["@agentclientprotocol/claude-agent-acp"]
```

这样 pinned command 即使保存成 `custom:npx`，master 在 initialize 成功后也能根据 adapter
自己报告的 exact implementation ID，把 effective family 解析为 `claude` 并回传给 helper。
这只是**兼容性识别**，不是信任证明：任意 custom agent 都可以伪造 `agentInfo.name`，所以不能
对 `agentInfo.title` 做模糊匹配，也不能仅凭 `(family + reporter + schema)` 为 custom instance
启用私有 adapter。私有 usage extension 还必须来自 host 按内置 profile 构造的 trusted launch；
custom instance 首版仍只能消费标准 ACP。

Settings 的长期 UX 仍可在 `+ Add New...` 中增加可选的 **Compatible agent** 字段，作为无法
自动识别 wrapper 时的显式设置：

- Generic ACP（默认）
- GitHub Copilot
- Claude
- Codex
- Gemini

WTA 可以根据 npm package 提示 `Claude`，但 command 只能用于 UI suggestion，不能作为
runtime identity。对于 custom connectivity smoke，官方 adapter 的 exact reporter ID 可解析
compatible family；如果 handshake 未返回该 ID，保持 Generic/unknown。显式 Compatible agent
设置仍不能绕过 custom-agent policy，也不能授权私有 extension adapter。

当前代码尚没有该字段，所以现在直接保存 pinned npx command 后：

- 基础 ACP 连接仍可工作；
- 标准 `UsageUpdate` 仍可由 family-agnostic normalizer 处理；
- 设置/UI 仍显示 custom agent；Claude 专属 logo、私有 usage adapter 和其他 family-specific
  行为不会启用；
- 不能通过手工把 `acpAgent` 改成 `claude` 补救，因为 C++ 会按 built-in family 重建 pinned
  built-in command，并忽略 custom command。

要完整满足本实验，代码至少需要新增：

1. `AgentProfile.acp_reporter_ids` 与 exact reporter→family resolver；
2. C++→helper→master 的独立 `agent_instance_id`，以及 initialize 后 master→helper 的 resolved
  `agent_family_id`；
3. master 绑定 `AgentCli` 时保留 trusted custom command，同时根据 reporter allowlist 回传
  最终 effective family；
4. `ClientState` 保存 effective family，供 UI projection 和 provider adapter registry 使用；
5. 可选的设置字段（暂名）`acpCustomAgentFamily` / Compatible agent picker，作为未知 wrapper
  的显式 fallback，而不是识别官方 adapter 的硬性前提。

#### 这条路径能证明什么

- 可以测试 IT 与 Claude ACP adapter 的连接、流式输出、tool calls、model config 和标准
  `UsageUpdate` 解析；
- `claude-agent-acp` 当前实现会把 SDK 的 context usage 和 `total_cost_usd` 转成标准
  ACP `usage_update`；
- Agent Maestro 的 Anthropic response 在 VS Code 提供真实 Copilot usage metadata 时使用
  该数据，否则回退到本地 token 估算。

因此这条路径适合做**协议和 UI mock/integration test**，但不能证明 Anthropic 官方计费：

- 实际推理来自 VS Code LM / Copilot，不是用户的 Anthropic 账户；
- token 可能是 Copilot metadata，也可能是 Agent Maestro fallback estimate；
- `total_cost_usd` 在代理模型下可能为 0、估算或与真实 Copilot AI Units 完全无关；
- UI 必须标明 source，不能把它称为 Anthropic 实际账单。

#### 安全与稳定性限制

- Agent Maestro 默认 API auth 关闭；虽然绑定 loopback，本机其他进程仍可调用。测试敏感
  workspace 时应设置 LLM API key，并保证 Claude settings token 匹配；
- Agent Maestro 必须随 VS Code 保持运行，VS Code reload/关闭会使 IT 的请求失败；
- `npx -y` 会联网下载代码。实验应固定 adapter 版本；产品集成不能依赖未固定最新版；
- Agent Maestro error logs 写入 workspace。Anthropic route 声称会清理内容，但仍应检查
  日志且避免提交 `*-debug.log`；OpenAI/Gemini route 文档明确尚未完整清理；
- 使用 VS Code LM/Copilot 经第三方 proxy 转发前，需要单独确认组织策略与服务条款。

首版产品方案仍不把 Agent Maestro 作为正式 provider dependency；它仅作为本地实验桥和
fixture 产生工具。

### 2.5 Gemini 暂停，后续迁移到 Antigravity

这是产品范围决策，不是对 Gemini 当前 ACP 能力的技术否定：Google 已发布新的 agent tool
Antigravity，而 Intelligent Terminal 计划未来逐步迁移到它。为避免在即将退出的 provider
路径上增加一次性代码，本 Usage feature：

- 不实现 Gemini `_meta.quota` 或其他私有 usage payload；
- 不调用 Gemini provider API，不维护 Gemini 单位/价格/配额语义；
- 不为 Gemini 建 E2E mock、raw fixture、kill switch 或 compatibility matrix；
- 不因名称或 Google ownership 把 Antigravity 自动映射为 `gemini`；
- 在 Antigravity 进入产品 registry 前，先独立调查它是否使用 ACP、`agentInfo.name`、session
  usage contract、认证与计费 issuer，再决定新的或兼容的 family identity。

标准 `UsageUpdate` normalizer 仍然是 provider-neutral。若当前 Gemini agent 或任意 custom ACP
agent 发送合法的稳定 v1 `UsageUpdate`，通用路径可以显示它；这不需要 Gemini 分支，也不代表
我们承诺测试或支持 Gemini-specific schema。若产品要求迁移期间完全禁止 Gemini 显示 Usage，
应另加显式 rollout policy，而不是在 normalizer 中硬编码 `gemini` 排除项。

---

## 3. 当前 WTA 数据流：实际忽略点在哪里

此前文档正确观察到 `UsageUpdate` 被忽略，但错误地归因于
`master/mod.rs::notification_kind()` 的 `_ => "other"`。这个函数只生成日志 tag，master
仍会把完整 `SessionNotification` 原样路由给所属 helper；真正的忽略点在 helper。

真实数据流：

```text
Agent CLI
  -> wta-master::MasterClient::session_notification()
     - notification_kind() 只做日志分类
     - 原样发送到 helper notification channel
  -> wta-helper::WtaClient::session_notification()
     - 处理 message/thought/tool/plan
     - 其他 update 进入 `_ => {}`
  -> AppEvent
  -> TabSession / UI
```

因此当前真正忽略 `UsageUpdate` 的位置是
[tools/wta/src/protocol/acp/client.rs](../../tools/wta/src/protocol/acp/client.rs) 中
`WtaClient::session_notification()` 的 `_ => {}` 分支，而不是 master 路由层。

已有可复用基础：

- [src/cascadia/inc/AgentRegistry.h](../../src/cascadia/inc/AgentRegistry.h) 的
  `BuiltinAcpAgents` 是设置、策略和 agent pane 使用的 ACP agent ID 集合；
- [tools/wta/src/agent_registry.rs](../../tools/wta/src/agent_registry.rs) 的
  `AgentProfile.id` 当前手工镜像同一组值，但没有 codegen 或 drift check；这还是两份来源，
  不是本设计要求的真正复用；
- `run_acp_client_over_pipe()` 当前收到每个 tab 的混合 `agent_id`，但尚未拆分 instance/family，
  `ClientState` 也未保存 family；
- `AppEvent` 和 `TabSession` 尚无 usage 类型或状态；
- `session_watcher/classify_*` 解析的是各 CLI 的落盘 session 文件，不是 agent pane 的
  实时 ACP 消息，因此不应作为本功能的主要实现位置。

---

## 4. 最终产品策略

2026-07-17 的“只接受标准 ACP”决策被本节取代。为了支持 Copilot AI Units 等私有单位，
同时不让私有协议污染通用 UI，采用：

1. **标准 ACP 优先**：先消费 `SessionUpdate::UsageUpdate`。
2. **白名单扩展兜底**：仅对已知 `agent_family_id + reporter/schema` 启用经过 fixture 测试的
  provider adapter。
3. **只展示报告值**：数值和单位必须来自 ACP 标准、agent extension 或 provider API；
   WTA 不自行计算价格、credits 或 token 数。
4. **标准覆盖私有**：相同 metric 同时出现时，标准 ACP 胜出，避免重复计数。
5. **缺失即隐藏**：没有报告值时不显示 `0`、`N/A` 或估算值。
6. **不承诺发票准确性**：UI 显示报告的单位，tooltip 说明来源；不使用 “Exact bill”。
7. **单位隔离**：Credits、USD、tokens 之间不换算、不相加、不横向排名。
8. **统一身份来源**：provider adapter 只使用 `AgentRegistry.h::BuiltinAcpAgents` 派生的
  canonical `agent_family_id`，不得维护第三套字符串 ID。

具体规则：

- Agent 报告 `0.55 USD`，就展示 `0.55 USD`；
- Copilot 扩展报告 `117.9 AI Units`，就展示 `117.9 AI Units`；
- 某个通用 ACP agent 只报告 `1,801 tokens`，就只能展示 `1,801 Tokens`，不能显示美元费用；
- 计费单位、价格和计算方式可能随 provider 政策立即改变，因此 WTA 永远不根据本地价格表、
  token 数或模型倍率补算另一个数值。

这意味着：

- 遵循 ACP 标准的新 agent：只注册 `AgentProfile`，usage 自动工作；
- 使用私有 usage 单位/字段的新 agent：注册 profile，再增加一个小型白名单 adapter；
- 没有报告 usage：不显示。

---

## 5. 统一 domain model

不能只用 `number + unit_display_name`。同一会话可能同时有累计费用、当前上下文、单轮
token 和账户配额；它们的更新与展示规则不同。

建议模型（示意，不代表代码已存在）：

```rust
struct UsageSnapshot {
  agent_instance_id: String,
  agent_family_id: Option<String>,
    metrics: Vec<UsageMetric>,
    observed_at: SystemTime,
}

/// One partial report produced from one wire message. TabSession merges reports
/// into its current UsageSnapshot by metric_id; a report is not a full snapshot.
struct UsageReport {
  agent_instance_id: String,
  agent_family_id: Option<String>,
    metrics: Vec<UsageMetric>,
    observed_at: SystemTime,
}

struct UsageMetric {
    /// 归一化后的稳定 ID，例如 "acp.billing.cost"、"acp.context.window"。
    /// Provider adapter 对同一逻辑指标必须映射到同一个 canonical ID。
    metric_id: String,
    /// Delta 去重所需。Cumulative/Gauge 通常不需要。
    event_id: Option<String>,
    /// 只有 wire message 明确给出 per-model attribution 时才填写。ACP session-level
    /// UsageUpdate 通常跨模型累计，不能用 UI 当前选择的模型反推。
    model_id: Option<String>,
    kind: UsageKind,
    value: UsageValue,
    unit: UsageUnit,
    aggregation: UsageAggregation,
    scope: UsageScope,
    source: UsageSource,
}

enum UsageValue {
    Count(u64),
    Decimal(Decimal),
    Ratio { used: u64, limit: u64 },
}

enum UsageKind {
    Billing,
    Context,
    TokenBreakdown,
    Quota,
}

enum UsageUnit {
    Currency { iso_4217: String },
    ProviderCredit {
        issuer_id: String,
        unit_id: String,
        display_name: String,
    },
    Token,
    Request,
    Percent,
    Custom { unit_id: String, display_name: String },
}

enum UsageAggregation {
    /// 本次增量；只有事件有稳定去重 ID 时才能累加。
    Delta,
    /// 累计值；新值替换旧值。
    Cumulative,
    /// 当前状态；新值替换旧值。
    Gauge,
}

enum UsageScope {
    Turn,
    Session,
    RollingWindow { reset_at: Option<SystemTime> },
    BillingPeriod { reset_at: Option<SystemTime> },
    Account,
}

enum UsageSource {
  AcpStandard {
    reporter_id: String,
    agent_family_id: Option<String>,
  },
    AgentExtension {
    agent_instance_id: String,
    agent_family_id: String,
    reporter_id: String,
        schema_id: String,
        // 仅用于诊断，不参与 UI 或业务判断。
        source_field_path: Option<String>,
    },
    ProviderApi { issuer_id: String, endpoint_id: String },
}
```

### 5.1 为什么不用裸 `f64`

ACP wire type 的 `Cost.amount` 已经是 `f64`，这是协议事实。内部 domain model 使用
`Decimal` 是为了稳定格式化并避免 WTA 后续累计产生新的浮点误差；它**不能恢复** wire
上已经丢失的十进制精度，也不能把报告值升级为权威账单。

实现时需要：

- 拒绝 NaN、Infinity 和负数；
- 将标准 ACP `f64` 通过规范化十进制字符串转换为内部 decimal；
- 不对 cumulative cost 再次求和；
- 若不愿引入 decimal crate，可使用经过校验的 decimal text 仅作展示，但不能用裸字符串
  参与业务计算。

### 5.2 为什么 `unit_field_path` 不属于业务 entity

字段路径描述“adapter 从哪里取值”，不描述 usage 本身：

- UI 不应知道 `_meta.quota.token_count`；
- provider 改字段时，只应修改 adapter 与 fixture；
- 通用 JSONPath 容易在字段改名或类型变化后静默显示错误数据。

因此公共模型只保留稳定 `unit_id`。`source_field_path` 最多作为诊断 provenance。内置
provider 应定义小型 `#[derive(Deserialize)]` payload 类型，不使用配置驱动 JSONPath。

### 5.3 Aggregation 与 Scope 的组合

| 示例 | Aggregation | Scope | 合并规则 |
|---|---|---|---|
| ACP `cost` | `Cumulative` | `Session` | 相同 `metric_id` 新值替换旧值 |
| ACP `used / size` | `Gauge` | `Session` | 新 ratio 替换旧 ratio |
| 某次调用新增 tokens | `Delta` | `Turn` | 只有 event/turn ID 可去重时才累加 |
| 本周剩余 quota | `Gauge` | `RollingWindow` | 新值替换；保存 reset time |
| 本月已用 credits | `Cumulative` | `BillingPeriod` | 新值替换；跨 reset time 清零 |

`UsageReport` 到达后，usage store 按 `metric_id` 合并到 `UsageSnapshot`：

- `Cumulative` / `Gauge`：替换旧值；
- `Delta`：只有携带尚未处理的 `event_id` 时才累加，否则拒绝该 metric；
- 同一 `metric_id` 的 unit 或 scope 突然改变：记录 schema mismatch 并拒绝覆盖；
- canonical `metric_id` 决定标准与私有通道是否表示同一逻辑指标；标准来源优先；
- `Turn` / `Session` 存入 `TabSession`；`RollingWindow` / `BillingPeriod` / `Account`
  存入 App-level store，不能误绑到某个 tab；
- session 切换或关闭时清除 session-scope metrics；窗口/账期指标按 `reset_at` 失效。

---

## 6. Adapter 边界与接口

建议实现位于 `tools/wta/src/protocol/acp/usage/`：

```text
Agent wire message
  -> Standard ACP normalizer
  -> allowlisted agent extension adapter（标准结果缺失时）
  -> UsageReport
  -> AppEvent::UsageReported { session_id, report }
  -> UsageStore 按 scope 合并到 TabSession 或 App-level UsageSnapshot
  -> provider-agnostic Usage UI
```

标准 ACP 不应由每个 provider 重复解析。建议：

```rust
enum UsageInput<'a> {
    SessionUpdate(&'a acp::schema::v1::SessionUpdate),
    PromptResponseMeta(Option<&'a acp::schema::v1::Meta>),
    // 只有 master 增加 agent extension 转发后，这个输入才可用。
    ExtNotification(&'a acp::schema::v1::ExtNotification),
}

trait UsageExtensionAdapter: Send + Sync {
    fn normalize(
        &self,
        input: UsageInput<'_>,
    ) -> Result<Option<UsageReport>, UsageAdapterError>;
}
```

`Ok(None)` 只表示该 adapter 不适用于这条消息；已识别 schema 但字段缺失、类型错误或数值
非法必须返回 `Err`，不能静默降级为 no usage。

### 6.1 Agent ID 的唯一来源与跨语言复用

> 本节的 `build.rs` registry codegen 尚未实现。当前状态仍是 C++ `BuiltinAcpAgents` 与
> Rust `KNOWN_AGENTS` 手工重复；下面内容是消除 drift 的目标设计。

Usage provider 分派使用 `agent_family_id`，其合法值必须以
[src/cascadia/inc/AgentRegistry.h](../../src/cascadia/inc/AgentRegistry.h) 中
`BuiltinAcpAgents` 的 ID 为唯一产品来源。这里应使用 `BuiltinAcpAgents`，而不是
`BuiltinDelegateAgents`：只有 ACP-capable agent 才能向 agent pane 实时报告 usage。

由于 Rust 不能直接引用 C++ `constexpr`，推荐在 WTA `build.rs` 中读取该 header，并在
`OUT_DIR` 生成 Rust constants，例如：

```rust
// Generated from AgentRegistry.h::BuiltinAcpAgents. Do not edit.
pub const COPILOT: &str = "copilot";
pub const CLAUDE: &str = "claude";
pub const CODEX: &str = "codex";
pub const GEMINI: &str = "gemini";
pub const BUILTIN_ACP_AGENT_IDS: &[&str] = &[COPILOT, CLAUDE, CODEX, GEMINI];
```

具体约束：

1. `tools/wta/src/agent_registry.rs::KNOWN_AGENTS` 的 `AgentProfile.id` 改用生成 family 常量；
2. Usage adapter registry 也使用同一批 family 常量，不允许裸字符串 key；
3. build script 对重复 ID、空 ID、非法字符和无法解析的 registry 直接失败；
4. Rust test 验证每个 `BUILTIN_ACP_AGENT_IDS` 都恰好对应一个 `AgentProfile`，并且每个
   ACP-capable profile 都来自生成列表；
5. header 变更必须触发 Cargo rebuild（`cargo:rerun-if-changed`）；
6. C++ 仍负责按设置和 GPO 选择 instance/command；Rust adapter 使用 master 最终确认的
  effective `agent_family_id`，不能根据命令行、display name 或消息内容重新猜测；
7. custom instance 的 family 必须作为独立设置/握手字段传递；family 不能替换 custom
  command，也不能绕过 `AllowCustomAgents`。若企业策略禁用该 family，是否允许 custom
  instance 使用该 family 必须 fail-closed 并由 policy 明确定义。

`agentInfo.name` 的 exact allowlist 只生成 compatible family，不生成私有 extension 的 trust。
私有 adapter 的授权还必须检查 `agent_instance_id` 来自 host 允许的内置 profile 及其重建的
launch command；用户提供的 `custom:*` 即使报告同名 reporter，首版也只能走标准 ACP。

这样新增 provider 时，canonical ID 只在 `AgentRegistry.h` 定义一次；Rust profile metadata
和 usage adapter 必须引用生成常量。Rust 构建会在缺少对应 `AgentProfile` 时失败，防止
C++ UI 已支持但 WTA 后端静默落到 `unknown`。

### 6.2 模块化与现有路径复用

实现必须保持以下单向边界，provider-specific 代码不能进入 store、projection 或 XAML：

1. **Identity resolver**：复用 `AgentProfile`、生成的 family constants 和 ACP initialize
  handshake，只负责 `instance + reporter -> effective family`；
2. **Standard normalizer**：唯一一处解析 typed ACP `UsageUpdate`，不按 provider 复制；
3. **Extension registry**：按生成的 family ID 查找小型、版本化、白名单 adapter；
4. **Usage store/merger**：复用 `TabSession` / App state，按 metric scope 与 aggregation 合并；
5. **Rust projection**：复用 `project_tab_state()` 和现有 `agent_state_changed`，不新建第二条
  COM/IDL/event transport；
6. **C++ rendering**：复用 `OnAgentStateChanged()`、`AgentPaneContent::StateChanged` 和
  `_UpdateBottomBarState()`，只消费 provider-neutral projection。

禁止重复实现 provider ID 字符串、ACP launch command、command-line family 猜测、usage
合并、单位格式化或 per-provider UI。新增 provider 的正常改动应局限于 registry metadata
和必要的 extension adapter；标准 ACP provider 不应需要专属 usage 代码。

首个实现 PR 应按上述模块提交小步改动并逐层测试，不在一个函数中同时完成 identity、wire
解析、状态合并和 XAML 格式化。测试直接调用内部模块，使错误在最接近根因处暴露。

处理顺序：

1. `StandardAcpUsageNormalizer` 总是先处理 typed `UsageUpdate`；
2. 按 `ClientState.agent_family_id` 查找白名单 extension adapter；该 ID 必须来自上一节的
  生成常量；
3. extension 只能补充标准结果缺少的 metric；
4. malformed private payload 返回带上下文的错误并记录日志，不能静默变成 `0`；
5. adapter 必须声明支持的 extension schema/version，并提供真实 payload fixture tests。

当前 master 的 agent-side `on_receive_notification` 只转发 `SessionNotification`，会忽略 agent
发来的 `ExtNotification`；helper 目前收到的 ext channel 主要是 master 自己的
`intellterm.wta/*` 通知。推荐首版只实现 typed `SessionUpdate::UsageUpdate`。虽然
`PromptResponse._meta` 在现有 prompt response 路径可达，但它属于尚未验证的 provider
extension 输入，与 `ExtNotification` 一样只预留接口，不纳入 §9.12 首版范围。如果后续实测
确认 provider 只通过 extension notification 报告 usage，必须先在 master 增加**按 session
定向路由**的 agent extension 转发；不能把带账户/session usage 的通知无条件 fan-out 给所有
helper。

`run_acp_client_over_pipe()` 当前只有 `agent_id`，它混合了 instance 与 family，而且 master
可能回退到自己的 default agent。需要增加独立 `agent_family_id`。Extension adapter 必须
使用 master 最终确认的 effective family，不能盲信 helper 请求值。实现时应由 initialize
response 的受控 `_meta.wta` 回传 resolved instance + family，或增加等价内部握手字段，再
写入 `ClientState`。Command/display title 不能用于 runtime 猜测；`agent_info.name` 只有在
精确命中 `AgentProfile.acp_reporter_ids` allowlist 时，才能作为 reporter→family 的证据。

`ProviderApi` 仅是 domain model 的未来来源类型。整体设计允许后续加入经验证的 agent
extension payload，但 §9.12 推荐首版只处理标准 ACP `UsageUpdate`，不新增私有 extension、
外部 billing API、凭据或后台轮询。

Provider 规则：

- **Copilot**：只有实际机器消息报告 provider usage 数值与单位时，才产生
  `ProviderCredit { issuer_id: "github", ... }`。`unit_id` 必须来自已验证 extension schema；
  `AI Units` 与历史 `AI Credits` 是否为同一单位不能由 WTA 猜测。不能根据 model multiplier
  推算。
- **Claude/Codex**：若 adapter 发送标准 `Cost`，不需要专属 usage adapter；source 的
  `reporter_id` 应记录 ACP adapter 名称，避免暗示一定来自 provider billing API。
- **Gemini**：不建立 provider-specific 规则或 adapter；仅可能经过 provider-neutral 标准 ACP
  路径。Antigravity 不继承该 family，直到单独的协议/identity 设计完成。

---

## 7. UI 规则（位置/ownership 已决定，展示内容待定）

Usage 位于 C++ window-level Bottom Bar 的右侧（session 按钮左边）。下面只是归一化数据的
展示示例，不代表最终决定展示哪些 metric、展示几个或采用什么 icon：

```text
GitHub Copilot · Claude Sonnet 5  •  117.9 AI Units
Claude Agent  · Claude Sonnet 5  •  0.55 USD
```

这些只是归一化后的**展示示例**，不代表当前 agent 已经实际发送对应字段。

如果最终主栏空间只允许一个 primary metric，可以采用以下**暂定**优先级：

1. session billing / provider credits；
2. session total tokens；
3. context ratio；
4. 无指标则隐藏右侧 usage。

格式化规则：

- ISO 4217 currency 保留 agent 报告的货币码，显示为 `<数值> <currency>`，例如
  `0.55 USD`；不在 WTA 中转换成其他货币；
- provider credit 使用稳定 `unit_id`，`display_name` 只是已知单位的本地化/fallback label；
- unknown/custom label 视为 agent 提供的显示文本，必须限制长度并清理控制字符；
- tooltip 显示 scope、aggregation、更新时间和 reporter/source；
- 不显示“精确费用”“实际账单”等无法由协议保证的措辞。

---

## 8. 实测与验收计划

在写 provider adapter 前，只对本功能范围内的 Copilot、Claude ACP adapter 和 Codex ACP
adapter 捕获 `wta-acp-debug.log` 中的 wire message。Gemini 不进入 provider E2E/compatibility
matrix；provider-neutral 标准 ACP contract 由 typed synthetic values 覆盖。每个 in-scope
agent 记录：

1. `initialize` / `session/new` 是否包含 usage capability 或 model multiplier metadata；
2. 是否发送标准 `session/update` → `usage_update`；
3. `used`、`size`、`cost.amount`、`cost.currency` 的实际值和更新频率；
4. `PromptResponse._meta` 是否包含 token、quota 或 credits；
5. 是否发送 usage 相关 extension notification；
6. 值是 turn/session/account 范围，delta 还是 cumulative；
7. `/clear`、切换模型、恢复 session 后如何重置；
8. 是否能与 agent 自己的 `/usage`、`/stats` 或退出摘要对应。

测试要求：

- 为每个启用的私有 adapter 保存去敏 fixture；
- 测试缺字段、错误类型、负数、NaN/Infinity、重复通知、乱序通知与 reset；
- 测试标准 ACP 与私有扩展同时出现时标准值胜出；
- 测试没有 usage 时 UI 完全隐藏，而不是显示零；
- 测试不同单位不会被累加或换算。

### 8.1 Claude/Codex 高保真本地 mock

这里的“完美 mock”指 **IT 所能观察的协议、身份和 routing 行为与真实 adapter 一致**，不是
伪造 Anthropic/OpenAI 的最终账单。必须运行真实 pinned ACP adapter，不能用自写 fake ACP
server 代替。下面是目标产品链路；它要求先把 C++/Rust 内置 profile 的启动映射更新为本文
固定版本：

```text
Claude:
IT -> @agentclientprotocol/claude-agent-acp@0.59.0
  -> Agent Maestro Anthropic-compatible API
  -> VS Code LM

Codex:
IT -> @agentclientprotocol/codex-acp@1.1.2
  -> Codex custom model-provider / gateway config
  -> Agent Maestro OpenAI Responses-compatible API
  -> VS Code LM
```

两条链路已在 2026-07-22 完成本机端到端 prompt round-trip，详见 §8.2。Codex 官方
`1.1.2` adapter 通过 Agent Maestro Responses route 返回预期文本；不能用自写响应替代这个
真实 adapter 验收。

每条链路都必须通过以下 acceptance gate：

1. 完整通过 ACP `initialize`、`session/new`、`session/prompt` 与流式 `session/update`；
2. 安装对应 standalone CLI，使 Settings 通过当前 `PATH` 检查显示内置 `Claude` / `Codex`；
  选择内置 ID 后，helper 只发送 family/model，master 按 host policy 和 `AgentProfile` 重建受信
  pinned command，不执行 pipe 上传来的 command；
3. 记录真实 `agentInfo.name`，精确命中 `AgentProfile.acp_reporter_ids` allowlist；不使用 command
  substring、display name 或模糊匹配；内置 instance 的 effective `agent_family_id` 必须分别是
  生成常量 `claude` / `codex`；
4. provider layer 只调用对应 family 的代码。首版标准 normalizer 与 family 无关；未来启用私有
  extension adapter 时，还必须验证 trusted built-in instance + exact reporter + negotiated
  schema/version。反向 family、unknown reporter 和伪造相似 reporter 必须 fail closed；
5. 覆盖 text streaming、tool call、model selection 和标准 `UsageUpdate`；若 adapter 未发送
  usage，记录“未发送”，不能为通过测试而注入数值；
6. 验证 source 中的 reporter、issuer、unit、scope、aggregation 与 model attribution，不把
  Agent Maestro / VS Code LM 的估算或 metadata 标成 Anthropic/OpenAI 官方账单；
7. 同一个 pinned package 在 clean cache 下重跑，结果仍满足相同 identity/routing contract。

另外保留 `custom:npx` 作为**负向信任测试**：它可以运行相同 pinned adapter，并可从 exact
reporter 得到 compatible family 供通用 UX 使用，但必须保持 `agent_instance_id = custom:npx`、
保留 host 已信任的 custom command，且只能进入标准 ACP normalizer。即使它报告官方 reporter
名称，也不得启用 Claude/Codex 私有 extension adapter。这样同时证明 family 分流准确且不会
把可伪造的 custom process 提升成受信 provider。

本地 E2E mock 的 proxy 配置、临时 gateway/model-provider 配置、启动脚本、凭据、raw 抓包、
debug log 和为实验修改的 Agent Maestro 代码**一律不提交**。这些文件放在 repo 外或 ignored
临时目录，并在测试后删除。生产 normalizer/identity resolver 的确定性单元测试仍应提交；
它们直接构造 typed ACP protocol value 验证产品 contract，不包含或实现 mock provider。

### 8.2 Feature 开发前 E2E 能力验证（2026-07-22）

在没有实现 Usage feature 的情况下，已先验证 build/deploy、桌面 UI automation、真实 ACP
adapter routing 和 Agent Maestro 后端。测试复用现有 [test/e2e](../../test/e2e) 的 ItE2E
PowerShell module；新增 orchestration scripts、screenshots、result JSON 和本机 provider 配置
全部位于 git-ignored `test/e2e/artifacts/acp-provider-preflight/` 或 user home，不进入 feature
commit。该 harness 与 Usage production code 没有依赖，可在未来独立整理成 test PR。

#### Build / deploy 基线

- PowerShell 7.6.3；Windows App CLI 0.5.0；Pester 6.0.1；
- ItE2E hermetic self-tests：11 passed / 0 failed；
- ItE2E Dev live self-tests：12 passed / 0 failed；
- Visual Studio 2026 Enterprise / MSBuild 18.7.12002.237；
- `cargo build --target x86_64-pc-windows-msvc` 成功；
- x64 Debug Terminal/CascadiaPackage build 成功；
- 先移除旧 loose Dev registration，再用 `DeployAppRecipe.exe` 部署成功（exit 0）；
- 部署后的 `wta.exe` SHA256 与 Cargo output 一致，部署后的 `WindowsTerminal.exe` SHA256 与
  appxrecipe source 一致。

#### Provider mock 与 UI 结果

“Mock” 使用真实 adapter + Agent Maestro 2.10.0 + VS Code LM，不使用 fake ACP server：

| Check | 结果 | 可见/协议证据 |
|---|---|---|
| Launch Terminal | PASS | Dev window PID/HWND 解析成功，截图可见 Windows PowerShell terminal |
| 左下 Agent button | PASS | UIA `AgentToggleButton` 打开 pane，截图显示 `Copilot v1.0.73` 和 chat input |
| Claude switch | PASS | `/agent claude` 后 master 解析为 `claude-agent-acp`；截图显示 `Claude Agent v0.59.0`，真实回复 `CLAUDE_MOCK_OK` |
| Codex switch（迁移前 repo） | PASS | `/agent codex` 后旧映射启动 `@zed-industries/codex-acp@0.16.0`；截图显示 `Codex v0.16.0`，真实回复 `CODEX_MOCK_OK`；Step 0 后新 session 使用官方 1.1.2 |
| Codex target adapter | PASS | `custom:npx` 启动官方 `@agentclientprotocol/codex-acp@1.1.2`；截图显示 `Codex v1.1.2`，真实回复 `OFFICIAL_CODEX_OK` |
| 右下 Session button | PASS | UIA `SessionToggleButton` 后截图显示 `Agent sessions: Copilot`、历史 session rows 与 navigation footer |

另外完成真实 terminal tool-call capability gate。Prompt 不包含完整 marker；只有 provider
通过 ACP tool call 执行指定 PowerShell、在 `%TEMP%` 创建且写入精确 marker 文件，测试才通过：

| Adapter | Tool side effect | Chat projection | 证据 |
|---|---|---|---|
| Claude ACP 0.59.0 | PASS：写入 `CLAUDE_TOOL_OK` | 显示 tool `Completed`，并显示 `CLAUDE_TOOL_OK` | `07-claude-tool-call.png` |
| 官方 Codex ACP 1.1.2 | PASS：写入 `CODEX_TOOL_OK` | 显示 tool `Completed`；当前 adapter 未把 stdout marker 投影到 chat | `08-codex-tool-call.png` |

Tool side-effect 是跨 adapter 的硬 oracle；chat 中是否重复显示 stdout 是 adapter presentation
差异，不能作为 tool 是否执行的唯一判据。

主 harness 的五张最终截图名为 `01-terminal.png` 到 `05-sessions.png`；官方 Codex target
adapter 另有 `06-official-codex-1.1.2.png`。每个 provider reply marker 都不以完整字符串出现
在 prompt 中，测试必须等待 agent output 出现 marker 后才能截图，避免把用户输入误判为回复。

#### 测试中发现的环境门槛与框架改进

- Agent Maestro 首次调用 VS Code LM 会弹出 language-model access consent；未点击 `Allow`
  时 `/v1/messages` 会一直等待，而 `/count_tokens` 仍可成功。Harness prerequisites 必须显式
  检查/处理该授权；授权后 Anthropic route 返回 `ROUTE_OK`；
- npx 首次下载 Codex adapter 可能超过 WTA `probe-models` 的 25 秒 initialize timeout；缓存后
  官方 `1.1.2` 和旧 `0.16.0` 均可稳定 initialize；
- 当前 ItE2E `Get-AgentPaneSession` 在多 tab pre-warm 与 pane 刚进入 connecting 时有解析竞态。
  本地 harness 先归一化为单 tab，再使用 explicit pane session ID，并通过
  `Wait-NewAgentPaneSession` 等待 registration；这部分适合未来独立 test-framework PR；
- 当前 `Get-SessionRows` 对此 build 的无边框/字符编码 capture 可能返回 0，即使截图和 raw pane
  text 已有 rows。Session button 的硬 oracle 是可见 header + navigation footer；row parser
  应在独立测试 PR 中修正；
- 真实模型是否遵从“必须调用 tool”的 prompt 不是确定性的：Claude capability gate 曾成功，
  后续一次重跑也曾选择不调用 tool；ItE2E 的 HWND/AgentToggleButton 定位也出现过瞬时失败。
  因此这些 live gates 证明端到端能力和兼容性，但不能替代可重复的 typed ACP fixture/unit test。
  独立 test-framework PR 应增加 retry 分类、window/pane identity 固定与 deterministic protocol
  harness；不能靠无限重试把 provider failure 变成通过；
- 官方 Codex `1.1.2` 对 Agent Maestro model `gpt-5.3-codex` 输出 metadata fallback warning，
  但 ACP initialize、session/new、prompt 和文本响应均成功；迁移时应加入 compatibility fixture。
- 截图顶部的缺失 `C:\Miniconda3\Scripts\conda.exe` 错误来自本机 PowerShell profile，与
  Intelligent Terminal、ACP adapter 或 provider routing 无关。

本轮验证证明可以在正式 feature 开发前自动化用户要求的窗口、点击、agent 切换、真实
provider round-trip 和 Session view 截图。它尚未证明 Usage payload：当前 helper 仍忽略
`UsageUpdate`，Usage feature 尚未实现；开发后必须继续按 §8 的 wire/fixture gates 验证。

只有完成实测后，provider 状态才能从“第三方报告/待验证”改为“本项目验证”。

---

## 9. 实现前仍需解决的设计问题

本节记录前面方案尚未完全覆盖的边界。标记为 **P0** 的项目必须在首版编码前确定；
**P1** 可以分阶段实现，但数据结构必须预留。

### 9.1 P0：ACP 标准与私有扩展如何声明能力和版本

标准 `UsageUpdate` 当前没有单独 capability；agent 可以发送，也可以永远不发送。WTA 对
标准路径的规则应是“能解析就接收”，不能因为 initialize 没有 usage capability 就拒绝。

私有扩展则必须显式协商：

- agent 在 `InitializeResponse.agentCapabilities._meta` 中声明 extension schema ID 和版本；
- adapter 使用精确 `(agent_family_id, reporter_id, schema_id, major_version)` 匹配，不根据
  字段长得像 usage 或 family 标签就猜；
- major version 不支持时禁用该 adapter；新增 optional field 的 minor version可向前兼容；
- 没有声明 capability 时，不主动调用私有 usage method，也不解析同名未知 payload；
- 未处理的 `SessionUpdate` / `ExtNotification` 记录**不含值**的 debug schema 信息，避免
  新版本出现时静默丢弃且无法诊断。

不能只在 WTA 自己的 `_meta.wta` 中声明一个版本就认为 provider 会遵守；私有 schema 必须
是双方已有约定或 provider 官方公开 contract。

### 9.2 P0：Usage 通知不能与文本 chunk 一起被无差别丢弃

当前 master→helper notification channel 有容量限制。满载时会丢弃 notification；这对
可继续流式补齐的文本尚可接受，但一次性的最终 usage report 如果被丢弃，UI 会永久缺值。

首版必须选择一种可靠策略：

- `UsageUpdate` / normalized cumulative/gauge 使用单独的 latest-value channel（推荐）；或
- master 按 `(session_id, metric_id)` coalesce，只保留最新值，helper 恢复后发送；
- 不能在 agent CLI I/O loop 中等待慢 helper，否则会阻塞共享 agent 和其他 tab；
- cumulative/gauge 丢中间值没关系，只要最新值最终送达；
- delta metric 必须有 provider event ID 才可重试/去重，否则首版不支持 delta。
- 高频 report 在 Rust 侧 coalesce，并限制发往 C++ UI 的刷新频率（例如每 250–500ms 最多
  一次），但 turn 结束时立即刷新最终值，避免 Bottom Bar 抖动和无障碍重复播报。

还要定义乱序规则：同一连接内按接收顺序应用；跨重连时没有 provider sequence 的旧报告
不得覆盖新连接报告。需要在 source 中记录 `connection_generation`，不仅记录本地时间。

### 9.3 P0：Session 生命周期和计费生命周期不是一回事

以下规则在 Usage feature 实现后生效；当前 `TabSession` 尚无 usage state。

必须按以下事件定义明确行为：

| 事件 | Turn/Session usage | Account/账期 usage |
|---|---|---|
| `session/new` / `/clear` | 新 snapshot；不继承旧 session | 保留，但标记同一 account generation |
| `session/load` / resume | 不从旧 UI snapshot 猜测；等待 agent 重新报告 | 保留；若 provider 报告新值则替换 |
| model switch | session 累计值不清零；没有 wire attribution 时不拆成 per-model | 保留 |
| prompt 失败、取消、拒绝、内部重试 | 不减、不回滚；provider 仍可能计费 | 只接受后续报告值 |
| agent/master 暂时断连 | 保留最后值但标记 stale | 同左 |
| agent/account 切换或重新登录 | 清除旧 account-scope 值，增加 account generation | 等待新账户报告 |
| tab/session 关闭 | 删除 turn/session scope | account scope 由 central store 生命周期决定 |

“prompt 没成功所以没有费用”不是合法推断；失败、取消和 provider 内部 retry 是否计费只能由
provider 报告。

### 9.4 P0：Account scope 不能只放在 per-tab helper

当前架构是一个 tab 一个 helper，而同一 agent CLI 可被 master 复用。若未来读取“本周剩余
AI Units”之类账户级 quota，把它只存在 `TabSession` 会导致多个 tab 显示不同的旧值。

建议：

- `Turn` / `Session` scope 由 helper 的 `TabSession` 保存；
- `Account` / `BillingPeriod` / 跨 session quota 由 master 的 central usage store 保存；
- central key 至少包含 `agent_instance_id`、`effective_agent_family_id`、
  `agent_process_generation` 和 `account_generation`；
- 不使用用户名、邮箱或 account ID 作为 UI/store key，也不写日志；
- master 将最新 account snapshot 定向广播给使用同一个 agent generation 的 helper；
- 首版如果只实现标准 session `UsageUpdate`，可以暂不实现 central store，但必须拒绝把
  account scope 私有数据错误地塞入某个 tab。

### 9.5 P0（已决定）：右下角 Usage 由 C++ Bottom Bar 渲染

已决定的 ownership/目标位置是 window-level Bottom Bar，而不是 Rust TUI 或 agent pane
顶部 header；`UsageGroup` 当前尚未实现。现有 Bottom Bar 定义在 `TerminalPage.xaml`，最右侧
session 按钮位于 `Grid.Column="3"`，`Grid.Column="2"` 是把它推到右边的 star spacer。因此
实现时在 Column 2 增加一个 `UsageGroup`，右对齐并紧邻 session 按钮：

```text
BottomBar
  Column 0: Agent toggle
  Column 1: Diagnostics
  Column 2: * spacer + right-aligned UsageGroup   <- designer 草稿位置
  Column 3: Session button
```

不应放在 Rust TUI，原因是：

- Rust TUI 在 agent pane 内部，pane stash/关闭时不可见；
- Bottom Bar 是窗口 chrome，始终显示并反映 active tab；
- C++ 已经负责 Bottom Bar 的主题、布局、tooltip、AutomationProperties 和 tab 切换刷新；
- 如果 Rust 和 C++ 同时渲染 usage，会出现两份 UI 和两份状态。

推荐 ownership：

```text
Rust WTA
  - 接收 ACP/provider usage
  - 校验、归一化、按 scope 合并
  - 保存每个 tab/session 的 UsageSnapshot
  - throttle/coalesce 更新
        |
        | existing agent_state_changed per-tab snapshot
        v
C++ TerminalPage / AgentPaneContent
  - 按 tab_id 路由
  - 缓存 normalized usage projection
  - _UpdateBottomBarState 读取 active tab
        |
        v
XAML TerminalPage.BottomBar.UsageGroup
  - 只负责显示、隐藏、布局、tooltip、无障碍
```

无需新增 `agent_usage_changed` COM/IDL route。现有 `agent_state_changed` 本来就是 WTA→C++
统一的 per-tab UI snapshot，并在 tab 切换时重新投影；给它增加可选 `usage` 字段即可：

```json
{
  "method": "agent_state_changed",
  "params": {
    "tab_id": "...",
    "view": "chat",
    "pane_open": false,
    "usage": {
      "items": [
        {
          "metric_id": "acp.context.window",
          "value_decimal_text": "20",
          "unit_id": "token",
          "unit_display_name": "Tokens",
          "stale": false
        }
      ]
    }
  }
}
```

这里的 payload 是**暂定 transport shape**，不代表最终展示内容已经确定。数据/单位仍在讨论，
所以 XAML 不应硬编码“恰好两个字段（tokens + cost）”。`UsageGroup` 应是可显示 0–N 个
normalized item 的数据驱动容器，并设一个小的产品上限（例如主栏最多 2 个，其余放 tooltip
或未来详情页）。`usage: null` 或空 items 明确清除并隐藏旧数据。

不要把 usage 拼进 model/name 字符串。事件只传 normalized 的 decimal/count text、稳定
`unit_id`、provider 报告的 unit display name、scope、stale/source category；不把 provider
原始 JSON 传到 C++。C++ 不计算价格、不换算单位，只格式化和显示。

落点建议：

- Rust：在 `project_tab_state()` 的 `agent_state_changed.params` 中投影 usage；
- C++：在 `OnAgentStateChanged()` 中解析并调用
  `AgentPaneContent::UpdateAgentUsage(...)`；
- `AgentPaneContent`：缓存 per-tab usage，变化时 raise `StateChanged`；
- `TerminalPage::_UpdateBottomBarState()`：从 active tab 的 `AgentPaneContent` 读取并更新
  `UsageGroup`；
- XAML：在 Bottom Bar Column 2 增加右对齐的动态 usage presenter。

#### Settings UI 与 pane position 的影响

Settings → Agents 的下拉框不是无条件列出所有 `BuiltinAcpAgents`。C++ 先应用 GPO
`AllowedAgents`，再检查对应 CLI 是否已安装且可从 `PATH` 找到。因此只安装了 Copilot 的
机器会看到 `GitHub Copilot` 和 `+ Add New...`，而不会看到 Claude/Codex/Gemini。

用户不应通过 `+ Add New...` 手工填写这三个内置 provider：

- 安装对应 CLI、确保它在 `PATH`，重启/重新打开 Settings 后，内置条目自动出现；
- Claude/Codex 的 npx ACP wrapper 由 WTA 自动启动，不由用户填写；
- `+ Add New...` 仅用于用户自己的 ACP command，并保存为 `custom:*` agent。

§2.4 中用 `custom:npx` 启动 pinned Claude adapter 是当前代码尚未更新内置版本时的本地
connectivity/负向信任实验，不是正式产品配置或 provider-specific routing 验收路径。

`agentPanePosition` 支持 `bottom`、`right`、`top`、`left`，改变的是 agent pane 相对 terminal
的拆分位置；运行时 `/move` 还可覆盖当前 tab。**它不改变 window-level Bottom Bar 的位置。**
因此右下角 `UsageGroup` 始终留在 Bottom Bar，只有 agent-pane toggle icon 根据 pane 方向
变化。切换 tab 或 per-tab `/move` 后，Usage 内容跟随 active tab，但控件本身不移动。

### 9.6 P0：Source trust、输入校验和 custom agent 边界

标准 ACP 只保证字段结构，不保证数值等于最终账单。私有扩展更不能因为进程名是
`copilot` 就自动可信。

- 标准 `UsageUpdate`：所有 ACP agent（包括 custom agent）都可使用；
- 私有 adapter：只允许 host policy 允许的内置 `agent_instance_id`、由 master 重建的 trusted
  launch、精确 `agent_family_id + reporter/schema/version` 全部匹配；
- custom agent 首版只能走标准 ACP；不解析 custom `_meta`/extension usage；
- 数值必须 finite、非负，并有合理上限；ratio 的 `size` 必须非零；
- currency 必须是规范化的 3 字符 ISO 4217 code；不合法属于 recognized malformed report，
  开发/test 返回 `Err`，release 由下述唯一最外层 containment 清除本次不可信 snapshot 并隐藏
  `UsageGroup`；
- provider unit label 必须限制长度、移除控制字符和换行；
- schema 识别成功但数据非法时返回 parse error，不能静默丢弃单个字段或显示 `0`；
- source 从标准切到私有或 connection/account generation 改变时，应清除旧冲突值，不能
  把两个来源拼成一个累计值。

#### 开发期 fail-fast，完成后只在最外层 containment

Usage 是辅助功能，但开发期间不能用“辅助功能应降级”掩盖实现错误。开发顺序固定为：
§9.12 推荐首版不启用私有 extension adapter，因此首版的同一规则只覆盖 standard
`UsageUpdate` normalizer/store/projection；未来启用私有 adapter 时复用同一个最外层边界，
不增加 provider-specific catch。

1. **实现与测试阶段不加兜底**：normalizer、identity resolver、extension adapter、store、
  projection 和 UI contract 不添加 blanket `try/catch`、`catch (...)`、`unwrap_or_default()`、
  recognized malformed schema 的 `Ok(None)` 或“保留旧值后继续”等静默路径；
2. 非法 identity、schema、数值、unit、scope 或 aggregation 立即返回有上下文的 error；单元/
  integration test 必须失败，debug 运行也应直接暴露错误；
3. 所有模块和 Claude/Codex E2E 验收完成后，才在 Rust helper 处理 usage-bearing
  `SessionUpdate` 的**最外层 Usage dispatch** 加一次 containment；normalizer、identity
  resolver、adapter、store 和 projection 都把 `Result` 原样传播到这里，不分别捕获；
4. 最外层只处理 Usage 子系统错误：输出去敏的 schema-level 诊断，清除本次不可信 snapshot
  并隐藏 `UsageGroup`；不得返回 ACP prompt error、断开 agent、阻塞消息流、影响 tool call
  或使 Terminal 崩溃；
5. 正常“不适用”仍可返回 `Ok(None)`，但只限 adapter 未匹配当前 message；已识别 schema
  的失败必须保持 `Err`，由最外层 containment 处理；
6. 单元测试继续直接调用 containment 内部函数，以保证最终加上外层保护后，开发测试仍然
  fail fast；另加一个边界测试证明 Usage failure 只隐藏 UI 而不终止会话。

该最外层保护使用单一 Rust `Result` handling boundary。C++ 只接收 normalized projection，
不新增 usage-specific `try/catch`；它沿用 `agent_state_changed` 的 JSON 输入校验，并在 debug/
contract tests 对 malformed usage sub-object 失败，在 release 拒绝该 sub-object 并隐藏
`UsageGroup`。这是现有跨进程输入验证，不是第二套业务异常吞错层。上述 fail-fast 约束只适用
于新增 Usage pipeline，不要求移除仓库中与本功能无关的既有异常边界。

### 9.7 P0：Usage 数值不能进入普通日志或遥测

当前 helper 在 trace 级别会格式化完整 `SessionUpdate`。一旦支持 `UsageUpdate`，其中费用、
quota 和 token 数可能进入 `wta-acp-debug.log`，随后又被 bug-report zip 收集。

必须增加 usage-specific redaction：

- 普通日志只记 `metric_id`、source category、schema version、是否解析成功；
- 不记录 amount、token count、余额、account 标识或原始 provider payload；
- ETW/telemetry 也不发送实际数值；
- fixture 必须去敏；
- provider API（未来）响应不能进入 wire trace；
- 如果产品需要持久化历史 usage，必须另做隐私评审；首版保持内存态。

### 9.8 P1：Stale、重连和恢复后的展示规则

ACP 没有“重新查询当前 session usage”的标准 request。Master/agent 重启后，agent 不一定
重发旧累计值。因此：

- 每个 metric 保存 `observed_at`、`connection_generation` 和可选 provider timestamp；
- 同一 App 运行中的临时断连保留最后值，但 UI 标记 stale（例如 tooltip 显示“上次更新”）；
- 新 generation 第一次报告前不能把旧值称为 current；
- agent/account identity 改变时立即清除旧值；仅 transport 重连才允许保留 stale snapshot；
- App 重启首版不恢复 usage history，直到 agent 新报告。

### 9.9 P1：Provider API 不能直接复用 CLI 登录凭据

若未来 ACP/extension 仍无数据，provider adapter 可以考虑官方机器接口，但必须作为独立
安全项目：

- 优先请求 agent CLI 提供官方结构化 RPC，而不是 WTA 自己调用 provider Web API；
- WTA 不读取 Copilot/Claude/Codex CLI 的私有 token 文件；Gemini provider API 不在范围内；
- 直接 API 需要独立 auth、最小 scope、rate-limit、缓存、退避和注销处理；
- 账户级 API 不可按每个 tab 轮询，应由 master 去重；
- HTTP 错误或 quota API 不可用属于未来 provider API 的预期 operational failure：同一 identity
  可保留 stale snapshot 并标明更新时间，identity 改变则清除；它不等同于开发期应 fail-fast
  的 schema/programming error，也不能影响正常聊天；
- provider API source 不自动覆盖标准 ACP；是否更权威必须按具体 contract 决定。

因此 §9.12 推荐首版只实现标准 ACP `UsageUpdate`，不启用私有 agent extension，也不实现
direct Web API。

### 9.10 P1：格式化、本地化和无障碍

- API 报告的稳定 `unit_id` 与显示名称分开保存；未知单位保留 provider 报告名称，不翻译；
- 标准 `Tokens`、currency code 等通用单位可由 UI 本地化格式化，但不能换算数值；
- 小数位规则按单位定义，不统一强制两位；不能把 `0.004 USD` 显示成 `0.00 USD`；
- Bottom Bar 在窄宽度下优先保留数值和单位，详细 source/scope 放 tooltip；
- XAML 增加 AutomationProperties.Name；终端路径需要纯文本、不能只靠颜色表达 stale/error；
- 数字更新不要被屏幕阅读器按 token/chunk 高频朗读，只在 turn 完成或显著变化时通知。

### 9.11 P1：Rollout、kill switch 和兼容矩阵

- 标准 ACP normalizer 与每个 provider adapter 分开 feature/kill switch；
- Dev/Preview 先开，Release 可远程/配置禁用有问题的私有 adapter；
- fixture 记录 agent CLI/adapter 版本和 schema version；
- 测试重复、乱序、channel 满、重连、session load、模型/账户切换、取消与失败 turn；
- 使用直接构造的 typed ACP values 做确定性 contract test；本地真实 pinned adapter E2E 只做
  compatibility/routing 验证，其 mock harness 不提交；
- 新 provider 默认只能走标准 ACP，私有 adapter 未经 review 不进入白名单。
- Gemini 不进入本功能 compatibility matrix；Antigravity 在独立调查并进入 registry 后再决定
  是否加入，不能把 Gemini 测试结果沿用到 Antigravity。

### 9.12 推荐的首版最小范围

为了避免第一版同时解决账户 API、持久化和跨窗口 quota，建议只实现：

1. 标准 ACP `UsageUpdate` 的 `used / size / cost`；
2. 仅 `Session` scope；
3. cumulative/gauge latest-value 可靠传输；
4. 内存存储，不跨 App 重启；
5. C++ Bottom Bar 的右侧 `UsageGroup` 显示 normalized metric（具体内容与数量待产品决定）；
6. 数值不进日志/遥测；
7. custom agent 标准 ACP 自动支持；
8. 私有 provider adapter 暂不启用，直到拿到真实结构化 fixture。
9. 不实现或验证 Gemini-specific provider；Antigravity 留待后续独立设计。

Copilot 1.0.71 因没有结构化动态 usage，首版仍隐藏其 usage。这样可以先验证通用 contract，
而不为了某一家 provider 引入不稳定文本解析或直接 billing API。
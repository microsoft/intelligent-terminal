# Shell / Terminal IntelliSense 市场与产品调研

> 调研日期：2026-07-27  
> 调研范围：Shell 原生补全、终端产品补全、AI 命令建议、编辑器 IntelliSense，以及对 Intelligent Terminal 的产品与技术启示。
>
> 源码实现、算法和延迟控制详见 [`shell-intellisense-key-path-research.md`](shell-intellisense-key-path-research.md)。

## 1. 执行摘要

Shell IntelliSense 不是单一功能，而是一组围绕命令行输入的能力：

1. **Completion（补全）**：对当前 token 给出确定性候选，例如命令、子命令、参数、文件、分支和资源名。
2. **Prediction / Autosuggestion（预测）**：根据历史、上下文或模型预测完整命令，以灰色 ghost text 展示。
3. **Syntax help（语法辅助）**：实时着色、错误提示、参数说明和文档。
4. **Validation / Quick Fix（校验与修复）**：执行前发现问题，或执行失败后给出修复命令。
5. **AI assistance（AI 辅助）**：通过自然语言生成、解释或修复命令，通常具有更高延迟和风险。

市场已经形成两种主要架构：

- **Shell-owned**：Shell/line editor 负责计算和绘制建议。PowerShell/PSReadLine、bash、zsh、fish、Nushell，以及绝大多数传统终端属于这一类。
- **Terminal-owned**：终端读取当前命令行、维护 completion spec，并用原生 GUI 绘制候选。Warp、VS Code Terminal Suggest、Fig/Amazon Q/Kiro 和 inshellisense 属于这一类。

核心结论：

- **PowerShell 7 + PSReadLine 是最成熟的 Shell 预测参考实现**：同时支持 history、插件、inline/list 两种视图、逐词接受、反馈回路，并对 predictor 设置了硬编码 **20ms** 截止时间。
- **Warp 和 VS Code 是最值得模仿的 Terminal-owned 产品**：前者拥有完整的 terminal-native 输入模型，后者在传统 PTY 终端约束下，通过 shell integration、缓存和 GUI popup 实现渐进增强。
- **行业 UX 已高度收敛**：灰色 ghost text、右箭头接受、逐词接受、Tab 打开/提交候选列表、继续输入即忽略。
- **AI 不能替代传统补全**：静态/spec/本地历史补全应运行在 20–100ms 交互预算内；网络 AI 更适合显式触发、后台生成、解释和修复，且不应自动执行。
- **Intelligent Terminal 不应从零设计 completion spec**：优先复用 Fig/withfig 生态或兼容层，并复用仓库已有的 `SuggestionsControl`、fzf matcher、shell integration、OSC 133、Quick Fix 和无障碍基础设施。

## 2. 什么是 Shell IntelliSense

### 2.1 能力分层

| 层级 | 用户问题 | 常见触发 | 数据来源 | 典型产品 |
|---|---|---|---|---|
| Token completion | “这里可以输入什么？” | Tab、Ctrl+Space、`-`、`/` | completion script、CLI spec、文件系统、动态命令 | bash、zsh、fish、VS Code、Warp |
| Command prediction | “我接下来大概率要运行什么？” | 每次输入 | 本地历史、当前目录、前一条命令、插件 | PSReadLine、fish、Warp |
| Documentation | “这个参数是什么意思？” | 选中候选、输入参数 | spec、man page、help output | VS Code、Nushell、Fig |
| Syntax/validation | “命令是否有效？” | 每次输入或 Enter | parser、AST、命令发现 | fish、PSReadLine |
| Quick Fix | “失败后怎么修？” | 非零退出码、错误模式 | shell integration、规则、包管理器、AI | VS Code、Intelligent Terminal |
| AI generation | “帮我完成一个目标” | 显式自然语言或 opt-in inline | LLM、历史、目录、终端输出 | Warp Agent、Kiro、Copilot CLI |

### 2.2 IntelliSense 与普通 Tab Completion 的区别

传统 Tab Completion 通常只返回文本候选；完整 IntelliSense 还包括：

- 当前光标和参数位置的语义理解；
- 模糊匹配、排序和推荐项；
- 参数描述、类型、示例和文档；
- inline prediction 与 popup list 协同；
- 异步动态候选及取消；
- 缓存、降级、隐私和无障碍；
- 接受、执行、失败后的反馈闭环。

### 2.3 用户的标准使用流程

现代产品中最常见的流程是：

1. 用户输入 `git ch`。
2. 系统立即显示 `git checkout main` 的灰色 ghost text。
3. 用户可以继续输入以忽略，按右箭头接受全部，或按组合键只接受一个词。
4. 用户按 Tab/Ctrl+Space 打开候选列表。
5. 候选显示 `checkout`、`cherry-pick` 等子命令及描述。
6. 输入 `git checkout ` 后，系统异步获取本地分支，并更新列表。
7. 用户接受候选时，默认只插入命令；是否立即执行应是独立且谨慎的设置。

## 3. Shell 原生能力

### 3.1 PowerShell 7 / PSReadLine

PowerShell 的 IntelliSense 实际主要由 PSReadLine 提供。它把传统 completion、历史预测和插件预测分开处理。

**用户体验**

- `InlineView`：默认灰色 ghost text。
- `ListView`：在提示符下显示列表，并标记每条建议的来源。
- F2 在两种视图间切换。
- 右箭头接受完整建议；`AcceptNextSuggestionWord`/`ForwardWord` 可逐词接受。
- 来源可配置为 `History`、`Plugin` 或 `HistoryAndPlugin`。

**架构**

- `ICommandPredictor` 接收 AST、tokens、光标位置和 cancellation token，而不只是原始字符串。
- 多个 predictor 可以同时注册。
- predictor 可收到 `SuggestionDisplayed`、`SuggestionAccepted`、`CommandLineAccepted` 和 `CommandLineExecuted` 反馈。
- PowerShell 将 predictor 并行调度，并在 **20ms** 后取消未完成任务，避免阻塞按键输入。

**隐私**

- PSReadLine 会过滤包含 password、token、apikey、secret 等敏感语义的历史记录。
- 较新实现使用 PowerShell AST 辅助判断，而不仅是字符串搜索。

**优点**

- Shell 对语法、AST、命令发现和当前编辑缓冲区拥有最准确的信息。
- 低延迟、本地优先、支持插件和反馈。
- inline/list 两种模式完整。

**局限**

- 体验依赖 PSReadLine 版本、Shell 配置和终端 VT 能力。
- Shell 自己绘制的列表对屏幕阅读器不够友好。
- 大型历史文件可能拖慢启动；插件配置也会增加 profile 启动时间。

### 3.2 bash / GNU Readline / bash-completion

bash 的核心是可编程补全：

- `complete` 注册 compspec。
- completion function 通过 `COMP_LINE`、`COMP_POINT`、`COMP_WORDS`、`COMP_CWORD` 获取上下文，并写入 `COMPREPLY`。
- 可以从文件、目录、静态词表、Shell function 或外部命令生成候选。
- `bash-completion` 按命令首次按 Tab 时懒加载脚本，避免把全部 completion 脚本放入启动路径。

**优点**：生态成熟、覆盖广、远程环境天然可用。  
**局限**：脚本格式松散，描述和结构化元数据不足，质量不统一，UI 主要是文本。

### 3.3 zsh

zsh 的 `compsys` 支持高度可配置的 completion context、tag、style、缓存和 menu selection。

常见的现代 zsh 体验还需要两个插件：

- `zsh-autosuggestions`：从历史、completion 或上一条命令上下文生成 ghost text。
- `zsh-syntax-highlighting`：在 ZLE redraw 时实时着色和校验。

其接受方式与 PowerShell/fish 类似：右箭头接受全部，forward-word 接受部分。

**主要风险**：插件通过包裹 ZLE widget 工作，加载顺序可能影响兼容性和性能。

### 3.4 fish

fish 是最早把现代命令行体验作为默认能力提供的 Shell 之一：

- 内置灰色 autosuggestion，来源包括历史、completion 和有效路径。
- 右箭头/Ctrl+F 接受全部，Alt+右箭头按词接受。
- 多候选时显示 pager，支持方向键、翻页和搜索。
- completion 可以从 man page、Makefile、`/etc/fstab` 和包管理器等真实数据派生。
- completion scripts 按命令懒加载。
- 内置实时语法着色和错误检测。

fish 的产品价值在于：用户安装后即可获得统一体验，不需要组合多个插件。

### 3.5 Nushell / Reedline

Nushell 是最接近 IDE 模型的原生 Shell：

- Reedline 统一负责 history、validation、completion、hint 和绘制。
- custom completer 可以返回 `value`、`description` 和 options。
- matching 可按参数配置为 prefix、substring 或 fuzzy。
- 提供 `ColumnarMenu`、`ListMenu`、`IdeMenu` 和 `DescriptionMenu`。
- `IdeMenu` 支持边框、描述区、自适应位置和滚动。

Nushell 证明了“结构化 Shell + 结构化 completion metadata”可以提供比传统 PTY 文本菜单更丰富的体验。

### 3.6 cmd.exe / Clink

cmd.exe 本身缺少现代 completion 和 prediction。Clink 通过 Readline 风格编辑、Lua completion 和历史能力补足 cmd.exe。

对 Intelligent Terminal 的意义是：不能假设所有 Shell 都能提供结构化候选；cmd.exe 需要独立兼容层或外部 completion engine。

## 4. Terminal 产品对比

### 4.1 产品矩阵

| 产品 | Completion 所有者 | Inline prediction | GUI popup | 结构化描述 | AI | 备注 |
|---|---|---:|---:|---:|---:|---|
| Warp | Terminal | 是 | 是 | 是 | 是 | 最完整的 terminal-native 方案 |
| VS Code Terminal | Terminal + Shell integration | 可共存 | 是 | 是 | 独立 Copilot/Agent | 传统 PTY 下最值得参考 |
| Fig / Amazon Q / Kiro | Overlay | 是 | 是 | 是 | 是 | completion spec 生态影响最大 |
| inshellisense | Shell/terminal overlay | 否/有限 | 是 | 是 | 否 | Microsoft，兼容 Fig specs |
| Windows Terminal | Shell | 由 Shell 提供 | 否 | 否 | 历史实验 | 主线不提供语义补全 |
| Intelligent Terminal | Shell | 由 Shell 提供 | 否 | 否 | Agent pane | 当前 AI 与 command completion 分离 |
| iTerm2 | Shell + history popup | 有限 | history popup | 有限 | Ask AI | 强 shell integration |
| Wave | Shell | 由 Shell 提供 | 否 | 否 | AI chat | AI 不等于 completion |
| WezTerm | Shell | 由 Shell 提供 | 否 | 否 | 否 | OSC 133 仅提供结构感知 |
| Ghostty | Shell | 由 Shell 提供 | 否 | 否 | 否 | 不提供 visual autocomplete |
| Kitty | Shell | 由 Shell 提供 | 否 | 否 | 否 | shell integration 非 completion |
| Tabby | Shell/Clink | 由 Shell 提供 | 否 | 否 | 否 | Windows cmd 依赖 Clink |
| Rio / Hyper / Alacritty / foot | Shell | 由 Shell 提供 | 否 | 否 | 否 | 终端保持轻量 |

### 4.2 Warp

Warp 是 terminal-owned IntelliSense 的上限参考：

- 自己解析命令并维护 completion signature/spec，不依赖 zsh compsys 或 bash-completion。
- Tab 打开 fuzzy completion menu，可配置为输入时自动打开。
- autosuggestion 来自历史和 completion；右箭头/Ctrl+F 接受，Ctrl+右箭头部分接受。
- completion spec 与 Fig 模型相近，覆盖大量 CLI，但不同命令支持深度不一致。
- shell hook/“Warpify” 为 block、completion、syntax highlighting 和远程环境提供上下文。
- AI Agent、Next Command 和错误修复是独立层，可分别关闭。

**优点**：输入、解析、UI 和 AI 在一个产品内，体验一致。  
**代价**：需要重建 Shell 编辑模型、completion 生态和 SSH/subshell 注入，维护成本最高。

### 4.3 VS Code Integrated Terminal

VS Code 的 Terminal Suggest 是 Intelligent Terminal 最现实的直接参照：

- 自动触发、`-`/`/` trigger character、Ctrl+Space 手动触发三种模式。
- popup 可提供命令、路径、参数、npm scripts、git branches 和环境变量。
- Tab 插入；Enter 通常只在用户已导航候选后提交，避免误执行。
- `runOnEnter` 将“插入”和“执行”分成独立设置。
- 可配置 Up Arrow 是导航 popup 还是保留 Shell history search。
- 依赖 shell integration，并显示 None/Basic/Rich 等质量层级。
- 对 `$PATH` globals 做积极缓存，并提供显式清缓存命令。

**启示**：传统终端不必完全接管 Shell，也可以通过 shell integration、provider API、缓存和原生 popup 获得接近 IDE 的体验。

### 4.4 Fig / Amazon Q / Kiro

Fig 建立了影响最大的跨 CLI completion spec 模型：

- TypeScript spec 描述 subcommand、option、arg、description 和 icon。
- generator 可以运行命令，动态返回 git branch、云资源等候选。
- 旧 Fig 使用终端无关 overlay；其 spec 生态被 Warp 和 inshellisense 等项目复用。
- 后续 Amazon Q/Kiro 叠加 AI dropdown 和 inline ghost text。

**优点**：一个 spec 可跨 Shell/终端复用，社区贡献门槛低于分别维护 bash/zsh/fish 脚本。  
**风险**：动态 generator 涉及进程启动、权限、网络、缓存和供应链安全。

### 4.5 Microsoft inshellisense

inshellisense 是 Microsoft 已有的关键参考：

- 跨 Windows/Linux/macOS 和多种 Shell 工作。
- 运行时消费 Fig/withfig completion specs，覆盖数百个 CLI。
- Tab 接受，方向键切换，Esc 关闭。
- 证明 Microsoft 生态中已经存在复用 Fig spec 的可行实现。

在为 Intelligent Terminal 设计 completion engine 前，应优先评估复用其 parser、spec loader、许可证和生态，而不是创建新的不兼容格式。

### 4.6 iTerm2

iTerm2 通过 shell integration 获得 marks、command history 和 directory tracking：

- Auto Command Completion 主要基于历史，不是完整语义 completion engine。
- Composer 提供多行命令编辑窗口。
- Ask AI 使用显式配置的 API key/endpoint 生成或解释命令。

其模式是“Shell-owned completion + Terminal-owned history/AI 辅助”，集成成本低于 Warp。

### 4.7 Linux 常见终端

WezTerm、Ghostty、Kitty、GNOME Terminal/Ptyxis、Konsole、Alacritty 和 foot 普遍不实现命令 IntelliSense。它们把 completion 视为 Shell 职责，最多支持：

- OSC 133 prompt/output marks；
- cwd/title tracking；
- jump-to-prompt；
- 选择或复制单条命令输出；
- 为终端自身 CLI 生成 completion script。

因此，不能把用户在 zsh/fish 中看到的 autosuggestion 归因于终端产品本身。

## 5. Shell-owned 与 Terminal-owned 的取舍

| 维度 | Shell-owned | Terminal-owned |
|---|---|---|
| 语法准确性 | Shell 拥有 parser/AST，通常更准确 | 需要重新解析或依赖 spec |
| UI 丰富度 | 受 VT 文本绘制限制 | 可使用原生 popup、图标、文档 |
| 无障碍 | redraw 菜单难被屏幕阅读器理解 | 可使用标准 WinUI/combobox 语义 |
| 跨终端一致性 | 同一 Shell 在不同终端中一致 | 绑定到产品或 overlay |
| 跨 Shell 一致性 | 每个 Shell 体验不同 | 可统一 PowerShell/bash/zsh/cmd |
| SSH/容器 | 天然工作 | 需要远程注入、proxy 或降级 |
| 生态 | completion scripts 分散 | 中央 spec 易复用但需维护 |
| 延迟 | 本地、接近编辑循环 | 动态 provider 需要严格预算 |
| 安全边界 | Shell 插件权限较高 | Terminal generator 同样需要隔离 |

最合理的产品方向通常不是纯二选一，而是 **Terminal-owned UI + Shell/provider-owned semantics**：

- Terminal 负责 popup、inline rendering、键盘交互、无障碍、缓存和统一排序。
- Shell 或 completion provider 负责语法、当前 token、动态候选和 descriptions。
- 没有 rich integration 时，降级到 history、filesystem、`$PATH` 和 Fig specs。

## 6. 用户体验设计

### 6.1 Inline 与 Popup 应同时存在

**Inline ghost text** 适合：

- 只有一个高置信度候选；
- 历史预测或完整命令预测；
- 用户无需离开输入流；
- 继续输入即可自然忽略。

**Popup list** 适合：

- 多个子命令、参数或动态资源；
- 需要描述、类型、来源和文档；
- 用户正在探索陌生 CLI；
- 需要 fuzzy filtering。

推荐行为：

- 右箭头：接受 inline suggestion；
- Ctrl/Alt+右箭头：接受下一个词；
- Tab/Ctrl+Space：打开或提交 completion menu；
- Enter：默认只在用户明确导航后提交候选；
- “插入并运行”必须是独立 opt-in；
- Esc：关闭 popup/inline；
- F2 或设置项：切换 inline/list 风格。

### 6.2 不要让多个建议系统互相争抢

PowerShell、fish、zsh 插件和终端都可能绘制 ghost text。终端应检测或配置：

- Shell 已提供 inline prediction 时，Terminal inline 默认关闭或协商；
- popup 打开时暂停 AI inline suggestion；
- Shell Tab completion 与 Terminal popup 不应同时消费同一个按键；
- 明确区分 menu mode 与 palette mode。

仓库已有 `Suggestions UI` spec 的区分值得沿用：

- **Menu mode**：无独立输入框，焦点仍在 Shell，适合 Shell completion。
- **Palette mode**：有过滤框，适合 history、directory、snippet 和 intent search。

### 6.3 Ranking

推荐采用分层排序，而不是把所有来源混成一个黑盒分数：

1. 当前语法位置的确定性候选；
2. exact prefix；
3. 当前目录/项目相关候选；
4. 最近使用和频率；
5. fuzzy match；
6. AI prediction。

每个 provider 负责自己的原始顺序，Terminal 负责跨 provider 的一致规则。应展示来源，例如 Shell、History、Git、AI。

质量指标不应只有“接受率”，还应包括：

- 接受后是否真正执行；
- 执行前是否立即修改；
- 执行后是否成功；
- 是否立即 Ctrl+C 或撤销；
- 建议中被保留的字符数。

### 6.4 Documentation 与 Signature Help

采用类似 LSP 的两阶段模型：

- 第一阶段快速返回 `label`、`insertText`、`kind`、`sortText`。
- 用户高亮候选后，再异步 resolve `description`、usage、example 和 documentation。

参数帮助应显示：

- 当前 subcommand；
- 参数名称和简短描述；
- 是否必选；
- value 类型；
- 枚举值或动态来源；
- 示例；
- 是否存在副作用或危险性。

### 6.5 无障碍

Shell 自己通过 VT 重绘列表时，屏幕阅读器难以理解“当前选中了第几项”。Terminal-owned popup 应：

- 使用标准 WinUI list/combobox 控件和稳定的 accessibility tree；
- 通知“共 N 项，当前第 M 项”；
- 朗读名称、描述、来源和是否将执行；
- 支持高对比度、放大和自定义 inline prediction 颜色；
- 不仅依赖颜色区分 History、Shell 和 AI；
- 提供完全键盘操作和可发现的帮助。

## 7. 性能

### 7.1 建议的延迟预算

| 操作 | 目标 | 超时/降级策略 |
|---|---:|---|
| 按键回显 | <16ms 理想，绝不能等待 provider | 永不阻塞 |
| 本地 inline prediction | 20ms 目标 | 参考 PSReadLine，超时即跳过 |
| 首批 popup 候选 | <50ms 理想，<100ms 可接受 | 先显示缓存/静态候选 |
| 动态本地候选 | <200ms | 显示 loading，取消旧请求 |
| 文档 resolve | <300ms | 仅对高亮项加载 |
| 网络/AI | 不进入按键关键路径 | 显式触发或延迟显示 |

公开资料中，PSReadLine 的 20ms predictor deadline 是最明确的 Shell 级指标。其他 Shell 很少发布 p50/p95/p99 completion latency，这是当前行业数据缺口。

### 7.2 必须采用的性能策略

- **并行 provider + deadline**：快速 provider 不等待慢 provider。
- **trailing-edge debounce**：用户持续输入时取消旧查询，只处理最新状态。
- **generation/request ID**：旧结果不得覆盖新输入。
- **本地过滤**：完整候选列表可缓存时，后续按键只在客户端过滤。
- **懒加载 provider**：参考 bash-completion 和 fish，首次需要时加载。
- **缓存 globals**：缓存 `$PATH` executable、alias 和 shell builtins，并提供显式清缓存。
- **按 session/profile/WSL distro 分区缓存**：避免不同环境串数据。
- **lazy resolve descriptions**：避免打开 popup 时读取 man page 或启动进程。
- **限制动态 generator**：设置 CPU、时间、输出数量和并发上限。
- **预热但不阻塞启动**：Shell ready 后低优先级建立常用索引。

### 7.3 应监控的指标

- keystroke-to-first-suggestion p50/p95/p99；
- provider latency、timeout 和 cancellation rate；
- popup shown rate；
- suggestion accept rate；
- accept-to-run rate；
- accepted-and-retained characters；
- suggestion execution success rate；
- cache hit rate；
- startup overhead；
- memory/CPU/power；
- 因 popup 导致的误提交、Esc 和关闭率。

## 8. 隐私、安全与可靠性

Command history 经常包含：

- token、password、connection string；
- 私有路径、仓库名和主机名；
- 云订阅、resource ID 和内部 endpoint；
- 带副作用或破坏性的命令。

因此：

- history corpus 必须在进入 ranking/prediction 前进行敏感信息过滤；
- 默认本地处理；上传历史或终端上下文必须显式 opt-in；
- telemetry 记录模式、延迟和结果状态，不记录完整命令文本；
- AI 建议必须标记来源，并默认只插入、不执行；
- destructive command 需要额外确认；
- completion generator 应有权限、超时、取消、输出和网络策略；
- 社区 spec/generator 需要签名、审查或信任分级；
- provider 失败时静默降级到已有 Shell 行为，不得破坏输入。

## 9. 编辑器 IntelliSense 对 Shell 的启示

Visual Studio、VS Code、JetBrains 和 Copilot 提供了若干成熟模式：

### 9.1 Trigger 分级

- 自动快速建议；
- trigger character 建议；
- Ctrl+Space 手动建议；
- 连续再次触发扩大搜索范围。

Shell 可以对应为：

- 自动：高置信 inline history；
- `-`/`/`：参数候选；
- Tab/Ctrl+Space：完整 menu；
- 再次触发：扩大到全局命令、远程资源或 AI。

### 9.2 Client 负责一致 UI，Provider 负责语义

LSP 把过滤和排序主要留给 client，server 通过 `sortText`、`filterText`、`preselect` 和 `isIncomplete` 提供提示。Shell IntelliSense 可采用相同边界：

- Terminal 是 client；
- Shell、Fig spec、history、git、cloud CLI 和 AI 是 providers；
- provider 返回结构化 item；
- UI、过滤、取消和 accessibility 由 Terminal 统一。

### 9.3 Ghost text 与 popup 互斥协调

编辑器通常在 completion popup 打开时暂停 Copilot ghost text，避免两个高亮候选竞争。Terminal 也应执行同样规则。

### 9.4 接受后保留率比接受率更重要

Copilot 的公开经验表明，单纯优化 acceptance rate 会鼓励短而泛化的建议。Shell 中更应关注用户是否保留、执行并成功，而不是是否按了 Tab。

## 10. 对 Intelligent Terminal 的建议

### 10.1 推荐架构：Terminal-owned UI，Provider-owned semantics

```text
Shell input / shell integration
          |
          v
Command-line context broker
  - shell/profile/cwd
  - buffer/cursor/tokens
  - OSC 133 state
  - integration quality
          |
          v
Completion coordinator
  |-- Native shell provider
  |-- Fig/spec provider
  |-- History provider
  |-- Filesystem/$PATH provider
  |-- Git/project provider
  |-- AI provider (slow lane)
          |
          v
Ranking + filtering + cache + cancellation
          |
          v
SuggestionsControl
  - inline mode
  - menu mode
  - palette mode
  - documentation resolve
  - accessibility
```

### 10.2 为什么不建议把 ACP Agent 放入每次按键路径

Intelligent Terminal 当前 ACP agent 适合会话式任务、解释、修复和多步骤执行，不适合基础 completion：

- 网络/模型延迟远高于 20–100ms；
- token 成本和资源消耗过高；
- 结果不确定，可能 hallucinate；
- 每次按键发送上下文存在隐私风险；
- agent 生命周期和 completion 生命周期不同。

AI 可以作为低优先级 slow lane：

- 用户停顿后给出 Next Command；
- 用户显式按快捷键请求自然语言生成；
- 命令失败后生成 Quick Fix；
- 对选中 completion 提供解释；
- 不影响本地 completion 的即时显示。

### 10.3 复用仓库已有能力

仓库中已经存在可直接利用的基础：

- `doc/specs/#1595 - Suggestions UI/`：menu/palette、source、description 和 cursor-relative UI 设计。
- `SuggestionsControl`：现有建议 UI。
- `src/cascadia/fzf/fzf.cpp`：Unicode-aware fuzzy matcher。
- `ThrottledFunc`：debounce/throttle 基础。
- OSC 133 shell integration：prompt、command、output 和 exit code 边界。
- Quick Fix / autofix：失败后建议、异步预计算和确认执行。
- WinUI/UIA：比 Shell VT redraw 更好的无障碍能力。
- inshellisense / Fig specs：已有 Microsoft 和社区生态可评估。

### 10.4 建议的分阶段路线

#### Phase 0：协议与指标

- 定义 `CompletionItem`、provider、request、cancellation 和 resolve 接口。
- 定义 integration quality：None / Basic / Rich。
- 建立 latency、timeout、accept、run 和 success 指标。
- 明确 telemetry 不采集命令文本。

#### Phase 1：低风险本地 MVP

- Terminal-owned popup；
- `$PATH` command、filesystem、history 和 snippet provider；
- Ctrl+Space/Tab 手动触发；
- fuzzy filter、description、来源；
- 插入但不执行；
- 缓存和显式清缓存；
- PowerShell、cmd、WSL bash/zsh/fish 的降级测试。

#### Phase 2：Shell semantic integration

- PowerShell structured completion；
- bash/zsh/fish completion bridge；
- 当前 token、cursor 和 quoting 语义；
- git branch、npm scripts 等动态 provider；
- Rich integration 下自动 trigger；
- native Shell popup 与 Terminal popup 的冲突检测。

#### Phase 3：Inline prediction

- local history prediction；
- full/word acceptance；
- inline/list toggle；
- sensitive-history filtering；
- provider feedback loop；
- 20ms fast-lane deadline。

#### Phase 4：AI slow lane

- 停顿后 opt-in Next Command；
- 自然语言生成；
- completion explanation；
- failure-driven Quick Fix；
- destructive-command confirmation；
- BYOK/local model 和企业策略。

## 11. 推荐产品原则

1. **输入永不等待建议。**
2. **本地、确定性建议先于网络和 AI。**
3. **默认插入，不默认执行。**
4. **Terminal 统一 UI，Shell/provider 保留语义所有权。**
5. **保留 Shell 原有快捷键和用户肌肉记忆。**
6. **inline、menu、palette 是三种不同场景，不应强行统一。**
7. **provider 必须可取消、有截止时间、可降级。**
8. **历史在成为 suggestion corpus 前必须过滤敏感信息。**
9. **无障碍是 Terminal-owned UI 的核心价值，不是附加项。**
10. **复用现有 spec 生态和仓库组件，避免重复建设。**

## 12. 主要反模式

- 每次按键同步调用网络、AI 或外部进程；
- 不取消旧请求，导致 stale suggestion 覆盖新输入；
- Terminal 与 Shell 同时绘制 ghost text；
- Tab/Enter 在不同模式下行为不可预测；
- 接受候选即自动执行；
- 将未经清洗的完整 command history 上传或用于预测；
- 只追求 acceptance rate；
- completion provider 失败后影响 Shell 正常输入；
- 为每个 Shell 重复发明一套不兼容 spec；
- 用自绘 VT 菜单替代可访问的原生控件。

## 13. 参考资料

### PowerShell 与 Shell

- [PowerShell: Using predictors](https://learn.microsoft.com/powershell/scripting/learn/shell/using-predictors)
- [PowerShell: Create a command-line predictor](https://learn.microsoft.com/powershell/scripting/dev-cross-plat/create-cmdline-predictor)
- [PSReadLine documentation](https://learn.microsoft.com/powershell/module/psreadline/about/about_psreadline)
- [PowerShell CompletionPredictor](https://github.com/PowerShell/CompletionPredictor)
- [GNU Bash programmable completion](https://www.gnu.org/software/bash/manual/html_node/Programmable-Completion.html)
- [bash-completion](https://github.com/scop/bash-completion)
- [zsh completion system](https://zsh.sourceforge.io/Doc/Release/Completion-System.html)
- [zsh-autosuggestions](https://github.com/zsh-users/zsh-autosuggestions)
- [zsh-syntax-highlighting](https://github.com/zsh-users/zsh-syntax-highlighting)
- [fish interactive use](https://fishshell.com/docs/current/interactive.html)
- [fish completions](https://fishshell.com/docs/current/completions.html)
- [Nushell custom completions](https://www.nushell.sh/book/custom_completions.html)
- [Nushell line editor](https://www.nushell.sh/book/line_editor.html)
- [Clink](https://chrisant996.github.io/clink/)
- [Atuin](https://docs.atuin.sh/)
- [Carapace](https://carapace.sh/)

### Terminal 产品

- [Warp completions](https://docs.warp.dev/terminal/command-completions/completions)
- [Warp autosuggestions](https://docs.warp.dev/terminal/command-completions/autosuggestions)
- [VS Code terminal shell integration and IntelliSense](https://code.visualstudio.com/docs/terminal/shell-integration)
- [Fig autocomplete specs](https://github.com/withfig/autocomplete)
- [Kiro CLI autocomplete](https://kiro.dev/docs/cli/autocomplete/)
- [Microsoft inshellisense](https://github.com/microsoft/inshellisense)
- [iTerm2 shell integration](https://iterm2.com/shell_integration.html)
- [WezTerm shell integration](https://wezterm.org/shell-integration.html)
- [Ghostty shell integration](https://ghostty.org/docs/features/shell-integration)
- [Kitty shell integration](https://sw.kovidgoyal.net/kitty/shell-integration/)
- [Tabby](https://github.com/Eugeny/tabby)
- [Wave Terminal documentation](https://docs.waveterm.dev/)
- [Windows Terminal documentation](https://learn.microsoft.com/windows/terminal/)

### 编辑器与协议

- [Visual Studio IntelliSense](https://learn.microsoft.com/visualstudio/ide/using-intellisense)
- [VS Code IntelliSense](https://code.visualstudio.com/docs/editing/intellisense)
- [VS Code AI-powered suggestions](https://code.visualstudio.com/docs/editing/ai-powered-suggestions)
- [JetBrains code completion](https://www.jetbrains.com/help/idea/auto-completing-code.html)
- [Language Server Protocol completion](https://github.com/microsoft/language-server-protocol/blob/gh-pages/_specifications/lsp/3.18/language/completion.md)
- [WAI-ARIA Combobox Pattern](https://www.w3.org/WAI/ARIA/apg/patterns/combobox/)
- [GitHub Copilot: The road to better completions](https://github.blog/ai-and-ml/github-copilot/the-road-to-better-completions-building-a-faster-smarter-github-copilot-with-a-new-custom-model/)
- [Nielsen Norman Group response-time limits](https://www.nngroup.com/articles/response-times-3-important-limits/)

### 本仓库相关设计

- [`doc/specs/#1595 - Suggestions UI/Suggestions-UI.md`](specs/%231595%20-%20Suggestions%20UI/Suggestions-UI.md)
- [`doc/specs/#1595 - Suggestions UI/Snippets.md`](specs/%231595%20-%20Suggestions%20UI/Snippets.md)
- [`doc/specs/#16599 - Quick Fix/#16599 - Quick Fix.md`](specs/%2316599%20-%20Quick%20Fix/%2316599%20-%20Quick%20Fix.md)
- [`doc/specs/#11000 - Marks/Shell-Integration-Marks.md`](specs/%2311000%20-%20Marks/Shell-Integration-Marks.md)

## 14. 调研限制

- 除 PSReadLine 的 20ms deadline 外，大多数 Shell/终端没有发布可比较的 completion p50/p95/p99 数据。
- Warp 的公开终端渲染 benchmark 使用了较早版本，不能直接代表 2026 年各产品性能。
- 部分产品频繁重命名或迁移，例如 Fig → Amazon Q Developer CLI → Kiro CLI，实际实现和文档会继续变化。
- “没有 Terminal IntelliSense”不代表用户没有 IntelliSense；很多 Linux 用户通过 fish、zsh 插件、Atuin、Carapace 或其他 Shell 工具获得相关体验。
- 本报告是市场和架构调研，不是最终功能规格；协议、Shell bridge、远程场景和安全模型仍需单独设计与原型验证。

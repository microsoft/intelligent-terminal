# Telemetry gap 改进计划

状态：第 1 至 11 项的客户端改动和查询参考已落实到源码，已完成构建与单元回归并部署 Dev；
实际后台查询迁移不在本地完成。第 4 项经侵入性 review 后收缩：
撤回专用命令派发/结果通道，保留原执行路径及旁路记录；执行结果保持未知，不报告修复成功。

本计划对照 [gap.txt](gap.txt) 和逐项讨论确认的报表需求，记录已确认的改动。
[当前 telemetry 参考](intelligent-terminal-telemetry.md) 描述现有实现，不应提前写入本计划中尚未实现的能力。

## 1. 活跃设备与 D7 / D28 留存

对应需求：1.1、1.5，以及其他报表使用的活跃设备分母。

### 问题

`Win32Host.SessionBecameInteractive` 只记录 Terminal 进程的首次键盘交互。
同一进程跨日继续使用不会再次发送，因此无法完整观察长驻窗口、Keep running
恢复窗口等场景下的后续活跃日期。

进程存活不代表用户活跃。需要补齐的是后续日期的真实交互，而不是后台心跳。
这里的进程也不是单个窗口、标签页或 ACP 对话 session。

### 已确认的改动

新增 `Microsoft.Windows.Terminal.Win32Host.UserInteract`，简称 `Win32Host.UserInteract`。

| 项目 | 约定 |
| --- | --- |
| 含义 | 该进程在当前 UTC 日期发生了符合条件的用户键盘交互 |
| 交互边界 | 首版沿用现有 `SessionBecameInteractive` 的键盘消息判断，包括普通键和系统键的按下、抬起 |
| 发送频率 | 每个进程每个 UTC 日期首次符合条件的交互发送一次；同日后续交互不重复发送 |
| 跨日行为 | 日期变化本身不发送；跨日后下一次符合条件的交互才发送 |
| 非活跃行为 | 后台运行、终端输出、agent 自主执行任务均不单独触发 |
| 多窗口 | 同一进程的窗口共享该进程的发送状态，不按窗口重复计数 |
| 多进程及重启 | 同设备同日可以有多条事件；后台按设备和 UTC 日期去重 |
| 旧事件 | 保留 `SessionBecameInteractive` 的名称、字段和进程内首次交互语义，不替换或扩大其含义 |

不记录按键值、命令、prompt、聊天内容、路径或窗口标题。
沿用现有事件的 `Branding`、`Distribution` 及使用数据隐私标记；
日期使用事件发生时间，设备使用后台已有设备维度，不新增客户端设备标识或日期业务字段。

不增加定时心跳、跨进程共享状态或持久化去重文件。
鼠标等其他交互不在首版范围内，报表应明确这是键盘交互活跃口径。

### 实现落点

- `src\cascadia\WindowsTerminal\WindowEmperor.cpp`：
  在现有主消息循环的键盘交互判断处新增 `UserInteract` 上报。
  使用独立于 `loggedInteraction` 的进程内 UTC 日期状态，避免改变旧事件行为。
- 为“首次交互、同日重复、跨日交互”的发送判断添加适合现有测试组织的覆盖。
  如果需要提取判断逻辑，应保持简单并允许用确定的日期输入测试，不为此增加计时线程。
- 实现完成后更新 `doc\intelligent-terminal-telemetry.md` 中的共享 `Active` 定义、
  1.1 / 1.5 查询说明及其他依赖活跃分母的口径；同步相关继承事件参考和 `doc\gap.txt`。

### 报表口径

1. 选择一致的版本、分发渠道、设备资格和采样口径，只纳入支持新事件的版本。
2. 按后台设备标识和事件发生时间的 UTC 日期去重，得到每日交互设备集合。
3. 报告窗口内的活跃设备是这些每日集合的并集，不是事件条数之和。
4. D0 使用明确历史观察范围内的首次观察交互日，不声称它一定是首次安装或首次使用。
5. D7 / D28 使用精确日期回访：
   `D0 cohort 中在 D0 + 7 / 28 天发生交互的设备数 / 该 cohort 设备数`。
   不是“前 7 / 28 天内任意一天使用过”。
6. 只计算已经到达相应随访日期的 cohort。需要时，查询范围延伸到
   28 天 cohort 选择窗口之外；尚未到期不能算流失。

后台负责同设备同日去重，不通过对照旧事件来推断每天是否活跃。
原报表使用的 scaled device counts 仍需遵守数据团队的采样和加权方法，
不能直接当作原始设备集合相除。

### 历史兼容

- 旧数据中漏掉的交互日期无法通过本次改动回补。
- 新口径从支持 `UserInteract` 的版本上线后积累，D28 需要足够的随访时间。
- 新旧留存和活跃指标必须注明边界，不直接拼接成无口径变化的趋势。
- 没有新事件的旧版本设备不能直接判为不活跃或流失。
- 各漏斗切换活跃分母时保持一致，不能混用新分子与旧活跃分母。

### 验收条件

| 场景 | 预期 |
| --- | --- |
| 启动后没有键盘交互 | 不发送 `UserInteract` |
| 进程第一次符合条件的键盘交互 | 新旧事件各发送一次 |
| 同日持续输入，包括按下和抬起 | 不重复发送 `UserInteract` |
| 同日操作同进程的其他窗口 | 不重复发送 `UserInteract` |
| UTC 日期变化，但没有用户操作 | 不发送 `UserInteract` |
| 跨日后首次键盘交互 | 再发送一次 `UserInteract`，旧事件不重发 |
| Keep running 后同进程在另一天恢复窗口并操作 | 发送当天的 `UserInteract` |
| 同日启动第二个进程并操作 | 可产生第二条事件，后台每日设备数仍计为一个 |
| 第一天和第八天在同一长驻进程中操作 | 以第一天为 D0，该设备计入 D7 回访 |
| 仅后台输出或 agent 自主运行 | 不因这些活动发送事件 |
| 检查事件载荷 | 不包含交互内容或新增设备标识 |

## 2. 用户提问的完成率与同一会话的第二次提问

对应需求：2.3、2.4；两项使用同一套事件和关联字段，不另建第二次提问事件。

### 问题

一轮 turn 指一次提问被发往 agent，到本轮请求结束；一个 ACP session 可以包含多轮。
现有 `WTA.AgentPromptSent` 和 `WTA.AgentResponseComplete` 没有共同的 turn 标识，
无法精确关联某批已发送提问与其完成结果。完成事件也没有 `IsAutofix`，
不能把用户聊天与自动修复分析分开计算。

现有 `UserPromptOrdinal=First / Second / Later` 能描述观察到的用户提问顺序，
但没有 session 级关联标识，无法将某次第二轮提问与对应的首轮组成精确 cohort。
跨窗口统计及新 helper 加载旧会话后重新计数，也会影响直接使用 `Second / First` 的结果。

RPC 正常完成只表示本轮响应完成，不证明 agent 解决了用户的问题。

### 已确认的改动

| 事件 | 关联与分类字段 | 约定 |
| --- | --- | --- |
| `WTA.AgentPromptSent` | 新增 `SessionId`、`TurnId`，保留 `IsAutofix` | 同一对话的多轮共享遥测会话标识，每轮使用不同的 turn 标识；保留 `UserPromptOrdinal` |
| `WTA.AgentResponseComplete` | 新增 `SessionId`、`TurnId`、`IsAutofix` | 使用发送侧同一轮的会话标识、turn 标识和分类，而不是完成时重新读取当前 UI 状态 |

保留现有 `Success`、耗时等字段及原有含义。
`SessionId` 是遥测用的不透明关联标识，不代表批准直接导出 agent 提供的原始 ACP session ID。
两个标识均不包含 prompt、session 内容或设备信息，并应避免跨 helper、跨进程的碰撞。
`SessionId` 连接同一对话中的多轮；`TurnId` 连接一轮的发送与完成。

### 已确认的会话边界

新 helper 加载旧会话视为新的观察会话：生成随机遥测 `SessionId`，
从 `First` 重新计数，不持久化原始 ACP ID 的稳定映射。
同一存活 helper 的 Keep running 恢复保留其观察 ID 和序号。
同一 helper 中已有的观察仅在对应 session 被忘记或替换时清除；
单纯加载仍在观察中的同一个 session 不重置身份和计数。

### 报表口径

- 以 `AgentPromptSent` 中的不同 `TurnId` 建立发送 cohort，按 `IsAutofix` 分开统计。
- 按同一个 `TurnId` 关联完成事件，去重后计算：
  `有匹配 Success=true 完成事件的已发送 turn 数 / 已发送 turn 数`。
- 保留已观察到的失败完成及尚未观察到完成的数量，不能用不相关的完成事件补齐。
- 为 cohort 规定一致的随访区间，允许完成事件落在发送统计窗口之外。
  尚无完成事件只表示未观察到结束，不能仅凭这一点认定为失败或崩溃。
- 现有 `Success` 不足以单独区分取消、错误等原因；细分结果字段尚未在本项中确定。
- 仅在支持新字段的版本上建立精确 turn 漏斗，旧事件不能回填关联标识。
- 第二次提问转化以观察到首轮用户提问的不同 `SessionId` 建立 cohort，
  在规定随访区间内查找同一会话的第二轮用户提问；两侧都排除 autofix。
  只有第二轮落在窗口内而首轮不属于 cohort 的会话，不能直接加入分子。
- 统一字段也支持组合分析：首轮 `TurnId` 正常完成的会话中，有多少随后发送第二轮。
  是否要求第二轮发生在首轮完成之后，应在该组合报表中明确，不能仅靠两事件都存在推断顺序。

### 实现落点与验收

在 `tools\wta\src\protocol\acp\client.rs` 的实际发送边界，
以及 `tools\wta\src\protocol\acp\turn_metrics.rs` 的请求计时生命周期中，
传递并保留同一轮的遥测 `SessionId`、`TurnId` 和 `IsAutofix`；
在 `tools\wta\src\telemetry.rs` 扩展两个事件的字段。
实现完成后同步 telemetry 参考及 `doc\gap.txt`。

验收应覆盖：单轮发送与完成使用相同的会话和 turn 标识；
同一 session 的多轮共享会话标识但 turn 标识不同；
不同 session 并发时不会串联；用户提问与 autofix 分类一致；
失败完成仍关联原请求；未发送请求不制造发送记录；
重复完成不会增加同一 turn 的完成数；第二轮正确归属首轮 cohort；
autofix 不充当第二轮用户提问；事件不泄露内容。
验收包含新观察 ID 与计数同时重置、存活观察的多轮共享 ID。

## 3. 错误检测到修复建议、接受的转化

对应需求：3.2、3.3，关联 3.1 的错误检测。

### 问题

已有 `WTA.ErrorFixOffered` 在具体修复建议卡片真正展示给用户时上报，
并通过 `OfferId` 与 `WTA.ErrorFixAccepted` 关联。
这部分无需再增加一个卡片展示事件。
当前缺口是无法回连触发建议的错误检测，且手动请求与自动检测触发的建议没有来源区分。

错误提示、开始分析或后台生成建议，不等于用户已经看到建议。
接受事件只表示用户确认 Run 且执行请求成功入队，不证明执行或修复成功。

### 已确认的改动

统一使用 `OfferId`，不新增 `ErrorId` 或 `DetectionId`。
将其语义从单张建议卡片的标识扩展为“一次候选修复流程的不透明关联标识”。
ID 的存在不代表该流程最终生成、展示或执行了建议。

| 事件或入口 | `OfferId` 生命周期 |
| --- | --- |
| `WTA.ErrorDetected` | 检测时生成并记录 ID；即使最终没有建议，也保留检测记录 |
| 由该检测触发的分析 | 传递同一个 ID，不在生成卡片时另建 ID |
| `WTA.ErrorFixOffered` | 卡片实际可见并完成展示时携带同一个 ID |
| `WTA.ErrorFixAccepted` | 用户确认 Run 且请求成功入队时携带同一个 ID |
| 独立的手动 `/fix` 请求 | 请求开始时生成新 ID，注明手动来源，不伪造 `ErrorDetected` |

记录流程来源，区分自动检测触发与独立手动请求；具体字段和枚举在实现设计时确定。
来源随流程传递，不能根据完成时的当前设置反推。
同一流程内重绘、隐藏后重新展示或重新生成建议，不改变该流程的 ID；
统计的是流程是否展示过、是否接受过，不是每张卡片的接受率。
互不相关的错误或新的独立修复流程不能复用同一个 ID。
标识及来源均不得包含错误文本、命令、路径或建议内容。

### 报表口径

- 按 `OfferId` 去重，建立检测 cohort，关联同 ID 的展示和接受记录。
- 检测到建议的转化：
  `检测 cohort 中出现过 ErrorFixOffered 的 ID 数 / 检测 cohort 的 ID 数`。
- 建议到接受的转化：
  `展示 cohort 中出现过 ErrorFixAccepted 的 ID 数 / 展示 cohort 的 ID 数`。
- 独立手动请求不纳入自动检测转化率分母，其展示与接受可以按来源单独分析。
- 全部检测与符合自动修复条件的检测应区分，不能把连接关闭等不会触发分析的信号
  一概视为分析失败。资格判据和必要字段仍需在实现前细化。
- 为展示和接受保留明确的随访区间；没观察到后续事件不能直接解释为用户拒绝。
- 同一流程内重复展示或接受不增加流程转化次数。

### 实现落点与验收

在 `tools\wta\src\app_events.rs` 的检测入口，以及
`tools\wta\src\app\autofix.rs` 的分析、建议和手动请求生命周期中传递流程标识与来源。
在 `tools\wta\src\telemetry.rs` 扩展检测及相关事件，保留真实展示和接受的现有触发边界。
实现完成后同步 telemetry 参考及 `doc\gap.txt`。

验收应覆盖：检测、展示、接受共享 ID；无建议时仍有检测记录；
隐藏卡片不计展示；重绘和重新打开不重复计同一流程；
手动请求有独立来源且不伪造检测；不同错误、并发流程及过期分析结果不串联；
接受不等于修复成功；载荷不包含内容。

旧 `OfferId` 是卡片级标识，新口径是流程级标识，查询需按支持新语义的版本区分。
历史检测事件没有该标识，不能回补关联或将新旧语义直接拼接。

## 4. 用户点击 Run 后的执行成功指标

对应需求：3.x。

### 已确认的指标调整

不承诺通用的“修复成功率”，不恢复旧 `ErrorFixResolved` 的推断逻辑。
采用“用户在修复建议卡片上点击 Run 后，该次执行成功”作为建议有效性的参考信号，
而不是原问题已解决的证明。
这替代了此前要求重新执行原失败命令或执行额外验证步骤来判定修复成功的方案。

| 信号 | 解释 |
| --- | --- |
| `ErrorFixAccepted` | 用户确认 Run，执行请求成功入队；不是执行成功 |
| Run 执行成功 | 观察到与本次 Run 明确关联的命令执行完成，且退出码为 0 |
| Run 执行失败 | 观察到与本次 Run 明确关联的命令执行完成，且退出码非 0 |
| 没有可关联的完成结果 | 未知或尚未完成，不能推断成功，也不能直接归类为执行失败 |

指标名称使用“Run 执行成功”或“建议命令执行成功”，不使用“错误已解决”。
即使命令退出码为 0，也不承诺根因消除、用户目标达成或没有副作用。
Insert、请求入队、发送按键成功、出现新提示符、UI 状态清除和无关命令的成功，
均不能充当本次 Run 的执行成功。

### 关联及统计原则

- 执行结果沿用修复流程的 `OfferId`，与检测、展示和接受关联。
- 必须在 Run 派发时保留执行归属，不能事后把同一 pane 的任意成功命令归给该流程。
- 以流程为单位，统计已接受流程中观察到至少一次关联 Run 成功的比例；
  同时保留未观察到成功的流程及其失败、未知状态，不将其一概解释为修复失败。
- 如展示“已知执行结果中的成功率”，须与前述流程转化指标分开，
  明确分母和结果可观测覆盖率，不能隐藏未知结果。
- 不为收集 telemetry 自动重跑原命令或增加验证命令；只观察用户正常授权的执行。
- 旧 44% 的来源及分母仍未确认，不与本指标直接延续或比较。

### 实施阶段确认的边界

`tools\wta\src\app_turn.rs` 当前的接受边界仅证明执行请求成功入队。
只记录 `unobservable` / `dispatchFailed`，不猜测命令退出码。
后续曾实现 PowerShell 专用通道，但 review 发现它改变了 Run 的执行依赖、
绕过自定义 Enter handler，并可能污染普通 shell 输入的错误状态。用户确认撤回该方案，
不为 telemetry 新增 pipe、热键、命令解析、握手、监听任务或超时。

已展示修复流程的每次 Run 在入队时生成独立 `RunId`；执行器按原有路径派发，
Send 仍发送原命令加 Enter，Insert 保留只插入行为。命令文本不进入 telemetry。

`WTA.ErrorFixRunStarted` 在执行器取出请求时记录 `OfferId` / `RunId`。
`ErrorFixRunResult` 与之关联：

- 原派发流程返回成功：`unobservable`；不能算执行成功或已知执行失败。
- 派发错误或无法确认派发：`dispatchFailed`；可能已有命令执行，不自动重试。

所有 shell 和输入沿用原有行为。记录只读取已有派发结果，不参与执行决策；
不监听后续 OSC、history 或其他命令结果。helper 退出可能只有 start 没有 result。
当前不能计算 Run 执行成功率，更不能计算修复成功率；报表应保留派发失败、未知和缺失。
精确执行结果关联留待产品本身需要可靠命令执行协议时独立设计，不属于本轮 telemetry。

回归覆盖有无 Run telemetry 的原样派发、Insert、派发失败不重试且保留原错误通知，
以及派发成功仍记录未知。telemetry 参考及 `doc\gap.txt` 与此范围保持一致。

## 5. 命令面板 `?` 模式进入到提交

对应需求：4.1，包括已确认的进入到提交精确转化需求。

### 已确认的改动

复用现有两个事件，新增共享的 `EntryId`，不另建进入或提交事件。
该标识仅用于命令面板中一次可见的前台 `?` 模式访问，
不是 agent pane 的遥测 `SessionId` 或 ACP 请求的 `TurnId`。

| 事件 | `EntryId` 的处理 |
| --- | --- |
| `App.CommandPaletteAgentPromptEntered` | 每次进入可见的前台 `?` 模式时生成新 ID |
| `App.CommandPaletteDispatchedAgentPrompt` | 提交非空前台问题时携带本次进入的同一个 ID，保留 `IsBackgroundMode` |

同一次进入期间编辑输入，不生成新 ID。
离开该模式或关闭命令面板后，本次访问结束；再次进入生成新 ID。
隐藏控件中的模式变化不构成可见入口，也不能错误复用上一次访问的标识。
空输入不算提交，不更改现有空输入处理行为。
后台 `&` 模式不纳入本项漏斗，不为它伪造 `?` 入口或复用前台入口标识。
ID 使用不包含输入内容的不透明值，不记录问题文本或按键内容。

### 报表口径

- 按不同 `EntryId` 建立进入 cohort，关联同 ID 且 `IsBackgroundMode=false` 的提交。
- 转化率为：
  `进入 cohort 中存在匹配提交的 EntryId 数 / 进入 cohort 的 EntryId 数`。
- 允许提交发生在入口选择窗口之外，使用一致的随访区间并对 ID 去重。
- 没有匹配提交只能称为“未观察到提交”，不能一概推断为用户主动放弃。
- 提交表示用户发出了请求，不代表 delegate 成功启动、agent 完成响应或解决了问题。
- 旧事件没有 `EntryId`，仅支持独立计数，不与新版本混算精确漏斗。

### 实现落点与验收

在 `src\cascadia\TerminalApp\CommandPalette.cpp` 的
`_recordAgentPromptEntry`、`_dispatchAgentPrompt` 及可见性和模式切换生命周期中
维护入口标识；沿用已有入口去重机制，不改变 UI 行为。
实现完成后同步 telemetry 参考及 `doc\gap.txt`。

验收应覆盖：一次进入和对应提交共享 ID；编辑不重复创建入口；
退出重进使用新 ID；隐藏状态不虚报入口；空输入不算提交；
后台模式不混入前台漏斗；多窗口访问不串联；载荷不包含输入内容。

## 6. Pin Tab 使用情况

对应需求：5.4。

### 已确认的范围

Pin Tab 和 Keep tab running 是独立功能，两者都需要 telemetry。
Keep running 已有开启、关闭后保留和恢复结果事件；本项补齐真正的 Pin Tab 使用埋点，
不把已有 Keep running 埋点当成 Pin Tab 数据。

Pin Tab 可在横向或竖向终端标签右键菜单中触发，将标签排在未固定标签之前；
Unpin tab 取消固定。固定不阻止正常关闭，也不自动启用后台保留。
现有 `SidebarTabPinned` 实际在启用 Keep running 时发送，历史含义不能改写。

### 计划改动

在真实固定或取消固定成功后记录状态变化，事件名建议为 `App.TabPinChanged`：

| 字段 | 含义 |
| --- | --- |
| `Pinned` | 本次操作后的固定状态，区分固定与取消固定 |
| `PinnedCount` | 操作后当前窗口内固定终端标签的数量 |

不为此增加漏斗关联 ID，不采集标签标题、命令或目录。
只记录实际成功的状态变化，重复设置相同状态、被阻止的操作及内部状态复制不算用户操作。
通过事件统计使用设备数、固定和取消次数，以及操作后的固定数量分布；
这些操作时快照不代表全部设备的当前固定状态。

实现落点为 `src\cascadia\TerminalApp\TabManagement.cpp` 的真实固定请求和状态变更路径，
覆盖横向与竖向入口；不在仅更新菜单或图标时上报。
保留 Keep running 的现有含义，旧 `SidebarTabPinned` 不加入 Pin Tab 报表。
实现完成后同步 telemetry 参考及 `doc\gap.txt`。

## 7. Rich tab 字段选择的启动状态与用户修改

对应需求：5.5。

### 已确认的改动

复用 `App.SidebarRowFieldsChanged`，保留规范化的 `fields` 字段，
增加 `Source` 区分每窗口启动快照和用户主动修改：

| `Source` | 触发边界 |
| --- | --- |
| `Launch` | 每个窗口启动时，在字段配置可用后记录一次当前选择，不依赖 ACP session 创建或用户是否修改过选择 |
| `UserChange` | 用户实际修改字段选择后，记录修改后的选择 |

移除原先依赖 agent session-start 的混合快照触发，不将其伪装成 `Launch` 或 `UserChange`。
重复 UI 刷新、程序初始化选项和没有实际变化的设置不算用户修改。
保留空选择、单字段和多字段组合，不假设用户一定选择两个字段。
只采集固定字段 ID，例如 `agentStatus`、`workingDirectory`、`repository`、
`branch`、`changes`，不采集字段展示的实际内容。

每窗口启动快照记录配置，不证明侧边栏当时可见或用户使用过这些字段。
功能不可用或配置读取失败不能伪装成空选择；
当前 feature gate 仅对 WindowsInbox 禁用，统计时区分不支持功能的版本和有效空选择。

### 报表与兼容

- `Source=Launch` 用于分析启动时字段组合和各字段的选择情况，
  `Source=UserChange` 用于分析主动修改次数及修改后的组合，不能将两者混算。
- 窗口快照占比不是设备占比；设备口径需明确去重或快照选取规则，
  例如报告窗口内每设备最近一次有效启动快照。
- 没有快照的设备保留为未知，不默认为未选择。
- 旧事件没有 `Source`，不能回填为启动或修改；新旧版本查询分开。

### 实现落点与验收

调整 `src\cascadia\TerminalApp\TerminalPage.cpp` 的
`_LogSidebarRowFieldsTelemetry` 及其调用点，把启动快照放到窗口配置已就绪的生命周期，
保留实际用户切换的上报；不为采集数据启动 agent 或改变字段配置。
实现完成后同步 telemetry 参考及 `doc\gap.txt`。

验收应覆盖：普通终端窗口在无 agent session 时仍有启动快照；
每窗口仅一次启动快照；agent session 创建或加载不重复上报启动；
用户修改携带正确来源和修改后字段；初始化不算修改；
空选择被保留；配置不可用不冒充空选择；不泄露目录、仓库名或分支名。

## 8. Keep running 的开启使用率

对应需求：6.1。调整原报告的指标，不新增 Keep running 开启事件。

### 已确认的产品问题与口径

核心问题是：过去 28 天的活跃设备中，有多少设备开启过 Keep running？
不再要求原报告中的“每个 agent session 的标记率”，也不优先计算所有标签的开启占比。
Keep running 的操作对象是整个 tab，但使用触达按设备统计。

复用 `App.KeepRunningMarked` 与第 1 项计划新增的 `Win32Host.UserInteract`：

1. 在同一 28 天窗口、版本和设备资格范围内，按后台设备标识分别去重。
2. 活跃集合 `A` 为发生过 `UserInteract` 的设备。
3. 开启集合 `M` 为发生过 `KeepRunningMarked` 的设备。
4. 开启使用率为 `size(A intersect M) / size(A)`，空分母为不可用，不报告为零。

指标表示“报告期内开启过”，不是“当前仍开启”，也不是首次采用或全部用户的比例。
窗口之前已开启且窗口内没有再次开启的设备，不计入本窗口的开启行为；
这不表示它在本窗口没有使用后台保留。
旧活跃事件存在长驻进程漏报，准确的新口径依赖第 1 项上线并采用一致的版本范围。

### 实现范围

本项只需调整报表查询和文档，不增加 Keep running 事件或启动状态采样。
现有 `KeepId` 仍可去重开启流程；
`TotalTabCount`、`KeepRunningTabCount` 和 `HasAgentPane` 保留作辅助分析，
不能把操作时快照解释为全体标签状态或 agent session 数量。

Keep running 是运行时选择，不以应用启动时大量未开启快照代替实际采用情况。
关闭后是否保留、是否恢复及是否继续使用，由后续生命周期指标分别回答。
同步更新 telemetry 参考的 6.1 查询与 `doc\gap.txt`，明确这是需求口径调整，
不是新埋点缺口；本计划不表示后台查询已经完成迁移。

## 9. Keep running 保留现场的恢复结果

对应需求：6.3。本项重新聚焦原报告的 Keep running 场景，
不将进程退出后加载历史会话混入同一漏斗。

### 已确认的场景与目标

用户开启 Keep running，关闭标签或窗口后，Terminal 进程仍保留原标签的运行现场；
用户之后通过恢复入口找回该标签。本项回答：
“用户尝试恢复之前保留的现场时，是否成功接回？”

恢复的是同一份 live 内容，包括原 shell、分屏及可能存在的 agent pane，
不是新建 shell 或通过 ACP `session/load` 加载历史。
重新打开普通窗口本身不等于发起保留标签的恢复请求。
进程退出、崩溃、系统重启后的历史会话加载是独立场景，不属于本项承诺。

### 现有能力与测量边界

复用 `App.KeepRunningDetached`、`App.KeepRunningReattached` 及其 `KeepId`。
已有恢复结果为 `live`（成功接回）或 `failed`（恢复失败）。
这里的 `live` 表示内容恢复成功，不表示 agent 正在执行任务或任务已完成。

必须区分两个问题：

- 已保留的流程中，有多少后来被成功恢复：按 detached cohort 的不同 `KeepId`
  关联后续 `live` 结果，衡量观察到的恢复使用情况。
- 真正发起的恢复尝试中，有多少成功：必须使用完整的恢复尝试及终态覆盖，
  不能直接把所有 detached 流程当作尝试分母。

同一个 `KeepId` 可以失败后重试成功，也可能再次保留和恢复。
流程级去重与单次尝试成功率不能混算，使用下述独立尝试标识区分。

### 已确认的埋点改动

| 事件 | 改动 |
| --- | --- |
| `App.KeepRunningReattachStarted` | 新增；实际发起一次恢复操作时记录，生成不含内容的 `AttemptId` |
| `App.KeepRunningReattached` | 增加同一次尝试的 `AttemptId` 和 `HasAgentSession`，保留 `KeepId`、`HasAgentPane` 和 `Outcome=live/failed` |

`KeepId` 关联原保留流程，`AttemptId` 关联本次恢复的开始与结果。
同一标签的重试或再次恢复使用新的 `AttemptId`；一项批量恢复中的不同目标分别记录尝试，
内部函数调用不重复创建一次外部恢复操作。
开始事件仅携带 `AttemptId`，不要求先成功查找目标。
结果携带可取得的 `KeepId`；前置失败无法取得该值时用空字符串，
仍通过 `AttemptId` 关联结果，不生成假的 `KeepId` 或导出运行时路由 ID。

补齐恢复前置步骤、内容转移及恢复提交的可观察失败，不只包围实际内容转移调用。
成功只在原内容恢复提交后记录；一次尝试的正常结果只记录一次。
保留原有异常传播、回滚和内容存活行为，不因 telemetry 改变恢复逻辑。

### 查询与验收

按开始事件的不同 `AttemptId` 建立 cohort，关联同 ID 的结果事件。
分别报告尝试数、成功数、明确失败数，以及随访区间内没有结果的尝试数。
未知结果不合并为失败；若展示仅有已知结果的成功率，必须同时说明结果覆盖率。
以 detached `KeepId` 为 cohort 的恢复使用指标仍独立计算，不能替代逐次尝试指标。
旧事件没有 `AttemptId`，不混入新口径的精确尝试漏斗。

验收覆盖：成功恢复的开始与结果一致；前置失败也有对应结果；
无法取得 `KeepId` 时不伪造关联；失败后重试使用新尝试 ID；
批量目标分别记录且内部操作不重复；进程中断造成的缺失结果保留未知；
失败回滚不因埋点改变；无标题、路径或内容进入载荷。

### 不推断无法观察的结果

没有恢复事件可能是用户没有回来、尚未操作、主动关闭保留内容、进程退出，
也可能是数据未被采集。不能据此判断 `gone` 或恢复失败。
不为凑齐原报告的 `live / gone / failed` 而伪造 `gone`，本次不新增该结果。
实际发起恢复后明确遇到目标不存在，属于观察到的失败；
没有发起恢复则不能凭记录缺失推断目标不存在。
已确认的目标是如实反映保留现场的恢复，而不是以 telemetry 承诺跨进程存活。

实现前核查 `src\cascadia\TerminalApp\TabManagement.cpp` 的 `RestoreKeptGroup`
及各恢复入口，包括进入实际内容转移前的失败和重复尝试；
仅按测量缺口补充埋点，不扩展恢复功能。
完成后更新 telemetry 参考与 `doc\gap.txt`，澄清这是聚焦后的场景口径，
不再把“历史加载缺少来源”列为 Keep running 恢复本身的缺口。

## 10. Keep running 恢复后继续在 agent pane 提问

对应需求：6.4；补齐 `gap.txt` 中逐次恢复到提问的精确关联。

### 原需求与已确认的扩展

原报告明确要求 `WTA.AgentPromptSent.Reattached=true`，
该字段已经实现，可以统计恢复后原 agent 会话的提问量。
本项不是补缺失的提问事件，而是增加恢复关联，回答：
“成功恢复的、具备可继续对话的 agent 会话中，有多少在规定时间内继续提问？”

范围仅为内置 agent pane 经 WTA / ACP 发送的用户 prompt。
普通 shell 中直接运行的 Copilot、Claude、Codex 等 CLI 内部提问不由该事件覆盖；
普通 shell 命令和进程退出后的历史会话加载也不纳入本项。

### 已确认的改动

成功恢复时，将原保留流程的 `KeepId` 和本次恢复的 `AttemptId`
传递并绑定到被恢复的同一个 ACP 会话。
该会话后续 `WTA.AgentPromptSent` 携带这两个字段，
保留现有 `Reattached`、`IsAutofix` 以及第 2 项计划的 session / turn 关联。
不新增恢复或提问事件，也不新增另一套关联 ID。

同一次恢复后多轮提问共享恢复关联；再次成功恢复后，后续提问关联新的 `AttemptId`。
新建、切换或加载为另一个 ACP 会话时不得继承原会话的恢复标识；
普通提问没有关联时不伪造 `KeepId` 或 `AttemptId`。
并发窗口、标签及尚在途的 prompt 应保留各自派发时的关联，不能在完成时读取其他流程状态。

### 分母与查询

- 以成功恢复且具备可继续对话的 agent 会话的不同 `AttemptId` 建立 cohort。
- 按同一 `KeepId + AttemptId` 关联规定随访区间内的
  `AgentPromptSent`，筛选 `Reattached=true AND IsAutofix=false`。
- 转化率为该 cohort 中至少有一次匹配用户提问的尝试数除以 cohort 尝试数。
  同次恢复后的十次提问只计一次继续使用。
- 普通 shell 标签不进入 agent 对话分母；无提问只能表示未观察到继续对话，
  不代表整个恢复标签未被使用。
- 现有 `HasAgentPane` 只证明包含 agent pane。新增 `HasAgentSession` 表示恢复时
  agent pane 已绑定非空 session ID，用于独立建立分母；这不是 provider 就绪保证。
  不能根据之后是否出现 prompt 反推资格，否则会把没有继续使用的会话排除。
- 旧 prompt 没有恢复关联，仅用于恢复后提问量统计，不混入精确尝试漏斗。

### 实现落点与验收

扩展 `src\cascadia\TerminalApp\TabManagement.cpp` 中成功恢复后的
`keep_running_reattached` 通知，在 WTA 对应的 tab / session 状态中保存遥测关联，
再经 prompt 派发路径传入 `tools\wta\src\telemetry.rs`。
仅传递不透明遥测 ID，不导出运行时路由标识、命令或内容。

验收覆盖：原会话的用户 prompt 关联正确恢复；多次提问不放大恢复数；
再次恢复使用新尝试；自动 autofix 不算用户继续提问；
会话切换和并发不会串联；普通 CLI / shell 不被冒充为 agent pane 使用；
没有后续 prompt 的合格成功恢复仍留在分母。
实现完成后同步 telemetry 参考及 `doc\gap.txt`，注明基础需求已实现、
本次新增的是精确恢复到提问漏斗。

## 11. Agent 配置指标的查询迁移与历史可比性

对应需求：1.3、7.1，并关联 7.2 的配置变更。
现有实现已经将启动配置状态与变更分开，本项不新增埋点。

### 已确认的处理

| 问题 | 使用的现有事件 |
| --- | --- |
| 每个窗口启动时配置了什么 agent | `App.AppCreated` 的主 agent、委派 agent 及其生效 provider 字段 |
| agent 配置发生了什么变化 | `Model.AgentProviderChanged` 的 `role`、`from`、`to` |

配置、安装、认证和实际使用是不同事实；不能把配置快照当成已使用 agent 的证据。
配置值与受策略影响后的生效值分别统计，主 agent 与委派 agent 不混成两个独立用户。
窗口快照占比与设备占比分开；设备口径明确快照选择和活跃集合交集规则，
缺少配置快照的设备保留为未知。

旧 `AgentProviderConfigured` 退役后的查询迁移不等于相关产品指标完全无法继续：

- 原报告的 53% 来自初始化设备数与配置设备数之比，
  配置分母迁移到新快照后需评估新旧人群、触发范围及版本的可比性。
- 原报告的 3.5% 来自提问设备数与新建 session 设备数之比，
  不直接依赖旧配置事件，不能仅因配置事件替换就判定无法延续。
- 独立汇总设备数相除本身不证明同一 cohort 的转化；
  新报表应按相同资格、时间及版本范围关联设备集合，必要时考虑事件顺序。
- 不直接拼接新旧事件次数，也不承诺历史比例完全可比或可精确回填。
  按版本和 schema 标记口径边界，并遵守后台 scaled counts 的加权方法。

本项仅调整报表查询与文档，更新 telemetry 参考及 `doc\gap.txt`，
归类为“已有能力，需查询迁移和历史口径说明”，不再归类为埋点缺失。
本计划不表示实际后台查询或历史数据重算已经完成。

## 12. 独立后续场景与未决细节

以下仅记录已确认的目标与核查结论，不代表已经批准具体埋点设计，
也不代表本次要一并实现。

| 条目 | 已明确的需求或边界 | 后续讨论 |
| --- | --- | --- |
| 独立后续场景：历史会话加载 | 此前提出进程退出后重建窗口/标签页、加载原 agent 历史并继续聊天；不要求原 shell 或运行中任务存活 | 从 6.3 Keep running 漏斗中移出；恢复来源、尝试及后续使用的统计方案另行讨论 |

特别注意：仓库已有持久化布局与 ACP `session/load` 恢复路径。
不能将 Keep running 不支持进程退出后的存活，等同于完全没有历史会话恢复能力。

会话观察边界已确认，见第 2 项；Run 执行结果在所有 shell 均保持未知，见第 4 项；
恢复后继续对话按有绑定 session 的恢复建立分母，见第 10 项。
旧 44% 的来源仍未核实。历史会话加载来源分析和实际后台查询部署不在本轮客户端改动范围。

## 13. 实施与验证记录

- 收缩后的 WTA 格式检查和显式 Windows target 构建通过；
  全量单元测试 2447 passed、0 failed、1 ignored。
  新增执行器回归覆盖原样 Run/Insert 派发、有无关联身份、
  失败不重试且保留错误通知，以及派发成功仍报告未知。
- TerminalApp 单元测试工程、WindowsTerminal 和 Debug 包构建通过；
  `CommandPaletteTelemetryTests` 的 5 个测试全部通过。
- 恢复关联测试覆盖跨 tab/window 隔离、花括号 UUID、重复恢复更新 AttemptId、
  无效/缺失 ID 不沿用旧关联，以及 session 替换清除关联。
- PowerShell shell integration 与本轮改动前版本完全一致，8 个原生单元测试通过；
  `execute_choice` 的签名和实现也恢复原样，旁路事件只留在执行器外围。
- 专用 Run 通道及其 `Feature.PowerShellRun` live 测试、对应的未提交 checklist 条目已撤回。
  历史测试结果仅属于已撤回方案，不是当前能力或无兼容性问题的证明。
- Dev 包已通过指定部署脚本更新；部署目标无运行中的进程，
  未卸载包、未终止当前 Copilot 宿主。Live 测试由 ItE2E 备份并恢复设置，
  使用独立生成的 integration 脚本，未更新用户 PowerShell profile。
- 撤回通道后的 Debug 包重新构建并安全更新到 Dev，
  部署的 WTA / TerminalApp 哈希与当前构建产物一致；本次未修改用户 profile 或设置，
  未新增进程终止操作。之前的 live 记录不替代本次回归。
- 本轮 live ETW 和包产物验收见下节；尚未进行后台入库/报表验证。
  查询文档不是后台查询已部署的证明。

## 14. PR 验收结果与剩余阻塞

PR：<https://github.com/microsoft/intelligent-terminal/pull/1077>。
生产代码构建版本为 `a05d6c366a93b61fce4b2e7fe5155d9aa532e86b`；
后续验收改动仅涉及测试、解码辅助和文档，没有改变生产代码。

| 验证 | 实际结果 |
| --- | --- |
| Debug Dev | WTA 与包构建通过，安全部署；实际加载的 WTA、TerminalApp 哈希与构建产物一致 |
| Release x64 MSIX | 构建和现有包验证脚本通过；包内 WTA、TerminalApp 及 13 个 hook 文件与源码/Release 产物逐一校验一致 |
| WTA 单元测试 | 2447 passed、0 failed、1 ignored |
| 原生回归 | Palette/daily activity 5/5；原有 PowerShell integration 8/8 |
| ETW 验收辅助自测 | 33/33，包括类型解码、策略恢复和仅关闭测试所属进程 |
| 完整 `Feature.TelemetryFunnels` | 18 passed、2 failed、0 skipped；失败为已有 HKCU 策略热更新问题，并未改成跳过或通过 |
| 完整 `Feature.SidebarTelemetry` | 尚未通过；首轮 UI 自动化在搜索/历史视图切换处失败，后续重跑在 UAC 提示超时取消，未执行场景 |

Release 产物为 Dev branding、版本 `0.8.0.9`、x64 的**未签名** MSIX，
不是 Store 签名包，也没有将 Release 包安装为用户正在使用的 Dev。
根目录打包 CMD 在本机解析失败后，使用相同顺序的 Settings Model、Settings Editor、
CascadiaPackage MSBuild 步骤完成构建；不将旧日志中的成功状态当成本次证据。

真实 ETW 已验证每日键盘活动、SessionId/TurnId 和 Autofix 分类关联、
Detection/Offer/Acceptance/Run 关联，以及 palette EntryId。
Run fixture 确实执行并生成预期文件，但遥测结果仍为 `unobservable`，
没有把已派发伪装成命令或修复成功。
首次 Sidebar 捕获也收到 2 个 `Launch` 和 10 个 `UserChange` 字段事件；
这只是部分证据，不能替代完整 pin/restore 场景验收。

策略热更新失败跟踪：<https://github.com/microsoft/intelligent-terminal/issues/991>。
启动前设置策略的独立场景通过，但不用于冒充热更新成功。
临时 HKCU 策略已恢复；设置和 state 已按原始哈希恢复，
无待恢复备份、无剩余 Dev 测试窗口，未卸载包或终止当前 Copilot 宿主。

完整与增量 release report 的条目状态一致：
新条目 C369/C370 为通过，C367/C368 保留自动化失败/未完成状态；
不复用已撤回 Run 通道的历史结果补齐验收。
PR 保持 Draft，待 Sidebar 完整 live 验收完成并处理或明确接受已有策略问题。
本地 ETW receipt 不能证明后台入库、查询部署或实际 D7/D28 cohort 已可用。

# Intelligent Terminal 在切换 Agent/模型时崩溃的调查记录

## 摘要

Intelligent Terminal 在切换 ACP agent 或模型时会崩溃，尤其是在 AI agent pane 已经打开或已经预热运行的情况下。Windows Event Log 显示崩溃发生在 packaged `WindowsTerminal.exe` 进程内，faulting module 是 `TerminalApp.dll`，而不是 `wta.exe` 或外部 agent 进程。

从日志看，模型切换本身更像是触发条件：它会触发 agent pane 的 teardown/recreate 生命周期路径。真正的问题大概率在 TerminalApp 的 agent pane 关闭和重建逻辑中。

## 观察到的崩溃

Application Event Log 中多次出现相同签名：

- Package: `IntelligentTerminal_0.7.0.8_arm64__rd9vj3e6a2mbr`
- Process: `WindowsTerminal.exe`
- Faulting module: `TerminalApp.dll`
- Fault offset: `0x00000000001adca4`
- Exception codes:
  - `0xc0000005`
  - `0xc000041d`

示例事件：

```text
Faulting application name: WindowsTerminal.exe
Faulting module name: TerminalApp.dll
Exception code: 0xc0000005 / 0xc000041d
Fault offset: 0x00000000001adca4
Faulting package full name: IntelligentTerminal_0.7.0.8_arm64__rd9vj3e6a2mbr
```

近期记录中，同一个 fault offset 连续出现：

```text
2026-05-11 09:51:07  TerminalApp.dll  0xc0000005  offset 0x1adca4
2026-05-11 09:51:27  TerminalApp.dll  0xc000041d  offset 0x1adca4
2026-05-11 09:53:55  TerminalApp.dll  0xc0000005  offset 0x1adca4
2026-05-11 09:53:59  TerminalApp.dll  0xc000041d  offset 0x1adca4
```

WTA / IntelligentTerminal 日志中，在崩溃时间附近能看到 agent pane rebuild：

```text
_RebuildAgentStack: agent settings changed, rebuilding
_TeardownAgentPane: closing agent pane
agent pane closed - _agentPane cleared
_AutoCreateHiddenAgentPane: cmdline=...
```

这说明崩溃与设置变化后的 agent pane 生命周期切换高度相关。

## 可能原因

最可能的原因是 agent pane teardown/recreate 过程中的生命周期或重入问题。

相关代码路径：

- `src\cascadia\TerminalApp\TerminalPage.cpp`
- `TerminalPage::_RebuildAgentStack()`
- `TerminalPage::_TeardownAgentPane()`
- `TerminalPage::_AutoCreateHiddenAgentPane()`
- `TerminalPage::OnAgentStatusChanged()`

切换 ACP agent 或模型时，settings 会变化，然后触发 `_RebuildAgentStack()`。该函数会关闭当前 agent pane，并在同一条路径中创建新的隐藏 agent pane。日志显示崩溃发生在这段 teardown/rebuild 过程附近。

因此，模型不是直接 crash root cause；模型切换只是触发了 live agent pane 的关闭和重建。

## 其他相关但非主要的异常

### Copilot ACP 初始化超时

日志中多次看到：

```text
run_acp_client failed: ACP initialize timed out after 15 s - 'copilot' did not respond
```

同一命令后来又能成功，但耗时约 15.9 秒：

```text
Session created ... (t+15.911s)
ACP session model set to claude-haiku-4.5
```

这说明当前 15 秒初始化超时偏紧，可能导致 agent pane 进入 `failed` 状态，但这不是 TerminalApp crash 的直接证据。

### Codex ACP usage_update 解码错误

日志中也有：

```text
failed to decode ... unknown variant `usage_update`
```

这看起来是 ACP schema 兼容性问题，可能影响 Codex session 的稳定性，但目前没有证据表明它直接导致 `TerminalApp.dll` 崩溃。

## 如何复现

1. 启动 packaged Intelligent Terminal `0.7.0.8`。
2. 打开 AI agent pane。
3. 等待 agent 初始化并推送 `agent_status` / model list。
4. 打开 AI agent 设置页面，或者使用模型选择入口。
5. 在 agent pane 仍然存在时切换 ACP agent 或模型。
   - 已观察到在 `copilot` 和 `codex` 之间切换时触发。
   - 切换 Copilot 模型，例如 `claude-haiku-4.5`，也更容易触发相关路径。
6. 观察应用可能在设置变化后很快崩溃。

复现时，日志中通常能看到：

```text
_RebuildAgentStack: agent settings changed, rebuilding
_TeardownAgentPane: closing agent pane
agent pane closed - _agentPane cleared
```

Event Log 中应能看到类似：

```text
Faulting application name: WindowsTerminal.exe
Faulting module name: TerminalApp.dll
Exception code: 0xc0000005 or 0xc000041d
Fault offset: 0x00000000001adca4
```

## 临时缓解方式

在代码修复前，可以使用以下方式降低触发概率：

1. 不要在 agent pane 打开或正在初始化时频繁切换 ACP agent/model。
2. 切换模型前，先关闭或隐藏 agent pane。
3. 修改 agent/model 设置后，重启 Intelligent Terminal。
4. 如果 Copilot ACP 反复初始化超时，先切回默认或 `auto` 模型，再重启 Terminal。
5. 避免短时间内来回切换 `copilot`、`codex` 和不同模型。

## 建议的产品修复方向

1. 加固 agent pane rebuild 生命周期：
   - 旧 pane 的 close path 完全结束后，再创建新 pane。
   - 避免 `Pane::Close()` 后继续使用过期的 `_agentPane` weak reference。
   - 防止 `Closed` handler 和 rebuild 逻辑发生重入。

2. 给 WTA / agent status 事件加 generation 或 session id：
   - teardown 后忽略旧 WTA 进程发来的 `agent_status`。
   - 避免旧进程事件更新新 pane 或 Settings UI。

3. 加固 `OnAgentStatusChanged()`：
   - 确认 runtime model cache 和 Settings UI 更新都在正确 UI thread 上执行。
   - 对 torn-down page 或 stale pane 状态做保护。

4. 放宽 ACP 初始化超时：
   - 当前 15 秒太接近实际成功耗时。
   - 增大 timeout 可以减少误判 `failed`。

5. 容忍未知 ACP session update：
   - 对 `usage_update` 这类未知 variant 做 ignore/log，而不是 decode failure。

## 下一步调试建议

捕获 `WindowsTerminal.exe` crash dump，并用匹配的 `TerminalApp.pdb` 解析 `TerminalApp.dll + 0x1adca4`。这能确认 fault offset 对应的具体函数，从而判断崩溃点是在 pane teardown、status event dispatch，还是 Settings UI model-list refresh。

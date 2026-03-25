# WTA (Windows Terminal Agent) — 项目概览

## 一句话总结

WTA 是一个 Rust 编写的命令行工具，**把 AI 代理（Agent）和 Windows Terminal 连接起来**。它让 AI（如 GitHub Copilot、Claude）能够直接操作你的终端：创建标签页、分割面板、执行命令、读取输出——就像一个 AI 驱动的 tmux。

---

## 它解决什么问题？

目前 AI 编程助手只能"说"代码，不能"做"事情。WTA 补上这一环：

- **AI 代理想运行命令？** → WTA 帮它在 Windows Terminal 里开一个 pane 执行
- **AI 代理想看命令输出？** → WTA 从终端读取内容返回给它
- **用户想用 AI 对话式操作终端？** → WTA 提供 TUI 聊天界面，输入自然语言，AI 帮你执行

---

## 三种运行模式

### 1. ACP TUI 模式（默认）— 交互式聊天 UI

```
wta
wta --agent "copilot --acp --stdio"
wta "帮我列出所有 git branch"
```

启动一个终端内的聊天界面（用 ratatui 渲染），WTA 作为 **ACP 客户端**，启动一个 AI 代理子进程（默认 Copilot），通过 JSON-RPC stdin/stdout 通信。用户在聊天框输入，AI 可以请求执行命令、创建终端等，WTA 代为执行并展示结果。

当 ACP 模式已经连上 Windows Terminal 时，当前推荐的 agent 控制面是本地 `wta` CLI，而不是在同一会话里依赖 MCP。也就是说，agent 通过 shell out 调 `wta active-pane --json`、`wta list-panes --json`、`wta send-keys --json` 这类命令，CLI 再经由命名管道调用 WT。

### 2. MCP Server 模式 — 给 AI 用的工具服务器

```
wta mcp
```

无头运行，WTA 作为 **MCP 服务器**，暴露 15 个工具给外部 AI 代理调用。AI 代理可以通过这些工具：
- 执行命令 (`run_command`)
- 创建/管理终端会话 (`create_terminal`, `get_terminal_output`, `kill_terminal`)
- 查询 Windows Terminal 状态 (`wt_list_windows`, `wt_list_tabs`, `wt_list_panes`)
- 控制 Windows Terminal (`wt_create_tab`, `wt_split_pane`, `wt_send_input`, `wt_close_pane`)
- 读取终端内容 (`wt_read_pane_output`, `wt_get_process_status`)

### 3. CLI 模式 — 类 tmux 命令

```
wta list-windows          # 列出所有 WT 窗口
wta list-tabs             # 列出标签页
wta send-keys -t 3 "cargo build" Enter    # 向第 3 个 pane 发送按键
wta capture-pane -t 3 -l 50              # 读取 pane 3 的最近 50 行
wta new-tab -c "pwsh.exe" -n "Build"      # 创建新标签页
wta split-pane -h                         # 水平分割当前面板
```

一次性命令，直接通过命名管道和 Windows Terminal 通信。对人和 AI 都有用。

---

## 架构图

```
 AI Agent CLI (copilot/claude)     外部 AI agent        用户 / AI shell out
       |  ACP/stdio                  |  MCP/stdio         |  CLI subcommands
       v                             v                    v
 +-----------+                +-----------+        +-------------+
 | ACP 模式   |                | MCP 模式   |        | CLI 模式     |
 | (TUI 聊天) |                | (无头服务器) |        | (一次性命令)  |
 | client.rs |                | server.rs |        | main.rs     |
 +-----+-----+                +-----+-----+        +------+------+
       |                             |                     |
       +---------------+-------------+                     |
                       |                                   |
                 ShellManager                        PipeChannel
                  |         |                        (直接调用)
           本地子进程      WtChannel (命名管道)            |
                                  |                        |
                                  +------------------------+
                                  |
                         Windows Terminal
                        (ProtocolRequestHandler)
```

---

## 核心模块

| 模块 | 文件 | 职责 |
|------|------|------|
| **main.rs** | `src/main.rs` | CLI 解析（clap），模式分发，管道发现逻辑 |
| **ACP Client** | `src/protocol/acp/client.rs` | ACP 客户端，启动 AI 子进程，处理 JSON-RPC 消息 |
| **MCP Server** | `src/protocol/mcp/server.rs` | MCP 服务器，用 rmcp 暴露 15 个工具 |
| **ShellManager** | `src/shell/shell_manager.rs` | 终端进程管理器，本地子进程或 WT pane |
| **PipeChannel** | `src/shell/wt_channel/pipe_channel.rs` | 命名管道通道，和 WT 双向通信 |
| **VtChannel** | `src/shell/wt_channel/vt_channel.rs` | VT OSC 9001 发现辅助；主控制链路不是它 |
| **TUI** | `src/ui/*.rs` | ratatui 聊天 UI：消息渲染、输入框、权限弹窗、状态栏 |
| **App** | `src/app.rs` | TUI 状态管理和事件循环 |

---

## 通信协议

### WTA ↔ AI Agent
- **ACP (Agent Client Protocol)**: JSON-RPC 2.0 over stdio，WTA 是客户端
- **MCP (Model Context Protocol)**: JSON-RPC 2.0 over stdio，WTA 是服务器

### WTA ↔ Windows Terminal
- **命名管道**: `\\.\pipe\WindowsTerminal-<PID>`，换行分隔的 JSON-RPC
- 管道发现优先级：`--pipe-name` 参数 > VT OSC 9001 发现 > `WT_PIPE_NAME` 环境变量
- 认证：token-based，空 token = dev bypass

---

## 技术栈

| 用途 | 库 |
|------|-----|
| 异步运行时 | tokio |
| CLI 解析 | clap 4 |
| TUI 渲染 | ratatui + crossterm |
| ACP 协议 | agent-client-protocol 0.10 |
| MCP 协议 | rmcp 1.1 |
| 序列化 | serde + serde_json |
| 错误处理 | anyhow |

---

## 构建 & 运行

```bash
# 前置条件：安装 Rust (rustup)
cd wta
cargo build

# 输出二进制：wta/target/debug/wta.exe

# 运行 ACP 聊天模式
wta

# 运行 MCP 服务器模式
wta mcp

# 测试和 Windows Terminal 的管道连接
wta test-pipe

# 类 tmux 操作
wta list-windows
wta send-keys "echo hello" Enter
```

---

## 和 Windows Terminal 仓库的关系

WTA 位于 Windows Terminal 源码仓库的 `wta/` 子目录下。它是一个独立的 Rust 项目，但设计上是 Windows Terminal 的**配套工具**：

- Windows Terminal (C++) 侧已实现了 `ProtocolRequestHandler`，提供命名管道 API
- WTA (Rust) 侧通过 `PipeChannel` 连接到这个管道
- 现状：主链路是命名管道；VT OSC 9001 主要用于发现 pipe/token
- 未来计划：如果后续要做更深的 in-pane 集成，再扩展 WT 的 VT parser

---

## 进程模型详解

整个系统涉及 **4 类进程**，以及它们之间 **3 种 IPC 通道**。下面按场景逐一拆解。

---

### 进程清单

| 进程 | 可执行文件 | 生命周期 | 角色 |
|------|-----------|---------|------|
| **Windows Terminal** | `WindowsTerminal.exe` | 用户启动，长期运行 | 窗口管理器 + 终端渲染器，内部运行 `ProtocolRequestHandler`，监听命名管道 |
| **WTA** | `wta.exe` | 用户或 AI 启动 | 桥接层，根据模式扮演不同角色 |
| **AI Agent** | `copilot`、`claude-agent-acp` 等 | 被 WTA spawn 或在外部独立运行 | AI 大脑，做决策 |
| **Shell 命令** | `cargo`、`git`、`pwsh` 等 | 被 WTA 或 WT 创建，命令完成后退出 | 实际干活的工具进程 |

---

### 场景 1：ACP TUI 模式（`wta` 默认）

用户在 Windows Terminal 的一个 pane 里运行 `wta`，启动聊天界面。

```
┌─────────────────────────────────────────────────────────────┐
│                   Windows Terminal (进程 A)                  │
│                   PID: 1000                                  │
│  ┌─────────────────────────────┐ ┌────────────────────────┐ │
│  │ Pane 1: wta.exe (进程 B)    │ │ Pane 2: cargo build    │ │
│  │ PID: 2000                   │ │ PID: 4000              │ │
│  │                             │ │ (由 AI 通过 WTA 创建)   │ │
│  │  ┌──────────────────────┐   │ │                        │ │
│  │  │ copilot (进程 C)     │   │ │                        │ │
│  │  │ PID: 3000            │   │ │                        │ │
│  │  │ (WTA 的子进程)       │   │ │                        │ │
│  │  └──────────────────────┘   │ │                        │ │
│  └─────────────────────────────┘ └────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

**进程关系和 IPC：**

```
进程 A: Windows Terminal          进程 B: wta.exe              进程 C: copilot
(WindowsTerminal.exe)             (TUI 聊天界面)               (AI Agent 子进程)
      │                                │                            │
      │◄── 命名管道 (JSON-RPC) ──────►│◄── stdio (ACP JSON-RPC) ──►│
      │    \\.\pipe\WT-<PID>          │    stdin/stdout             │
      │                                │                            │
      │                                │  B 是 C 的父进程            │
      │                                │  B spawn C                 │
```

**信息流（用户说"运行 cargo build"）：**

1. 用户在 WTA TUI 输入 "运行 cargo build"
2. **WTA → Agent** (ACP stdio): `prompt("运行 cargo build")`
3. **Agent → WTA** (ACP stdio): `create_terminal({command: "cargo", args: ["build"]})`
4. **WTA → WT** (命名管道): `create_tab({commandline: "cargo build", background: true})`
5. **WT** 创建新 pane，spawn `cargo build` 进程（进程 D，PID: 4000）
6. **WT → WTA** (命名管道): `{pane_id: "2"}`
7. WTA 把 pane_id 映射到 terminal_id，返回给 Agent
8. Agent 后续调用 `terminal_output` → WTA 通过管道读 `read_pane_output`
9. Agent 调用 `wait_for_terminal_exit` → WTA 轮询 `get_process_status`

**进程数量：** 最少 3 个（WT + WTA + Agent），每执行一个命令 WT 里多一个 shell 进程。

---

### 场景 2：MCP Server 模式（`wta mcp`）

WTA 作为无头工具服务器，被外部 AI agent 通过 MCP 调用。

```
┌─────────────────────────────────────────────────────────────┐
│                   Windows Terminal (进程 A)                  │
│  ┌─────────────────────────────┐ ┌────────────────────────┐ │
│  │ Pane 1                      │ │ Pane 2: pwsh           │ │
│  │ (某个 shell)                │ │ (由 wta 创建)          │ │
│  └─────────────────────────────┘ └────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
        ▲                           命名管道
        │                             │
┌───────┴───────────────────────────────┐
│            wta.exe mcp (进程 B)        │  ← MCP Server
│            PID: 2000                    │
└───────────────────┬────────────────────┘
                    │ stdio (MCP JSON-RPC)
                    │
┌───────────────────▼────────────────────┐
│        外部 AI Agent (进程 C)           │  ← MCP Client
│        (VS Code Copilot, Claude Desktop│
│         或任意 MCP client)              │
│        进程 C 是 B 的父进程!            │
└────────────────────────────────────────┘
```

**关键区别：** 在 MCP 模式下，**调用方向反转**：

| | ACP 模式 | MCP 模式 |
|---|---|---|
| 谁 spawn 谁 | WTA spawn Agent | Agent spawn WTA |
| WTA 角色 | 客户端（发请求） | 服务器（响应请求） |
| Agent 角色 | 服务器（处理 prompt） | 客户端（调用 tool） |
| stdio 方向 | WTA → Agent stdin | Agent → WTA stdin |

**信息流（AI 想运行 `git status`）：**

1. **Agent → WTA** (MCP stdio): `tools/call run_command {command: "git", args: ["status"]}`
2. WTA 的 ShellManager 决定路由：
   - **有 WT 管道** → 走 `create_tab` 在 WT 里执行，通过管道读输出
   - **无 WT 管道** → 走本地 `tokio::process::Command` spawn 子进程
3. **WTA → Agent** (MCP stdio): `{stdout: "On branch main\n...", exit_code: 0}`

---

### 场景 3：CLI 模式（`wta list-windows` 等）

最简单，WTA 是一个一次性命令行工具，直接和 WT 通信后退出。

```
┌─────────────────────────────────────┐
│    Windows Terminal (进程 A)         │
└──────────────┬──────────────────────┘
               │ 命名管道
┌──────────────▼──────────────────────┐
│    wta list-windows (进程 B)         │
│    连接 → 发请求 → 打印结果 → 退出   │
└─────────────────────────────────────┘
```

**没有 AI Agent，没有 ShellManager。** WTA 直接用 `PipeChannel` 发一次 JSON-RPC 请求，打印结果，进程退出。生命周期只有几百毫秒。

---

### Shell 命令的两种执行路径

当 AI Agent 需要执行一个命令时，ShellManager 有两条路：

```
                    ShellManager.create_terminal(config)
                              │
                    ┌─────────┴─────────┐
                    │ has_wt_channel()?  │
                    └─────────┬─────────┘
                     Yes      │      No
                  ┌───────────┴───────────┐
                  ▼                       ▼
         路径 A: WT Pane            路径 B: Local 子进程
         (通过命名管道)              (tokio::process::Command)
                  │                       │
     ┌────────────┴────────────┐   ┌──────┴──────────────┐
     │ 1. WTA → WT pipe:       │   │ 1. WTA 直接 spawn    │
     │    create_tab(cmd)      │   │    子进程 (kill_on_drop)│
     │ 2. WT spawn 进程在新 pane│   │ 2. stdout/stderr pipe │
     │ 3. 读输出: read_pane_output│  │    到 WTA 内存 buffer │
     │ 4. 查状态: get_process_status│ │ 3. 直接读 buffer     │
     │                          │   │ 4. child.wait()       │
     │ 优点: 用户可见 pane       │   │                      │
     │ 缺点: 依赖 WT 管道       │   │ 优点: 无依赖          │
     └─────────────────────────┘   │ 缺点: 无 TUI 可见     │
                                    └──────────────────────┘
```

**Fallback 机制：** 如果 WT pane 创建失败，自动降级到 Local 子进程模式。

---

### IPC 通道汇总

| 通道 | 传输层 | 协议 | 方向 | 用途 |
|------|--------|------|------|------|
| **WTA ↔ Agent (ACP)** | stdio (stdin/stdout pipe) | JSON-RPC 2.0 (ACP) | 双向 | WTA 发 prompt，Agent 回流式消息、请求执行命令 |
| **WTA ↔ Agent (MCP)** | stdio (stdin/stdout pipe) | JSON-RPC 2.0 (MCP) | 双向 | Agent 调 tool，WTA 返回结果 |
| **WTA ↔ WT** | 命名管道 `\\.\pipe\WT-<PID>` | 换行分隔 JSON-RPC | 双向 | 管理 tab/pane，读取输出，发送按键 |

---

### 进程生命周期

```
时间 ──────────────────────────────────────────────────────────────►

Windows Terminal ════════════════════════════════════════════════════
  (用户启动，一直运行)

  wta.exe ────────────────────────────────┐ (用户 Ctrl+C 退出)
    │                                      │
    ├─ copilot (子进程) ──────────────────┤ (随 WTA kill_on_drop)
    │                                      │
    ├─[AI 请求] WT 创建 pane ──────────────┼── cargo build ─── (完成退出)
    │                                      │
    ├─[AI 请求] WT 创建 pane ──────────────┼── git status ──── (完成退出)
    │                                      │
    └──────────────────────────────────────┘

  wta list-windows ─┐ (一次性，立即退出)
                    └─
```

**关键点：**
- WTA 退出时，`kill_on_drop(true)` 确保 Agent 子进程被杀死
- WT 创建的 pane 进程**不随 WTA 退出而死亡**（它们是 WT 的子进程）
- Local 子进程（fallback 路径）**会随 WTA 退出而死亡**（`kill_on_drop`）
- CLI 模式的 WTA 进程生命极短，只做一次请求就退出

---

### 进程树总览

```
WindowsTerminal.exe (PID 1000)        ← 操作系统层面的进程树
  ├── conhost / conpty
  │   ├── pwsh.exe (Pane 1 的 shell)
  │   │   └── wta.exe (PID 2000)      ← 用户在 pane 里运行 wta
  │   │       └── copilot (PID 3000)   ← WTA spawn 的 AI agent
  │   │
  │   ├── pwsh.exe (Pane 2 的 shell)   ← 用户自己的 pane
  │   │
  │   ├── cargo.exe (Pane 3)           ← AI 通过 WTA 让 WT 创建的
  │   └── git.exe (Pane 4)             ← AI 通过 WTA 让 WT 创建的
  │
  └── ProtocolRequestHandler           ← WT 内部线程，监听命名管道
      (不是独立进程，是 WT 内的线程)
```

> **注意：** `ProtocolRequestHandler` 不是独立进程，它是 Windows Terminal 进程内部的一个组件，运行在线程池上，负责处理命名管道上的 JSON-RPC 请求。

---

## 当前状态

- **Part 1** (双模式架构 ACP + MCP): ✅ 完成
- **Part 2** (Windows Terminal 集成 — 命名管道 + CLI 命令): ✅ 完成，且这是当前主路径
- **Part 3** (CLI 子命令): ✅ 完成
- **未来**: 更深的 VT/OSC 集成、`focus_pane`、`rename-window`、`resize-pane` 等

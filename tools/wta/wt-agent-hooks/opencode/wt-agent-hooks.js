// Managed by Intelligent Terminal: wt-agent-hooks

import { appendFileSync } from "node:fs"

function eventMessage(value) {
  if (typeof value === "string") return value
  if (value && typeof value === "object") {
    if (typeof value.message === "string") return value.message
    if (typeof value.data?.message === "string") return value.data.message
    if (typeof value.name === "string") return value.name
  }
  return ""
}

// Every other CLI reaches the bridge through a shell, so `CreateProcess`
// resolves the `wtcli.exe` on PATH — which is the MSIX app-execution alias, a
// zero-byte reparse point. This plugin spawns an argv array instead, and Bun
// does its own PATH lookup, which rejects the alias outright ("Executable not
// found in $PATH"). Terminal injects the real path for exactly this case;
// falling back to the bare name keeps unpackaged dev builds working, where the
// name resolves to an ordinary file.
function bridgeCommand() {
  const injected = process.env.WTCLI_PATH
  return injected && injected.length > 0 ? injected : "wtcli.exe"
}

// A spawn failure here must never disturb OpenCode, but it must not be
// invisible either: this exact failure shipped once and left no trace anywhere,
// because the handler was empty. Terminal already injects the hook log
// directory, so record it there and carry on.
function noteBridgeFailure(topic, error) {
  try {
    const dir = process.env.WTA_HOOK_LOG_DIR
    if (!dir) return
    const message = error && error.message ? error.message : String(error)
    const line = `${new Date().toISOString()} opencode bridge spawn failed topic=${topic} cmd=${bridgeCommand()} err=${message}\n`
    appendFileSync(`${dir}\\hook-trace.log`, line)
  } catch {
    // Diagnostics are best effort; never let them become the failure.
  }
}

export const WtAgentHooks = async ({ directory }) => {
  const rootSessions = new Map()
  const childSessions = new Set()
  const enabled =
    process.platform === "win32" &&
    Boolean(process.env.WT_COM_CLSID) &&
    Boolean(process.env.WT_SESSION) &&
    process.env.OPENCODE_CLIENT !== "acp"
  function emit(topic, sessionID, payload = {}) {
    if (!enabled || !sessionID) return

    try {
      const child = Bun.spawn({
        cmd: [
          bridgeCommand(),
          "agent-hook",
          "--cli-source",
          "opencode",
          "--event",
          topic,
        ],
        stdin: new TextEncoder().encode(
          JSON.stringify({
            session_id: sessionID,
            cwd: directory,
            ...payload,
          }),
        ),
        stdout: "ignore",
        stderr: "ignore",
        windowsHide: true,
      })
      void child.exited.catch(() => {})
    } catch (error) {
      // Session tracking must never affect OpenCode's own execution.
      noteBridgeFailure(topic, error)
    }
  }

  function rememberSession(info) {
    if (!info?.id) return false
    if (info.parentID) {
      childSessions.add(info.id)
      rootSessions.delete(info.id)
      return false
    }

    childSessions.delete(info.id)
    const previous = rootSessions.get(info.id)
    const session = {
      cwd: info.directory || previous?.cwd || directory,
      title: info.title || previous?.title || "",
    }
    rootSessions.set(info.id, session)
    if (!previous || (info.title && info.title !== previous.title)) {
      emit("agent.session.start", info.id, {
        cwd: session.cwd,
        title: session.title,
      })
    }
    return true
  }

  function isRootSession(sessionID) {
    return rootSessions.has(sessionID) && !childSessions.has(sessionID)
  }

  return {
    "chat.message": async (input) => {
      const sessionID = input.sessionID
      if (!sessionID) return

      if (!childSessions.has(sessionID) && !rootSessions.has(sessionID)) {
        rootSessions.set(sessionID, { cwd: directory, title: "" })
      }
      if (isRootSession(sessionID)) {
        // Rebind an existing OpenCode session when the user returns to it.
        emit("agent.session.start", sessionID, {
          cwd: rootSessions.get(sessionID).cwd,
        })
        emit("agent.prompt.submit", sessionID)
      }
    },

    "tool.execute.before": async (input, output) => {
      if (!isRootSession(input.sessionID)) return
      emit("agent.tool.starting", input.sessionID, {
        tool_name: input.tool,
        tool_input: output.args,
      })
    },

    event: async ({ event }) => {
      const properties = event.properties || {}

      switch (event.type) {
        case "session.created":
        case "session.updated":
          rememberSession(properties.info)
          return
        case "session.status": {
          if (!isRootSession(properties.sessionID)) return
          if (properties.status?.type === "idle") {
            emit("agent.stop", properties.sessionID)
          } else if (properties.status?.type === "busy" || properties.status?.type === "retry") {
            emit("agent.prompt.submit", properties.sessionID)
          }
          return
        }
        case "session.idle":
          if (isRootSession(properties.sessionID)) {
            emit("agent.stop", properties.sessionID)
          }
          return
        case "session.error":
          if (properties.sessionID && isRootSession(properties.sessionID)) {
            emit("agent.error", properties.sessionID, {
              error: eventMessage(properties.error) || "OpenCode session error",
            })
          }
          return
        case "session.deleted": {
          const sessionID = properties.info?.id
          if (sessionID && isRootSession(sessionID)) {
            emit("agent.session.end", sessionID, { reason: "deleted" })
          }
          rootSessions.delete(sessionID)
          childSessions.delete(sessionID)
          return
        }
        case "permission.asked":
        case "question.asked":
          if (isRootSession(properties.sessionID)) {
            emit("agent.notification", properties.sessionID, {
              message:
                event.type === "permission.asked"
                  ? `Permission required: ${properties.permission || "tool use"}`
                  : "OpenCode is waiting for input",
            })
          }
          return
        case "permission.replied":
        case "question.replied":
          if (isRootSession(properties.sessionID)) {
            emit("agent.prompt.submit", properties.sessionID)
          }
          return
      }
    },

    dispose: async () => {
      for (const sessionID of rootSessions.keys()) {
        emit("agent.session.end", sessionID, { reason: "OpenCode exited" })
      }
      rootSessions.clear()
      childSessions.clear()
    },
  }
}

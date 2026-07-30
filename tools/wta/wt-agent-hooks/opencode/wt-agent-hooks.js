// Managed by Intelligent Terminal: wt-agent-hooks

function eventMessage(value) {
  if (typeof value === "string") return value
  if (value && typeof value === "object") {
    if (typeof value.message === "string") return value.message
    if (typeof value.data?.message === "string") return value.data.message
    if (typeof value.name === "string") return value.name
  }
  return ""
}

export const WtAgentHooks = async ({ directory }) => {
  const rootSessions = new Map()
  const childSessions = new Set()
  const enabled =
    process.platform === "win32" &&
    Boolean(process.env.WT_COM_CLSID) &&
    Boolean(process.env.WT_SESSION) &&
    process.env.OPENCODE_CLIENT !== "acp"
  const script = `${import.meta.dir}\\wt-agent-hooks\\send-event.ps1`

  function emit(topic, sessionID, payload = {}) {
    if (!enabled || !sessionID) return

    try {
      const child = Bun.spawn({
        cmd: [
          "powershell.exe",
          "-NoProfile",
          "-NonInteractive",
          "-ExecutionPolicy",
          "Bypass",
          "-File",
          script,
          "-CliSource",
          "opencode",
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
    } catch {
      // Session tracking must never affect OpenCode's own execution.
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

    "tool.execute.after": async (input) => {
      if (!isRootSession(input.sessionID)) return
      emit("agent.tool.finished", input.sessionID, {
        tool_name: input.tool,
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

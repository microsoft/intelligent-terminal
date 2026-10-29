// Managed by Intelligent Terminal: wt-agent-hooks
// OpenCode API variant: v1

import { clientContext, emit, eventMessage } from "./wt-agent-hooks/bridge.js"

export const WtAgentHooks = async ({ directory }) => {
  const context = clientContext()
  const roots = new Map()
  const children = new Set()
  let disposed = false
  const publish = (topic, id, payload = {}) =>
    emit(context, topic, id, { cwd: roots.get(id)?.cwd || directory, ...payload })
  const isRoot = (id) => !disposed && roots.has(id) && !children.has(id)

  function remember(info) {
    if (disposed || !info?.id) return
    if (info.parentID) {
      children.add(info.id)
      roots.delete(info.id)
      return
    }
    children.delete(info.id)
    const previous = roots.get(info.id)
    const session = {
      cwd: info.directory || previous?.cwd || directory,
      title: info.title || previous?.title || "",
    }
    roots.set(info.id, session)
    if (previous && session.cwd !== previous.cwd) {
      publish("agent.cwd.changed", info.id, { cwd: session.cwd })
    }
    if (!previous || (info.title && info.title !== previous.title)) {
      publish("agent.session.start", info.id, session)
    }
  }

  return {
    "chat.message": async ({ sessionID }) => {
      if (disposed || !sessionID || children.has(sessionID)) return
      if (!roots.has(sessionID)) remember({ id: sessionID })
      publish("agent.session.start", sessionID)
      publish("agent.prompt.submit", sessionID)
    },
    "tool.execute.before": async (input, output) => {
      if (!isRoot(input.sessionID)) return
      publish("agent.tool.starting", input.sessionID, {
        tool_name: input.tool,
        tool_input: output.args,
      })
    },
    event: async ({ event }) => {
      if (disposed) return
      const data = event.properties || {}
      const id = data.sessionID
      switch (event.type) {
        case "session.created":
        case "session.updated":
          remember(data.info)
          return
        case "session.deleted":
          if (isRoot(data.info?.id)) {
            publish("agent.session.end", data.info.id, { reason: "deleted" })
          }
          roots.delete(data.info?.id)
          children.delete(data.info?.id)
          return
      }
      if (!isRoot(id)) return
      switch (event.type) {
        case "session.status":
          if (data.status?.type === "idle") publish("agent.stop", id)
          else if (data.status?.type === "busy" || data.status?.type === "retry") {
            publish("agent.prompt.submit", id)
          }
          return
        case "session.idle":
          publish("agent.stop", id)
          return
        case "session.error":
          publish("agent.error", id, { error: eventMessage(data.error) || "OpenCode session error" })
          return
        case "permission.asked":
        case "question.asked":
          publish("agent.notification", id, {
            message: event.type === "permission.asked"
              ? `Permission required: ${data.permission || "tool use"}`
              : "OpenCode is waiting for input",
          })
          return
        case "permission.replied":
        case "question.replied":
          publish("agent.prompt.submit", id)
          return
      }
    },
    dispose: async () => {
      if (disposed) return
      for (const id of roots.keys()) {
        publish("agent.session.end", id, { reason: "OpenCode exited" })
      }
      disposed = true
      roots.clear()
      children.clear()
    },
  }
}

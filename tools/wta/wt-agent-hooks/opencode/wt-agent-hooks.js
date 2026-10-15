// Managed by Intelligent Terminal: wt-agent-hooks

import { appendFileSync, mkdirSync } from "node:fs"
import { bindingRpc } from "./wt-agent-hooks/rpc.js"

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
// because the handler was empty.
//
// Written straight to a file rather than reported through the bridge, because
// the thing being reported IS the bridge being unreachable — any channel that
// went through wtcli or the protocol server would fail for the same reason.
// Terminal already injects the directory, and the bug report tars that tree
// recursively.
function noteFailure(topic, error, operation = "bridge spawn") {
  try {
    const dir = process.env.WTA_HOOK_LOG_DIR
    if (!dir) return
    // The directory is created by whichever wta or Terminal component logs
    // first, so it usually exists — but "usually" is not good enough here.
    // The states where this note matters most are the degraded ones (wta never
    // started, blocked by policy), which are exactly the states where nothing
    // has created it yet, and `appendFileSync` fails with ENOENT straight into
    // the catch below. Recursive mkdir is idempotent, so pay it every time.
    mkdirSync(dir, { recursive: true })
    const message = error && error.message ? error.message : String(error)
    const line = `${new Date().toISOString()} opencode ${operation} failed topic=${topic} cmd=${bridgeCommand()} err=${message}\n`
    appendFileSync(`${dir}\\hook-trace.log`, line)
  } catch {
    // Diagnostics are best effort; never let them become the failure.
  }
}

function enabled() {
  return (
    process.platform === "win32" &&
    Boolean(process.env.WT_COM_CLSID) &&
    Boolean(process.env.WT_SESSION) &&
    process.env.OPENCODE_CLIENT !== "acp"
  )
}

function confirmedMissingSession(error, sessionID) {
  // The Promise plugin adapter can wrap typed lookup failures in FiberFailure.
  const cause = error?.[Symbol.for("effect/Runtime/FiberFailure/Cause")]
  const failure = cause?._tag === "Fail" ? cause.error ?? cause.failure : error
  return failure?._tag === "Session.NotFoundError" && failure.sessionID === sessionID
}

const WtAgentHooks = async ({ directory, resolveSession, ownsPane = true, deliver, onSession }) => {
  const rootSessions = new Map()
  const childSessions = new Set()
  const sessionStates = new Map()
  let disposed = false
  const active = ownsPane ? enabled() : true
  function emit(topic, sessionID, payload = {}) {
    if (!active || !sessionID) return

    if (deliver) {
      deliver(topic, sessionID, payload)
      return
    }
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
            cwd: rootSessions.get(sessionID)?.cwd || directory,
            ...payload,
            ...(ownsPane ? {} : { pane_owner: false }),
          }),
        ),
        stdout: "ignore",
        stderr: "ignore",
        windowsHide: true,
      })
      void child.exited.catch(() => {})
    } catch (error) {
      // Session tracking must never affect OpenCode's own execution.
      noteFailure(topic, error)
    }
  }

  function rememberSession(info) {
    if (disposed || !info?.id) return false
    sessionState(info.id)
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
    onSession?.(info.id, session, previous)
    if (ownsPane && previous && session.cwd !== previous.cwd) {
      emit("agent.cwd.changed", info.id, { cwd: session.cwd })
    }
    if (ownsPane && (!previous || (info.title && info.title !== previous.title))) {
      emit("agent.session.start", info.id, {
        cwd: session.cwd,
        title: session.title,
      })
    }
    return true
  }

  function isRootSession(sessionID) {
    return !disposed && rootSessions.has(sessionID) && !childSessions.has(sessionID)
  }

  function sessionState(sessionID) {
    let state = sessionStates.get(sessionID)
    if (!state) {
      state = {}
      sessionStates.set(sessionID, state)
    }
    return state
  }

  function isCurrentSession(sessionID, state) {
    return !disposed && state !== undefined && sessionStates.get(sessionID) === state
  }

  async function ensureSession(sessionID) {
    if (!active || disposed || !sessionID) return
    const state = sessionState(sessionID)
    if (!rootSessions.has(sessionID) && !childSessions.has(sessionID)) {
      // Share one lookup per lifecycle. Deletion removes its token, so a
      // delayed result cannot recreate a deleted or subsequently reused ID.
      state.lookup ??= (async () => {
        try {
          const info = await resolveSession(sessionID)
          if (isCurrentSession(sessionID, state)) {
            rememberSession({ ...info, directory: info.location?.directory })
          }
        } catch (error) {
          if (isCurrentSession(sessionID, state)) {
            state.missing = confirmedMissingSession(error, sessionID)
            if (!state.missing) noteFailure("session.get", error, "session lookup")
          }
        }
      })()
      await state.lookup
      if (!isCurrentSession(sessionID, state)) return
      if (!rootSessions.has(sessionID) && !childSessions.has(sessionID)) {
        sessionStates.delete(sessionID)
        return state
      }
    }
    return state
  }

  return {
    ensureRoot: async (sessionID) => {
      const state = await ensureSession(sessionID)
      return { root: isRootSession(sessionID), missing: state?.missing === true }
    },
    "chat.message": async (input) => {
      const sessionID = input.sessionID
      if (!sessionID) return
      if (disposed) return

      if (resolveSession && !isCurrentSession(sessionID, await ensureSession(sessionID))) return

      if (!resolveSession && !childSessions.has(sessionID) && !rootSessions.has(sessionID)) {
        rootSessions.set(sessionID, { cwd: directory, title: "" })
      }
      if (isRootSession(sessionID)) {
        // Rebind an existing OpenCode session when the user returns to it.
        if (ownsPane) {
          emit("agent.session.start", sessionID, {
            cwd: rootSessions.get(sessionID).cwd,
          })
        }
        emit("agent.prompt.submit", sessionID)
      }
    },

    "tool.execute.before": async (input, output) => {
      if (resolveSession && !isCurrentSession(input.sessionID, await ensureSession(input.sessionID))) return
      if (!isRootSession(input.sessionID)) return
      emit("agent.tool.starting", input.sessionID, {
        tool_name: input.tool,
        tool_input: output.args,
      })
    },

    event: async ({ event }) => {
      if (disposed) return
      const properties = event.properties || event.data || {}

      switch (event.type) {
        case "session.created":
        case "session.updated":
          rememberSession(properties.info || {
            id: properties.sessionID,
            parentID: properties.parentID,
            directory: properties.location?.directory,
            title: properties.title,
          })
          return
        case "session.renamed":
          if (isRootSession(properties.sessionID)) {
            rememberSession({ id: properties.sessionID, title: properties.title })
          }
          return
        case "session.moved":
          if (isRootSession(properties.sessionID) && properties.location?.directory) {
            rememberSession({ id: properties.sessionID, directory: properties.location.directory })
          }
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
        case "session.execution.succeeded":
        case "session.execution.interrupted":
          if (isRootSession(properties.sessionID)) {
            emit("agent.stop", properties.sessionID)
          }
          return
        case "session.error":
        case "session.execution.failed":
          if (properties.sessionID && isRootSession(properties.sessionID)) {
            emit("agent.error", properties.sessionID, {
              error: eventMessage(properties.error) || "OpenCode session error",
            })
          }
          return
        case "session.deleted": {
          const sessionID = properties.info?.id || properties.sessionID
          sessionStates.delete(sessionID)
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
                  ? `Permission required: ${properties.permission || properties.action || "tool use"}`
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
      if (disposed) return
      disposed = true
      sessionStates.clear()
      if (ownsPane) {
        for (const sessionID of rootSessions.keys()) {
          emit("agent.session.end", sessionID, { reason: "OpenCode exited" })
        }
      }
      rootSessions.clear()
      childSessions.clear()
    },
  }
}

// V1 calls server(); V2 loads id + setup(). Keep this static
// bundle dependency-free: Plugin.define() returns this same object shape.
export default {
  id: "wt-agent-hooks",
  server: WtAgentHooks,
  async setup(ctx) {
    if (process.env.OPENCODE_CLIENT === "acp") return

    const directory = ctx.location.directory
    const bindings = new Map()
    const clients = new Map()
    const observed = new Map()
    const snapshots = new Map()
    let revision = 0
    let delivery = Promise.resolve()
    let disposed = false
    let registration

    function enqueue(topic, sessionID, payload = {}) {
      const previous = snapshots.get(sessionID) ?? {
        revision: 0, status: "unknown", cwd: "", title: "", error: "", notification: "",
      }
      const next = { ...previous, revision: ++revision }
      switch (topic) {
        case "agent.session.renamed":
          next.title = payload.title
          break
        case "agent.cwd.changed":
          next.cwd = payload.cwd
          break
        case "agent.prompt.submit":
        case "agent.tool.starting":
          next.status = "working"
          next.error = ""
          next.notification = ""
          break
        case "agent.stop":
          next.status = "idle"
          next.error = ""
          next.notification = ""
          break
        case "agent.notification":
          next.status = "attention"
          next.notification = payload.message
          break
        case "agent.error":
          next.status = "error"
          next.error = payload.error
          break
        case "agent.session.end":
          next.status = "deleted"
          break
      }
      snapshots.set(sessionID, next)
      const owners = topic === "agent.session.end"
        ? [...observed.values()].flatMap((sessions) => {
          const owner = sessions.get(sessionID)
          return owner ? [owner] : []
        })
        : [...bindings.values()].filter((owner) => owner.sessionID === sessionID)
      delivery = delivery.then(async () => {
        for (const owner of owners) {
          await registration.events.emit("signal", {
            ...owner, revision: next.revision, topic, payload,
          })
        }
      }).catch((error) => noteFailure(topic, error, "RPC event"))
    }

    function remember(sessionID, session, previous) {
      if (previous?.cwd !== session.cwd) enqueue("agent.cwd.changed", sessionID, { cwd: session.cwd })
      if (session.title && previous?.title !== session.title) {
        enqueue("agent.session.renamed", sessionID, { title: session.title })
      }
    }

    const hooks = await WtAgentHooks({
      directory,
      resolveSession: (sessionID) => ctx.session.get({ sessionID }),
      ownsPane: false,
      deliver: enqueue,
      onSession: remember,
    })
    async function ensureRootOrReportDeletion(sessionID) {
      if (snapshots.get(sessionID)?.status === "deleted") return
      const lookup = await hooks.ensureRoot(sessionID)
      if (!lookup.root && snapshots.get(sessionID)?.status !== "deleted") {
        if (!lookup.missing) {
          throw new Error(`OpenCode root session not found: ${sessionID}`)
        }
        enqueue("agent.session.end", sessionID, { reason: "deleted" })
      }
    }
    function ownerKey(terminalID, sessionID) {
      return `${terminalID}\0${sessionID}`
    }
    function isOwner(input) {
      const owner = bindings.get(ownerKey(input.terminalID, input.sessionID))
      return owner?.clientID === input.clientID &&
        owner.paneID === input.paneID &&
        owner.generation === input.generation
    }
    function getSnapshot(input) {
      const previous = observed.get(input.clientID)?.get(input.sessionID)
      const subscribed = previous?.terminalID === input.terminalID &&
        previous?.paneID === input.paneID &&
        previous?.generation === input.generation
      if (disposed || (!isOwner(input) && !subscribed)) {
        throw new Error("OpenCode pane binding is no longer current")
      }
      return snapshots.get(input.sessionID)
    }
    registration = await ctx.rpc.register(bindingRpc, {
      bind: async (input) => {
        if (disposed) throw new Error("OpenCode hook is disposing")
        await ensureRootOrReportDeletion(input.sessionID)
        if (snapshots.get(input.sessionID)?.status === "deleted") return snapshots.get(input.sessionID)
        const key = ownerKey(input.terminalID, input.sessionID)
        const priorClient = clients.get(input.clientID)
        if (priorClient && priorClient.generation > input.generation) {
          throw new Error("Outdated OpenCode pane binding generation")
        }
        if (priorClient) {
          bindings.delete(ownerKey(priorClient.terminalID, priorClient.sessionID))
        }
        const previous = bindings.get(key)
        if (previous && previous.clientID !== input.clientID) {
          delivery = delivery.then(() => registration.events.emit("signal", {
            ...previous,
            revision: snapshots.get(input.sessionID)?.revision ?? 0,
            topic: "agent.session.unbind",
            payload: {},
          })).catch((error) => noteFailure("agent.session.unbind", error, "RPC event"))
          clients.delete(previous.clientID)
        }
        const owner = {
          clientID: input.clientID,
          terminalID: input.terminalID,
          paneID: input.paneID,
          sessionID: input.sessionID,
          generation: input.generation,
        }
        bindings.set(key, owner)
        clients.set(input.clientID, owner)
        if (!observed.has(input.clientID)) observed.set(input.clientID, new Map())
        observed.get(input.clientID).set(input.sessionID, owner)
        const snapshot = snapshots.get(input.sessionID)
        if (snapshot.status === "unknown") {
          snapshot.status = input.running ? "working" : "idle"
        }
        return { ...snapshot, cwd: snapshot.cwd || input.cwd, title: snapshot.title || input.title }
      },
      unbind: async (input) => {
        const detached = isOwner(input)
        if (detached) {
          bindings.delete(ownerKey(input.terminalID, input.sessionID))
          clients.delete(input.clientID)
        }
        return { detached }
      },
      snapshot: async (input) => getSnapshot(input),
      observe: async (input) => {
        if (disposed) throw new Error("OpenCode hook is disposing")
        await ensureRootOrReportDeletion(input.sessionID)
        if (!observed.has(input.clientID)) observed.set(input.clientID, new Map())
        const subscriptions = observed.get(input.clientID)
        if ((subscriptions.get(input.sessionID)?.generation ?? 0) > input.generation) {
          throw new Error("Outdated OpenCode session observer generation")
        }
        subscriptions.set(input.sessionID, input)
        return snapshots.get(input.sessionID)
      },
      forget: async ({ clientID }) => {
        const owner = clients.get(clientID)
        if (owner) bindings.delete(ownerKey(owner.terminalID, owner.sessionID))
        clients.delete(clientID)
        observed.delete(clientID)
        return {}
      },
    })
    await ctx.session.hook("prompt", hooks["chat.message"])
    await ctx.tool.hook("execute.before", (event) =>
      hooks["tool.execute.before"](event, { args: event.input }),
    )

    const controller = new AbortController()
    const events = (async () => {
      try {
        for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
          if (controller.signal.aborted) break
          // The public stream can include events from other locations. Each
          // plugin instance only owns sessions in its own location.
          if (event.location && (
            event.location.directory !== directory ||
            event.location.workspaceID !== ctx.location.workspaceID
          )) continue
          await hooks.event({ event })
        }
      } catch (error) {
        if (!controller.signal.aborted) noteFailure("event.subscribe", error, "event subscription")
      }
    })()

    return async () => {
      disposed = true
      controller.abort()
      await hooks.dispose()
      await events
      await delivery
      await registration.dispose()
      bindings.clear()
      clients.clear()
      observed.clear()
    }
  },
}

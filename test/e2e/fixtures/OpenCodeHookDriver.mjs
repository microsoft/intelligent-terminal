// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

import { createInterface } from "node:readline"
import { createContext, deferred, installNativeBridge, loadPlugin } from "./OpenCodeHookContext.mjs"

const [pluginPath, mode, directory, tuiPluginPath] = process.argv.slice(2)
if (!["v1", "v2"].includes(mode)) throw new Error("Expected v1 or v2 hook entrypoint")
if (mode === "v2" && !tuiPluginPath) throw new Error("V2 requires the packaged TUI plugin")
const flush = installNativeBridge()
const plugin = await loadPlugin(pluginPath)
const sessions = new Map()
const handlers = new Map()
let route = { type: "home" }
const tuiContext = {
  ui: { router: { current: () => route } },
  data: {
    session: {
      get: (id) => sessions.get(id),
      status: () => "idle",
    },
    on: (type, handler) => {
      handlers.set(type, handler)
      return () => handlers.delete(type)
    },
  },
}
let pendingLookup
let pendingID
const sdk = createContext({
  directory,
  getSession: async (id) => id === pendingID ? pendingLookup.promise : sessions.get(id) ?? { id, location: sdk.location },
})
tuiContext.client = sdk.client
tuiContext.location = sdk.location
const hooks = mode === "v1" ? await plugin.server({ directory }) : undefined
const serverDispose = mode === "v1" ? () => hooks.dispose() : await plugin.setup(sdk.context)
const tuiDispose = mode === "v2" ? (await loadPlugin(tuiPluginPath)).setup(tuiContext) : undefined
if (mode === "v2" && (typeof serverDispose !== "function" || typeof tuiDispose !== "function")) {
  throw new Error("V2 server and TUI hooks did not register")
}
let disposed = false
async function dispose() {
  if (disposed) return
  disposed = true
  await tuiDispose?.()
  await serverDispose()
}

async function event(type, data, location) {
  if (type === "session.created") {
    sessions.set(data.sessionID, { ...data, id: data.sessionID, location: location ?? sdk.location })
  }
  if (type === "session.renamed" && sessions.has(data.sessionID)) {
    sessions.get(data.sessionID).title = data.title
  }
  if (mode === "v2") await sdk.event(type, data, location)
  else {
    const properties = ["session.created", "session.updated"].includes(type)
      ? { info: { ...data, id: data.sessionID, directory } }
      : data
    await hooks.event({ event: { type, properties } })
  }
  handlers.get(type)?.({ data })
  if (type === "session.deleted") sessions.delete(data.sessionID)
}

const input = createInterface({ input: process.stdin })
console.log(JSON.stringify({ ready: true, mode }))
try {
  for await (const line of input) {
    const request = JSON.parse(line)
    switch (request.action) {
      case "event":
        await event(request.type, request.data, request.location)
        break
      case "view":
        if (mode !== "v2") throw new Error("Viewing sessions requires V2")
        route = { type: "session", sessionID: request.id }
        handlers.get("session.viewed")?.({ data: { sessionID: request.id } })
        break
      case "prompt":
        if (mode === "v2") await sdk.prompt(request.id)
        else await hooks["chat.message"]({ sessionID: request.id })
        break
      case "tool":
        if (mode === "v2") await sdk.tool(request.id, request.tool, request.args)
        else await hooks["tool.execute.before"]({ sessionID: request.id, tool: request.tool }, { args: request.args })
        break
      case "lookup-race": {
        if (mode !== "v2") throw new Error("Lookup race requires V2")
        pendingID = request.id
        pendingLookup = deferred()
        const prompt = sdk.prompt(pendingID)
        const tool = sdk.tool(pendingID, "bash", {})
        if (request.dispose) await dispose()
        else await event("session.deleted", { sessionID: pendingID })
        pendingLookup.resolve({ id: pendingID, location: sdk.location })
        await Promise.all([prompt, tool])
        break
      }
      case "dispose":
        await dispose()
        break
      default:
        throw new Error(`Unknown hook action: ${request.action}`)
    }
    await new Promise(setImmediate)
    await tuiDispose?.flush()
    await flush()
    console.log(JSON.stringify({ completed: request.token }))
  }
} finally {
  await dispose()
  await flush()
  input.close()
}

// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

import { createInterface } from "node:readline"
import { createContext, deferred, installNativeBridge, loadPlugin } from "./OpenCodeHookContext.mjs"

const [pluginPath, mode, directory] = process.argv.slice(2)
if (!["v1", "v2"].includes(mode)) throw new Error("Expected v1 or v2 hook entrypoint")
const flush = installNativeBridge()
const plugin = await loadPlugin(pluginPath)
const sessions = new Map()
let pendingLookup
let pendingID
const sdk = createContext({
  directory,
  getSession: async (id) => id === pendingID ? pendingLookup.promise : sessions.get(id) ?? { id, location: sdk.location },
})
const hooks = mode === "v1" ? await plugin.server({ directory }) : undefined
const dispose = mode === "v1" ? () => hooks.dispose() : await plugin.setup(sdk.context)
if (mode === "v2" && typeof dispose !== "function") throw new Error("V2 hooks did not register")

async function event(type, data, location) {
  if (type === "session.created") sessions.set(data.sessionID, { ...data, id: data.sessionID, location: sdk.location })
  if (mode === "v2") await sdk.event(type, data, location)
  else {
    const properties = ["session.created", "session.updated"].includes(type)
      ? { info: { ...data, id: data.sessionID, directory } }
      : data
    await hooks.event({ event: { type, properties } })
  }
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
    await flush()
    console.log(JSON.stringify({ completed: request.token }))
  }
} finally {
  await dispose()
  await flush()
  input.close()
}

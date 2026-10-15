// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

import { readFile } from "node:fs/promises"
import { spawn } from "node:child_process"
import { dirname, join } from "node:path"

export async function loadPlugin(path) {
  const rpcPath = path.endsWith("tui.js")
    ? join(dirname(path), "rpc.js")
    : join(dirname(path), "wt-agent-hooks", "rpc.js")
  const rpc = await readFile(rpcPath)
  const rpcURL = `data:text/javascript;base64,${rpc.toString("base64")}`
  let source = await readFile(path, "utf8")
  source = source.replace(/from "\.\/(?:wt-agent-hooks\/)?rpc\.js"/, `from "${rpcURL}"`)
  if (source.includes('from "solid-js"')) {
    const shim = [
      "let root;",
      "export function createRoot(callback) { const effects = new Set(); root = effects;",
      "  try { return callback(() => { for (const effect of effects) globalThis.__openCodeHookEffects.delete(effect); }); }",
      "  finally { root = undefined; } }",
      "export function createEffect(effect) { root.add(effect);",
      "  globalThis.__openCodeHookEffects ??= new Set(); globalThis.__openCodeHookEffects.add(effect);",
      "  queueMicrotask(effect); }",
    ].join("\n")
    const shimURL = `data:text/javascript;base64,${Buffer.from(shim).toString("base64")}`
    source = source.replace('from "solid-js"', `from "${shimURL}"`)
  }
  return (await import(`data:text/javascript;base64,${Buffer.from(source).toString("base64")}`)).default
}

export function deferred() {
  let resolve
  let reject
  const promise = new Promise((onResolve, onReject) => {
    resolve = onResolve
    reject = onReject
  })
  return { promise, resolve, reject }
}

export function createRpcLink(location = { directory: "C:\\ite2e\\opencode", workspaceID: "fixture-workspace" }) {
  let handlers
  let definition
  const listeners = new Set()
  const emitted = []
  let signalDelivery = true
  const publish = (event) => {
    for (const listener of listeners) listener(event)
  }
  const server = {
    async register(rpc, methods) {
      definition = rpc
      handlers = methods
      return {
        events: {
          async emit(name, data) {
            const event = { type: `rpc.${rpc.id}.${name}`, data, location }
            emitted.push(event)
            if (signalDelivery) publish(event)
          },
        },
        async dispose() {
          handlers = undefined
        },
      }
    },
  }
  const client = {
    rpc(rpc) {
      return Object.fromEntries(Object.keys(rpc.methods).map((name) => [
        name,
        async (input) => {
          if (!handlers) throw new Error(`RPC ${name} is not registered`)
          const schema = rpc.methods[name].input
          const missing = schema.required.filter((key) => !(key in input))
          const unexpected = Object.keys(input).filter((key) => !(key in schema.properties))
          if (missing.length || (schema.additionalProperties === false && unexpected.length)) {
            throw new Error(`Invalid RPC ${name} input: missing ${missing.join(",")}, unexpected ${unexpected.join(",")}`)
          }
          return handlers[name](input)
        },
      ]))
    },
    event: {
      subscribe({ signal } = {}) {
        return {
          async *[Symbol.asyncIterator]() {
            const queue = [{ type: "server.connected", data: {} }]
            let wake
            const listen = (event) => { queue.push(event); wake?.() }
            const abort = () => wake?.()
            listeners.add(listen)
            signal?.addEventListener("abort", abort)
            try {
              while (!signal?.aborted) {
                if (!queue.length) await new Promise((resolve) => { wake = resolve })
                if (signal?.aborted) return
                const event = queue.shift()
                if (event) yield event
              }
            } finally {
              listeners.delete(listen)
              signal?.removeEventListener("abort", abort)
            }
          },
        }
      },
    },
  }
  return {
    server, client, emitted, publish,
    setSignalDelivery(enabled) { signalDelivery = enabled },
    get definition() { return definition },
    get listeners() { return listeners.size },
  }
}

export function createContext({
  directory = "C:\\ite2e\\opencode",
  workspaceID = "fixture-workspace",
  getSession = async (sessionID) => ({ id: sessionID, location: { directory } }),
  rpc = createRpcLink({ directory, workspaceID }),
} = {}) {
  const promptHooks = new Map()
  const toolHooks = new Map()
  const queue = []
  let wake
  let signal
  let lookupCount = 0
  const location = { directory, workspaceID }

  const context = {
    location,
    rpc: rpc.server,
    session: {
      async get({ sessionID }) {
        lookupCount++
        return getSession(sessionID)
      },
      async hook(name, callback) { promptHooks.set(name, callback) },
    },
    tool: {
      async hook(name, callback) { toolHooks.set(name, callback) },
    },
    event: {
      async *subscribe(options) {
        signal = options.signal
        const abort = () => wake?.()
        signal.addEventListener("abort", abort)
        try {
          while (!signal.aborted) {
            if (!queue.length) await new Promise((resolve) => { wake = resolve })
            if (signal.aborted) return
            const item = queue.shift()
            if (!item) continue
            yield item.event
            item.handled.resolve()
          }
        } finally {
          signal.removeEventListener("abort", abort)
        }
      },
    },
  }

  return {
    context,
    location,
    prompt: (sessionID) => promptHooks.get("prompt")({ sessionID }),
    tool: (sessionID, tool, input) => toolHooks.get("execute.before")({ sessionID, tool, input }),
    async event(type, data, eventLocation = location) {
      const handled = deferred()
      queue.push({ event: { type, data, location: eventLocation }, handled })
      wake?.()
      await handled.promise
    },
    get lookupCount() { return lookupCount },
    get aborted() { return signal?.aborted },
    get registered() { return promptHooks.size + toolHooks.size },
    client: rpc.client,
    rpc,
  }
}

export function installNativeBridge() {
  const pending = []
  globalThis.Bun = {
    spawn(options) {
      const child = spawn(options.cmd[0], options.cmd.slice(1), {
        stdio: ["pipe", "ignore", "ignore"],
        windowsHide: options.windowsHide,
      })
      const exited = new Promise((resolve, reject) => {
        child.once("error", reject)
        child.stdin.once("error", reject)
        child.once("exit", (code, signal) => {
          if (code === 0) resolve(code)
          else reject(new Error(`hook bridge exited with code=${code}, signal=${signal}`))
        })
      })
      child.stdin.end(options.stdin)
      pending.push(exited)
      return { exited }
    },
  }
  return async () => {
    const results = await Promise.allSettled(pending)
    const failures = results.filter((result) => result.status === "rejected")
    if (failures.length) throw new AggregateError(failures.map((result) => result.reason), "Native hook delivery failed")
  }
}

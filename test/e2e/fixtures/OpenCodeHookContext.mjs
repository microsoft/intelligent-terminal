// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

import { readFile } from "node:fs/promises"
import { spawn } from "node:child_process"

export async function loadPlugin(path) {
  const source = await readFile(path)
  return (await import(`data:text/javascript;base64,${source.toString("base64")}`)).default
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

export function createContext({
  directory = "C:\\ite2e\\opencode",
  workspaceID = "fixture-workspace",
  getSession = async (sessionID) => ({ id: sessionID, location: { directory } }),
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

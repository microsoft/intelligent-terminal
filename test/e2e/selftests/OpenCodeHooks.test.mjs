// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

import assert from "node:assert/strict"
import { afterEach, beforeEach, describe, it } from "node:test"
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { spawn } from "node:child_process"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { createInterface } from "node:readline"
import { createContext, deferred, loadPlugin } from "../fixtures/OpenCodeHookContext.mjs"

const pluginPath = process.env.ITE2E_OPENCODE_PLUGIN ??
  fileURLToPath(new URL("../../../tools/wta/wt-agent-hooks/opencode/v2/wt-agent-hooks.js", import.meta.url))
const plugin = await loadPlugin(pluginPath)
const v1Plugin = await loadPlugin(fileURLToPath(new URL(
  "../../../tools/wta/wt-agent-hooks/opencode/v1/wt-agent-hooks.js", import.meta.url,
)))
const tuiPluginPath = fileURLToPath(new URL(
  "../../../tools/wta/wt-agent-hooks/opencode/v2/tui.js", import.meta.url,
))
const tuiPlugin = await loadPlugin(tuiPluginPath)
const bridgeSource = await readFile(fileURLToPath(new URL(
  "../../../tools/wta/wt-agent-hooks/opencode/shared/bridge.js", import.meta.url,
)))
const { tryWriteHookVt } = await import(`data:text/javascript;base64,${bridgeSource.toString("base64")}`)
const environmentKeys = ["WT_COM_CLSID", "WT_SESSION", "WTCLI_PATH", "WT_AGENT_HOOK_DIR", "OPENCODE_CLIENT", "WTA_HOOK_LOG_DIR"]
let originalEnvironment
let originalBun
let calls

describe("OpenCode bundled hook behavior", { skip: process.platform !== "win32" }, () => {
  beforeEach(() => {
    originalEnvironment = Object.fromEntries(environmentKeys.map((key) => [key, process.env[key]]))
    originalBun = globalThis.Bun
    process.env.WT_COM_CLSID = "{00000000-0000-0000-0000-000000000001}"
    process.env.WT_SESSION = "00000000-0000-0000-0000-000000000002"
    process.env.WTCLI_PATH = "C:\\fixture with spaces\\wtcli.exe"
    delete process.env.OPENCODE_CLIENT
    delete process.env.WTA_HOOK_LOG_DIR
    delete process.env.WT_AGENT_HOOK_DIR
    calls = []
    globalThis.Bun = {
      spawn(options) {
        calls.push({ topic: options.cmd.at(-1), payload: JSON.parse(new TextDecoder().decode(options.stdin)), options })
        return { exited: Promise.resolve(0) }
      },
    }
  })

  it("writes encoded events in process and falls back when the owning console is unavailable", () => {
    process.env.WT_AGENT_HOOK_DIR = "C:\\fixture with spaces\\wt-agent-hooks"
    const payload = { session_id: "root", message: "question\x1b]9;4;2\x07" }
    let sequence
    assert.equal(tryWriteHookVt("agent.notification", "root", payload, (path, value, options) => {
      assert.equal(path, "CONOUT$")
      assert.equal(options.flag, "r+")
      sequence = value
    }), true)
    const match = /^\x1b\]9001;AgentEvent64;([A-Za-z0-9+/=]+)\x07$/.exec(sequence)
    assert.ok(match, "payload control bytes must not escape the OSC envelope")
    assert.deepEqual(JSON.parse(Buffer.from(match[1], "base64").toString("utf8")), {
      event: "agent.notification", cli_source: "opencode", agent_session_id: "root", payload,
    })
    assert.equal(tryWriteHookVt("agent.notification", "root", payload, () => {
      throw Object.assign(new Error("no controlling console"), { code: "ENXIO" })
    }), false)
  })

  it("relays server-owned status through a bound local TUI without server Terminal identity", async () => {
    delete process.env.WT_SESSION
    delete process.env.WT_COM_CLSID
    const sdk = createContext()
    const stopServer = await plugin.setup(sdk.context)
    assert.equal(typeof stopServer, "function")
    process.env.WT_COM_CLSID = "{00000000-0000-0000-0000-000000000001}"
    process.env.WT_SESSION = "00000000-0000-0000-0000-000000000002"
    const session = { id: "root", title: "Original", location: sdk.location }
    const handlers = new Map()
    const tui = {
      location: sdk.location,
      client: sdk.client,
      ui: { router: { current: () => ({ type: "session", sessionID: "root" }) } },
      data: {
        session: {
          get: () => session,
          status: () => "running",
        },
        on: (name, handler) => {
          handlers.set(name, handler)
          return () => handlers.delete(name)
        },
      },
    }
    await sdk.event("session.created", { sessionID: "root", title: "Original" })
    const stopTui = tuiPlugin.setup(tui)
    try {
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.deepEqual(calls.map(({ topic }) => topic), ["agent.session.start"])
      assert.equal(calls[0].payload.running, true)
      assert.equal([...handlers.keys()].some((name) => name.startsWith("session.execution.")), false)
      await sdk.event("session.status", { sessionID: "root", status: { type: "idle" } })
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.equal(calls.at(-1).topic, "agent.stop")
      assert.equal(calls.at(-1).payload.pane_owner, false)
      await sdk.event("session.renamed", { sessionID: "root", title: "Updated" })
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.equal(calls.at(-1).topic, "agent.session.renamed")
      await sdk.event("session.deleted", { sessionID: "root" })
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.deepEqual(calls.slice(-2).map(({ topic }) => topic), [
        "agent.session.end", "agent.session.unbind",
      ])
      assert.equal(calls.at(-2).payload.pane_owner, false)
    } finally {
      await stopTui()
      await stopServer()
    }
    assert.equal(handlers.size, 0)
  })

  it("reconciles missed server events and detaches a pane without deleting its server session", async () => {
    const sdk = createContext()
    const stopServer = await plugin.setup(sdk.context)
    const handlers = new Map()
    const session = { id: "root", title: "Original", location: sdk.location }
    let route = { type: "session", sessionID: "root" }
    const tui = {
      location: sdk.location,
      client: sdk.client,
      ui: { router: { current: () => route } },
      data: {
        session: { get: () => session, status: () => "idle" },
        on: (name, handler) => {
          handlers.set(name, handler)
          return () => handlers.delete(name)
        },
      },
    }
    await sdk.event("session.created", { sessionID: "root", title: "Original" })
    const stopTui = tuiPlugin.setup(tui)
    try {
      await new Promise(setImmediate)
      await stopTui.flush()
      sdk.rpc.setSignalDelivery(false)
      await sdk.event("session.status", { sessionID: "root", status: { type: "busy" } })
      await sdk.event("session.renamed", { sessionID: "root", title: "Updated" })
      await new Promise(setImmediate)
      assert.equal(calls.some((call) => call.topic === "agent.prompt.submit"), false)
      sdk.rpc.publish({ type: "server.connected", data: {} })
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.equal(calls.at(-1).topic, "agent.session.start")
      assert.equal(calls.at(-1).payload.running, true)
      assert.equal(calls.at(-1).payload.title, "Updated")
      route = { type: "home" }
      handlers.get("session.viewed")({ data: {} })
      await stopTui.flush()
      assert.equal(calls.at(-1).topic, "agent.session.unbind")
      assert.equal(calls.some((call) => call.topic === "agent.session.end"), false)
      const owner = {
        clientID: "second", terminalID: "terminal", paneID: "pane",
        generation: 1, sessionID: "root", cwd: sdk.location.directory, title: "", running: false,
      }
      assert.equal((await sdk.client.rpc(sdk.rpc.definition).bind(owner)).status, "working")
    } finally {
      await stopTui()
      await stopServer()
    }
    assert.equal(handlers.size, 0)
    assert.equal(sdk.rpc.listeners, 0)
  })

  it("reconciles a deletion missed while disconnected instead of leaving a stale pane binding", async () => {
    const sdk = createContext()
    const stopServer = await plugin.setup(sdk.context)
    const session = { id: "root", location: sdk.location }
    const tui = {
      location: sdk.location,
      client: sdk.client,
      ui: { router: { current: () => ({ type: "session", sessionID: "root" }) } },
      data: {
        session: { get: () => session, status: () => "idle" },
        on: () => () => {},
      },
    }
    await sdk.event("session.created", { sessionID: "root" })
    const stopTui = tuiPlugin.setup(tui)
    try {
      await new Promise(setImmediate)
      await stopTui.flush()
      sdk.rpc.setSignalDelivery(false)
      await sdk.event("session.deleted", { sessionID: "root" })
      await new Promise(setImmediate)
      assert.equal(calls.some((call) => call.topic === "agent.session.end"), false)
      sdk.rpc.publish({ type: "server.connected", data: {} })
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.deepEqual(calls.slice(-2).map((call) => call.topic), [
        "agent.session.end", "agent.session.unbind",
      ])
    } finally {
      await stopTui()
      await stopServer()
    }
  })

  it("replays an authoritative idle snapshot after a missed stop during binding recovery", async () => {
    const sdk = createContext()
    const stopServer = await plugin.setup(sdk.context)
    const session = { id: "root", location: sdk.location }
    const tui = {
      location: sdk.location,
      client: sdk.client,
      ui: { router: { current: () => ({ type: "session", sessionID: "root" }) } },
      data: {
        session: { get: () => session, status: () => "running" },
        on: () => () => {},
      },
    }
    await sdk.event("session.created", { sessionID: "root" })
    const stopTui = tuiPlugin.setup(tui)
    try {
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.deepEqual(calls.map((call) => call.topic), ["agent.session.start"])
      sdk.rpc.setSignalDelivery(false)
      await sdk.event("session.status", { sessionID: "root", status: { type: "idle" } })
      sdk.rpc.publish({ type: "server.connected", data: {} })
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.deepEqual(calls.slice(-2).map((call) => call.topic), [
        "agent.session.start", "agent.stop",
      ])
      assert.equal(calls.at(-2).payload.running, false)
      assert.equal(calls.at(-1).payload.pane_owner, false)
    } finally {
      await stopTui()
      await stopServer()
    }
  })

  for (const returnMode of ["same TUI", "new TUI"]) {
    it(`replays idle completion after returning from home in the ${returnMode}`, async () => {
      const sdk = createContext()
      const stopServer = await plugin.setup(sdk.context)
      const session = { id: "root", location: sdk.location }
      const clients = []
      function openTui() {
        const handlers = new Map()
        let route = { type: "session", sessionID: "root" }
        const stop = tuiPlugin.setup({
          location: sdk.location,
          client: sdk.client,
          ui: { router: { current: () => route } },
          data: {
            session: { get: () => session, status: () => "running" },
            on: (name, handler) => {
              handlers.set(name, handler)
              return () => handlers.delete(name)
            },
          },
        })
        const client = {
          stop,
          view(next) {
            route = next
            handlers.get("session.viewed")({ data: next })
          },
        }
        clients.push(client)
        return client
      }
      await sdk.prompt("root")
      const first = openTui()
      try {
        await new Promise(setImmediate)
        await first.stop.flush()
        assert.equal(calls.at(-1).topic, "agent.session.start")
        assert.equal(calls.at(-1).payload.running, true)
        assert.equal(calls.some((call) => call.topic === "agent.stop"), false)
        first.view({ type: "home" })
        await first.stop.flush()
        assert.equal(calls.at(-1).topic, "agent.session.unbind")
        const detached = calls.length
        await sdk.event("session.execution.succeeded", { sessionID: "root" })
        await new Promise(setImmediate)
        assert.equal(calls.length, detached)
        let returned = first
        if (returnMode === "new TUI") {
          await first.stop()
          process.env.WT_SESSION = "00000000-0000-0000-0000-000000000003"
          returned = openTui()
        } else {
          first.view({ type: "session", sessionID: "root" })
        }
        await new Promise(setImmediate)
        await returned.stop.flush()
        assert.deepEqual(calls.slice(detached).map((call) => call.topic), [
          "agent.session.start", "agent.stop",
        ])
        assert.equal(calls.at(-2).payload.running, false)
        assert.equal(calls.at(-1).payload.session_id, "root")
        assert.equal(calls.at(-1).payload.pane_owner, false)
        assert.equal(calls.some((call) => call.topic === "agent.session.end"), false)
      } finally {
        for (const client of clients) await client.stop()
        await stopServer()
      }
    })
  }

  it("preserves newer work buffered while an idle binding is being reported", async () => {
    const sdk = createContext()
    const stopServer = await plugin.setup(sdk.context)
    const session = { id: "root", location: sdk.location }
    const entered = deferred()
    const delivered = deferred()
    const spawn = globalThis.Bun.spawn
    globalThis.Bun.spawn = (options) => {
      const child = spawn(options)
      if (options.cmd.at(-1) !== "agent.session.start") return child
      entered.resolve()
      return { exited: delivered.promise }
    }
    const stopTui = tuiPlugin.setup({
      location: sdk.location,
      client: sdk.client,
      ui: { router: { current: () => ({ type: "session", sessionID: "root" }) } },
      data: {
        session: { get: () => session, status: () => "idle" },
        on: () => () => {},
      },
    })
    try {
      await entered.promise
      await sdk.prompt("root")
      await new Promise(setImmediate)
      delivered.resolve(0)
      await stopTui.flush()
      await new Promise(setImmediate)
      await stopTui.flush()
      assert.deepEqual(calls.map((call) => call.topic), [
        "agent.session.start", "agent.stop", "agent.prompt.submit",
      ])
      assert.equal(calls.at(-1).payload.session_id, "root")
    } finally {
      delivered.resolve(0)
      await stopTui()
      await stopServer()
    }
  })

  for (const nextRoute of ["same session", "home"]) {
    it(`reconciles an interrupted reconnect bind after switching to ${nextRoute}`, async (t) => {
      const errors = []
      t.mock.method(console, "error", (...args) => errors.push(String(args[0])))
      let refresh
      const originalSetInterval = globalThis.setInterval
      globalThis.setInterval = (callback, delay) => {
        if (delay === 30000) refresh = callback
        return originalSetInterval(callback, delay)
      }
      t.after(() => { globalThis.setInterval = originalSetInterval })
      const sdk = createContext()
      const stopServer = await plugin.setup(sdk.context)
      const waiting = deferred()
      const resume = deferred()
      let bindCount = 0
      const client = {
        ...sdk.client,
        rpc: (definition) => {
          const rpc = sdk.client.rpc(definition)
          return {
            ...rpc,
            bind: async (...args) => {
              const snapshot = await rpc.bind(...args)
              if (++bindCount === 2) {
                waiting.resolve()
                await resume.promise
              }
              return snapshot
            },
          }
        },
      }
      const handlers = new Map()
      const session = { id: "root", title: "Original", location: sdk.location }
      let route = { type: "session", sessionID: "root" }
      const tui = {
        location: sdk.location,
        client,
        ui: { router: { current: () => route } },
        data: {
          session: { get: () => session, status: () => "idle" },
          on: (name, handler) => {
            handlers.set(name, handler)
            return () => handlers.delete(name)
          },
        },
      }
      await sdk.event("session.created", { sessionID: "root" })
      const stopTui = tuiPlugin.setup(tui)
      try {
        await new Promise(setImmediate)
        await stopTui.flush()
        assert.equal(calls.filter((call) => call.topic === "agent.session.start").length, 1)
        sdk.rpc.publish({ type: "server.connected", data: {} })
        await waiting.promise
        if (nextRoute === "home") {
          route = { type: "home" }
          handlers.get("session.viewed")({ data: {} })
        } else {
          session.title = "Updated"
          handlers.get("session.created")({ data: { sessionID: "root" } })
        }
        resume.resolve()
        await new Promise(setImmediate)
        await stopTui.flush()
        refresh()
        await stopTui.flush()
        if (nextRoute === "home") {
          assert.equal(calls.filter((call) => call.topic === "agent.session.unbind").length, 1)
          await sdk.event("session.deleted", { sessionID: "root" })
          await new Promise(setImmediate)
          await stopTui.flush()
          assert.equal(calls.at(-1).topic, "agent.session.end")
        } else {
          assert.equal(calls.filter((call) => call.topic === "agent.session.start").length, 2)
          assert.equal(calls.findLast((call) => call.topic === "agent.session.start").payload.title, "Updated")
        }
        assert.deepEqual(errors, [])
      } finally {
        resume.resolve()
        await stopTui()
        await stopServer()
      }
    })
  }

  it("polls snapshots with identity-only RPC input after the bound title changes", async (t) => {
    let refresh
    const originalSetInterval = globalThis.setInterval
    globalThis.setInterval = (callback, delay) => {
      if (delay === 30000) refresh = callback
      return originalSetInterval(callback, delay)
    }
    t.after(() => { globalThis.setInterval = originalSetInterval })
    const sdk = createContext()
    const stopServer = await plugin.setup(sdk.context)
    const session = { id: "root", title: "Original", location: sdk.location }
    const tui = {
      location: sdk.location,
      client: sdk.client,
      ui: { router: { current: () => ({ type: "session", sessionID: "root" }) } },
      data: {
        session: { get: () => session, status: () => "running" },
        on: () => () => {},
      },
    }
    await sdk.event("session.created", { sessionID: "root", title: "Original" })
    const stopTui = tuiPlugin.setup(tui)
    try {
      await new Promise(setImmediate)
      await stopTui.flush()
      await sdk.event("session.renamed", { sessionID: "root", title: "Updated" })
      await new Promise(setImmediate)
      await stopTui.flush()
      sdk.rpc.setSignalDelivery(false)
      await sdk.event("session.status", { sessionID: "root", status: { type: "idle" } })
      refresh()
      await stopTui.flush()
      assert.equal(calls.at(-1).topic, "agent.stop")
    } finally {
      await stopTui()
      await stopServer()
    }
  })

  for (const missed of [false, true]) {
    it(`relays deletion after pane detachment${missed ? " even when SSE is missed" : ""}`, async () => {
      const sdk = createContext()
      const stopServer = await plugin.setup(sdk.context)
      const handlers = new Map()
      const session = { id: "root", location: sdk.location }
      let route = { type: "session", sessionID: "root" }
      const tui = {
        location: sdk.location,
        client: sdk.client,
        ui: { router: { current: () => route } },
        data: {
          session: { get: () => session, status: () => "idle" },
          on: (name, handler) => {
            handlers.set(name, handler)
            return () => handlers.delete(name)
          },
        },
      }
      await sdk.event("session.created", { sessionID: "root" })
      const stopTui = tuiPlugin.setup(tui)
      try {
        await new Promise(setImmediate)
        await stopTui.flush()
        route = { type: "home" }
        handlers.get("session.viewed")({ data: {} })
        await stopTui.flush()
        assert.equal(calls.at(-1).topic, "agent.session.unbind")
        if (missed) sdk.rpc.setSignalDelivery(false)
        await sdk.event("session.deleted", { sessionID: "root" })
        if (missed) sdk.rpc.publish({ type: "server.connected", data: {} })
        await new Promise(setImmediate)
        await stopTui.flush()
        assert.equal(calls.at(-1).topic, "agent.session.end")
        assert.equal(calls.at(-1).payload.pane_owner, false)
        assert.equal(calls.filter((call) => call.topic === "agent.session.end").length, 1)
      } finally {
        await stopTui()
        await stopServer()
      }
    })
  }

  for (const scenario of ["active", "deleted during downtime", "transient lookup"]) {
    it(`restores a detached deletion observer after restart with ${scenario}`, async (t) => {
      const errors = []
      if (scenario === "transient lookup") {
        t.mock.method(console, "error", (...args) => errors.push(String(args[0])))
      }
      const directory = "C:\\ite2e\\opencode"
      let lookup = "present"
      const sdk = createContext({
        directory,
        getSession: async (sessionID) => {
          if (lookup === "transient") throw new Error("session storage temporarily unavailable")
          if (lookup === "deleted") {
            const missing = Object.assign(new Error(`Session not found: ${sessionID}`), {
              _tag: "Session.NotFoundError", sessionID,
            })
            if (scenario === "deleted during downtime") {
              throw Object.assign(new Error("wrapped missing session"), {
                [Symbol.for("effect/Runtime/FiberFailure/Cause")]: { _tag: "Fail", failure: missing },
              })
            }
            throw missing
          }
          return { id: sessionID, location: { directory } }
        },
      })
      let stopServer = await plugin.setup(sdk.context)
      const handlers = new Map()
      const session = { id: "root", location: sdk.location }
      let route = { type: "session", sessionID: "root" }
      const tui = {
        location: sdk.location,
        client: sdk.client,
        ui: { router: { current: () => route } },
        data: {
          session: { get: () => session, status: () => "idle" },
          on: (name, handler) => {
            handlers.set(name, handler)
            return () => handlers.delete(name)
          },
        },
      }
      await sdk.event("session.created", { sessionID: "root" })
      const stopTui = tuiPlugin.setup(tui)
      try {
        await new Promise(setImmediate)
        await stopTui.flush()
        route = { type: "home" }
        handlers.get("session.viewed")({ data: {} })
        await stopTui.flush()
        await stopServer()
        if (scenario === "deleted during downtime") lookup = "deleted"
        if (scenario === "transient lookup") lookup = "transient"
        stopServer = await plugin.setup(sdk.context)
        sdk.rpc.publish({ type: "server.connected", data: {} })
        await new Promise(setImmediate)
        await stopTui.flush()
        if (scenario === "transient lookup") {
          assert.equal(calls.some((call) => call.topic === "agent.session.end"), false)
          assert.ok(errors.some((message) => message.includes("detached session lookup failed")))
          lookup = "deleted"
          sdk.rpc.publish({ type: "server.connected", data: {} })
        } else if (scenario === "active") {
          await sdk.event("session.deleted", { sessionID: "root" })
        }
        await new Promise(setImmediate)
        await stopTui.flush()
        assert.equal(calls.at(-1).topic, "agent.session.end")
        assert.equal(calls.at(-1).payload.pane_owner, false)
        assert.equal(calls.filter((call) => call.topic === "agent.session.end").length, 1)
      } finally {
        await stopTui()
        await stopServer()
      }
    })
  }

  for (const lookupFailure of ["confirmed missing", "transient then missing"]) {
    it(`ends a still-viewed session after server downtime with ${lookupFailure}`, async (t) => {
      const errors = []
      if (lookupFailure === "transient then missing") {
        t.mock.method(console, "error", (...args) => errors.push(String(args[0])))
      }
      let lookup = "present"
      const sdk = createContext({
        getSession: async (sessionID) => {
          if (lookup === "transient") throw new Error("session lookup temporarily failed")
          if (lookup === "missing") {
            throw Object.assign(new Error("session not found"), {
              _tag: "Session.NotFoundError", sessionID,
            })
          }
          return { id: sessionID, location: sdk.location }
        },
      })
      let stopServer = await plugin.setup(sdk.context)
      const session = { id: "root", location: sdk.location }
      const tui = {
        location: sdk.location,
        client: sdk.client,
        ui: { router: { current: () => ({ type: "session", sessionID: "root" }) } },
        data: {
          session: { get: () => session, status: () => "idle" },
          on: () => () => {},
        },
      }
      await sdk.event("session.created", { sessionID: "root" })
      const stopTui = tuiPlugin.setup(tui)
      try {
        await new Promise(setImmediate)
        await stopTui.flush()
        await stopServer()
        lookup = lookupFailure === "confirmed missing" ? "missing" : "transient"
        stopServer = await plugin.setup(sdk.context)
        sdk.rpc.publish({ type: "server.connected", data: {} })
        await new Promise(setImmediate)
        await stopTui.flush()
        if (lookup === "transient") {
          assert.equal(calls.some((call) => call.topic === "agent.session.end"), false)
          assert.ok(errors.some((message) => message.includes("pane sync failed")))
          lookup = "missing"
          for (let retry = 0; retry < 60 && !calls.some((call) => call.topic === "agent.session.end"); retry++) {
            await new Promise((resolve) => setTimeout(resolve, 50))
          }
          await stopTui.flush()
        }
        assert.deepEqual(calls.map(({ topic }) => topic), [
          "agent.session.start", "agent.stop", "agent.session.end", "agent.session.unbind",
        ])
        assert.equal(calls.find((call) => call.topic === "agent.session.end").payload.pane_owner, false)
      } finally {
        await stopTui()
        await stopServer()
      }
    })
  }

  it("does not let an evicted TUI reclaim a shared session until it is explicitly viewed", async (t) => {
    const refreshes = []
    const originalSetInterval = globalThis.setInterval
    globalThis.setInterval = (callback, delay) => {
      if (delay === 30000) refreshes.push(callback)
      return originalSetInterval(callback, delay)
    }
    t.after(() => { globalThis.setInterval = originalSetInterval })
    const sdk = createContext()
    const stopServer = await plugin.setup(sdk.context)
    const session = { id: "root", location: sdk.location }
    function localTui() {
      const handlers = new Map()
      return {
        handlers,
        context: {
          location: sdk.location,
          client: sdk.client,
          ui: { router: { current: () => ({ type: "session", sessionID: "root" }) } },
          data: {
            session: { get: () => session, status: () => "idle" },
            on: (name, handler) => {
              handlers.set(name, handler)
              return () => handlers.delete(name)
            },
          },
        },
      }
    }
    await sdk.event("session.created", { sessionID: "root" })
    const first = localTui()
    const stopFirst = tuiPlugin.setup(first.context)
    let stopSecond
    try {
      await new Promise(setImmediate)
      await stopFirst.flush()
      const second = localTui()
      stopSecond = tuiPlugin.setup(second.context)
      await new Promise(setImmediate)
      await Promise.all([stopFirst.flush(), stopSecond.flush()])
      assert.equal(calls.filter((call) => call.topic === "agent.session.start").length, 2)
      assert.equal(calls.filter((call) => call.topic === "agent.session.unbind").length, 1)
      refreshes[0]()
      first.handlers.get("session.created")({ data: { sessionID: "root" } })
      sdk.rpc.publish({ type: "server.connected", data: {} })
      await new Promise(setImmediate)
      await Promise.all([stopFirst.flush(), stopSecond.flush()])
      assert.equal(calls.filter((call) => call.topic === "agent.session.start").length, 3)
      first.handlers.get("session.viewed")({ data: { sessionID: "root" } })
      await new Promise(setImmediate)
      await Promise.all([stopFirst.flush(), stopSecond.flush()])
      assert.equal(calls.filter((call) => call.topic === "agent.session.start").length, 4)
      refreshes[1]()
      await stopSecond.flush()
      assert.equal(calls.filter((call) => call.topic === "agent.session.start").length, 4)
    } finally {
      await stopSecond?.()
      await stopFirst()
      await stopServer()
    }
  })

  it("does not install TUI hooks outside a Terminal pane or in ACP mode", () => {
    const context = { data: { on: () => { throw new Error("unexpected TUI hook") } } }
    delete process.env.WT_SESSION
    assert.equal(tuiPlugin.setup(context), undefined)
    process.env.WT_SESSION = "00000000-0000-0000-0000-000000000002"
    process.env.OPENCODE_CLIENT = "acp"
    assert.equal(tuiPlugin.setup(context), undefined)
  })

  afterEach(() => {
    for (const key of environmentKeys) {
      if (originalEnvironment[key] === undefined) delete process.env[key]
      else process.env[key] = originalEnvironment[key]
    }
    if (originalBun === undefined) delete globalThis.Bun
    else globalThis.Bun = originalBun
  })

  it("V1 root lifecycle retains source, payloads, child suppression and idempotent disposal", async () => {
    const hooks = await v1Plugin({ directory: "C:\\root" })
    await hooks.event({ event: { type: "session.created", properties: { info: { id: "root", directory: "C:\\root", title: "Original" } } } })
    await hooks.event({ event: { type: "session.created", properties: { info: { id: "child", parentID: "root" } } } })
    await hooks["chat.message"]({ sessionID: "child" })
    await hooks["tool.execute.before"]({ sessionID: "child", tool: "bash" }, { args: {} })
    assert.equal(calls.length, 1)
    await hooks["chat.message"]({ sessionID: "root" })
    await hooks["tool.execute.before"]({ sessionID: "root", tool: "question" }, { args: { question: "Approve?" } })
    await hooks.event({ event: { type: "session.status", properties: { sessionID: "root", status: { type: "idle" } } } })
    assert.deepEqual(calls.map((call) => call.topic), [
      "agent.session.start", "agent.session.start", "agent.prompt.submit", "agent.tool.starting", "agent.stop",
    ])
    assert.deepEqual(calls[3].payload, { session_id: "root", cwd: "C:\\root", tool_name: "question", tool_input: { question: "Approve?" } })
    assert.deepEqual(calls[0].options.cmd, [process.env.WTCLI_PATH, "agent-hook", "--cli-source", "opencode", "--event", "agent.session.start"])
    assert.equal(calls[0].options.windowsHide, true)
    await hooks.dispose()
    await hooks.dispose()
    assert.equal(calls.filter((call) => call.topic === "agent.session.end").length, 1)
    await hooks["chat.message"]({ sessionID: "root" })
    assert.equal(calls.at(-1).topic, "agent.session.end")
  })

  it("V2 keeps server-owned status, metadata and deletion without a local Terminal pane", async () => {
    const sdk = createContext({ getSession: async (id) => ({ id, parentID: id === "child" ? "root" : undefined }) })
    const dispose = await plugin.setup(sdk.context)
    try {
      await sdk.event("session.created", { sessionID: "root", title: "Original" })
      await sdk.prompt("root")
      await sdk.prompt("child")
      await sdk.tool("child", "bash", {})
      await sdk.event("session.renamed", { sessionID: "root", title: "Renamed" })
      await sdk.tool("root", "question", { questions: ["Continue?"] })
      for (const type of ["permission.asked", "question.asked", "permission.replied", "question.replied"]) {
        await sdk.event(type, { sessionID: "root", action: "edit" })
      }
      for (const type of ["session.execution.succeeded", "session.execution.interrupted", "session.execution.failed"]) {
        await sdk.event(type, { sessionID: "root", error: { data: { message: "Fixture failure" } } })
      }
      const owner = {
        clientID: "client", terminalID: "terminal", paneID: "pane", generation: 1,
        sessionID: "root", cwd: "C:\\root", title: "", running: false,
      }
      const snapshot = await sdk.client.rpc(sdk.rpc.definition).bind(owner)
      assert.equal(snapshot.title, "Renamed")
      assert.equal(snapshot.status, "error")
      assert.equal(snapshot.error, "Fixture failure")
      assert.equal(calls.length, 0)
      await sdk.event("session.deleted", { sessionID: "root" })
      await new Promise(setImmediate)
      const { cwd, title, running, ...identity } = owner
      await assert.rejects(sdk.client.rpc(sdk.rpc.definition).snapshot(owner), /unexpected cwd,title,running/)
      assert.equal((await sdk.client.rpc(sdk.rpc.definition).snapshot(identity)).status, "deleted")
      assert.deepEqual(sdk.rpc.emitted.filter((event) => event.data.topic === "agent.session.end")
        .map((event) => event.data.sessionID), ["root"])
    } finally { await dispose() }
    assert.equal(sdk.aborted, true)
    assert.equal(calls.length, 0)
  })

  it("V2 location filtering rejects both foreign directories and foreign workspaces", async () => {
    const sdk = createContext()
    const dispose = await plugin.setup(sdk.context)
    try {
      await sdk.event("session.created", { sessionID: "foreign-directory" }, { ...sdk.location, directory: "C:\\elsewhere" })
      await sdk.event("session.created", { sessionID: "foreign-workspace" }, { ...sdk.location, workspaceID: "other" })
      assert.equal(calls.length, 0)
      await sdk.event("session.created", { sessionID: "owned", title: "Owned" })
      await sdk.prompt("owned")
      assert.equal(calls.length, 0)
      const owner = {
        clientID: "client", terminalID: "terminal", paneID: "pane", generation: 1,
        sessionID: "owned", cwd: "C:\\owned", title: "", running: false,
      }
      assert.equal((await sdk.client.rpc(sdk.rpc.definition).bind(owner)).status, "working")
    } finally { await dispose() }
  })

  it("V2 server-only prompt and disposal never emit a pane-binding lifecycle event", async () => {
    const sdk = createContext()
    const dispose = await plugin.setup(sdk.context)
    await sdk.event("session.created", { sessionID: "unowned", title: "Unowned" })
    await sdk.prompt("unowned")
    await dispose()
    assert.equal(calls.length, 0)
    assert.equal(sdk.rpc.emitted.length, 0)
  })

  for (const teardown of ["delete", "dispose"]) {
    it(`V2 ${teardown} invalidates a shared pending lookup before prompts and tools can emit`, async () => {
      const lookup = deferred()
      const sdk = createContext({ getSession: () => lookup.promise })
      const dispose = await plugin.setup(sdk.context)
      const prompt = sdk.prompt("resumed")
      const tool = sdk.tool("resumed", "bash", {})
      assert.equal(sdk.lookupCount, 1)
      if (teardown === "delete") await sdk.event("session.deleted", { sessionID: "resumed" })
      else await dispose()
      lookup.resolve({ id: "resumed", location: sdk.location })
      await Promise.all([prompt, tool])
      assert.equal(calls.length, 0)
      await dispose()
    })
  }

  it("V2 deletion fences an older lookup even when the same ID starts a new lifecycle", async () => {
    const oldLookup = deferred()
    const newLookup = deferred()
    let first = true
    const sdk = createContext({ getSession: () => {
      const result = first ? oldLookup.promise : newLookup.promise
      first = false
      return result
    } })
    const dispose = await plugin.setup(sdk.context)
    try {
      const oldPrompt = sdk.prompt("reused")
      await sdk.event("session.deleted", { sessionID: "reused" })
      const newPrompt = sdk.prompt("reused")
      oldLookup.resolve({ id: "reused", title: "Stale" })
      await oldPrompt
      assert.equal(calls.length, 0)
      newLookup.resolve({ id: "reused", title: "Current", location: sdk.location })
      await newPrompt
      assert.equal(calls.some((call) => call.payload.title === "Stale"), false)
      const owner = {
        clientID: "client", terminalID: "terminal", paneID: "pane", generation: 1,
        sessionID: "reused", cwd: "C:\\current", title: "", running: false,
      }
      assert.equal((await sdk.client.rpc(sdk.rpc.definition).bind(owner)).status, "working")
    } finally { await dispose() }
  })

  it("V2 lookup failures are diagnosed without blocking hooks, and lookup can retry", async (t) => {
    const logDirectory = await mkdtemp(join(tmpdir(), "ite2e-opencode-log-"))
    t.after(() => rm(logDirectory, { recursive: true }))
    process.env.WTA_HOOK_LOG_DIR = logDirectory
    let failLookup = true
    const sdk = createContext({ getSession: async (id) => {
      if (failLookup) throw new Error("lookup unavailable")
      return { id, location: sdk.location }
    } })
    const dispose = await plugin.setup(sdk.context)
    try {
      await sdk.prompt("resumed")
      assert.equal(calls.length, 0)
      failLookup = false
      await sdk.prompt("resumed")
      const owner = {
        clientID: "client", terminalID: "terminal", paneID: "pane", generation: 1,
        sessionID: "resumed", cwd: "C:\\resumed", title: "", running: false,
      }
      assert.equal((await sdk.client.rpc(sdk.rpc.definition).bind(owner)).status, "working")
      const log = await readFile(join(logDirectory, "hook-trace.log"), "utf8")
      assert.match(log, /session lookup failed.*lookup unavailable/)
    } finally { await dispose() }
  })

  it("V2 driver combines the server and TUI hooks for start, cwd, and disposal", async (t) => {
    const directory = await mkdtemp(join(tmpdir(), "ite2e-opencode-driver-"))
    t.after(() => rm(directory, { recursive: true }))
    const log = join(directory, "events.jsonl")
    await writeFile(join(directory, "agent-hook"), `
      const fs = require("node:fs")
      let input = ""
      process.stdin.on("data", (chunk) => { input += chunk })
      process.stdin.on("end", () => fs.appendFileSync(process.env.MOCK_HOOK_LOG,
        JSON.stringify({ topic: process.argv.at(-1), payload: JSON.parse(input) }) + "\\n"))
    `)
    const driver = spawn(process.execPath, [
      fileURLToPath(new URL("../fixtures/OpenCodeHookDriver.mjs", import.meta.url)),
      pluginPath, "v2", directory, tuiPluginPath,
    ], {
      cwd: directory,
      env: {
        ...process.env,
        WTCLI_PATH: process.execPath,
        MOCK_HOOK_LOG: log,
      },
      stdio: ["pipe", "pipe", "pipe"],
    })
    const output = createInterface({ input: driver.stdout })[Symbol.asyncIterator]()
    let errors = ""
    driver.stderr.on("data", (chunk) => { errors += chunk })
    const exited = new Promise((resolve, reject) => {
      driver.once("error", reject)
      driver.once("exit", (code) => code === 0 ? resolve() : reject(new Error(`driver exited ${code}: ${errors}`)))
    })
    try {
      assert.equal(JSON.parse((await output.next()).value).ready, true)
      async function action(token, request) {
        driver.stdin.write(JSON.stringify({ ...request, token }) + "\n")
        assert.equal(JSON.parse((await output.next()).value).completed, token)
      }
      await action("create", { action: "event", type: "session.created", data: { sessionID: "root", title: "Root" } })
      await action("view", { action: "view", id: "root" })
      await action("rename", { action: "event", type: "session.renamed", data: {
        sessionID: "root", title: "Updated",
      } })
      await action("move", { action: "event", type: "session.moved", data: {
        sessionID: "root", location: { directory: "C:\\moved" },
      } })
      await action("dispose", { action: "dispose" })
      assert.deepEqual((await readFile(log, "utf8")).trim().split("\n").map((line) => {
        const { topic, payload } = JSON.parse(line)
        return [topic, payload.cwd, payload.reason]
      }), [
        ["agent.session.start", directory, undefined],
        ["agent.stop", undefined, undefined],
        ["agent.session.renamed", undefined, undefined],
        ["agent.cwd.changed", "C:\\moved", undefined],
        ["agent.session.unbind", undefined, undefined],
      ])
    } finally {
      driver.stdin.end()
      await exited
    }
  })

  it("ACP clients stay unbound while the shared server remains available", async () => {
    for (const suppressed of ["acp", "WT_SESSION", "WT_COM_CLSID"]) {
      const previous = { ...process.env }
      if (suppressed === "acp") process.env.OPENCODE_CLIENT = "acp"
      else delete process.env[suppressed]
      const sdk = createContext()
      const dispose = await plugin.setup(sdk.context)
      assert.equal(typeof dispose, "function")
      await sdk.prompt("unbound")
      await dispose()
      if (suppressed === "acp") assert.equal(tuiPlugin.setup({}), undefined)
      const hooks = await v1Plugin({ directory: sdk.location.directory })
      await hooks["chat.message"]({ sessionID: "suppressed" })
      await hooks.dispose()
      for (const key of environmentKeys) {
        if (previous[key] === undefined) delete process.env[key]
        else process.env[key] = previous[key]
      }
    }
    assert.equal(calls.length, 0)
  })
})

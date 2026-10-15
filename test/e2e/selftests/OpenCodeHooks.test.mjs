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
  fileURLToPath(new URL("../../../tools/wta/wt-agent-hooks/opencode/wt-agent-hooks.js", import.meta.url))
const plugin = await loadPlugin(pluginPath)
const tuiPluginPath = fileURLToPath(new URL(
  "../../../tools/wta/wt-agent-hooks/opencode/wt-agent-hooks/tui.js", import.meta.url,
))
const tuiPlugin = await loadPlugin(tuiPluginPath)
const environmentKeys = ["WT_COM_CLSID", "WT_SESSION", "WTCLI_PATH", "OPENCODE_CLIENT", "WTA_HOOK_LOG_DIR"]
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
    calls = []
    globalThis.Bun = {
      spawn(options) {
        calls.push({ topic: options.cmd.at(-1), payload: JSON.parse(new TextDecoder().decode(options.stdin)), options })
        return { exited: Promise.resolve(0) }
      },
    }
  })

  describe("OpenCode V2 TUI cwd binding", () => {
    let sessions
    let handlers
    let route
    let context
    let statuses

    beforeEach(() => {
      sessions = new Map()
      statuses = new Map()
      handlers = new Map()
      route = { type: "session", sessionID: "root" }
      context = {
        ui: { router: { current: () => route } },
        data: {
          session: {
            get: (sessionID) => sessions.get(sessionID),
            status: (sessionID) => statuses.get(sessionID) ?? "idle",
          },
          on: (event, handler) => {
            handlers.set(event, handler)
            return () => handlers.delete(event)
          },
        },
      }
    })

    function emit(event, data) {
      handlers.get(event)({ data })
    }

    it("binds the visible root and sends moved cwd without attributing other panes or child sessions", async () => {
      sessions.set("root", { id: "root", title: "Current", location: { directory: "C:\\first" } })
      sessions.set("other", { id: "other", location: { directory: "C:\\foreign" } })
      sessions.set("child", { id: "child", parentID: "root", location: { directory: "C:\\child" } })
      const dispose = tuiPlugin.setup(context)
      emit("session.viewed", { sessionID: "root" })
      emit("session.moved", { sessionID: "root", location: { directory: "C:\\second" } })
      emit("session.moved", { sessionID: "root", location: { directory: "C:\\second" } })
      emit("session.moved", { sessionID: "other", location: { directory: "C:\\foreign" } })
      route = { type: "session", sessionID: "child" }
      emit("session.viewed", { sessionID: "child" })
      await dispose()
      assert.deepEqual(calls.map(({ topic, payload }) => [topic, payload.session_id, payload.cwd]), [
        ["agent.session.start", "root", "C:\\first"],
        ["agent.cwd.changed", "root", "C:\\second"],
        ["agent.session.end", "root", undefined],
      ])
      assert.equal(calls[0].payload.title, "Current")
      assert.deepEqual(calls[0].options.cmd, [process.env.WTCLI_PATH, "agent-hook", "--cli-source", "opencode", "--event", "agent.session.start"])
      assert.equal(handlers.size, 0)
    })

    it("does not bind a moved session missing from the TUI cache, then binds it when viewed", async () => {
      const dispose = tuiPlugin.setup(context)
      emit("session.moved", { sessionID: "root", location: { directory: "C:\\second" } })
      assert.equal(calls.length, 0)
      sessions.set("root", { id: "root", location: { directory: "C:\\second" } })
      emit("session.viewed", { sessionID: "root" })
      await dispose()
      assert.deepEqual(calls.map((call) => call.payload.cwd), ["C:\\second", undefined])
    })

    it("binds an already visible running session and tracks V2 execution outcomes and deletion", async () => {
      sessions.set("root", { id: "root", title: "Restored", location: { directory: "C:\\restored" } })
      statuses.set("root", "running")
      const dispose = tuiPlugin.setup(context)
      emit("session.execution.started", { sessionID: "other" })
      emit("session.execution.succeeded", { sessionID: "root" })
      emit("session.renamed", { sessionID: "root", title: "New title" })
      emit("session.execution.started", { sessionID: "root" })
      emit("session.execution.failed", { sessionID: "root" })
      emit("session.execution.started", { sessionID: "root" })
      emit("session.execution.interrupted", { sessionID: "root" })
      emit("session.deleted", { sessionID: "root" })
      emit("session.execution.started", { sessionID: "root" })
      await dispose()
      assert.deepEqual(calls.map((call) => call.topic), [
        "agent.session.start", "agent.prompt.submit", "agent.stop", "agent.session.start",
        "agent.prompt.submit", "agent.stop", "agent.prompt.submit", "agent.stop", "agent.session.end",
      ])
      assert.equal(calls[3].payload.title, "New title")
      assert.equal(calls[8].payload.reason, "deleted")
      assert.equal(handlers.size, 0)
    })

    it("serializes bridge children and ends the bound session exactly once on disposal", async () => {
      sessions.set("root", { id: "root", title: "Current", location: { directory: "C:\\first" } })
      const first = deferred()
      globalThis.Bun.spawn = (options) => {
        calls.push({ topic: options.cmd.at(-1), payload: JSON.parse(new TextDecoder().decode(options.stdin)) })
        return { exited: calls.length === 1 ? first.promise : Promise.resolve(0) }
      }
      const dispose = tuiPlugin.setup(context)
      emit("session.moved", { sessionID: "root", location: { directory: "C:\\second" } })
      const ending = dispose()
      await Promise.resolve()
      assert.deepEqual(calls.map((call) => call.topic), ["agent.session.start"])
      first.resolve(0)
      await ending
      await dispose()
      assert.deepEqual(calls.map((call) => call.topic), [
        "agent.session.start", "agent.cwd.changed", "agent.session.end",
      ])
      assert.equal(calls.at(-1).payload.reason, "tui_disposed")
    })

    it("suppresses hooks outside a Terminal pane and in ACP mode", () => {
      delete process.env.WT_SESSION
      assert.equal(tuiPlugin.setup(context), undefined)
      process.env.WT_SESSION = "00000000-0000-0000-0000-000000000002"
      process.env.OPENCODE_CLIENT = "acp"
      assert.equal(tuiPlugin.setup(context), undefined)
      assert.equal(handlers.size, 0)
    })
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
    const hooks = await plugin.server({ directory: "C:\\root" })
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

  it("V2 forwards rename, waiting, replies and execution outcomes without treating children as roots", async () => {
    const sdk = createContext({ getSession: async (id) => ({ id, parentID: id === "child" ? "root" : undefined }) })
    const dispose = await plugin.setup(sdk.context)
    try {
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
      assert.equal(calls.some((call) => call.payload.session_id === "child"), false)
      assert.deepEqual(calls.filter((call) => call.topic === "agent.notification").map((call) => call.payload.message),
        ["Permission required: edit", "OpenCode is waiting for input"])
      assert.equal(calls.filter((call) => call.topic === "agent.stop").length, 2)
      assert.equal(calls.find((call) => call.topic === "agent.error").payload.error, "Fixture failure")
      assert.equal(calls.some((call) => call.topic === "agent.session.start"), false)
      assert.deepEqual(calls.find((call) => call.topic === "agent.tool.starting").payload.tool_input, { questions: ["Continue?"] })
      await sdk.event("session.deleted", { sessionID: "root" })
      assert.equal(calls.some((call) => call.topic === "agent.session.end"), false)
    } finally { await dispose() }
    assert.equal(sdk.aborted, true)
    assert.equal(calls.some((call) => call.topic === "agent.session.end"), false)
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
      assert.deepEqual(calls.map((call) => call.topic), ["agent.prompt.submit"])
      assert.equal(calls[0].payload.session_id, "owned")
    } finally { await dispose() }
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
      assert.equal(calls.filter((call) => call.topic === "agent.prompt.submit").length, 1)
    } finally { await dispose() }
  })

  it("V2 lookup and bridge failures are diagnosed without blocking hooks, and lookup can retry", async (t) => {
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
      assert.equal(calls.at(-1).topic, "agent.prompt.submit")
      globalThis.Bun.spawn = () => { throw new Error("bridge unavailable") }
      await sdk.tool("resumed", "bash", {})
      const log = await readFile(join(logDirectory, "hook-trace.log"), "utf8")
      assert.match(log, /session lookup failed.*lookup unavailable/)
      assert.match(log, /bridge spawn failed.*bridge unavailable/)
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
      await action("move", { action: "event", type: "session.moved", data: {
        sessionID: "root", location: { directory: "C:\\moved" },
      } })
      await action("dispose", { action: "dispose" })
      assert.deepEqual((await readFile(log, "utf8")).trim().split("\n").map((line) => {
        const { topic, payload } = JSON.parse(line)
        return [topic, payload.cwd, payload.reason]
      }), [
        ["agent.session.start", directory, undefined],
        ["agent.cwd.changed", "C:\\moved", undefined],
        ["agent.session.end", undefined, "tui_disposed"],
      ])
    } finally {
      driver.stdin.end()
      await exited
    }
  })

  it("ACP mode and missing Terminal identity suppress V2 registration and V1 bridge emission", async () => {
    for (const suppressed of ["acp", "WT_SESSION", "WT_COM_CLSID"]) {
      const previous = { ...process.env }
      if (suppressed === "acp") process.env.OPENCODE_CLIENT = "acp"
      else delete process.env[suppressed]
      const sdk = createContext()
      assert.equal(await plugin.setup(sdk.context), undefined)
      assert.equal(sdk.registered, 0)
      const hooks = await plugin.server({ directory: sdk.location.directory })
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

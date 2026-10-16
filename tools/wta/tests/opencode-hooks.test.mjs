import assert from "node:assert/strict"
import { randomUUID } from "node:crypto"
import { readFile } from "node:fs/promises"
import { test } from "node:test"
import vm from "node:vm"
import { fileURLToPath } from "node:url"
import { createContext, loadPlugin } from "../../../test/e2e/fixtures/OpenCodeHookContext.mjs"

const bundle = new URL("../wt-agent-hooks/opencode/", import.meta.url)
const paneA = "11111111-1111-1111-1111-111111111111"
const paneB = "22222222-2222-2222-2222-222222222222"
const clsid = "33333333-3333-3333-3333-333333333333"
const location = { directory: "C:\\same-project" }
const tick = () => new Promise((resolve) => setImmediate(resolve))

async function load(entry, env = {}) {
  const calls = []
  const failures = []
  const effects = new Set()
  const timers = new Map()
  const context = vm.createContext({
    process: { platform: "win32", env },
    crypto: { randomUUID },
    TextEncoder, AbortController, AbortSignal, console: { error: (...args) => failures.push(args) },
    setTimeout, clearTimeout,
    setInterval: (callback) => { const id = {}; timers.set(id, callback); return id },
    clearInterval: (id) => timers.delete(id),
    Bun: {
      spawn: (options) => {
        calls.push({ topic: options.cmd.at(-1), payload: JSON.parse(new TextDecoder().decode(options.stdin)), options })
        return { exited: Promise.resolve(0) }
      },
    },
  })
  const modules = new Map()
  async function moduleFor(name) {
    if (modules.has(name)) return modules.get(name)
    if (["node:fs", "node:crypto", "solid-js"].includes(name)) {
      const exports = name === "node:fs"
        ? { mkdirSync() {}, appendFileSync: (...args) => failures.push(args) }
        : name === "node:crypto" ? { randomUUID }
          : {
            createEffect: (effect) => { effects.add(effect); effect() },
            createRoot: (callback) => callback(() => effects.clear()),
          }
      const module = new vm.SyntheticModule(Object.keys(exports), function () {
        for (const [key, value] of Object.entries(exports)) this.setExport(key, value)
      }, { context, identifier: name })
      modules.set(name, module)
      return module
    }
    const module = new vm.SourceTextModule(await readFile(new URL(name, bundle), "utf8"), { context, identifier: name })
    modules.set(name, module)
    await module.link((specifier, parent) => {
      if (!specifier.startsWith(".")) return moduleFor(specifier)
      if (specifier.endsWith("bridge.js")) return moduleFor("shared/bridge.js")
      return moduleFor(new URL(specifier, new URL(parent.identifier, bundle)).href.slice(bundle.href.length))
    })
    return module
  }
  const module = await moduleFor(entry)
  await module.evaluate()
  return { plugin: module.namespace, calls, failures, timers, effects }
}

test("V2 pane binding definition loads without installing the OpenCode SDK", async () => {
  await assert.doesNotReject(import(new URL("v2/rpc.js", bundle)))
})

test("V1 preserves its named entrypoint, own cwd updates and ACP suppression", async () => {
  const fixture = await load("v1/wt-agent-hooks.js", { WT_SESSION: paneA, WT_COM_CLSID: clsid })
  assert.deepEqual(Object.keys(fixture.plugin), ["WtAgentHooks"])
  const hooks = await fixture.plugin.WtAgentHooks({ directory: location.directory })
  for (const directory of [location.directory, "C:\\moved"]) {
    await hooks.event({ event: { type: "session.updated", properties: { info: { id: "a", directory, title: "A" } } } })
  }
  assert.equal(fixture.calls.at(-1).topic, "agent.cwd.changed")
  assert.equal(fixture.calls.at(-1).payload.cwd, "C:\\moved")
  await hooks.dispose()
  const acp = await load("v1/wt-agent-hooks.js", { WT_SESSION: paneA, WT_COM_CLSID: clsid, OPENCODE_CLIENT: "acp" })
  const suppressed = await acp.plugin.WtAgentHooks({ directory: location.directory })
  await suppressed["chat.message"]({ sessionID: "a" })
  await suppressed.dispose()
  assert.equal(acp.calls.length, 0)
})

test("V2 ACP-started server registers RPC while an ACP TUI never binds", async () => {
  const fixture = await load("v2/wt-agent-hooks.js", { OPENCODE_CLIENT: "acp" })
  const sdk = createContext({ ...location })
  const dispose = await fixture.plugin.default.setup(sdk.context)
  try {
    await sdk.prompt("unbound")
    assert.equal(fixture.calls.length, 0)
    assert.ok(sdk.rpc.definition.methods.bind)
    const client = await load("v2/tui.js", { WT_SESSION: paneA, WT_COM_CLSID: clsid, OPENCODE_CLIENT: "acp" })
    assert.equal(client.plugin.default.setup({}), undefined)
    assert.equal(client.timers.size, 0)
  } finally { await dispose() }
})

test("V2 shared tabs cannot steal two independent pane bindings or status", async () => {
  const server = await loadPlugin(fileURLToPath(new URL("v2/wt-agent-hooks.js", bundle)))
  const sessions = { a: { id: "a", title: "A", location }, b: { id: "b", title: "B", location } }
  const sdk = createContext({ ...location, getSession: async (id) => sessions[id] })
  const disposeServer = await server.setup(sdk.context)
  const clients = []
  try {
    for (const [pane, id] of [[paneA, "a"], [paneB, "b"]]) {
      const client = await load("v2/tui.js", { WT_SESSION: pane, WT_COM_CLSID: clsid, WTCLI_PATH: "C:\\Terminal\\wtcli.exe" })
      const handlers = new Map()
      let route = { type: "session", sessionID: id }
      const dispose = client.plugin.default.setup({
        location: sdk.location,
        client: sdk.client,
        ui: {
          tabs: { enabled: () => true, list: () => [{ sessionID: "a" }, { sessionID: "b" }] },
          router: { current: () => route },
        },
        data: {
          session: { get: (id) => sessions[id], status: () => "idle" },
          on: (name, handler) => { handlers.set(name, handler); return () => handlers.delete(name) },
        },
      })
      clients.push({ client, dispose, home: () => { route = { type: "home" }; handlers.get("session.viewed")({ data: {} }) } })
    }
    await tick()
    for (const { dispose } of clients) await dispose.flush()
    for (const [index, id] of ["a", "b"].entries()) {
      assert.deepEqual(clients[index].client.calls.map((call) => call.payload.session_id), [id])
    }
    await sdk.event("session.status", { sessionID: "a", status: { type: "busy" } })
    await sdk.event("question.asked", { sessionID: "b" })
    await tick()
    for (const { dispose } of clients) await dispose.flush()
    assert.equal(clients[0].client.calls.at(-1).topic, "agent.prompt.submit")
    assert.equal(clients[1].client.calls.at(-1).topic, "agent.notification")
    clients[0].home()
    await clients[0].dispose.flush()
    assert.equal(clients[0].client.calls.at(-1).topic, "agent.session.unbind")
    assert.equal(clients[0].client.calls.some((call) => call.topic === "agent.session.end"), false)
    await sdk.event("session.execution.succeeded", { sessionID: "b" })
    await tick()
    await clients[1].dispose.flush()
    assert.equal(clients[1].client.calls.at(-1).topic, "agent.stop")
    assert.ok(clients.every(({ client }) => client.failures.length === 0))
  } finally {
    for (const { dispose } of clients) await dispose()
    await disposeServer()
  }
})

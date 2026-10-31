// Managed by Intelligent Terminal: wt-agent-hooks

import { appendFileSync, mkdirSync, writeFileSync } from "node:fs"

export function tryWriteHookVt(topic, sessionID, payload, write = writeFileSync) {
  if (!process.env.WT_AGENT_HOOK_DIR) return false
  const params = {
    event: topic, cli_source: "opencode", agent_session_id: sessionID, payload,
  }
  const encoded = Buffer.from(JSON.stringify(params), "utf8").toString("base64")
  const sequence = `\x1b]9001;AgentEvent64;${encoded}\x07`
  try {
    // Hook stdout may be captured or owned by the provider's renderer.
    write("CONOUT$", sequence, { flag: "r+" })
    return true
  } catch (error) {
    try {
      const dir = process.env.WTA_HOOK_LOG_DIR
      if (dir) {
        mkdirSync(dir, { recursive: true })
        appendFileSync(`${dir}\\hook-trace.log`,
          `${new Date().toISOString()} opencode VT unavailable topic=${topic} code=${error.code || "unknown"}\n`)
      }
    } catch {
      // Diagnostics cannot make an optional hook fail.
    }
    return false
  }
}

export function eventMessage(value) {
  if (typeof value === "string") return value
  if (value && typeof value === "object") {
    if (typeof value.message === "string") return value.message
    if (typeof value.data?.message === "string") return value.data.message
    if (typeof value.name === "string") return value.name
  }
  return ""
}

export function clientContext() {
  if (
    process.platform !== "win32" ||
    !process.env.WT_COM_CLSID ||
    !process.env.WT_SESSION ||
    process.env.OPENCODE_CLIENT === "acp"
  ) return

  return {
    pane_session_id: process.env.WT_SESSION,
    com_clsid: process.env.WT_COM_CLSID,
    bridge_path: process.env.WTCLI_PATH || "wtcli.exe",
    log_dir: process.env.WTA_HOOK_LOG_DIR,
  }
}

export function noteFailure(context, topic, error) {
  const message = eventMessage(error) || String(error)
  try {
    if (context?.log_dir) {
      mkdirSync(context.log_dir, { recursive: true })
      appendFileSync(
        `${context.log_dir}\\hook-trace.log`,
        `${new Date().toISOString()} opencode hook failed topic=${topic} err=${message}\n`,
      )
      return
    }
  } catch {
    // The diagnostic must not interrupt the CLI when the log directory fails.
  }
  console.error(`opencode wt-agent-hooks: ${topic}: ${message}`)
}

export function emit(context, topic, sessionID, payload = {}) {
  if (!context || !sessionID) return
  if (context.pane_session_id === process.env.WT_SESSION &&
      context.com_clsid === process.env.WT_COM_CLSID &&
      tryWriteHookVt(topic, sessionID, { session_id: sessionID, ...payload })) return
  try {
    const child = Bun.spawn({
      // Bun cannot resolve the packaged app-execution alias; use WTCLI_PATH
      // supplied by the owning client, not the background server's PATH.
      cmd: [
        context.bridge_path,
        "agent-hook",
        "--cli-source",
        "opencode",
        "--event",
        topic,
      ],
      env: {
        ...process.env,
        WT_SESSION: context.pane_session_id,
        WT_COM_CLSID: context.com_clsid,
        WTCLI_PATH: context.bridge_path,
        WTA_HOOK_LOG_DIR: context.log_dir || "",
      },
      stdin: new TextEncoder().encode(JSON.stringify({ session_id: sessionID, ...payload })),
      stdout: "ignore",
      stderr: "ignore",
      windowsHide: true,
    })
    void child.exited.then(
      (code) => {
        if (code !== 0) noteFailure(context, topic, `bridge exited with code ${code}`)
      },
      (error) => noteFailure(context, topic, error),
    )
  } catch (error) {
    noteFailure(context, topic, error)
  }
}

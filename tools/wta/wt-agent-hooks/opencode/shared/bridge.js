// Managed by Intelligent Terminal: wt-agent-hooks

import { appendFileSync, mkdirSync } from "node:fs"

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

// Managed by Intelligent Terminal: wt-agent-hooks
const source = "opencode";

async function bridge(topic, sessionID, data) {
  const executable = process.env.WTCLI_PATH || "wtcli.exe";
  try {
    const child = Bun.spawn({
      cmd: [executable, "agent-hook", "--cli-source", source, "--event", topic],
      stdin: new TextEncoder().encode(JSON.stringify({ session_id: sessionID, ...data })),
      stdout: "ignore",
      stderr: "ignore",
      windowsHide: true,
    });
    await child.exited.then((code) => {
      if (code !== 0) console.error(`wt-agent-hooks: ${topic} exited with ${code}`);
    });
  } catch (error) {
    console.error(`wt-agent-hooks: ${topic} failed`, error);
  }
}

export default {
  id: "wt-agent-hooks",
  setup(context) {
    if (
      process.platform !== "win32" ||
      !process.env.WT_COM_CLSID ||
      !process.env.WT_SESSION ||
      process.env.OPENCODE_CLIENT === "acp"
    ) return;

    let bound;
    let delivery = Promise.resolve();
    let disposed = false;
    function send(topic, sessionID, data) {
      delivery = delivery.then(() => bridge(topic, sessionID, data));
    }
    function selected(sessionID) {
      const route = context.ui.router.current();
      return route.type === "session" && route.sessionID === sessionID;
    }
    function bind(sessionID, directory, reclaim = false) {
      if (!sessionID || !directory || !selected(sessionID)) return;
      const session = context.data.session.get(sessionID);
      if (!session || session.parentID) return;
      if (bound?.id !== sessionID || reclaim) {
        const running = context.data.session.status(sessionID) === "running";
        bound = { id: sessionID, directory, title: session.title };
        const start = {
          cwd: directory,
          title: session.title,
        };
        if (running) start.running = true;
        send("agent.session.start", sessionID, start);
      } else if (bound.directory !== directory) {
        bound.directory = directory;
        send("agent.cwd.changed", sessionID, { cwd: directory });
      } else if (bound.title !== session.title) {
        bound.title = session.title;
        send("agent.session.renamed", sessionID, { title: session.title });
      }
    }

    const current = context.ui.router.current();
    if (current.type === "session") {
      bind(current.sessionID, context.data.session.get(current.sessionID)?.location?.directory);
    }
    const onViewed = context.data.on("session.viewed", ({ data }) => {
      const session = context.data.session.get(data.sessionID);
      if (session) bind(data.sessionID, session.location?.directory, true);
    });
    const onCreated = context.data.on("session.created", ({ data }) => {
      bind(data.sessionID, data.location?.directory);
    });
    const onMoved = context.data.on("session.moved", ({ data }) => {
      bind(data.sessionID, data.location?.directory);
    });
    const onRenamed = context.data.on("session.renamed", ({ data }) => {
      if (bound?.id === data.sessionID && selected(data.sessionID)) {
        bound.title = data.title;
        send("agent.session.renamed", data.sessionID, { title: data.title });
      }
    });
    const onExecutionStarted = context.data.on("session.execution.started", ({ data }) => {
      if (bound?.id !== data.sessionID || !selected(data.sessionID)) return;
      send("agent.prompt.submit", data.sessionID);
    });
    const onExecutionStopped = ["session.execution.succeeded", "session.execution.failed", "session.execution.interrupted"]
      .map((event) => context.data.on(event, ({ data }) => {
        if (bound?.id === data.sessionID && selected(data.sessionID)) {
          send("agent.stop", data.sessionID);
        }
      }));
    const onDeleted = context.data.on("session.deleted", ({ data }) => {
      if (bound?.id !== data.sessionID) return;
      send("agent.session.end", data.sessionID, { reason: "deleted" });
      bound = undefined;
    });

    const dispose = async () => {
      if (disposed) return delivery;
      disposed = true;
      onViewed();
      onCreated();
      onMoved();
      onRenamed();
      onExecutionStarted();
      for (const dispose of onExecutionStopped) dispose();
      onDeleted();
      if (bound) {
        send("agent.session.end", bound.id, { reason: "tui_disposed" });
        bound = undefined;
      }
      await delivery;
    };
    dispose.flush = () => delivery;
    return dispose;
  },
};

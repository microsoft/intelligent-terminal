// Managed by Intelligent Terminal: wt-agent-hooks
import { createEffect, createRoot } from "solid-js";
import { bindingRpc } from "./rpc.js";

async function bridge(topic, sessionID, data) {
  const executable = process.env.WTCLI_PATH || "wtcli.exe";
  const child = Bun.spawn({
    cmd: [executable, "agent-hook", "--cli-source", "opencode", "--event", topic],
    stdin: new TextEncoder().encode(JSON.stringify({ session_id: sessionID, ...data })),
    stdout: "ignore",
    stderr: "ignore",
    windowsHide: true,
  });
  const code = await child.exited;
  if (code !== 0) throw new Error(`wt-agent-hooks: ${topic} exited with ${code}`);
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

    const terminalID = process.env.WT_COM_CLSID;
    const paneID = process.env.WT_SESSION;
    const clientID = crypto.randomUUID();
    const rpc = context.client.rpc(bindingRpc);
    const options = { location: context.location };
    const controller = new AbortController();
    let bound;
    let generation = 0;
    let revision = 0;
    let desired;
    let intent = 0;
    let connected = false;
    let disposed = false;
    let evictedSessionID;
    const observed = new Map();
    let delivery = Promise.resolve();
    let pending = [];

    function identity(owner) {
      const { clientID, terminalID, paneID, sessionID, generation } = owner;
      return { clientID, terminalID, paneID, sessionID, generation };
    }
    function enqueue(work) {
      const result = delivery.then(work);
      delivery = result.catch((error) => console.error("wt-agent-hooks: pane sync failed", error));
      return result;
    }
    function selected() {
      const route = context.ui.router.current();
      if (route.type !== "session") return;
      const session = context.data.session.get(route.sessionID);
      if (!session || session.parentID || !session.location?.directory) return;
      return {
        sessionID: route.sessionID,
        cwd: session.location.directory,
        title: session.title || "",
        running: context.data.session.status(route.sessionID) === "running",
      };
    }
    function matches(signal, owner) {
      return owner &&
        signal.clientID === owner.clientID &&
        signal.terminalID === owner.terminalID &&
        signal.paneID === owner.paneID &&
        signal.sessionID === owner.sessionID &&
        signal.generation === owner.generation;
    }
    async function release() {
      if (!bound) return;
      const old = bound;
      const result = await rpc.unbind(identity(old), options);
      if (result.detached) await bridge("agent.session.unbind", old.sessionID, {});
      if (bound === old) bound = undefined;
    }
    async function reconcile(snapshot, owner, initial) {
      if (snapshot.status === "deleted") {
        await bridge("agent.session.end", owner.sessionID, { reason: "deleted", pane_owner: false });
        await bridge("agent.session.unbind", owner.sessionID, {});
        observed.delete(owner.sessionID);
        if (bound === owner) bound = undefined;
        return;
      }
      if (!initial && snapshot.cwd && snapshot.cwd !== owner.cwd) {
        await bridge("agent.cwd.changed", owner.sessionID, { cwd: snapshot.cwd, pane_owner: false });
      }
      if (!initial && snapshot.title && snapshot.title !== owner.title) {
        await bridge("agent.session.renamed", owner.sessionID, { title: snapshot.title, pane_owner: false });
      }
      owner.cwd = snapshot.cwd || owner.cwd;
      owner.title = snapshot.title || owner.title;
      if (snapshot.status === "working" && !initial) {
        await bridge("agent.prompt.submit", owner.sessionID, { pane_owner: false });
      } else if (snapshot.status === "idle") {
        await bridge("agent.stop", owner.sessionID, { pane_owner: false });
      } else if (snapshot.status === "attention") {
        await bridge("agent.notification", owner.sessionID, {
          message: snapshot.notification || "OpenCode is waiting for input", pane_owner: false,
        });
      } else if (snapshot.status === "error") {
        await bridge("agent.error", owner.sessionID, {
          error: snapshot.error || "OpenCode session error", pane_owner: false,
        });
      }
      revision = snapshot.revision;
    }
    async function synchronize(expected = intent, reclaim = false) {
      if (!connected || disposed || expected !== intent) return;
      if (!desired) {
        await release();
        return;
      }
      if (evictedSessionID === desired.sessionID) return;
      if (bound?.sessionID === desired.sessionID && !reclaim) return;
      if (bound && bound.sessionID !== desired.sessionID) await release();
      if (disposed || expected !== intent) return;
      const owner = {
        clientID, terminalID, paneID, sessionID: desired.sessionID, generation: ++generation,
      };
      const snapshot = await rpc.bind({ ...owner, ...desired }, options);
      if (snapshot.status === "deleted") {
        if (bound?.sessionID === owner.sessionID) {
          await bridge("agent.session.end", owner.sessionID, { reason: "deleted", pane_owner: false });
          await bridge("agent.session.unbind", owner.sessionID, {});
          observed.delete(owner.sessionID);
          bound = undefined;
        }
        await rpc.unbind(identity(owner), options);
        return;
      }
      if (disposed || expected !== intent) {
        await rpc.unbind(identity(owner), options);
        if (bound?.sessionID === owner.sessionID) {
          const old = bound;
          observed.set(owner.sessionID, owner);
          if (disposed || desired?.sessionID !== old.sessionID) {
            await bridge("agent.session.unbind", old.sessionID, {});
          }
          if (bound === old) bound = undefined;
        }
        return;
      }
      await bridge("agent.session.start", owner.sessionID, {
        cwd: snapshot.cwd || desired.cwd,
        title: snapshot.title || desired.title,
        running: snapshot.status === "working" || snapshot.status === "attention",
      });
      bound = owner;
      observed.set(owner.sessionID, owner);
      await reconcile(snapshot, owner, true);
      const buffered = pending;
      pending = [];
      for (const signal of buffered) {
        if (matches(signal, owner)) await forward(signal);
      }
    }
    async function forward(signal) {
      if (!matches(signal, bound)) return;
      if (signal.topic === "agent.session.unbind") {
        await bridge("agent.session.unbind", signal.sessionID, {});
        evictedSessionID = signal.sessionID;
        bound = undefined;
        return;
      }
      if (signal.revision <= revision) return;
      await bridge(signal.topic, signal.sessionID, { ...signal.payload, pane_owner: false });
      revision = signal.revision;
      if (signal.topic === "agent.session.end") {
        await bridge("agent.session.unbind", signal.sessionID, {});
        observed.delete(signal.sessionID);
        bound = undefined;
      } else if (signal.topic === "agent.cwd.changed") {
        bound.cwd = signal.payload.cwd;
      } else if (signal.topic === "agent.session.renamed") {
        bound.title = signal.payload.title;
      }
    }
    function update(explicitView = false) {
      const next = selected();
      if (explicitView || next?.sessionID !== desired?.sessionID) evictedSessionID = undefined;
      desired = next;
      const expected = ++intent;
      void enqueue(() => synchronize(expected));
    }
    async function recoverDetached() {
      for (const [sessionID, owner] of observed) {
        if (bound?.sessionID === sessionID) continue;
        let snapshot;
        try {
          snapshot = await rpc.observe(identity(owner), options);
        } catch (error) {
          if (!(error instanceof Error) || !error.message.includes("OpenCode root session not found")) {
            throw error;
          }
          console.error("wt-agent-hooks: detached session lookup failed", error);
          continue;
        }
        if (snapshot.status === "deleted") {
          await bridge("agent.session.end", sessionID, { reason: "deleted", pane_owner: false });
          observed.delete(sessionID);
        }
      }
    }
    function receive(event) {
      const signal = event.data;
      if (event.location && context.location &&
          (event.location.directory !== context.location.directory ||
           event.location.workspaceID !== context.location.workspaceID)) return;
      if (signal.clientID !== clientID || signal.terminalID !== terminalID ||
          signal.paneID !== paneID) return;
      if (signal.topic === "agent.session.end" && !matches(signal, bound) &&
          matches(signal, observed.get(signal.sessionID))) {
        void enqueue(async () => {
          if (!matches(signal, observed.get(signal.sessionID))) return;
          await bridge("agent.session.end", signal.sessionID, { ...signal.payload, pane_owner: false });
          observed.delete(signal.sessionID);
        });
        return;
      }
      if (!bound || matches(signal, bound)) {
        if (!bound) pending.push(signal);
        else void enqueue(() => forward(signal));
      }
    }
    async function watch() {
      while (!disposed) {
        try {
          for await (const event of context.client.event.subscribe({ signal: controller.signal })) {
            if (disposed) return;
            if (event.type === "server.connected") {
              connected = true;
              const expected = ++intent;
              await enqueue(async () => {
                await synchronize(expected, true);
                await recoverDetached();
              });
            } else if (event.type === "rpc.wt-agent-hooks.signal") {
              receive(event);
            }
          }
        } catch (error) {
          if (!disposed) console.error("wt-agent-hooks: server event stream failed", error);
        }
        connected = false;
        if (!disposed) await new Promise((resolve) => setTimeout(resolve, 1000));
      }
    }
    const stopRouteEffect = createRoot((stop) => {
      createEffect(() => { update(); });
      return stop;
    });
    const onViewed = context.data.on("session.viewed", ({ data }) =>
      update(data?.sessionID === selected()?.sessionID));
    const onCreated = context.data.on("session.created", () => update());
    const onDeleted = context.data.on("session.deleted", () => update());
    const watcher = watch();
    const refresh = setInterval(() => {
      if (disposed || !connected) return;
      void enqueue(async () => {
        if (!bound) {
          await synchronize(intent);
        } else {
          const snapshot = await rpc.snapshot(identity(bound), options);
          if (snapshot.revision > revision) await reconcile(snapshot, bound, false);
        }
        await recoverDetached();
      });
    }, 30000);

    const dispose = async () => {
      if (disposed) return delivery;
      stopRouteEffect();
      onViewed();
      onCreated();
      onDeleted();
      clearInterval(refresh);
      controller.abort();
      connected = true;
      disposed = true;
      try {
        await enqueue(async () => {
          try {
            await release();
          } finally {
            await rpc.forget({ clientID }, options);
            observed.clear();
          }
        });
      } finally {
        await watcher;
      }
      await delivery;
    };
    dispose.flush = () => delivery;
    return dispose;
  },
};

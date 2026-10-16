// Managed by Intelligent Terminal: wt-agent-hooks

const text = { type: "string", minLength: 1 };
const identity = {
  clientID: text,
  terminalID: text,
  paneID: text,
  sessionID: text,
  generation: { type: "integer", minimum: 1 },
};
const binding = {
  type: "object",
  properties: identity,
  required: Object.keys(identity),
  additionalProperties: false,
};
const snapshot = {
  type: "object",
  properties: {
    revision: { type: "integer", minimum: 0 },
    status: { type: "string", enum: ["unknown", "idle", "working", "attention", "error", "deleted"] },
    cwd: { type: "string" },
    title: { type: "string" },
    error: { type: "string" },
    notification: { type: "string" },
  },
  required: ["revision", "status", "cwd", "title", "error", "notification"],
  additionalProperties: false,
};

export const bindingRpc = {
  id: "wt-agent-hooks",
  methods: {
    bind: {
      input: {
        ...binding,
        properties: {
          ...identity,
          cwd: text,
          title: { type: "string" },
          running: { type: "boolean" },
        },
        required: [...binding.required, "cwd", "title", "running"],
      },
      output: snapshot,
    },
    unbind: {
      input: binding,
      output: {
        type: "object",
        properties: { detached: { type: "boolean" } },
        required: ["detached"],
        additionalProperties: false,
      },
    },
    snapshot: {
      input: binding,
      output: snapshot,
    },
    observe: {
      input: binding,
      output: snapshot,
    },
    forget: {
      input: {
        type: "object",
        properties: { clientID: text },
        required: ["clientID"],
        additionalProperties: false,
      },
      output: {
        type: "object",
        properties: {},
        additionalProperties: false,
      },
    },
  },
  events: {
    signal: {
      schema: {
        type: "object",
        properties: {
          ...identity,
          revision: { type: "integer", minimum: 0 },
          topic: {
            type: "string",
            enum: [
              "agent.session.unbind",
              "agent.session.renamed",
              "agent.cwd.changed",
              "agent.prompt.submit",
              "agent.tool.starting",
              "agent.stop",
              "agent.notification",
              "agent.error",
              "agent.session.end",
            ],
          },
          payload: { type: "object", additionalProperties: true },
        },
        required: [...binding.required, "revision", "topic", "payload"],
        additionalProperties: false,
      },
    },
  },
};

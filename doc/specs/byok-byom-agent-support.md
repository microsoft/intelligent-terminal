# BYOK/BYOM support across built-in agents

Last researched: 2026-07-27

This document records the upstream, vendor-documented BYOK/BYOM behavior of
the agent CLIs built into Intelligent Terminal. It intentionally starts from
upstream documentation rather than from Intelligent Terminal's implementation.

## Terminology and scope

- **BYOK** means supplying credentials for a model provider or gateway.
- **BYOM** means selecting a model or endpoint that is not the agent's default.
- **Agent pane** means the ACP process launched by `wta-master`.
- **Delegate** means the normal interactive CLI launched by `?<prompt>`.
- "OpenAI-compatible" is not one protocol. The two relevant APIs are:
  - **Chat Completions**: `POST /v1/chat/completions`
  - **Responses**: `POST /v1/responses`

An endpoint that implements one API is not necessarily compatible with the
other.

## Summary matrix

| Agent | Upstream BYOK/BYOM support | Primary configuration surfaces | Provider protocol used by custom endpoints | Fit for Intelligent Terminal's current shared provider |
|---|---|---|---|---|
| GitHub Copilot CLI | Yes | Environment variables; `--model` | OpenAI Chat Completions, Azure OpenAI, or Anthropic | Supported for the OpenAI Chat Completions subset |
| Claude Code | Yes | Login/setup commands, environment variables, settings files, `--model`, `/model` | Anthropic Messages API, Bedrock, Vertex AI, Microsoft Foundry | Not compatible with a generic OpenAI endpoint without an Anthropic-format gateway |
| OpenAI Codex CLI | Yes | `config.toml`, profiles, `-c`/`--config`, `--model`; adapter environment variables in ACP mode | OpenAI Responses API for custom providers | Not supported by the current Chat Completions-only shared provider |
| Gemini CLI | Yes | Login flow, environment variables, `.env`, settings files, `--model` | Gemini API or Vertex AI; custom Gemini/Vertex gateway URL | Not compatible with a generic OpenAI endpoint |
| OpenCode | Yes | `/connect`, config files, `OPENCODE_CONFIG_CONTENT`, environment substitution, `/models` | Chat Completions or Responses, selected by provider package | Current integration supports only the Chat Completions package |
| Custom ACP command | Agent-specific | Agent-specific | Agent-specific | No safe generic injection contract |

## GitHub Copilot CLI

### Ground truth

Copilot CLI documents three custom provider types:

- `openai`: OpenAI, Ollama, vLLM, Foundry Local, and other **OpenAI Chat
  Completions** compatible endpoints.
- `azure`: Azure OpenAI.
- `anthropic`: Anthropic.

The provider is configured before process startup:

| Mechanism | Purpose |
|---|---|
| `COPILOT_PROVIDER_BASE_URL` | Provider endpoint |
| `COPILOT_PROVIDER_TYPE` | `openai`, `azure`, or `anthropic`; defaults to `openai` |
| `COPILOT_PROVIDER_API_KEY` | Provider API key; optional for keyless local endpoints |
| `COPILOT_MODEL` | Provider model identifier |
| `--model` | CLI alternative to `COPILOT_MODEL` |
| `COPILOT_OFFLINE=true` | Prevent Copilot CLI from contacting GitHub services |

The selected model must support streaming and tool/function calling.

### Intelligent Terminal mapping

The implementation sets:

```text
COPILOT_PROVIDER_BASE_URL=<configured URL>
COPILOT_PROVIDER_TYPE=openai
COPILOT_PROVIDER_API_KEY=<credential, when present>
COPILOT_MODEL=<configured model>
COPILOT_OFFLINE=true
```

This is correct for the documented OpenAI Chat Completions subset. It does not
expose Copilot CLI's separate `azure` or `anthropic` provider types.

### Official source

- [Using your own LLM models in GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/use-byok-models)

## Claude Code

### Ground truth

Claude Code supports several BYOK/provider paths, but they use Anthropic or
cloud-provider protocols rather than a generic OpenAI-compatible API.

#### Anthropic API or an Anthropic-format gateway

| Mechanism | Purpose |
|---|---|
| `ANTHROPIC_BASE_URL` | Override the Anthropic API endpoint with a proxy or gateway |
| `ANTHROPIC_API_KEY` | Credential sent in the `x-api-key` header |
| `ANTHROPIC_AUTH_TOKEN` | Credential sent as a bearer token |
| `ANTHROPIC_MODEL` | Model name or gateway-specific model identifier |
| `claude --model <model>` | Per-launch model selection |
| `/model` | In-session model selection |
| `model` in Claude settings | Persistent model selection |

Claude's gateway documentation requires a supported Anthropic-format API and
explicitly does not support routing Claude Code to non-Claude models through a
gateway.

#### Amazon Bedrock

Claude Code can be configured interactively with `/setup-bedrock` or through
environment/settings values including:

- `CLAUDE_CODE_USE_BEDROCK=1`
- AWS's standard credential chain, such as `AWS_PROFILE`,
  `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, or
  `AWS_BEARER_TOKEN_BEDROCK`
- `AWS_REGION`
- optional `ANTHROPIC_BEDROCK_BASE_URL`
- model pins such as `ANTHROPIC_DEFAULT_SONNET_MODEL`

#### Google Cloud's Agent Platform / Vertex AI

Claude Code can be configured with `/setup-vertex` or:

- `CLAUDE_CODE_USE_VERTEX=1`
- `ANTHROPIC_VERTEX_PROJECT_ID`
- `CLOUD_ML_REGION`
- Google Application Default Credentials or `GOOGLE_APPLICATION_CREDENTIALS`
- optional `ANTHROPIC_VERTEX_BASE_URL`
- model pins such as `ANTHROPIC_DEFAULT_SONNET_MODEL`

#### Microsoft Foundry

Claude Code uses:

- `CLAUDE_CODE_USE_FOUNDRY=1`
- `ANTHROPIC_FOUNDRY_RESOURCE` or `ANTHROPIC_FOUNDRY_BASE_URL`
- `ANTHROPIC_FOUNDRY_API_KEY`, `ANTHROPIC_FOUNDRY_AUTH_TOKEN`, or the Azure
  default credential chain
- model deployment names through the Anthropic model-selection variables

### Intelligent Terminal mapping

The shared provider currently marks Claude as unsupported. That is correct for
the current **generic OpenAI-compatible** provider shape, but it must not be
described as "Claude Code does not support BYOK." Claude Code has extensive
native BYOK support; it requires a separate provider-specific integration and
additional fields rather than another value in the shared contract field.

The ACP adapter uses the official Claude Agent SDK and inherits the process
environment when it launches the agent subprocess. It adds deployment-oriented
variables such as `CLAUDE_CODE_EXECUTABLE` and `CLAUDE_MODEL_CONFIG`, but it
does not define a generic OpenAI-compatible BYOK schema. Provider configuration
should therefore follow the Claude/SDK environment and settings contract.

### Official sources

- [Claude Code environment variables](https://code.claude.com/docs/en/env-vars)
- [Claude Code model configuration](https://code.claude.com/docs/en/model-config)
- [Connect Claude Code to an LLM gateway](https://code.claude.com/docs/en/llm-gateway-connect)
- [Claude Code on Amazon Bedrock](https://code.claude.com/docs/en/amazon-bedrock)
- [Claude Code on Google Cloud's Agent Platform](https://code.claude.com/docs/en/google-vertex-ai)
- [Claude Code on Microsoft Foundry](https://code.claude.com/docs/en/microsoft-foundry)
- [Claude ACP adapter](https://github.com/agentclientprotocol/claude-agent-acp)

## OpenAI Codex CLI

### Ground truth

Codex supports persistent configuration in `~/.codex/config.toml`, profiles,
and per-launch overrides with `-c`/`--config`. A custom provider is selected by
setting `model_provider` and defining a matching `model_providers.<id>` table:

```toml
model = "provider-model-id"
model_provider = "example"

[model_providers.example]
name = "Example"
base_url = "https://example.test/v1"
env_key = "EXAMPLE_API_KEY"
wire_api = "responses"
```

Relevant provider fields include:

- `base_url`
- `env_key`
- `wire_api = "responses"`
- optional query parameters and HTTP headers
- optional command-backed bearer-token authentication

Codex also has built-in provider-specific support, including local
Ollama/LM Studio operation, Amazon Bedrock, and Azure configuration.

The ACP adapter adds its own process-level configuration contract:

| Adapter mechanism | Purpose |
|---|---|
| `CODEX_CONFIG` | JSON object merged into the Codex session configuration |
| `MODEL_PROVIDER` | Provider selected for new sessions |
| `CODEX_API_KEY` / `OPENAI_API_KEY` | Adapter API-key authentication |
| `CODEX_PATH` | Override the bundled Codex executable |

### Intelligent Terminal mapping

Intelligent Terminal does not map the shared provider into Codex. The current
shared provider contract is deliberately limited to OpenAI-compatible Chat
Completions endpoints, while Codex custom providers require the Responses API.
Saved providers remain available for Copilot and OpenCode when the user
switches agents.

### Official sources

- [Codex advanced configuration: custom model providers](https://learn.chatgpt.com/docs/config-file/config-advanced#custom-model-providers)
- [Codex configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
- [Codex ACP adapter](https://github.com/agentclientprotocol/codex-acp)

## Gemini CLI

### Ground truth

Gemini CLI supports Google account login, Gemini API keys, and Vertex AI.

| Mechanism | Purpose |
|---|---|
| `GEMINI_API_KEY` | Gemini API key |
| `GOOGLE_API_KEY` | Vertex AI API-key authentication |
| `GOOGLE_APPLICATION_CREDENTIALS` | Vertex/service-account credential file |
| `GOOGLE_CLOUD_PROJECT` | Vertex AI project |
| `GOOGLE_CLOUD_LOCATION` | Vertex AI location |
| `GOOGLE_GEMINI_BASE_URL` | Override the Gemini API base URL for a Gemini-format gateway |
| `GOOGLE_VERTEX_BASE_URL` | Override the Vertex base URL |
| `GEMINI_MODEL` | Model selected when no CLI flag is present |
| `--model` | Highest-priority launch-time model selection |
| `model.name` in `settings.json` | Persistent model selection |

Gemini CLI also loads environment values from `.gemini/.env` and supports user,
project, and system settings files.

The custom base URLs still expect Gemini or Vertex semantics. The fact that
Google separately exposes an OpenAI-compatible Gemini API does not make Gemini
CLI an arbitrary OpenAI-compatible client.

Gemini's native ACP implementation also advertises an `AI API Gateway`
authentication method. Its ACP metadata fixes the gateway protocol to
`google`; the client can provide a base URL and headers, but cannot select an
OpenAI protocol.

### Intelligent Terminal mapping

The shared provider currently marks Gemini as unsupported. That is correct for
the current generic OpenAI-compatible provider contract. A future Gemini
integration would need a separate Gemini/Vertex-specific configuration surface,
authentication mode, and project/location fields instead of another shared
contract value.

### Official sources

- [Gemini CLI authentication](https://github.com/google-gemini/gemini-cli/blob/main/docs/get-started/authentication.mdx)
- [Gemini CLI configuration](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md)
- [Gemini CLI model routing and selection precedence](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/model-routing.md)

## OpenCode

### Ground truth

OpenCode supports provider credentials through `/connect`, stored in
`~/.local/share/opencode/auth.json`, and provider configuration in
`opencode.json`/`opencode.jsonc`.

Runtime configuration can also be supplied with:

- `OPENCODE_CONFIG`: path to a custom config file
- `OPENCODE_CONFIG_CONTENT`: inline JSON/JSONC with high precedence
- `{env:VARIABLE_NAME}` substitutions inside config

A generic Chat Completions-compatible provider uses:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "model": "example/model-id",
  "provider": {
    "example": {
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://example.test/v1",
        "apiKey": "{env:EXAMPLE_API_KEY}"
      },
      "models": {
        "model-id": {
          "name": "model-id"
        }
      }
    }
  }
}
```

The underlying `@ai-sdk/openai-compatible` language model is a chat model and
sends requests to the Chat Completions-compatible API. Upstream OpenCode can
instead use `@ai-sdk/openai` for a Responses-shaped custom endpoint.

### Intelligent Terminal mapping

The implementation injects an inline `OPENCODE_CONFIG_CONTENT` provider using
`@ai-sdk/openai-compatible`, references the API key through an environment
substitution, and selects `<provider>/<model>`. This matches the documented
OpenCode Chat Completions configuration model and keeps the secret out of
persisted JSON. It does not select `@ai-sdk/openai` for Responses providers.

### Official sources

- [OpenCode providers](https://opencode.ai/docs/providers/)
- [OpenCode configuration and inline config precedence](https://opencode.ai/docs/config/)
- [AI SDK OpenAI-compatible provider](https://ai-sdk.dev/providers/openai-compatible-providers)

## Custom ACP commands

There is no universal BYOK mechanism for a custom ACP command. A custom agent
may use environment variables, CLI flags, a config file, an ACP authentication
method, or no configurable provider at all.

Intelligent Terminal should not inject the shared provider into a custom
command. The persisted shared-provider contract is the canonical
`openai-compatible` value, but custom ACP commands have no defined adapter
mapping for it.

## Implementation review findings

Reviewed branch: `dev/vanzue/byok-model-runtime` at
`33bd741f3a31973789d6cd0a38374c394381ff63`.

### Correct mappings

- Copilot's OpenAI provider environment variables match upstream names.
- OpenCode's inline config, provider package, environment substitution, and
  model identifier shape match upstream.
- API keys are resolved from Windows Credential Manager only when launching the
  selected supported agent.
- Claude, Codex, Gemini, and custom agents do not receive the shared provider
  metadata or secret.

### Issues and limitations

#### 1. The persisted API contract is canonical

`CustomModelProvider.ApiContract` has one accepted persisted value:
`openai-compatible`. Missing or blank legacy values normalize to that value.
Unsupported values are not shown in Settings or model pickers and cannot
populate the launch environment; WTA applies the same validation to
helper/master model metadata.

This is a provider compatibility contract, not a user-selectable wire-format
switch. Agent adapters render the same provider through the API shape they
support:

- Copilot and OpenCode use Chat Completions-compatible configuration.
- Codex uses its Responses `wire_api`.

Separate `openai-chat-completions` and `openai-responses` persisted values would
incorrectly expose adapter implementation details as different provider
contracts.

#### 2. "Unsupported" describes the integration, not the upstream agent

Claude and Gemini both support native BYOK/BYOM. They are unsupported only by
the current shared **OpenAI-compatible** provider abstraction.

**Recommended fix:** keep the runtime gating, but use user-facing wording and
code comments that distinguish "shared provider unsupported" from "agent has no
BYOK support."

#### 3. Copilot provider coverage is intentionally narrower than upstream

Copilot CLI also supports `azure` and `anthropic`, while Intelligent Terminal
always sets `COPILOT_PROVIDER_TYPE=openai`.

**Recommended fix:** document the current scope as OpenAI Chat Completions. Add
a provider-type field only if Azure OpenAI and Anthropic are intended features.

#### 4. The shared provider affects the ACP agent pane, not delegate launches

Provider adaptation happens when WTA spawns the ACP agent process. The delegate
command builder only applies the delegate model flag and does not translate the
shared provider into agent-specific environment/config.

This is observable when the same agent, especially the default `copilot`
agent, is selected for both surfaces: the agent pane uses the configured BYOK
endpoint, while `?<prompt>` launches the normal CLI without
`COPILOT_PROVIDER_*` or `OPENCODE_CONFIG_CONTENT`. The delegate
can therefore fail for a user who has only BYOK credentials, or use a different
hosted provider/model than the visible agent pane.

**Recommended fix:** state this scope in Settings and product documentation. If
delegate BYOK is required later, configure its process environment separately
and account for Windows, PowerShell, and WSL launch paths.

#### 5. API keys necessarily enter the agent process environment

The credential is not persisted in generated Codex/OpenCode config, but it is
placed in the selected agent process environment because all three upstream
contracts require process-visible credentials.

**Recommended fix:** retain the current per-agent injection and scrubbing, and
document that tools or subprocesses spawned by the agent may inherit the key
unless the upstream agent filters its child environment.

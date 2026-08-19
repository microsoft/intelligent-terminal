# Agent-native BYOK configuration

Last researched: 2026-08-19

## Summary

Intelligent Terminal should configure each agent through that agent's native
BYOK surface instead of becoming a general-purpose model client.

The user-facing abstraction should be:

1. A **model connection**, representing an account or endpoint and its
   credential.
2. An **agent model binding**, selecting a connection and model for one agent.

Users should not select an implementation adapter. For a known service,
Intelligent Terminal selects the correct agent-specific configuration
automatically. Only a custom endpoint may require the user to select an
**API format** when it cannot be detected safely.

Internally, an agent BYOK configurator translates a connection and model into
the environment variables, arguments, or temporary configuration understood
by the selected agent. The agent remains responsible for authentication
refresh, provider behavior, wire-protocol requests, streaming, tool calling,
and inference errors.

```text
Settings
  |
  +-- Model connection
  |     service, endpoint, credential reference
  |
  +-- Agent model binding
        agent, connection, selected model
                  |
                  v
        Agent BYOK configurator
        environment / arguments / temporary config
                  |
                  v
        Agent CLI or ACP adapter
                  |
                  v
        Agent-owned provider implementation
                  |
                  v
        Model endpoint
```

## Goals

- Let every built-in agent use the BYOK mechanisms it officially supports.
- Keep credentials separate from model selections.
- Support known cloud services, enterprise gateways, and local endpoints.
- Preserve deterministic startup: when BYOK is active, the selected agent must
  not silently fall back to its hosted service.
- Reuse model connections across compatible agents without pretending that all
  agents support the same protocols.
- Keep provider HTTP and inference protocol implementations inside the agent.
- Provide a clear escape hatch for endpoints whose model catalog cannot be
  discovered.

## Non-goals

- Implementing Chat Completions, Responses, Anthropic Messages, Gemini,
  Bedrock, or Vertex inference inside Intelligent Terminal.
- Defining a universal model-provider protocol.
- Inferring an endpoint's protocol from its URL and silently committing to the
  guess.
- Treating a model listed in a catalog as proof that the credential can invoke
  it or that it supports agent-required capabilities.

## Terminology

### ACP adapter

An ACP adapter exposes an agent CLI through Agent Client Protocol. It solves
the Terminal-to-agent communication problem.

Examples:

- Claude Code is hosted through `claude-agent-acp`.
- Codex is hosted through `codex-acp`.
- Copilot, Gemini, and OpenCode expose native ACP modes.

An ACP adapter is not a BYOK provider adapter.

### Agent BYOK configurator

An Intelligent Terminal component that translates normalized settings into an
agent's native configuration surface.

Examples:

- Copilot environment variables such as `COPILOT_PROVIDER_BASE_URL`.
- Claude environment variables for Anthropic, Bedrock, Vertex, or Foundry.
- Codex `config.toml` or adapter configuration.
- Gemini API or Vertex configuration.
- OpenCode `OPENCODE_CONFIG_CONTENT`.

This is the adapter layer Intelligent Terminal should own.

### Provider implementation

Code inside an agent that communicates with a model service using a concrete
wire protocol. OpenCode's AI SDK provider packages are one example.

Examples include:

- OpenAI Chat Completions
- OpenAI Responses
- Anthropic Messages
- Gemini `generateContent`
- Bedrock runtime APIs
- Vertex AI APIs

Intelligent Terminal should not reimplement these provider clients.

### Model connection

A user-visible connection to a known service or custom endpoint:

```text
OpenRouter - Work
  credential: saved
  endpoint: managed by the OpenRouter template
```

The connection owns service identity, endpoint, credential reference, and
service-specific options. It does not own an agent's default model.

### Agent model binding

The association between an agent, a model connection, and that agent's model
selection:

```text
GitHub Copilot
  connection: OpenRouter - Work
  model: anthropic/claude-sonnet-4
```

The same connection may be reused by another compatible agent with a different
model.

## Current Intelligent Terminal implementation

The current settings editor creates one `CustomModelProvider` from:

- Base URL
- Model ID
- optional API key

Base URL and Model ID are required at creation time. The API key is written to
Windows Credential Manager under an opaque credential ID. Settings persist the
credential ID, not the secret.

Relevant implementation:

- `src/cascadia/TerminalSettingsEditor/AIAgentsViewModel.cpp`
- `src/cascadia/inc/CustomModelCredential.h`
- `src/cascadia/TerminalSettingsModel/CustomModelProvider.cpp`

Adding a provider alone does not activate it. The user must select its
`custom:<provider-id>:<model-id>` entry. Terminal then supplies WTA with:

```text
WTA_CUSTOM_MODEL_BASE_URL
WTA_CUSTOM_MODEL_ID
WTA_CUSTOM_MODEL_CREDENTIAL_ID
WTA_CUSTOM_MODEL_API_KEY_REQUIRED
```

WTA resolves the credential only while spawning a supported agent process.
Unsupported agents have the shared-provider metadata removed.

### Current Copilot mapping

WTA sets:

```text
COPILOT_PROVIDER_BASE_URL=<endpoint>
COPILOT_PROVIDER_TYPE=openai
COPILOT_PROVIDER_API_KEY=<resolved credential, when present>
COPILOT_MODEL=<model>
COPILOT_OFFLINE=true
```

It then launches Copilot's ACP mode. Intelligent Terminal does not invoke
Copilot login, provider discovery, or a Models API for this path.

### Current OpenCode mapping

WTA generates an inline OpenCode configuration containing:

- an `intelligent-terminal` provider;
- `@ai-sdk/openai-compatible`;
- the configured base URL;
- an API key environment reference;
- exactly the configured model;
- a root `model` selection pointing to that provider and model.

The inline configuration is supplied through `OPENCODE_CONFIG_CONTENT`, and
the resolved key is supplied through
`INTELLIGENT_TERMINAL_MODEL_API_KEY`.

Intelligent Terminal does not call OpenCode `/connect`, write OpenCode's
`auth.json`, query Models.dev, or use OpenCode's provider catalog for this
path.

### Assessment

The current process-launch design is a sound first implementation:

- credentials are stored outside settings JSON;
- only selected and supported agents receive credentials;
- the selected model is deterministic;
- failure to resolve a required credential is explicit;
- Copilot cannot silently fall back to hosted models;
- OpenCode user-owned configuration files are not modified.

Its settings model is not a sufficient long-term abstraction:

- provider creation requires a model too early;
- no credential or endpoint verification occurs before agent startup;
- no model catalog is available beyond user-entered records;
- the shared contract is limited to OpenAI-compatible Chat Completions;
- agent-native BYOK surfaces for Claude, Codex, and Gemini are not represented;
- Copilot and OpenCode capabilities are reduced to their lowest common subset.

The current Copilot and OpenCode launch mappings should be retained and
refactored into agent BYOK configurators.

## Built-in agent capability matrix

| Agent | ACP hosting | Upstream BYOK configuration | Primary model protocols | Current shared BYOK |
|---|---|---|---|---|
| GitHub Copilot | Native ACP | Provider environment variables and model selection | OpenAI-compatible, Azure OpenAI, Anthropic | OpenAI Chat Completions subset |
| Claude Code | `claude-agent-acp` | Anthropic/gateway variables, Bedrock, Vertex, Foundry | Anthropic Messages and cloud-provider runtimes | Unsupported |
| OpenAI Codex | `codex-acp` | `config.toml`, profiles, command-line overrides, adapter configuration | OpenAI Responses primarily; provider-specific runtimes | Unsupported |
| Gemini CLI | Native ACP | Google login, Gemini API key, Vertex credentials, gateway configuration | Gemini, Vertex, Code Assist | Unsupported |
| OpenCode | Native ACP | `/connect`, `auth.json`, provider configuration, AI SDK adapters | Provider-dependent | OpenAI Chat Completions subset |

Custom ACP commands remain agent-specific. Intelligent Terminal cannot safely
inject a shared BYOK connection into an arbitrary command without an explicit
configuration contract.

## Agent-native BYOK surfaces

### GitHub Copilot CLI

Copilot supports provider-specific process configuration. Its local BYOK flow
requires a model when the process starts.

Representative settings:

```text
COPILOT_PROVIDER_TYPE=openai|azure|anthropic
COPILOT_PROVIDER_BASE_URL=...
COPILOT_PROVIDER_API_KEY=...
COPILOT_MODEL=...
COPILOT_OFFLINE=true
```

The `openai`, `azure`, and `anthropic` provider types are not interchangeable.
The current implementation always selects `openai`.

Product consequence: a Copilot BYOK binding cannot become active until a model
has been selected. A model connection may be saved without a model, but
Copilot startup must remain blocked from using that connection until the
binding is ready.

Official documentation:

- <https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/use-byok-models>
- <https://docs.github.com/en/copilot/how-tos/copilot-sdk/auth/byok>

### Claude Code

Claude Code supports multiple native backends:

| Backend | Typical configuration | Protocol/runtime |
|---|---|---|
| Anthropic API or gateway | `ANTHROPIC_BASE_URL`, API key or auth token, model settings | Anthropic Messages |
| Amazon Bedrock | AWS credential chain, region, Bedrock selection | Bedrock runtime |
| Google Vertex AI | ADC/service account, project, region | Vertex |
| Microsoft Foundry | resource URL/name, API key/token/Entra | Foundry Anthropic endpoint |

Claude model aliases and defaults mean a full provider model ID is not always
required before startup. Gateway model discovery can be enabled for supported
Anthropic-format gateways, but it is not a generic OpenAI model discovery
mechanism.

Official documentation:

- <https://code.claude.com/docs/en/authentication>
- <https://code.claude.com/docs/en/model-config>
- <https://code.claude.com/docs/en/llm-gateway>
- <https://code.claude.com/docs/en/llm-gateway-protocol#model-discovery>
- <https://code.claude.com/docs/en/amazon-bedrock>
- <https://code.claude.com/docs/en/google-vertex-ai>
- <https://code.claude.com/docs/en/microsoft-foundry>

### OpenAI Codex

Codex separates model selection from provider configuration:

```toml
model = "provider-model-id"
model_provider = "example"

[model_providers.example]
base_url = "https://example.test/v1"
env_key = "EXAMPLE_API_KEY"
wire_api = "responses"
```

Provider configuration can include authentication references, headers, query
parameters, retries, and wire API selection. Responses is the primary custom
provider protocol; a Chat Completions-only connection must not be assumed
compatible.

Official documentation:

- <https://developers.openai.com/codex/auth/>
- <https://developers.openai.com/codex/config-advanced/#custom-model-providers>
- <https://developers.openai.com/codex/models/>

### Gemini CLI

Gemini supports Google-specific authentication and model backends:

| Backend | Typical configuration |
|---|---|
| Gemini Developer API | `GEMINI_API_KEY` |
| Vertex AI | ADC/service account, project, location |
| Google Code Assist | Google login |
| Gemini gateway | Gemini gateway URL and headers |

These paths use Gemini, Vertex, or Code Assist semantics. The existence of an
OpenAI-compatible Gemini endpoint does not make Gemini CLI a generic
OpenAI-compatible client.

Official documentation:

- <https://google-gemini.github.io/gemini-cli/docs/get-started/authentication.html>
- <https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md>
- <https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/model-routing.md>

### OpenCode

OpenCode separates authentication, provider metadata, and model selection:

1. `/connect` stores provider credentials.
2. A provider becomes available when credentials, environment, a plugin, or
   explicit configuration activates it.
3. `/models` displays a catalog assembled from Models.dev, plugins, and user
   configuration.
4. Startup chooses an explicit model, configured default, last-used model, or
   an internally prioritized model.

For most built-in providers, entering a key appears to reveal models because
OpenCode already has a provider and model catalog. It does not generally call
the provider's Models API with that key.

For `Other` or a generic OpenAI-compatible provider, saving a key is
insufficient. The user must also configure the provider adapter, base URL, and
model IDs.

Official documentation and implementation:

- <https://opencode.ai/docs/providers/>
- <https://opencode.ai/docs/models/>
- <https://opencode.ai/docs/config/>
- <https://github.com/anomalyco/opencode/blob/da4730e4a41dcbb2cb2d907dd2b06ac481b8f962/packages/tui/src/component/dialog-provider.tsx>
- <https://github.com/anomalyco/opencode/blob/da4730e4a41dcbb2cb2d907dd2b06ac481b8f962/packages/opencode/src/provider/provider.ts>

## Comparative research: Qwen Code

Qwen Code is not currently a built-in Intelligent Terminal agent, but its
provider setup is a useful product comparison.

Qwen Code's `/auth` command, also exposed as `/connect` and `/login`, offers
known provider presets and a custom provider path.

For a known provider, one installation flow writes:

- endpoint and protocol;
- credential;
- preset model records;
- default model;
- provider configuration.

This makes models appear immediately after a key is entered, but the models
usually come from the preset rather than a live provider Models API.

For a custom provider, the user selects OpenAI, Anthropic, or Gemini protocol,
enters a base URL and credential, and must enter at least one model ID. Qwen
Code does not provide generic endpoint model discovery.

Representative official sources:

- <https://github.com/QwenLM/qwen-code>
- <https://github.com/QwenLM/qwen-code/blob/daa7d619907f2f73038cb0b1132c6f6cbfde6431/packages/core/src/providers/all-providers.ts>
- <https://github.com/QwenLM/qwen-code/blob/daa7d619907f2f73038cb0b1132c6f6cbfde6431/packages/cli/src/ui/auth/useProviderSetupFlow.ts>
- <https://github.com/QwenLM/qwen-code/blob/daa7d619907f2f73038cb0b1132c6f6cbfde6431/packages/core/src/models/modelRegistry.ts>

## User abstraction

### Adding a known model connection

```text
Add model connection

Service: OpenRouter
Name: OpenRouter - Work
API key: ********
```

The service template determines endpoint defaults, authentication shape, and
known API formats. The user does not select a provider adapter.

Saving a connection does not automatically make it the active model source for
every agent.

### Binding a connection to an agent

```text
Agent: GitHub Copilot

Model source:
  (*) Use the agent's own account
  ( ) Use a model connection

Connection: OpenRouter - Work
Model: anthropic/claude-sonnet-4
```

The available connection list is filtered by agent compatibility. For
example, a connection that only exposes Anthropic Messages should not be
offered to a Codex configuration that requires Responses.

The same connection can have independent bindings:

```text
OpenRouter - Work
  +-- Copilot: anthropic/claude-sonnet-4
  +-- OpenCode: qwen/qwen3-coder
```

### Connection and binding states

Connection state and model readiness must remain separate:

```text
Connection
  Saved       endpoint and credential reference persisted
  Connected   optional remote credential check succeeded
  Unknown     no supported credential check is available
  Rejected    remote authentication failed

Catalog
  Registry    known catalog is available
  Discovered  account or endpoint catalog was fetched
  Manual      user-entered model records
  Stale       cached catalog is usable but refresh failed
  Unsupported no supported discovery mechanism exists

Binding
  Incomplete  no usable model selected when one is required
  Ready       configurator can produce a deterministic launch
  Active      the agent is currently using the binding
  Failed      the agent rejected the configuration or model
```

A saved credential, a populated model picker, and successful inference are
three different facts.

## Custom endpoint experience

A URL alone does not identify its API format. Unknown endpoints should use a
detect-then-ask flow.

```text
Add model connection

Service: Custom endpoint
URL: http://localhost:11434/v1
API key: optional

[Detect connection]
```

Detection may use safe, non-inference requests implemented by a service
template:

- OpenAI-style `GET /models`
- Ollama `GET /api/tags`
- LM Studio's OpenAI-compatible Models API
- provider-specific account catalog APIs

Detection must not issue a billable inference request.

### Recognized endpoint

```text
Detected: Ollama
API format: OpenAI Chat Completions compatibility
Authentication: none
Models: qwen3:8b, deepseek-r1:14b
```

The user can review and override the result.

### Catalog found, protocol ambiguous

A successful OpenAI-style Models API does not prove whether inference uses
Chat Completions or Responses.

```text
Models were found, but the inference API format could not be determined.

API format:
  (*) OpenAI Chat Completions
  ( ) OpenAI Responses
  ( ) Anthropic Messages
  ( ) Gemini API
```

`API format` is the only protocol-level choice exposed to users of a custom
endpoint. The product must not expose internal adapter names.

### Detection unavailable

```text
The endpoint could not be detected.

API format: OpenAI Chat Completions
Model ID: [                         ]
```

Manual model entry must remain available because:

- many compatible endpoints omit a Models API;
- a credential may have inference permission but not catalog permission;
- private deployments can rename models;
- a registry can lag behind a provider release;
- Azure deployment names and cloud resource identifiers may not map cleanly
  to a generic model catalog.

### OpenAI Models API scope

`GET /v1/models` is an OpenAI API operation, but "OpenAI-compatible" is not a
formal conformance level requiring every implementation to expose it. A
service may implement Chat Completions without implementing model discovery.

OpenRouter implements an OpenAI-style `/models` endpoint and adds richer
metadata. It also exposes account-oriented model behavior beyond the generic
OpenAI shape. Such extensions should be implemented by an OpenRouter service
template, not assumed for every endpoint.

References:

- <https://platform.openai.com/docs/api-reference/models/list>
- <https://openrouter.ai/docs/api/api-reference/models/list-all-models-and-their-properties>

## Proposed internal model

The persisted representation should keep service connections and agent
bindings separate.

```text
ModelConnection
  id
  name
  serviceKind
  endpoint
  credentialRef
  serviceOptions
  detectedApiFormats

ModelCatalogEntry
  connectionId
  modelId
  displayName
  source
  capabilities

ModelCatalogSnapshot
  connectionId
  entries
  fetchedAt
  expiresAt
  etag
  lastError

AgentModelBinding
  agentId
  connectionId
  requestedModel
  agentOptions
```

`serviceKind` represents a user-recognizable service such as `openrouter`,
`anthropic`, `azure-openai`, `bedrock`, `vertex`, or `custom`. It is not an
inference adapter implementation name.

For custom services, `serviceOptions` includes the user-confirmed API format.

### Agent BYOK configurator contract

Conceptually:

```text
configure(agent, connection, binding) -> AgentLaunchConfiguration

AgentLaunchConfiguration
  environment
  arguments
  temporaryConfig
  restartRequired
```

Implementations may include:

```text
CopilotOpenAIConfigurator
CopilotAzureConfigurator
CopilotAnthropicConfigurator

ClaudeAnthropicConfigurator
ClaudeBedrockConfigurator
ClaudeVertexConfigurator
ClaudeFoundryConfigurator

CodexResponsesConfigurator
CodexAzureConfigurator
CodexBedrockConfigurator

GeminiApiConfigurator
GeminiVertexConfigurator

OpenCodeProviderConfigurator
```

The compatibility registry maps `(agentId, serviceKind, apiFormat)` to a
configurator. Unsupported combinations are hidden or rejected before launch.

## Model catalog strategy

Catalog sources should be merged without overstating confidence:

```text
policy
  > user override
  > account discovery
  > service registry
  > built-in preset
```

Each model entry retains its source. A registry model is not labeled as
account-discovered.

Recommended cache behavior:

- use cached models immediately;
- refresh in the background;
- support ETag when the service does;
- keep stale models when a temporary refresh fails;
- invalidate on endpoint, credential, tenant, region, or service-template
  changes;
- never reuse an account-scoped catalog under another credential;
- provide Refresh and Enter model ID manually actions.

Model discovery is optional. It is not a prerequisite for supporting a
connection when the user can provide a valid model ID.

## Startup model rules

Different agents have different requirements:

- Copilot local BYOK requires a selected model before launch.
- OpenCode itself can select a configured, last-used, or internally prioritized
  model. Intelligent Terminal's injected custom provider still requires an
  explicit model to guarantee that the selected BYOK connection is used.
- Claude can often use an alias or backend default.
- Codex separates provider and model but custom providers commonly require an
  explicit model selection.
- Gemini supports configured models and automatic routing within its supported
  ecosystem.

An agent binding therefore declares whether it is ready according to its
configurator. The product must not apply one global "model is always required"
rule to connection creation.

## Credential ownership

Two credential patterns are required:

1. **Intelligent Terminal-owned secret reference**
   - API key stored in Windows Credential Manager.
   - Configurator injects it only into the selected agent process.
2. **Agent or platform-owned authentication**
   - OAuth login, AWS credential chain, GCP ADC, Entra ID, or an agent-managed
     credential store.
   - Intelligent Terminal records the selected authentication mode but does
     not copy the credential.

Secrets may need to enter an agent process environment because upstream agents
require it. Configurators must:

- inject only into supported agent processes;
- scrub unrelated provider variables;
- avoid logging secrets or rendered configurations containing secrets;
- preserve explicit errors for missing credential references;
- document that agent-owned child processes may inherit environment secrets.

## Migration from the current format

Existing records can be migrated without changing behavior:

```text
CustomModelProvider
  BaseUrl
  ApiKeyCredential
  Models[0]

becomes

ModelConnection
  serviceKind = custom
  apiFormat = openai-chat-completions
  endpoint = BaseUrl
  credentialRef = ApiKeyCredential

AgentModelBinding
  connectionId = migrated connection
  requestedModel = selected model
```

The first migration should preserve:

- the selected agent;
- the selected model;
- credential IDs;
- keyless local endpoints;
- current Copilot and OpenCode launch output.

Later releases can add service templates and agent configurators without
rewriting existing custom connections.

## Recommended delivery phases

### Phase 1: Separate connections and bindings

- Introduce model connections.
- Move Model ID out of connection creation.
- Migrate existing custom providers.
- Preserve current Copilot and OpenCode launch behavior.
- Show Incomplete until an agent binding has the model required by its
  configurator.

### Phase 2: Known service templates and catalogs

- Add OpenAI, OpenRouter, Anthropic, Azure OpenAI, Ollama, and LM Studio
  templates.
- Add registry, optional discovery, cache, and manual entry.
- Distinguish saved, connected, catalog, ready, and active states.

### Phase 3: Complete built-in agent configurators

- Add Copilot Azure and Anthropic.
- Add Claude Anthropic, Bedrock, Vertex, and Foundry.
- Add Codex Responses and supported cloud backends.
- Add Gemini API and Vertex.
- Expand OpenCode beyond the current OpenAI-compatible package.

### Phase 4: Policy and advanced routing

- Connection and model allowlists.
- Per-profile and per-session bindings.
- Alias, fallback, lightweight, planner, and subagent model selections where
  supported by the agent.
- Enterprise-managed connection templates without managed plaintext secrets.

## Decisions

1. Intelligent Terminal configures agents; it does not become an inference
   client.
2. Users select services and models, not adapters.
3. Known services hide protocol details.
4. Custom endpoints expose API format only when detection cannot establish it.
5. Connections and agent model bindings are separate persisted concepts.
6. Model discovery is optional and provider-specific.
7. A populated picker does not imply credential verification or entitlement.
8. Agent startup is deterministic whenever a BYOK binding is active.
9. Current Copilot and OpenCode process mappings remain valid configurator
   implementations.

## Open questions

- Should known-service credentials be managed exclusively by Intelligent
  Terminal, or should each service template prefer the agent's native login
  store when available?
- Which model catalogs can be redistributed or cached under their upstream
  licenses and terms?
- Should custom endpoint detection run automatically on URL changes or only
  after an explicit user action?
- How should multiple agents sharing one connection surface incompatible model
  capabilities?
- Which agent-reported ACP model options can be reused after BYOK startup, and
  which agents require restart-only model changes?
- How should enterprise policy manage connection templates, allowed endpoints,
  credential modes, and model allowlists?

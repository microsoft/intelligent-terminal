# Remote Agent Control MVP

This directory contains the stateless, Microsoft Entra-protected control plane
for Intelligent Terminal remote Agent Hosts. It deliberately does **not** carry
AHP frames, terminal output, prompts, chat history, or any Azure access key.
Azure Web PubSub is only a short-lived relay credential issuer; hosts and
clients communicate through the relay directly.

## Design boundaries

- Cosmos DB stores a host's server-generated ID, Entra tenant/object owner,
  RSA public key, display/capability/version metadata, ACL object IDs, and
  presence expiry. Short-lived TTL replay records contain only a SHA-256 nonce
  fingerprint and expiry.
- A separate `host-discovery` projection is partitioned by
  `{tenantId}:{objectId}`. Host listing is therefore a bounded single-partition
  query followed by point reads, not a cross-partition scan of all hosts.
- The API obtains Cosmos DB and Web PubSub access through its user-assigned
  managed identity. It has no Azure resource key or data-plane connection
  string setting.
- Host discovery returns hosts the caller owns and hosts whose
  `AllowedClientObjectIds` ACL contains the caller's Entra object ID.
- Request and dependency telemetry is sent to the workspace-based Application
  Insights resource through its ingestion connection string. That value is
  non-secret telemetry configuration; IP masking remains enabled and no
  request body, relay URI, token, or owner object ID is logged.
- A host must sign the canonical challenge
  `remote-agent-control.host-proof.v1\n{hostId}\n{clientInstanceId}\n{issuedAt}\n{nonce}`
  with its registered RSA (2048-bit or larger) public key. Proofs are accepted
  only for five minutes, have a 30-second future skew limit, and their nonce
  is atomically consumed in Cosmos DB.
- Web PubSub access URIs expire in five minutes. Deletion blocks future
  negotiation immediately; already-issued relay credentials expire naturally
  within that bounded interval.

The token permissions are intentionally limited:

| Participant | Join groups | Send groups |
| --- | --- | --- |
| Host | `host.{HostId}.requests` | `host.{HostId}.broadcast`, `host.{HostId}.client.*` |
| Client | `host.{HostId}.broadcast`, its `host.{HostId}.client.{ClientInstanceId}` | `host.{HostId}.requests` |

## Local validation

Prerequisites: .NET 8 SDK, Docker (for image builds), Azure CLI with Bicep,
and `azd` for provisioning. The repository's root NuGet configuration only
permits Terminal packages; this service has its own `NuGet.Config` that uses
nuget.org.

```powershell
dotnet restore services\remote-agent-control\RemoteAgentControl.slnx
dotnet test services\remote-agent-control\RemoteAgentControl.slnx --no-restore
dotnet build services\remote-agent-control\RemoteAgentControl.slnx --no-restore
az bicep build --file infra\main.bicep
```

For a locally running API, set the same non-secret endpoint and Entra settings
that the Bicep deployment injects. `RemoteAgentControl__UseInMemoryStore=true`
is accepted only in the Development environment for registry-only development;
the real Web PubSub negotiation path still needs an Azure Web PubSub endpoint
and an Azure developer credential.

## Entra setup (manual prerequisite)

This must be completed by a tenant administrator or application owner; it is
not automated by Bicep.

1. Register a **single-tenant** API application in the target tenant.
2. Expose an API scope such as `RemoteAgentControl.Access`; use the API
   application ID URI (normally `api://{application-client-id}`) as
   `REMOTE_AGENT_CONTROL_API_AUDIENCE`.
3. Register each native/client application separately and grant it delegated
   permission to that API scope. Configure only its required redirect URIs and
   avoid client secrets for native applications.
4. Acquire delegated user tokens from the same tenant. The API fails closed
   unless `iss`, `aud`, `tid`, `oid`, `scp`, signature, and lifetime all
   validate. `scp` must contain `RemoteAgentControl.Access`; the owner identity
   is always `{tid}:{oid}`.

## Provisioning

The deployer needs permission to create resources and role assignments at the
resource-group scope. The Bicep template assigns the API identity only:

- **AcrPull** at the individual ACR;
- **Web PubSub Service Owner** at the individual Web PubSub resource, required
  to call its Entra Auth API and mint restricted client access URIs;
- Cosmos DB **Built-in Data Contributor** at that Cosmos account's data-plane
  root.

No role is assigned at subscription scope. ACR admin users are disabled and
anonymous pull remains disabled by its secure service default (the explicit
anonymous-pull setting is currently a preview-only ACR API feature). Cosmos
local authentication and Web PubSub local authentication are disabled.
The Container Apps minimum replica default is `0` for MVP development cost;
set the Bicep `minReplicas` parameter to at least `1` for production.

```powershell
azd auth login
azd env new <environment-name>
azd env set AZURE_LOCATION <azure-region>
azd env set AZURE_TENANT_ID <tenant-guid>
azd env set REMOTE_AGENT_CONTROL_API_AUDIENCE api://<api-client-id>
azd up
```

`azd provision` creates the Container App with a public placeholder image so a
fresh environment does not depend on an image that has not been built yet.
`azd deploy` then builds, pushes, and installs the private ACR image after the
managed identity and `AcrPull` assignment exist. `azd` exports the API URL,
Cosmos endpoint, Web PubSub endpoint, and managed identity client ID as
non-secret deployment outputs.

Remote endpoints require public ingress for the MVP. Production hardening
should add a VNet, private endpoints/private DNS, and IP/egress restrictions
once the remote-host connectivity topology is finalized.

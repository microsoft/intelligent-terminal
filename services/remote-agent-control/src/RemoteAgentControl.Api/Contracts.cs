using System.ComponentModel.DataAnnotations;
using System.Security.Claims;

namespace RemoteAgentControl.Api;

public sealed class ControlPlaneOptions
{
    public const string SectionName = "RemoteAgentControl";

    public AzureAdOptions AzureAd { get; init; } = new();
    public string CosmosEndpoint { get; init; } = string.Empty;
    public string CosmosDatabaseName { get; init; } = "remote-agent-control";
    public string CosmosContainerName { get; init; } = "hosts";
    public string CosmosDiscoveryContainerName { get; init; } = "host-discovery";
    public string WebPubSubEndpoint { get; init; } = string.Empty;
    public string WebPubSubHubName { get; init; } = "remote-agent";
    public int PresenceTtlSeconds { get; init; } = 120;
    public int ProofLifetimeSeconds { get; init; } = 300;
    public int AccessUriLifetimeSeconds { get; init; } = 300;
    public int MaxHostDiscoveryResults { get; init; } = 100;
    public bool UseInMemoryStore { get; init; }

    public bool IsValid(out string failure)
    {
        if (!Guid.TryParse(AzureAd.TenantId, out _))
        {
            failure = "RemoteAgentControl:AzureAd:TenantId must be a tenant GUID.";
            return false;
        }

        if (string.IsNullOrWhiteSpace(AzureAd.Audience))
        {
            failure = "RemoteAgentControl:AzureAd:Audience is required.";
            return false;
        }

        if (string.IsNullOrWhiteSpace(AzureAd.RequiredScope))
        {
            failure = "RemoteAgentControl:AzureAd:RequiredScope is required.";
            return false;
        }

        if (!IsHttpsUri(CosmosEndpoint) || !IsHttpsUri(WebPubSubEndpoint))
        {
            failure = "CosmosEndpoint and WebPubSubEndpoint must be absolute HTTPS endpoints.";
            return false;
        }

        if (string.IsNullOrWhiteSpace(CosmosDatabaseName) ||
            string.IsNullOrWhiteSpace(CosmosContainerName) ||
            string.IsNullOrWhiteSpace(CosmosDiscoveryContainerName) ||
            string.IsNullOrWhiteSpace(WebPubSubHubName))
        {
            failure = "Cosmos and Web PubSub names are required.";
            return false;
        }

        if (PresenceTtlSeconds is < 30 or > 3600 ||
            ProofLifetimeSeconds is < 30 or > 600 ||
            AccessUriLifetimeSeconds is < 60 or > 600 ||
            MaxHostDiscoveryResults is < 1 or > 500)
        {
            failure = "Lifetime settings are outside their safe bounds.";
            return false;
        }

        failure = string.Empty;
        return true;
    }

    private static bool IsHttpsUri(string value) =>
        Uri.TryCreate(value, UriKind.Absolute, out var uri) &&
        string.Equals(uri.Scheme, Uri.UriSchemeHttps, StringComparison.OrdinalIgnoreCase);
}

public sealed class AzureAdOptions
{
    public string TenantId { get; init; } = string.Empty;
    public string Audience { get; init; } = string.Empty;
    public string RequiredScope { get; init; } = "RemoteAgentControl.Access";
}

public sealed record HostRegistrationRequest(
    string? PublicKeyPem,
    string? DisplayName,
    IReadOnlyList<string>? Capabilities,
    string? Version,
    IReadOnlyList<string>? AllowedClientObjectIds);

public sealed record HeartbeatRequest;

public sealed record HostConnectionRequest(string? ClientInstanceId, HostProof? Proof);

public sealed record ClientConnectionRequest(string? ClientInstanceId);

public sealed record HostProof(long IssuedAtUnixSeconds, string? Nonce, string? Signature);

public sealed record HostSummary(
    string HostId,
    string DisplayName,
    IReadOnlyList<string> Capabilities,
    string Version,
    DateTimeOffset RegisteredAt,
    bool IsOnline);

public sealed record ConnectionNegotiationResponse(
    Uri Url,
    DateTimeOffset ExpiresAt,
    IReadOnlyList<string> Roles,
    IReadOnlyList<string> Groups);

public sealed record OwnerIdentity(string TenantId, string ObjectId)
{
    public string Key => $"{TenantId}:{ObjectId}";

    public static bool TryFromClaims(ClaimsPrincipal principal, out OwnerIdentity owner)
    {
        var tenantId = principal.FindFirstValue("tid");
        var objectId = principal.FindFirstValue("oid");

        if (Guid.TryParse(tenantId, out var tenantGuid) && Guid.TryParse(objectId, out var objectGuid))
        {
            owner = new OwnerIdentity(tenantGuid.ToString("D"), objectGuid.ToString("D"));
            return true;
        }

        owner = default!;
        return false;
    }
}

public sealed record HostRecord(
    string HostId,
    OwnerIdentity Owner,
    string PublicKeyPem,
    string DisplayName,
    IReadOnlyList<string> Capabilities,
    string Version,
    IReadOnlyList<string> AllowedClientObjectIds,
    DateTimeOffset RegisteredAt,
    DateTimeOffset? PresenceExpiresAt);

public enum ProofFailure
{
    Invalid,
    Expired,
    BadSignature,
    Replay,
}

public sealed class RequestValidationException : Exception
{
    public RequestValidationException(string message)
        : base(message)
    {
    }
}

public sealed class HostNotFoundException : Exception
{
}

public sealed class HostProofException : Exception
{
    public HostProofException(ProofFailure failure)
    {
        Failure = failure;
    }

    public ProofFailure Failure { get; }
}

public sealed class OptionsValidationException : Exception
{
    public OptionsValidationException(string message)
        : base(message)
    {
    }
}

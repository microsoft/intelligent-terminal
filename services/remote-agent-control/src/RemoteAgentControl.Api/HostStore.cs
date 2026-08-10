using System.Collections.Concurrent;
using System.Net;
using System.Runtime.ExceptionServices;
using System.Text.Json.Serialization;
using Microsoft.Azure.Cosmos;
using Microsoft.Extensions.Options;

namespace RemoteAgentControl.Api;

public interface IHostStore
{
    Task CreateAsync(HostRecord host, CancellationToken cancellationToken);
    Task<HostRecord?> GetAsync(string hostId, CancellationToken cancellationToken);
    Task<IReadOnlyList<HostRecord>> ListAccessibleToAsync(OwnerIdentity requester, CancellationToken cancellationToken);
    Task TouchPresenceAsync(string hostId, DateTimeOffset expiresAt, CancellationToken cancellationToken);
    Task<bool> TryConsumeProofNonceAsync(string hostId, string nonceHash, DateTimeOffset expiresAt, CancellationToken cancellationToken);
    Task DeleteAsync(string hostId, CancellationToken cancellationToken);
    Task<bool> IsHealthyAsync(CancellationToken cancellationToken);
}

public sealed class CosmosHostStore : IHostStore
{
    private const string HostDocumentId = "host";
    private readonly Container _hostContainer;
    private readonly Container _discoveryContainer;
    private readonly TimeProvider _clock;
    private readonly int _maxHostDiscoveryResults;

    public CosmosHostStore(
        CosmosClient cosmosClient,
        IOptions<ControlPlaneOptions> options,
        TimeProvider clock)
    {
        _hostContainer = cosmosClient.GetContainer(
            options.Value.CosmosDatabaseName,
            options.Value.CosmosContainerName);
        _discoveryContainer = cosmosClient.GetContainer(
            options.Value.CosmosDatabaseName,
            options.Value.CosmosDiscoveryContainerName);
        _clock = clock;
        _maxHostDiscoveryResults = options.Value.MaxHostDiscoveryResults;
    }

    public async Task CreateAsync(HostRecord host, CancellationToken cancellationToken)
    {
        await _hostContainer.CreateItemAsync(
            CosmosHostDocument.From(host),
            new PartitionKey(host.HostId),
            cancellationToken: cancellationToken);
        var principals = DiscoveryPrincipals(host).ToList();
        var createdPrincipals = new List<string>(principals.Count);
        try
        {
            foreach (var principalId in principals)
            {
                await _discoveryContainer.CreateItemAsync(
                    new CosmosDiscoveryDocument
                    {
                        Id = host.HostId,
                        PrincipalId = principalId,
                        HostId = host.HostId,
                    },
                    new PartitionKey(principalId),
                    cancellationToken: cancellationToken);
                createdPrincipals.Add(principalId);
            }
        }
        catch (Exception registrationError)
        {
            try
            {
                await _hostContainer.DeleteItemAsync<CosmosHostDocument>(
                    HostDocumentId,
                    new PartitionKey(host.HostId),
                    cancellationToken: cancellationToken);
                await Task.WhenAll(createdPrincipals.Select(principalId =>
                    DeleteDiscoveryAsync(principalId, host.HostId, cancellationToken)));
            }
            catch (Exception rollbackError)
            {
                throw new AggregateException(
                    "Host registration failed and its Cosmos DB rollback was incomplete.",
                    registrationError,
                    rollbackError);
            }

            ExceptionDispatchInfo.Capture(registrationError).Throw();
            throw;
        }
    }

    public async Task<HostRecord?> GetAsync(string hostId, CancellationToken cancellationToken)
    {
        try
        {
            var response = await _hostContainer.ReadItemAsync<CosmosHostDocument>(
                HostDocumentId,
                new PartitionKey(hostId),
                cancellationToken: cancellationToken);
            return response.Resource.ToHostRecord();
        }
        catch (CosmosException exception) when (exception.StatusCode == HttpStatusCode.NotFound)
        {
            return null;
        }
    }

    public async Task<IReadOnlyList<HostRecord>> ListAccessibleToAsync(OwnerIdentity requester, CancellationToken cancellationToken)
    {
        var query = new QueryDefinition("SELECT c.hostId FROM c");
        var iterator = _discoveryContainer.GetItemQueryIterator<CosmosDiscoveryDocument>(
            query,
            requestOptions: new QueryRequestOptions
            {
                PartitionKey = new PartitionKey(requester.Key),
                MaxItemCount = _maxHostDiscoveryResults,
            });
        var hostIds = new List<string>();

        while (iterator.HasMoreResults && hostIds.Count < _maxHostDiscoveryResults)
        {
            var page = await iterator.ReadNextAsync(cancellationToken);
            hostIds.AddRange(page
                .Take(_maxHostDiscoveryResults - hostIds.Count)
                .Select(document => document.HostId));
        }

        var hosts = await Task.WhenAll(hostIds.Select(hostId => GetAsync(hostId, cancellationToken)));
        return hosts
            .OfType<HostRecord>()
            .Where(host => HostAuthorization.CanReadOrConnectAsClient(host, requester))
            .OrderBy(host => host.RegisteredAt)
            .ToList();
    }

    public async Task TouchPresenceAsync(string hostId, DateTimeOffset expiresAt, CancellationToken cancellationToken)
    {
        await _hostContainer.PatchItemAsync<CosmosHostDocument>(
            HostDocumentId,
            new PartitionKey(hostId),
            [PatchOperation.Set("/presenceExpiresAt", expiresAt)],
            cancellationToken: cancellationToken);
    }

    public async Task<bool> TryConsumeProofNonceAsync(
        string hostId,
        string nonceHash,
        DateTimeOffset expiresAt,
        CancellationToken cancellationToken)
    {
        var document = new CosmosProofNonceDocument
        {
            Id = $"proof-{nonceHash}",
            HostId = hostId,
            ExpiresAt = expiresAt,
            Ttl = Math.Max(1, (int)Math.Ceiling((expiresAt - _clock.GetUtcNow()).TotalSeconds)),
        };

        try
        {
            await _hostContainer.CreateItemAsync(document, new PartitionKey(hostId), cancellationToken: cancellationToken);
            return true;
        }
        catch (CosmosException exception) when (exception.StatusCode == HttpStatusCode.Conflict)
        {
            return false;
        }
    }

    public async Task DeleteAsync(string hostId, CancellationToken cancellationToken)
    {
        var host = await GetAsync(hostId, cancellationToken);
        try
        {
            await _hostContainer.DeleteItemAsync<CosmosHostDocument>(
                HostDocumentId,
                new PartitionKey(hostId),
                cancellationToken: cancellationToken);
        }
        catch (CosmosException exception) when (exception.StatusCode != HttpStatusCode.NotFound)
        {
            throw;
        }

        if (host is not null)
        {
            await Task.WhenAll(DiscoveryPrincipals(host).Select(principalId =>
                DeleteDiscoveryAsync(principalId, hostId, cancellationToken)));
        }
    }

    public async Task<bool> IsHealthyAsync(CancellationToken cancellationToken)
    {
        try
        {
            await Task.WhenAll(
                _hostContainer.ReadContainerAsync(cancellationToken: cancellationToken),
                _discoveryContainer.ReadContainerAsync(cancellationToken: cancellationToken));
            return true;
        }
        catch (CosmosException)
        {
            return false;
        }
    }

    private async Task DeleteDiscoveryAsync(
        string principalId,
        string hostId,
        CancellationToken cancellationToken)
    {
        try
        {
            await _discoveryContainer.DeleteItemAsync<CosmosDiscoveryDocument>(
                hostId,
                new PartitionKey(principalId),
                cancellationToken: cancellationToken);
        }
        catch (CosmosException exception) when (exception.StatusCode == HttpStatusCode.NotFound)
        {
        }
    }

    private static IEnumerable<string> DiscoveryPrincipals(HostRecord host)
    {
        yield return host.Owner.Key;
        foreach (var objectId in host.AllowedClientObjectIds)
        {
            var principalId = $"{host.Owner.TenantId}:{objectId}";
            if (!string.Equals(principalId, host.Owner.Key, StringComparison.Ordinal))
            {
                yield return principalId;
            }
        }
    }

    private sealed class CosmosHostDocument
    {
        [JsonPropertyName("id")]
        public string Id { get; init; } = HostDocumentId;

        [JsonPropertyName("kind")]
        public string Kind { get; init; } = "host";

        [JsonPropertyName("hostId")]
        public string HostId { get; init; } = string.Empty;

        [JsonPropertyName("ownerId")]
        public string OwnerId { get; init; } = string.Empty;

        [JsonPropertyName("tenantId")]
        public string TenantId { get; init; } = string.Empty;

        [JsonPropertyName("objectId")]
        public string ObjectId { get; init; } = string.Empty;

        [JsonPropertyName("publicKeyPem")]
        public string PublicKeyPem { get; init; } = string.Empty;

        [JsonPropertyName("displayName")]
        public string DisplayName { get; init; } = string.Empty;

        [JsonPropertyName("capabilities")]
        public List<string> Capabilities { get; init; } = [];

        [JsonPropertyName("version")]
        public string Version { get; init; } = string.Empty;

        [JsonPropertyName("allowedClientObjectIds")]
        public List<string> AllowedClientObjectIds { get; init; } = [];

        [JsonPropertyName("registeredAt")]
        public DateTimeOffset RegisteredAt { get; init; }

        [JsonPropertyName("presenceExpiresAt")]
        public DateTimeOffset? PresenceExpiresAt { get; init; }

        public static CosmosHostDocument From(HostRecord host) => new()
        {
            HostId = host.HostId,
            OwnerId = host.Owner.Key,
            TenantId = host.Owner.TenantId,
            ObjectId = host.Owner.ObjectId,
            PublicKeyPem = host.PublicKeyPem,
            DisplayName = host.DisplayName,
            Capabilities = host.Capabilities.ToList(),
            Version = host.Version,
            AllowedClientObjectIds = host.AllowedClientObjectIds.ToList(),
            RegisteredAt = host.RegisteredAt,
            PresenceExpiresAt = host.PresenceExpiresAt,
        };

        public HostRecord ToHostRecord() => new(
            HostId,
            new OwnerIdentity(TenantId, ObjectId),
            PublicKeyPem,
            DisplayName,
            Capabilities,
            Version,
            AllowedClientObjectIds,
            RegisteredAt,
            PresenceExpiresAt);
    }

    private sealed class CosmosProofNonceDocument
    {
        [JsonPropertyName("id")]
        public string Id { get; init; } = string.Empty;

        [JsonPropertyName("kind")]
        public string Kind { get; init; } = "proofNonce";

        [JsonPropertyName("hostId")]
        public string HostId { get; init; } = string.Empty;

        [JsonPropertyName("expiresAt")]
        public DateTimeOffset ExpiresAt { get; init; }

        [JsonPropertyName("ttl")]
        public int Ttl { get; init; }
    }

    private sealed class CosmosDiscoveryDocument
    {
        [JsonPropertyName("id")]
        public string Id { get; init; } = string.Empty;

        [JsonPropertyName("principalId")]
        public string PrincipalId { get; init; } = string.Empty;

        [JsonPropertyName("hostId")]
        public string HostId { get; init; } = string.Empty;
    }
}

public sealed class InMemoryHostStore : IHostStore
{
    private readonly ConcurrentDictionary<string, HostRecord> _hosts = new(StringComparer.Ordinal);
    private readonly ConcurrentDictionary<string, DateTimeOffset> _proofNonces = new(StringComparer.Ordinal);
    private readonly TimeProvider _clock;

    public InMemoryHostStore(TimeProvider clock)
    {
        _clock = clock;
    }

    public Task CreateAsync(HostRecord host, CancellationToken cancellationToken)
    {
        if (!_hosts.TryAdd(host.HostId, host))
        {
            throw new InvalidOperationException("Host identifier collision.");
        }

        return Task.CompletedTask;
    }

    public Task<HostRecord?> GetAsync(string hostId, CancellationToken cancellationToken)
    {
        _hosts.TryGetValue(hostId, out var host);
        return Task.FromResult(host);
    }

    public Task<IReadOnlyList<HostRecord>> ListAccessibleToAsync(OwnerIdentity requester, CancellationToken cancellationToken)
    {
        IReadOnlyList<HostRecord> hosts = _hosts.Values
            .Where(host => HostAuthorization.CanReadOrConnectAsClient(host, requester))
            .OrderBy(host => host.RegisteredAt)
            .ToList();
        return Task.FromResult(hosts);
    }

    public Task TouchPresenceAsync(string hostId, DateTimeOffset expiresAt, CancellationToken cancellationToken)
    {
        while (_hosts.TryGetValue(hostId, out var current))
        {
            if (_hosts.TryUpdate(hostId, current with { PresenceExpiresAt = expiresAt }, current))
            {
                break;
            }
        }
        return Task.CompletedTask;
    }

    public Task<bool> TryConsumeProofNonceAsync(
        string hostId,
        string nonceHash,
        DateTimeOffset expiresAt,
        CancellationToken cancellationToken)
    {
        var key = $"{hostId}:{nonceHash}";
        var now = _clock.GetUtcNow();
        if (_proofNonces.TryGetValue(key, out var existing) && existing > now)
        {
            return Task.FromResult(false);
        }

        _proofNonces.TryRemove(key, out _);
        return Task.FromResult(_proofNonces.TryAdd(key, expiresAt));
    }

    public Task DeleteAsync(string hostId, CancellationToken cancellationToken)
    {
        _hosts.TryRemove(hostId, out _);
        foreach (var key in _proofNonces.Keys.Where(key => key.StartsWith($"{hostId}:", StringComparison.Ordinal)))
        {
            _proofNonces.TryRemove(key, out _);
        }

        return Task.CompletedTask;
    }

    public Task<bool> IsHealthyAsync(CancellationToken cancellationToken) => Task.FromResult(true);
}

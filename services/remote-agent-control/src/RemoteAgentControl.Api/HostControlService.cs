using Microsoft.Extensions.Options;

namespace RemoteAgentControl.Api;

public sealed class HostControlService
{
    private readonly IHostStore _store;
    private readonly HostProofVerifier _proofVerifier;
    private readonly IRelayAccessIssuer _relayAccessIssuer;
    private readonly TimeProvider _clock;
    private readonly ControlPlaneOptions _options;

    public HostControlService(
        IHostStore store,
        HostProofVerifier proofVerifier,
        IRelayAccessIssuer relayAccessIssuer,
        TimeProvider clock,
        IOptions<ControlPlaneOptions> options)
    {
        _store = store;
        _proofVerifier = proofVerifier;
        _relayAccessIssuer = relayAccessIssuer;
        _clock = clock;
        _options = options.Value;
    }

    public async Task<HostSummary> RegisterAsync(
        OwnerIdentity owner,
        HostRegistrationRequest request,
        CancellationToken cancellationToken)
    {
        var validated = RequestValidation.ValidateRegistration(request);
        var host = new HostRecord(
            $"host-{Guid.NewGuid():N}",
            owner,
            validated.PublicKeyPem,
            validated.DisplayName,
            validated.Capabilities,
            validated.Version,
            validated.AllowedClientObjectIds,
            _clock.GetUtcNow(),
            null);
        await _store.CreateAsync(host, cancellationToken);
        return ToSummary(host);
    }

    public async Task HeartbeatAsync(OwnerIdentity owner, string hostId, CancellationToken cancellationToken)
    {
        var host = await GetOwnerHostAsync(owner, hostId, cancellationToken);
        await _store.TouchPresenceAsync(
            host.HostId,
            _clock.GetUtcNow().AddSeconds(_options.PresenceTtlSeconds),
            cancellationToken);
    }

    public async Task<IReadOnlyList<HostSummary>> ListAsync(OwnerIdentity owner, CancellationToken cancellationToken)
    {
        var hosts = await _store.ListAccessibleToAsync(owner, cancellationToken);
        return hosts.Select(ToSummary).ToList();
    }

    public async Task<HostSummary> GetAsync(OwnerIdentity requester, string hostId, CancellationToken cancellationToken)
    {
        var host = await GetClientAuthorizedHostAsync(requester, hostId, cancellationToken);
        return ToSummary(host);
    }

    public async Task<ConnectionNegotiationResponse> NegotiateHostAsync(
        OwnerIdentity owner,
        string hostId,
        HostConnectionRequest request,
        CancellationToken cancellationToken)
    {
        var host = await GetOwnerHostAsync(owner, hostId, cancellationToken);
        var clientInstanceId = RequestValidation.NormalizeClientInstanceId(request.ClientInstanceId);
        await _proofVerifier.VerifyAndConsumeAsync(host, clientInstanceId, request.Proof, cancellationToken);

        var routes = RelayRouteFactory.Create(host.HostId, clientInstanceId);
        var access = await _relayAccessIssuer.IssueAsync(
            $"host:{host.HostId}:{clientInstanceId}",
            routes.HostRoles,
            routes.HostGroups,
            cancellationToken);
        await EnsureHostStillRegisteredAsync(host, cancellationToken);
        return new ConnectionNegotiationResponse(access.Url, access.ExpiresAt, routes.HostRoles, routes.HostGroups);
    }

    public async Task<ConnectionNegotiationResponse> NegotiateClientAsync(
        OwnerIdentity requester,
        string hostId,
        ClientConnectionRequest request,
        CancellationToken cancellationToken)
    {
        var host = await GetClientAuthorizedHostAsync(requester, hostId, cancellationToken);
        var clientInstanceId = RequestValidation.NormalizeClientInstanceId(request.ClientInstanceId);
        var routes = RelayRouteFactory.Create(host.HostId, clientInstanceId);
        var access = await _relayAccessIssuer.IssueAsync(
            $"client:{host.HostId}:{clientInstanceId}",
            routes.ClientRoles,
            routes.ClientGroups,
            cancellationToken);
        await EnsureHostStillRegisteredAsync(host, cancellationToken);
        return new ConnectionNegotiationResponse(access.Url, access.ExpiresAt, routes.ClientRoles, routes.ClientGroups);
    }

    public async Task DeleteAsync(OwnerIdentity owner, string hostId, CancellationToken cancellationToken)
    {
        _ = await GetOwnerHostAsync(owner, hostId, cancellationToken);
        await _store.DeleteAsync(hostId, cancellationToken);
    }

    public async Task<bool> IsHealthyAsync(CancellationToken cancellationToken) =>
        await _store.IsHealthyAsync(cancellationToken) &&
        await _relayAccessIssuer.IsHealthyAsync(cancellationToken);

    private async Task EnsureHostStillRegisteredAsync(HostRecord expected, CancellationToken cancellationToken)
    {
        var current = await _store.GetAsync(expected.HostId, cancellationToken);
        if (current is null ||
            current.Owner != expected.Owner ||
            current.RegisteredAt != expected.RegisteredAt)
        {
            throw new HostNotFoundException();
        }
    }

    private async Task<HostRecord> GetOwnerHostAsync(OwnerIdentity owner, string hostId, CancellationToken cancellationToken)
    {
        RequestValidation.EnsureHostId(hostId);
        var host = await _store.GetAsync(hostId, cancellationToken);
        if (host is null || !HostAuthorization.IsOwner(host, owner))
        {
            throw new HostNotFoundException();
        }

        return host;
    }

    private async Task<HostRecord> GetClientAuthorizedHostAsync(
        OwnerIdentity requester,
        string hostId,
        CancellationToken cancellationToken)
    {
        RequestValidation.EnsureHostId(hostId);
        var host = await _store.GetAsync(hostId, cancellationToken);
        if (host is null || !HostAuthorization.CanReadOrConnectAsClient(host, requester))
        {
            throw new HostNotFoundException();
        }

        return host;
    }

    private HostSummary ToSummary(HostRecord host) => new(
        host.HostId,
        host.DisplayName,
        host.Capabilities,
        host.Version,
        host.RegisteredAt,
        host.PresenceExpiresAt > _clock.GetUtcNow());
}

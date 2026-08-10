using Azure.Identity;
using Azure.Messaging.WebPubSub;
using Microsoft.Extensions.Options;

namespace RemoteAgentControl.Api;

public sealed record RelayRoutes(
    string RequestsGroup,
    string BroadcastGroup,
    string ClientGroup,
    IReadOnlyList<string> HostRoles,
    IReadOnlyList<string> HostGroups,
    IReadOnlyList<string> ClientRoles,
    IReadOnlyList<string> ClientGroups);

public static class RelayRouteFactory
{
    public static RelayRoutes Create(string hostId, string clientInstanceId)
    {
        var requests = $"host.{hostId}.requests";
        var broadcast = $"host.{hostId}.broadcast";
        var client = $"host.{hostId}.client.{clientInstanceId}";
        var clientPattern = $"host.{hostId}.client.*";

        return new RelayRoutes(
            requests,
            broadcast,
            client,
            [
                $"webpubsub.joinLeaveGroup.{requests}",
                $"webpubsub.sendToGroup.{broadcast}",
                $"webpubsub.sendToGroups.{clientPattern}",
            ],
            [requests],
            [
                $"webpubsub.sendToGroup.{requests}",
                $"webpubsub.joinLeaveGroup.{broadcast}",
                $"webpubsub.joinLeaveGroup.{client}",
            ],
            [broadcast, client]);
    }
}

public sealed record IssuedRelayAccess(Uri Url, DateTimeOffset ExpiresAt);

public interface IRelayAccessIssuer
{
    Task<IssuedRelayAccess> IssueAsync(
        string userId,
        IReadOnlyList<string> roles,
        IReadOnlyList<string> groups,
        CancellationToken cancellationToken);
    Task<bool> IsHealthyAsync(CancellationToken cancellationToken);
}

public sealed class WebPubSubRelayAccessIssuer : IRelayAccessIssuer
{
    private readonly WebPubSubServiceClient _client;
    private readonly ControlPlaneOptions _options;
    private readonly TimeProvider _clock;

    public WebPubSubRelayAccessIssuer(IOptions<ControlPlaneOptions> options, TimeProvider clock)
    {
        _options = options.Value;
        _clock = clock;
        _client = new WebPubSubServiceClient(
            new Uri(_options.WebPubSubEndpoint),
            _options.WebPubSubHubName,
            new DefaultAzureCredential());
    }

    public Task<IssuedRelayAccess> IssueAsync(
        string userId,
        IReadOnlyList<string> roles,
        IReadOnlyList<string> groups,
        CancellationToken cancellationToken)
    {
        var expiresAt = _clock.GetUtcNow().AddSeconds(_options.AccessUriLifetimeSeconds);
        var url = _client.GetClientAccessUri(expiresAt, userId, roles, groups, cancellationToken: cancellationToken);
        return Task.FromResult(new IssuedRelayAccess(url, expiresAt));
    }

    public async Task<bool> IsHealthyAsync(CancellationToken cancellationToken)
    {
        try
        {
            _ = await _client.GroupExistsAsync(
                "__remote-agent-control-health__",
                new Azure.RequestContext
                {
                    CancellationToken = cancellationToken,
                });
            return true;
        }
        catch (Azure.RequestFailedException)
        {
            return false;
        }
        catch (Azure.Identity.AuthenticationFailedException)
        {
            return false;
        }
    }
}

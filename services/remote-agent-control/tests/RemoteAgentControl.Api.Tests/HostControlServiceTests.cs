using System.Security.Cryptography;
using Microsoft.Extensions.Options;
using RemoteAgentControl.Api;

namespace RemoteAgentControl.Api.Tests;

public sealed class HostControlServiceTests
{
    [Fact]
    public async Task CrossUserAccess_IsConcealedAndDenied()
    {
        using var rsa = RSA.Create(2048);
        var fixture = new Fixture();
        var owner = Owner("11111111-1111-1111-1111-111111111111");
        var otherUser = Owner("22222222-2222-2222-2222-222222222222");
        var host = await fixture.RegisterAsync(owner, rsa);

        await Assert.ThrowsAsync<HostNotFoundException>(() => fixture.Service.GetAsync(otherUser, host.HostId, CancellationToken.None));
        await Assert.ThrowsAsync<HostNotFoundException>(() => fixture.Service.NegotiateClientAsync(
            otherUser,
            host.HostId,
            new ClientConnectionRequest(Guid.NewGuid().ToString("D")),
            CancellationToken.None));
    }

    [Fact]
    public async Task ExplicitAcl_GrantsOnlyTheListedClientObject()
    {
        using var rsa = RSA.Create(2048);
        var fixture = new Fixture();
        var owner = Owner("77777777-7777-7777-7777-777777777777");
        var delegatedClient = Owner("88888888-8888-8888-8888-888888888888");
        var host = await fixture.RegisterAsync(owner, rsa, [delegatedClient.ObjectId]);

        var discovered = await fixture.Service.ListAsync(delegatedClient, CancellationToken.None);
        var access = await fixture.Service.NegotiateClientAsync(
            delegatedClient,
            host.HostId,
            new ClientConnectionRequest(Guid.NewGuid().ToString("D")),
            CancellationToken.None);

        Assert.Contains(discovered, item => item.HostId == host.HostId);
        Assert.Contains($"webpubsub.sendToGroup.host.{host.HostId}.requests", access.Roles);
    }

    [Fact]
    public async Task BadHostProof_IsRejected()
    {
        using var rsa = RSA.Create(2048);
        using var wrongKey = RSA.Create(2048);
        var fixture = new Fixture();
        var owner = Owner("33333333-3333-3333-3333-333333333333");
        var host = await fixture.RegisterAsync(owner, rsa);
        var clientId = Guid.NewGuid().ToString("D");
        var proof = Sign(wrongKey, host.HostId, clientId, fixture.Clock.GetUtcNow().ToUnixTimeSeconds(), NewNonce());

        var error = await Assert.ThrowsAsync<HostProofException>(() => fixture.Service.NegotiateHostAsync(
            owner,
            host.HostId,
            new HostConnectionRequest(clientId, proof),
            CancellationToken.None));

        Assert.Equal(ProofFailure.BadSignature, error.Failure);
    }

    [Fact]
    public async Task ReplayedHostProof_IsRejected()
    {
        using var rsa = RSA.Create(2048);
        var fixture = new Fixture();
        var owner = Owner("44444444-4444-4444-4444-444444444444");
        var host = await fixture.RegisterAsync(owner, rsa);
        var clientId = Guid.NewGuid().ToString("D");
        var proof = Sign(rsa, host.HostId, clientId, fixture.Clock.GetUtcNow().ToUnixTimeSeconds(), NewNonce());
        var request = new HostConnectionRequest(clientId, proof);

        _ = await fixture.Service.NegotiateHostAsync(owner, host.HostId, request, CancellationToken.None);
        var error = await Assert.ThrowsAsync<HostProofException>(() => fixture.Service.NegotiateHostAsync(
            owner,
            host.HostId,
            request,
            CancellationToken.None));

        Assert.Equal(ProofFailure.Replay, error.Failure);
    }

    [Fact]
    public async Task ExpiredHostProof_IsRejected()
    {
        using var rsa = RSA.Create(2048);
        var fixture = new Fixture();
        var owner = Owner("55555555-5555-5555-5555-555555555555");
        var host = await fixture.RegisterAsync(owner, rsa);
        var clientId = Guid.NewGuid().ToString("D");
        var proof = Sign(
            rsa,
            host.HostId,
            clientId,
            fixture.Clock.GetUtcNow().AddMinutes(-6).ToUnixTimeSeconds(),
            NewNonce());

        var error = await Assert.ThrowsAsync<HostProofException>(() => fixture.Service.NegotiateHostAsync(
            owner,
            host.HostId,
            new HostConnectionRequest(clientId, proof),
            CancellationToken.None));

        Assert.Equal(ProofFailure.Expired, error.Failure);
    }

    [Fact]
    public async Task Delete_RevokesFutureClientNegotiation()
    {
        using var rsa = RSA.Create(2048);
        var fixture = new Fixture();
        var owner = Owner("66666666-6666-6666-6666-666666666666");
        var host = await fixture.RegisterAsync(owner, rsa);

        await fixture.Service.DeleteAsync(owner, host.HostId, CancellationToken.None);

        await Assert.ThrowsAsync<HostNotFoundException>(() => fixture.Service.NegotiateClientAsync(
            owner,
            host.HostId,
            new ClientConnectionRequest(Guid.NewGuid().ToString("D")),
            CancellationToken.None));
    }

    [Fact]
    public async Task Heartbeat_MakesHostDiscoverableAsOnline()
    {
        using var rsa = RSA.Create(2048);
        var fixture = new Fixture();
        var owner = Owner("99999999-9999-9999-9999-999999999999");
        var host = await fixture.RegisterAsync(owner, rsa);

        await fixture.Service.HeartbeatAsync(owner, host.HostId, CancellationToken.None);
        var discovered = await fixture.Service.ListAsync(owner, CancellationToken.None);

        Assert.True(Assert.Single(discovered).IsOnline);
    }

    private static OwnerIdentity Owner(string objectId) =>
        new("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa", objectId);

    private static string NewNonce() => Base64Url.Encode(RandomNumberGenerator.GetBytes(32));

    private static HostProof Sign(RSA rsa, string hostId, string clientId, long issuedAt, string nonce) => new(
        issuedAt,
        nonce,
        Base64Url.Encode(rsa.SignData(
            HostProofCanonicalizer.Create(hostId, clientId, issuedAt, nonce),
            HashAlgorithmName.SHA256,
            RSASignaturePadding.Pkcs1)));

    private sealed class Fixture
    {
        private readonly ControlPlaneOptions _options = new()
        {
            AzureAd = new AzureAdOptions
            {
                TenantId = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                Audience = "api://remote-agent-control",
                RequiredScope = "RemoteAgentControl.Access",
            },
            CosmosEndpoint = "https://example.documents.azure.com:443/",
            WebPubSubEndpoint = "https://example.webpubsub.azure.com",
            ProofLifetimeSeconds = 300,
            PresenceTtlSeconds = 120,
            AccessUriLifetimeSeconds = 300,
        };

        public Fixture()
        {
            Clock = new FixedTimeProvider(DateTimeOffset.Parse("2026-08-09T16:00:00Z"));
            var store = new InMemoryHostStore(Clock);
            var verifier = new HostProofVerifier(store, Clock, Options.Create(_options));
            Service = new HostControlService(store, verifier, new TestRelayAccessIssuer(Clock), Clock, Options.Create(_options));
        }

        public FixedTimeProvider Clock { get; }
        public HostControlService Service { get; }

        public Task<HostSummary> RegisterAsync(
            OwnerIdentity owner,
            RSA rsa,
            IReadOnlyList<string>? allowedClientObjectIds = null) =>
            Service.RegisterAsync(
                owner,
                new HostRegistrationRequest(
                    rsa.ExportSubjectPublicKeyInfoPem(),
                    "Development host",
                    ["terminal"],
                    "0.1.0",
                    allowedClientObjectIds ?? []),
                CancellationToken.None);
    }

    private sealed class TestRelayAccessIssuer : IRelayAccessIssuer
    {
        private readonly TimeProvider _clock;

        public TestRelayAccessIssuer(TimeProvider clock)
        {
            _clock = clock;
        }

        public Task<IssuedRelayAccess> IssueAsync(
            string userId,
            IReadOnlyList<string> roles,
            IReadOnlyList<string> groups,
            CancellationToken cancellationToken) =>
            Task.FromResult(new IssuedRelayAccess(
                new Uri("wss://relay.example/client/hubs/remote-agent?access_token=redacted"),
                _clock.GetUtcNow().AddMinutes(5)));

        public Task<bool> IsHealthyAsync(CancellationToken cancellationToken) =>
            Task.FromResult(true);
    }
}

public sealed class RelayRouteFactoryTests
{
    [Fact]
    public void Routes_GrantOnlyTheRequiredHostAndClientPermissions()
    {
        var hostId = "host-0123456789abcdef0123456789abcdef";
        var clientId = "12345678-1234-1234-1234-123456789abc";

        var routes = RelayRouteFactory.Create(hostId, clientId);

        Assert.Equal($"host.{hostId}.requests", routes.RequestsGroup);
        Assert.Equal($"host.{hostId}.broadcast", routes.BroadcastGroup);
        Assert.Equal($"host.{hostId}.client.{clientId}", routes.ClientGroup);
        Assert.Equal(
            [
                $"webpubsub.joinLeaveGroup.host.{hostId}.requests",
                $"webpubsub.sendToGroup.host.{hostId}.broadcast",
                $"webpubsub.sendToGroups.host.{hostId}.client.*",
            ],
            routes.HostRoles);
        Assert.Equal(
            [
                $"webpubsub.sendToGroup.host.{hostId}.requests",
                $"webpubsub.joinLeaveGroup.host.{hostId}.broadcast",
                $"webpubsub.joinLeaveGroup.host.{hostId}.client.{clientId}",
            ],
            routes.ClientRoles);
        Assert.Equal([routes.BroadcastGroup, routes.ClientGroup], routes.ClientGroups);
    }
}

public sealed class FixedTimeProvider : TimeProvider
{
    private readonly DateTimeOffset _utcNow;

    public FixedTimeProvider(DateTimeOffset utcNow)
    {
        _utcNow = utcNow;
    }

    public override DateTimeOffset GetUtcNow() => _utcNow;
}

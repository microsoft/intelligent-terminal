using System.Security.Cryptography;
using System.Text;
using System.Text.RegularExpressions;
using Microsoft.Extensions.Options;

namespace RemoteAgentControl.Api;

public static partial class RequestValidation
{
    private static readonly Regex HostIdPattern = HostIdRegex();
    private static readonly Regex CapabilityPattern = CapabilityRegex();
    private static readonly Regex NoncePattern = NonceRegex();

    public static void EnsureHostId(string hostId)
    {
        if (!HostIdPattern.IsMatch(hostId))
        {
            throw new HostNotFoundException();
        }
    }

    public static string NormalizeClientInstanceId(string? clientInstanceId)
    {
        if (!Guid.TryParse(clientInstanceId, out var parsed))
        {
            throw new RequestValidationException("ClientInstanceId must be a GUID.");
        }

        return parsed.ToString("D");
    }

    public static string NormalizeNonce(string? nonce)
    {
        if (string.IsNullOrWhiteSpace(nonce) || nonce.Length is < 22 or > 128 || !NoncePattern.IsMatch(nonce))
        {
            throw new RequestValidationException("Proof nonce is invalid.");
        }

        return nonce;
    }

    public static ValidatedRegistration ValidateRegistration(HostRegistrationRequest request)
    {
        if (!RsaPublicKey.TryNormalizePem(request.PublicKeyPem, out var publicKeyPem))
        {
            throw new RequestValidationException("PublicKeyPem must be an RSA public key of at least 2048 bits.");
        }

        var displayName = request.DisplayName?.Trim();
        if (string.IsNullOrWhiteSpace(displayName) || displayName.Length > 128)
        {
            throw new RequestValidationException("DisplayName is required and must not exceed 128 characters.");
        }

        var capabilities = (request.Capabilities ?? [])
            .Select(capability => capability.Trim())
            .Distinct(StringComparer.Ordinal)
            .ToList();
        if (capabilities.Count > 20 || capabilities.Any(capability => !CapabilityPattern.IsMatch(capability)))
        {
            throw new RequestValidationException("Capabilities must contain at most 20 safe names.");
        }

        var version = request.Version?.Trim() ?? string.Empty;
        if (version.Length > 64)
        {
            throw new RequestValidationException("Version must not exceed 64 characters.");
        }

        var allowedClientObjectIds = (request.AllowedClientObjectIds ?? [])
            .Select(value => Guid.TryParse(value, out var id) ? id.ToString("D") : throw new RequestValidationException("ACL object IDs must be GUIDs."))
            .Distinct(StringComparer.Ordinal)
            .ToList();
        if (allowedClientObjectIds.Count > 50)
        {
            throw new RequestValidationException("The ACL must contain at most 50 object IDs.");
        }

        return new ValidatedRegistration(publicKeyPem, displayName, capabilities, version, allowedClientObjectIds);
    }

    [GeneratedRegex("^host-[0-9a-f]{32}$", RegexOptions.CultureInvariant)]
    private static partial Regex HostIdRegex();

    [GeneratedRegex("^[A-Za-z0-9._:-]{1,64}$", RegexOptions.CultureInvariant)]
    private static partial Regex CapabilityRegex();

    [GeneratedRegex("^[A-Za-z0-9_-]+$", RegexOptions.CultureInvariant)]
    private static partial Regex NonceRegex();
}

public sealed record ValidatedRegistration(
    string PublicKeyPem,
    string DisplayName,
    IReadOnlyList<string> Capabilities,
    string Version,
    IReadOnlyList<string> AllowedClientObjectIds);

public static class RsaPublicKey
{
    public static bool TryNormalizePem(string? value, out string normalizedPem)
    {
        normalizedPem = string.Empty;
        if (string.IsNullOrWhiteSpace(value) || value.Length > 16384)
        {
            return false;
        }

        try
        {
            using var rsa = RSA.Create();
            rsa.ImportFromPem(value);
            if (rsa.KeySize < 2048)
            {
                return false;
            }

            normalizedPem = rsa.ExportSubjectPublicKeyInfoPem();
            return true;
        }
        catch (CryptographicException)
        {
            return false;
        }
    }

    public static bool Verify(string publicKeyPem, ReadOnlySpan<byte> data, string? signature)
    {
        if (!Base64Url.TryDecode(signature, out var signatureBytes))
        {
            return false;
        }

        try
        {
            using var rsa = RSA.Create();
            rsa.ImportFromPem(publicKeyPem);
            return rsa.VerifyData(data, signatureBytes, HashAlgorithmName.SHA256, RSASignaturePadding.Pkcs1);
        }
        catch (CryptographicException)
        {
            return false;
        }
    }
}

public static class HostProofCanonicalizer
{
    public static byte[] Create(string hostId, string clientInstanceId, long issuedAtUnixSeconds, string nonce) =>
        Encoding.UTF8.GetBytes(
            $"remote-agent-control.host-proof.v1\n{hostId}\n{clientInstanceId}\n{issuedAtUnixSeconds}\n{nonce}");
}

public sealed class HostProofVerifier
{
    private readonly IHostStore _store;
    private readonly TimeProvider _clock;
    private readonly ControlPlaneOptions _options;

    public HostProofVerifier(IHostStore store, TimeProvider clock, IOptions<ControlPlaneOptions> options)
    {
        _store = store;
        _clock = clock;
        _options = options.Value;
    }

    public async Task VerifyAndConsumeAsync(
        HostRecord host,
        string clientInstanceId,
        HostProof? proof,
        CancellationToken cancellationToken)
    {
        if (proof is null)
        {
            throw new HostProofException(ProofFailure.Invalid);
        }

        string nonce;
        try
        {
            nonce = RequestValidation.NormalizeNonce(proof.Nonce);
        }
        catch (RequestValidationException)
        {
            throw new HostProofException(ProofFailure.Invalid);
        }

        DateTimeOffset issuedAt;
        try
        {
            issuedAt = DateTimeOffset.FromUnixTimeSeconds(proof.IssuedAtUnixSeconds);
        }
        catch (ArgumentOutOfRangeException)
        {
            throw new HostProofException(ProofFailure.Expired);
        }

        var now = _clock.GetUtcNow();
        var lifetime = TimeSpan.FromSeconds(_options.ProofLifetimeSeconds);
        if (issuedAt < now - lifetime || issuedAt > now + TimeSpan.FromSeconds(30))
        {
            throw new HostProofException(ProofFailure.Expired);
        }

        var challenge = HostProofCanonicalizer.Create(host.HostId, clientInstanceId, proof.IssuedAtUnixSeconds, nonce);
        if (!RsaPublicKey.Verify(host.PublicKeyPem, challenge, proof.Signature))
        {
            throw new HostProofException(ProofFailure.BadSignature);
        }

        var nonceHash = Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(nonce))).ToLowerInvariant();
        if (!await _store.TryConsumeProofNonceAsync(host.HostId, nonceHash, issuedAt + lifetime, cancellationToken))
        {
            throw new HostProofException(ProofFailure.Replay);
        }
    }
}

public static class HostAuthorization
{
    public static bool CanReadOrConnectAsClient(HostRecord host, OwnerIdentity requester) =>
        host.Owner == requester ||
        host.AllowedClientObjectIds.Contains(requester.ObjectId, StringComparer.Ordinal);

    public static bool IsOwner(HostRecord host, OwnerIdentity requester) => host.Owner == requester;
}

public static class Base64Url
{
    public static string Encode(ReadOnlySpan<byte> value) =>
        Convert.ToBase64String(value).TrimEnd('=').Replace('+', '-').Replace('/', '_');

    public static bool TryDecode(string? value, out byte[] bytes)
    {
        bytes = [];
        if (string.IsNullOrWhiteSpace(value) || value.Length > 8192 || value.Length % 4 == 1)
        {
            return false;
        }

        try
        {
            var padded = value.Replace('-', '+').Replace('_', '/');
            padded = padded.PadRight(padded.Length + ((4 - padded.Length % 4) % 4), '=');
            bytes = Convert.FromBase64String(padded);
            return true;
        }
        catch (FormatException)
        {
            return false;
        }
    }
}

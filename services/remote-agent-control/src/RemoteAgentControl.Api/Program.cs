using System.Diagnostics;
using System.Security.Claims;
using System.Threading.RateLimiting;
using Azure.Identity;
using Microsoft.AspNetCore.Authentication.JwtBearer;
using Microsoft.Azure.Cosmos;
using Microsoft.Extensions.Options;
using Microsoft.IdentityModel.Tokens;
using Microsoft.OpenApi.Models;
using RemoteAgentControl.Api;

var builder = WebApplication.CreateBuilder(args);

builder.Logging.ClearProviders();
builder.Logging.AddJsonConsole();

builder.Services.AddOptions<ControlPlaneOptions>()
    .Bind(builder.Configuration.GetSection(ControlPlaneOptions.SectionName))
    .Validate(options => options.IsValid(out _), "Remote Agent Control configuration is invalid.")
    .ValidateOnStart();
builder.Services.AddSingleton<TimeProvider>(TimeProvider.System);
builder.Services.AddApplicationInsightsTelemetry();

var configuredOptions = builder.Configuration
    .GetSection(ControlPlaneOptions.SectionName)
    .Get<ControlPlaneOptions>() ?? new ControlPlaneOptions();
if (!configuredOptions.IsValid(out var configurationFailure))
{
    throw new InvalidOperationException(configurationFailure);
}

builder.Services
    .AddAuthentication(JwtBearerDefaults.AuthenticationScheme)
    .AddJwtBearer(options =>
    {
        options.Authority = $"https://login.microsoftonline.com/{configuredOptions.AzureAd.TenantId}/v2.0";
        options.Audience = configuredOptions.AzureAd.Audience;
        options.MapInboundClaims = false;
        options.TokenValidationParameters = new TokenValidationParameters
        {
            ValidateIssuer = true,
            ValidIssuer = $"https://login.microsoftonline.com/{configuredOptions.AzureAd.TenantId}/v2.0",
            ValidateAudience = true,
            ValidAudience = configuredOptions.AzureAd.Audience,
            ValidateLifetime = true,
            RequireExpirationTime = true,
            ClockSkew = TimeSpan.FromMinutes(1),
        };
        options.Events = new JwtBearerEvents
        {
            OnTokenValidated = context =>
            {
                if (!OwnerIdentity.TryFromClaims(context.Principal!, out var owner) ||
                    !string.Equals(owner.TenantId, configuredOptions.AzureAd.TenantId, StringComparison.OrdinalIgnoreCase))
                {
                    context.Fail("The token must contain the configured tenant and an object identifier.");
                    return Task.CompletedTask;
                }

                var scopes = context.Principal!.FindFirstValue("scp")?
                    .Split(' ', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries) ?? [];
                if (!scopes.Contains(configuredOptions.AzureAd.RequiredScope, StringComparer.Ordinal))
                {
                    context.Fail($"The token must contain the {configuredOptions.AzureAd.RequiredScope} delegated scope.");
                }

                return Task.CompletedTask;
            },
        };
    });
builder.Services.AddAuthorization(options =>
{
    options.FallbackPolicy = new Microsoft.AspNetCore.Authorization.AuthorizationPolicyBuilder()
        .RequireAuthenticatedUser()
        .Build();
});

builder.Services.AddRateLimiter(options =>
{
    options.RejectionStatusCode = StatusCodes.Status429TooManyRequests;
    options.AddPolicy("api", context => RateLimitPartition.GetFixedWindowLimiter(
        RateLimitPartitionKey(context),
        _ => new FixedWindowRateLimiterOptions
        {
            PermitLimit = 60,
            Window = TimeSpan.FromMinutes(1),
            QueueLimit = 0,
            AutoReplenishment = true,
        }));
    options.AddPolicy("registration", context => RateLimitPartition.GetFixedWindowLimiter(
        RateLimitPartitionKey(context),
        _ => new FixedWindowRateLimiterOptions
        {
            PermitLimit = 10,
            Window = TimeSpan.FromMinutes(1),
            QueueLimit = 0,
            AutoReplenishment = true,
        }));
});

if (builder.Environment.IsDevelopment() && configuredOptions.UseInMemoryStore)
{
    builder.Services.AddSingleton<IHostStore, InMemoryHostStore>();
}
else
{
    builder.Services.AddSingleton(sp =>
    {
        var options = sp.GetRequiredService<IOptions<ControlPlaneOptions>>().Value;
        return new CosmosClient(
            options.CosmosEndpoint,
            new DefaultAzureCredential(),
            new CosmosClientOptions
            {
                ApplicationName = "intelligent-terminal-remote-agent-control",
            });
    });
    builder.Services.AddSingleton<IHostStore, CosmosHostStore>();
}

builder.Services.AddSingleton<HostProofVerifier>();
builder.Services.AddSingleton<IRelayAccessIssuer, WebPubSubRelayAccessIssuer>();
builder.Services.AddSingleton<HostControlService>();
builder.Services.AddHealthChecks()
    .AddCheck<ControlPlaneHealthCheck>("control-plane");
builder.Services.AddEndpointsApiExplorer();
builder.Services.AddSwaggerGen(options =>
{
    options.SwaggerDoc("v1", new OpenApiInfo
    {
        Title = "Intelligent Terminal Remote Agent Control API",
        Version = "v1",
    });
    options.AddSecurityDefinition("bearer", new OpenApiSecurityScheme
    {
        Type = SecuritySchemeType.Http,
        Scheme = "bearer",
        BearerFormat = "JWT",
        Description = "Microsoft Entra access token for the configured API audience.",
    });
    options.AddSecurityRequirement(new OpenApiSecurityRequirement
    {
        [new OpenApiSecurityScheme
        {
            Reference = new OpenApiReference
            {
                Type = ReferenceType.SecurityScheme,
                Id = "bearer",
            },
        }] = [],
    });
});

var app = builder.Build();

if (!app.Environment.IsDevelopment())
{
    app.UseHsts();
}

if (app.Environment.IsDevelopment())
{
    app.UseSwagger();
    app.UseSwaggerUI();
}
app.UseAuthentication();
app.UseRateLimiter();
app.UseAuthorization();
app.Use(async (context, next) =>
{
    var stopwatch = Stopwatch.StartNew();
    await next(context);
    app.Logger.LogInformation(
        "Control API request completed {Method} {Path} {StatusCode} {ElapsedMilliseconds}",
        context.Request.Method,
        context.Request.Path.Value,
        context.Response.StatusCode,
        stopwatch.ElapsedMilliseconds);
});

app.MapGet("/health/live", () => Results.Ok()).AllowAnonymous();
app.MapHealthChecks("/health/ready").AllowAnonymous();

var hosts = app.MapGroup("/v1/hosts")
    .RequireAuthorization()
    .RequireRateLimiting("api")
    .WithTags("Hosts");

hosts.MapPost("/register", async (
    ClaimsPrincipal principal,
    HostRegistrationRequest request,
    HostControlService service,
    CancellationToken cancellationToken) =>
{
    if (!OwnerIdentity.TryFromClaims(principal, out var owner))
    {
        return Results.Unauthorized();
    }

    try
    {
        var host = await service.RegisterAsync(owner, request, cancellationToken);
        app.Logger.LogInformation("Host registered {HostId}", host.HostId);
        return Results.Created($"/v1/hosts/{host.HostId}", host);
    }
    catch (RequestValidationException)
    {
        return Results.BadRequest();
    }
}).RequireRateLimiting("registration").WithName("RegisterHost");

hosts.MapPost("/{hostId}/heartbeat", async (
    string hostId,
    ClaimsPrincipal principal,
    HostControlService service,
    CancellationToken cancellationToken) =>
{
    if (!OwnerIdentity.TryFromClaims(principal, out var owner))
    {
        return Results.Unauthorized();
    }

    try
    {
        await service.HeartbeatAsync(owner, hostId, cancellationToken);
        return Results.NoContent();
    }
    catch (HostNotFoundException)
    {
        return Results.NotFound();
    }
}).WithName("HeartbeatHost");

hosts.MapGet("", async (
    ClaimsPrincipal principal,
    HostControlService service,
    CancellationToken cancellationToken) =>
{
    if (!OwnerIdentity.TryFromClaims(principal, out var owner))
    {
        return Results.Unauthorized();
    }

    return Results.Ok(await service.ListAsync(owner, cancellationToken));
}).WithName("ListHosts");

hosts.MapGet("/{hostId}", async (
    string hostId,
    ClaimsPrincipal principal,
    HostControlService service,
    CancellationToken cancellationToken) =>
{
    if (!OwnerIdentity.TryFromClaims(principal, out var owner))
    {
        return Results.Unauthorized();
    }

    try
    {
        return Results.Ok(await service.GetAsync(owner, hostId, cancellationToken));
    }
    catch (HostNotFoundException)
    {
        return Results.NotFound();
    }
}).WithName("GetHost");

hosts.MapPost("/{hostId}/connections/host", async (
    string hostId,
    ClaimsPrincipal principal,
    HostConnectionRequest request,
    HostControlService service,
    CancellationToken cancellationToken) =>
{
    if (!OwnerIdentity.TryFromClaims(principal, out var owner))
    {
        return Results.Unauthorized();
    }

    try
    {
        return Results.Ok(await service.NegotiateHostAsync(owner, hostId, request, cancellationToken));
    }
    catch (HostNotFoundException)
    {
        return Results.NotFound();
    }
    catch (RequestValidationException)
    {
        return Results.BadRequest();
    }
    catch (HostProofException)
    {
        return Results.StatusCode(StatusCodes.Status403Forbidden);
    }
}).WithName("NegotiateHostConnection");

hosts.MapPost("/{hostId}/connections/client", async (
    string hostId,
    ClaimsPrincipal principal,
    ClientConnectionRequest request,
    HostControlService service,
    CancellationToken cancellationToken) =>
{
    if (!OwnerIdentity.TryFromClaims(principal, out var owner))
    {
        return Results.Unauthorized();
    }

    try
    {
        return Results.Ok(await service.NegotiateClientAsync(owner, hostId, request, cancellationToken));
    }
    catch (HostNotFoundException)
    {
        return Results.NotFound();
    }
    catch (RequestValidationException)
    {
        return Results.BadRequest();
    }
}).WithName("NegotiateClientConnection");

hosts.MapDelete("/{hostId}", async (
    string hostId,
    ClaimsPrincipal principal,
    HostControlService service,
    CancellationToken cancellationToken) =>
{
    if (!OwnerIdentity.TryFromClaims(principal, out var owner))
    {
        return Results.Unauthorized();
    }

    try
    {
        await service.DeleteAsync(owner, hostId, cancellationToken);
        app.Logger.LogInformation("Host revoked {HostId}", hostId);
        return Results.NoContent();
    }
    catch (HostNotFoundException)
    {
        return Results.NotFound();
    }
}).WithName("DeleteHost");

app.Run();

static string RateLimitPartitionKey(HttpContext context)
{
    if (OwnerIdentity.TryFromClaims(context.User, out var owner))
    {
        return owner.Key;
    }

    return context.Connection.RemoteIpAddress?.ToString() ?? "unknown";
}

public sealed class ControlPlaneHealthCheck : Microsoft.Extensions.Diagnostics.HealthChecks.IHealthCheck
{
    private readonly HostControlService _service;

    public ControlPlaneHealthCheck(HostControlService service)
    {
        _service = service;
    }

    public async Task<Microsoft.Extensions.Diagnostics.HealthChecks.HealthCheckResult> CheckHealthAsync(
        Microsoft.Extensions.Diagnostics.HealthChecks.HealthCheckContext context,
        CancellationToken cancellationToken = default)
    {
        return await _service.IsHealthyAsync(cancellationToken)
            ? Microsoft.Extensions.Diagnostics.HealthChecks.HealthCheckResult.Healthy()
            : Microsoft.Extensions.Diagnostics.HealthChecks.HealthCheckResult.Unhealthy(
                "Cosmos DB or Azure Web PubSub is unavailable.");
    }
}

public partial class Program;

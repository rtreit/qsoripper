using System.Linq;
using System.Net.Sockets;
using Grpc.Core;
using Grpc.Net.Client;
using Microsoft.Extensions.Options;
using QsoRipper.DebugHost.Models;
using QsoRipper.EngineSelection;
using QsoRipper.Services;

namespace QsoRipper.DebugHost.Services;

internal sealed class DebugWorkbenchState
{
    private const string SuggestedManagedConfigPath = @".\artifacts\run\dotnet-engine.json";
    private readonly DebugWorkbenchOptions _options;

    public DebugWorkbenchState(IOptions<DebugWorkbenchOptions> options)
    {
        ArgumentNullException.ThrowIfNull(options);

        _options = options.Value;
        EngineImplementation = EngineCatalog.ResolveImplementation(_options.DefaultEngineImplementation);
        EngineEndpoint = EngineCatalog.ResolveEndpoint(EngineImplementation, _options.DefaultEngineEndpoint);
        EngineStorageBackend = ParseStorageBackend(_options.DefaultEngineStorageBackend);
        EngineSqlitePath = NormalizeSqlitePath(_options.DefaultEngineSqlitePath);
    }

    public EngineImplementation EngineImplementation { get; private set; }

    public string EngineEndpoint { get; private set; }

    public EngineStorageBackend EngineStorageBackend { get; private set; }

    public string EngineSqlitePath { get; private set; }

    public EngineInfo? ReportedEngineInfo { get; private set; }

    public TransportProbeResult? LastProbe { get; private set; }

    public RuntimeConfigSnapshot? RuntimeConfigSnapshot { get; private set; }

    public string? RuntimeConfigErrorMessage { get; private set; }

    public SetupStatus? SetupStatus { get; private set; }

    public string? SetupErrorMessage { get; private set; }

    public ListStationProfilesResponse? StationProfileCatalog { get; private set; }

    public ActiveStationContext? ActiveStationContext { get; private set; }

    public string? StationProfileErrorMessage { get; private set; }

    public void UpdateEngineEndpoint(string endpoint)
    {
        ArgumentNullException.ThrowIfNull(endpoint);

        EngineEndpoint = endpoint.Trim();
        LastProbe = null;
        ReportedEngineInfo = null;
    }

    public void UpdateEngineImplementation(EngineImplementation implementation)
    {
        var previousImplementation = EngineImplementation;
        EngineImplementation = implementation;
        if (string.IsNullOrWhiteSpace(EngineEndpoint)
            || EngineCatalog.IsDefaultEndpoint(EngineEndpoint, previousImplementation))
        {
            EngineEndpoint = EngineCatalog.GetDefaultEndpoint(implementation);
        }

        LastProbe = null;
        ReportedEngineInfo = null;
    }

    public void UpdateStorageOptions(EngineStorageBackend backend, string sqlitePath)
    {
        ArgumentNullException.ThrowIfNull(sqlitePath);

        EngineStorageBackend = backend;
        EngineSqlitePath = NormalizeSqlitePath(sqlitePath);
    }

    public void UpdateRuntimeConfig(RuntimeConfigSnapshot snapshot)
    {
        ArgumentNullException.ThrowIfNull(snapshot);

        RuntimeConfigSnapshot = snapshot;
        RuntimeConfigErrorMessage = null;
        EngineStorageBackend = ParseStorageBackend(snapshot.ActiveStorageBackend);

        var sqlitePath = snapshot.Values.FirstOrDefault(value =>
            string.Equals(value.Key, "QSORIPPER_SQLITE_PATH", StringComparison.OrdinalIgnoreCase));
        if (sqlitePath is { HasValue: true })
        {
            EngineSqlitePath = NormalizeSqlitePath(sqlitePath.DisplayValue);
        }
    }

    public void UpdateRuntimeConfigError(string? message)
    {
        RuntimeConfigErrorMessage = string.IsNullOrWhiteSpace(message)
            ? null
            : message.Trim();
    }

    public void UpdateSetupStatus(SetupStatus status)
    {
        ArgumentNullException.ThrowIfNull(status);

        SetupStatus = status;
        SetupErrorMessage = null;

#pragma warning disable CS0612 // Type or member is obsolete
        var persistedLogFilePath = string.IsNullOrWhiteSpace(status.LogFilePath)
            ? status.SqlitePath
            : status.LogFilePath;
#pragma warning restore CS0612
        if (!string.IsNullOrWhiteSpace(persistedLogFilePath))
        {
            EngineStorageBackend = EngineStorageBackend.Sqlite;
            EngineSqlitePath = NormalizeSqlitePath(persistedLogFilePath);
        }
    }

    public void UpdateSetupError(string? message)
    {
        SetupErrorMessage = string.IsNullOrWhiteSpace(message)
            ? null
            : message.Trim();
    }

    public void ClearSetupError()
    {
        SetupErrorMessage = null;
    }

    public void UpdateStationProfiles(
        ListStationProfilesResponse catalog,
        ActiveStationContext context)
    {
        ArgumentNullException.ThrowIfNull(catalog);
        ArgumentNullException.ThrowIfNull(context);

        StationProfileCatalog = catalog;
        ActiveStationContext = context;
        StationProfileErrorMessage = null;
    }

    public void UpdateStationProfileError(string? message)
    {
        StationProfileErrorMessage = string.IsNullOrWhiteSpace(message)
            ? null
            : message.Trim();
    }

    public void ClearStationProfileError()
    {
        StationProfileErrorMessage = null;
    }

    public string GetStorageBackendDisplayName()
    {
        return EngineStorageBackend switch
        {
            EngineStorageBackend.Sqlite => "SQLite",
            _ => "Memory"
        };
    }

    public string GetSelectedEngineDisplayName()
    {
        return EngineCatalog.GetDisplayName(EngineImplementation);
    }

    public string GetSelectedEngineId()
    {
        return EngineCatalog.GetEngineId(EngineImplementation);
    }

    public string BuildEngineLaunchCommand()
    {
        return EngineImplementation switch
        {
            EngineImplementation.DotNet => $"dotnet run --project src\\dotnet\\QsoRipper.Engine.DotNet -- --listen {GetListenAddress()} --config {SuggestedManagedConfigPath}",
            _ => EngineStorageBackend switch
            {
                EngineStorageBackend.Sqlite => $"cargo run -p qsoripper-server -- --storage sqlite --sqlite-path {EngineSqlitePath}",
                _ => "cargo run -p qsoripper-server -- --storage memory"
            }
        };
    }

    public IReadOnlyDictionary<string, string> GetEngineEnvironmentOverrides()
    {
        if (RuntimeConfigSnapshot is not null)
        {
            return RuntimeConfigSnapshot.Values
                .Where(value => value.HasValue)
                .ToDictionary(
                    value => value.Key,
                    value => value.DisplayValue,
                    StringComparer.OrdinalIgnoreCase);
        }

        var environment = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            ["QSORIPPER_STORAGE_BACKEND"] = EngineStorageBackend == EngineStorageBackend.Sqlite ? "sqlite" : "memory"
        };

        if (EngineStorageBackend == EngineStorageBackend.Sqlite)
        {
            environment["QSORIPPER_SQLITE_PATH"] = EngineSqlitePath;
        }

        return environment;
    }

    public async Task<TransportProbeResult> ProbeAsync(CancellationToken cancellationToken = default)
    {
        var attemptedAt = DateTimeOffset.UtcNow;
        if (!Uri.TryCreate(EngineEndpoint, UriKind.Absolute, out var endpointUri))
        {
            ReportedEngineInfo = null;
            LastProbe = new TransportProbeResult(
                false,
                EngineProbeStage.InvalidEndpoint,
                "Endpoint is not a valid absolute URI.",
                "Correct the endpoint format to an absolute http:// or https:// URI.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }

        var port = endpointUri.IsDefaultPort
            ? endpointUri.Scheme.Equals("https", StringComparison.OrdinalIgnoreCase) ? 443 : 80
            : endpointUri.Port;

        try
        {
            using var tcpTimeoutSource = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            tcpTimeoutSource.CancelAfter(TimeSpan.FromSeconds(_options.ProbeTimeoutSeconds));

            using var tcpClient = new TcpClient();
            await tcpClient.ConnectAsync(endpointUri.Host, port, tcpTimeoutSource.Token);
        }
        catch (OperationCanceledException ex)
        {
            ReportedEngineInfo = null;
            LastProbe = new TransportProbeResult(
                false,
                EngineProbeStage.TcpUnreachable,
                $"TCP connection timed out: {ex.Message}",
                "Check that the engine host is running and reachable on the network.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }
        catch (SocketException ex)
        {
            ReportedEngineInfo = null;
            LastProbe = new TransportProbeResult(
                false,
                EngineProbeStage.TcpUnreachable,
                $"TCP connection failed: {ex.Message}",
                "Check that the engine host is running and reachable on the network.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }
        catch (IOException ex)
        {
            ReportedEngineInfo = null;
            LastProbe = new TransportProbeResult(
                false,
                EngineProbeStage.TcpUnreachable,
                $"TCP connection failed: {ex.Message}",
                "Check that the engine host is running and reachable on the network.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }

        // TCP succeeded; now probe gRPC capability via EngineService and GetSyncStatus.
        var grpcChannel = GrpcChannel.ForAddress(endpointUri);
        try
        {
            using var grpcTimeoutSource = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            grpcTimeoutSource.CancelAfter(TimeSpan.FromSeconds(_options.ProbeTimeoutSeconds));

            var callOptions = new CallOptions(cancellationToken: grpcTimeoutSource.Token);
            EngineInfo? engineInfo = null;
            var engineClient = new EngineService.EngineServiceClient(grpcChannel);
            try
            {
                engineInfo = (await engineClient.GetEngineInfoAsync(new GetEngineInfoRequest(), callOptions)).Engine;
            }
            catch (RpcException ex) when (ex.StatusCode == StatusCode.Unimplemented)
            {
            }

            ReportedEngineInfo = engineInfo?.Clone();
            var client = new LogbookService.LogbookServiceClient(grpcChannel);
            await client.GetSyncStatusAsync(new GetSyncStatusRequest(), callOptions);
            var engineLabel = engineInfo is null
                ? "Engine"
                : $"{engineInfo.DisplayName} ({engineInfo.EngineId})";

            LastProbe = new TransportProbeResult(
                true,
                EngineProbeStage.MethodSucceeded,
                $"{engineLabel} is reachable and GetSyncStatus succeeded. Baseline service is live.",
                "The engine is operational. Proceed with logbook and lookup workflows.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }
        catch (RpcException ex) when (ex.StatusCode == StatusCode.Unimplemented)
        {
            var engineLabel = ReportedEngineInfo is null
                ? "The selected engine"
                : $"{ReportedEngineInfo.DisplayName} ({ReportedEngineInfo.EngineId})";
            LastProbe = new TransportProbeResult(
                false,
                EngineProbeStage.MethodUnimplemented,
                $"{engineLabel} is reachable, but GetSyncStatus is not implemented.",
                "Implement GetSyncStatus in the selected engine host to enable baseline service health checks.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }
        catch (RpcException ex) when (ex.StatusCode == StatusCode.Unavailable)
        {
            ReportedEngineInfo = null;
            LastProbe = new TransportProbeResult(
                false,
                EngineProbeStage.GrpcUnavailable,
                $"TCP is reachable but gRPC is unavailable: {ex.Status.Detail}",
                "Start the selected gRPC engine host. The port is open but no gRPC service is responding.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }
        catch (RpcException ex)
        {
            ReportedEngineInfo = null;
            LastProbe = new TransportProbeResult(
                false,
                EngineProbeStage.GrpcUnavailable,
                $"gRPC call failed ({ex.StatusCode}): {ex.Status.Detail}",
                "Investigate the gRPC service error. Check engine logs for details.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }
        catch (OperationCanceledException)
        {
            ReportedEngineInfo = null;
            LastProbe = new TransportProbeResult(
                false,
                EngineProbeStage.GrpcUnavailable,
                "gRPC call timed out. TCP is reachable but the gRPC service did not respond in time.",
                "Start the selected gRPC engine host. The port is open but no gRPC service is responding.",
                attemptedAt,
                EngineEndpoint);
            return LastProbe;
        }
        finally
        {
            await grpcChannel.ShutdownAsync();
            grpcChannel.Dispose();
        }
    }

    private static EngineStorageBackend ParseStorageBackend(string? configuredBackend)
    {
        return configuredBackend?.Trim().ToUpperInvariant() switch
        {
            "SQLITE" => EngineStorageBackend.Sqlite,
            _ => EngineStorageBackend.Memory
        };
    }

    private static string NormalizeSqlitePath(string sqlitePath)
    {
        return string.IsNullOrWhiteSpace(sqlitePath)
            ? @".\data\qsoripper.db"
            : sqlitePath.Trim();
    }

    private string GetListenAddress()
    {
        if (Uri.TryCreate(EngineEndpoint, UriKind.Absolute, out var endpointUri))
        {
            return $"{endpointUri.Host}:{endpointUri.Port}";
        }

        var fallbackUri = new Uri(EngineCatalog.GetDefaultEndpoint(EngineImplementation), UriKind.Absolute);
        return $"{fallbackUri.Host}:{fallbackUri.Port}";
    }
}

using System.Diagnostics.CodeAnalysis;

namespace QsoRipper.EngineSelection;

public static class EngineCatalog
{
    public const string EngineImplementationEnvironmentVariable = "QSORIPPER_ENGINE_IMPLEMENTATION";
    public const string EndpointEnvironmentVariable = "QSORIPPER_ENDPOINT";
    public const string DefaultRustEndpoint = "http://127.0.0.1:50051";
    public const string DefaultDotNetEndpoint = "http://127.0.0.1:50052";

    public static IReadOnlyList<EngineTargetProfile> GetLocalProfiles()
    {
        return
        [
            CreateLocalProfile(EngineImplementation.Rust),
            CreateLocalProfile(EngineImplementation.DotNet),
        ];
    }

    public static EngineTargetProfile CreateLocalProfile(
        EngineImplementation implementation,
        string? endpoint = null)
    {
        return implementation switch
        {
            EngineImplementation.DotNet => new EngineTargetProfile(
                "local-dotnet",
                implementation,
                GetEngineId(implementation),
                "Local .NET engine",
                endpoint ?? GetDefaultEndpoint(implementation)),
            _ => new EngineTargetProfile(
                "local-rust",
                implementation,
                GetEngineId(implementation),
                "Local Rust engine",
                endpoint ?? GetDefaultEndpoint(implementation)),
        };
    }

    public static string GetEngineId(EngineImplementation implementation)
    {
        return implementation switch
        {
            EngineImplementation.DotNet => "dotnet-aspnet",
            _ => "rust-tonic",
        };
    }

    public static string GetDisplayName(EngineImplementation implementation)
    {
        return implementation switch
        {
            EngineImplementation.DotNet => "QsoRipper .NET Engine",
            _ => "QsoRipper Rust Engine",
        };
    }

    public static string GetDefaultEndpoint(EngineImplementation implementation)
    {
        return implementation switch
        {
            EngineImplementation.DotNet => DefaultDotNetEndpoint,
            _ => DefaultRustEndpoint,
        };
    }

    public static EngineImplementation ResolveImplementation(string? value = null)
    {
        return TryParseImplementation(
            value ?? Environment.GetEnvironmentVariable(EngineImplementationEnvironmentVariable),
            out var implementation)
            ? implementation.Value
            : EngineImplementation.Rust;
    }

    public static string ResolveEndpoint(
        EngineImplementation implementation,
        string? explicitEndpoint = null,
        string? environmentEndpoint = null)
    {
        var endpoint = string.IsNullOrWhiteSpace(explicitEndpoint)
            ? environmentEndpoint ?? Environment.GetEnvironmentVariable(EndpointEnvironmentVariable)
            : explicitEndpoint;

        return string.IsNullOrWhiteSpace(endpoint)
            ? GetDefaultEndpoint(implementation)
            : endpoint.Trim();
    }

    public static bool IsDefaultEndpoint(string? endpoint, EngineImplementation implementation)
    {
        return string.Equals(
            endpoint?.Trim(),
            GetDefaultEndpoint(implementation),
            StringComparison.OrdinalIgnoreCase);
    }

    public static int GetDefaultPort(EngineImplementation implementation)
    {
        return new Uri(GetDefaultEndpoint(implementation), UriKind.Absolute).Port;
    }

    public static bool TryParseImplementation(
        string? value,
        [NotNullWhen(true)] out EngineImplementation? implementation)
    {
        switch (value?.Trim().ToUpperInvariant())
        {
            case "RUST":
            case "RUST-TONIC":
                implementation = EngineImplementation.Rust;
                return true;
            case "DOTNET":
            case "DOTNET-ASPNET":
            case "MANAGED":
                implementation = EngineImplementation.DotNet;
                return true;
            default:
                implementation = null;
                return false;
        }
    }
}

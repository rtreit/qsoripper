using System;
using System.Globalization;

namespace QsoRipper.Shared.Persistence;

internal static class WsjtxIngestSetup
{
    public const string DefaultUdpBind = "127.0.0.1:2237";
    public const uint DefaultPollIntervalMs = 1000;

    public const string EnabledEnvironmentVariable = "QSORIPPER_WSJTX_INGEST_ENABLED";
    public const string UdpEnabledEnvironmentVariable = "QSORIPPER_WSJTX_INGEST_UDP_ENABLED";
    public const string UdpBindEnvironmentVariable = "QSORIPPER_WSJTX_INGEST_UDP_BIND";
    public const string AdifTailEnabledEnvironmentVariable = "QSORIPPER_WSJTX_INGEST_ADIF_TAIL_ENABLED";
    public const string AdifTailPathEnvironmentVariable = "QSORIPPER_WSJTX_INGEST_ADIF_TAIL_PATH";
    public const string PollIntervalMsEnvironmentVariable = "QSORIPPER_WSJTX_INGEST_POLL_INTERVAL_MS";
    public const string SyncToQrzEnvironmentVariable = "QSORIPPER_WSJTX_INGEST_SYNC_TO_QRZ";

    public static string? ReadEnvironmentValue(string canonicalName, params string[] aliases)
    {
        var value = Environment.GetEnvironmentVariable(canonicalName);
        if (!string.IsNullOrWhiteSpace(value))
        {
            return value;
        }

        foreach (var alias in aliases)
        {
            value = Environment.GetEnvironmentVariable(alias);
            if (!string.IsNullOrWhiteSpace(value))
            {
                return value;
            }
        }

        return null;
    }

    public static bool TryValidateHostPort(string? value, string label, out string? errorMessage)
    {
        errorMessage = null;
        if (string.IsNullOrWhiteSpace(value))
        {
            errorMessage = $"{label} must be host:port with a port between 1 and 65535.";
            return false;
        }

        var trimmed = value.Trim();
        var separator = trimmed.LastIndexOf(':');
        if (separator <= 0 || separator == trimmed.Length - 1)
        {
            errorMessage = $"{label} must be host:port with a port between 1 and 65535.";
            return false;
        }

        var host = trimmed[..separator];
        var portText = trimmed[(separator + 1)..];
        if (string.IsNullOrWhiteSpace(host)
            || !ushort.TryParse(portText, NumberStyles.None, CultureInfo.InvariantCulture, out var port)
            || port == 0)
        {
            errorMessage = $"{label} must be host:port with a port between 1 and 65535.";
            return false;
        }

        return true;
    }

    public static bool IsValidPollInterval(uint _) => true;
}

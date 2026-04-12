using Google.Protobuf.WellKnownTypes;
using Grpc.Net.Client;
using LogRipper.Domain;
using LogRipper.Services;

namespace LogRipper.Cli.Commands;

internal static class LogQsoCommand
{
    public static async Task<int> RunAsync(GrpcChannel channel, string callsign, string[] args)
    {
        if (!TryBuildQso(callsign, args, out var qso, out var error))
        {
            Console.Error.WriteLine(error);
            return 1;
        }

        var requestQso = qso!;

        var client = new LogbookService.LogbookServiceClient(channel);
        var response = await client.LogQsoAsync(new LogQsoRequest { Qso = requestQso });

        Console.WriteLine($"QSO logged: {response.LocalId}");
        Console.WriteLine($"  {callsign} on {requestQso.Band} {requestQso.Mode} at {requestQso.UtcTimestamp!.ToDateTime():u}");

        if (response.HasSyncError)
        {
            Console.WriteLine($"  QRZ sync: {response.SyncError}");
        }

        return 0;
    }

    internal static bool TryBuildQso(string callsign, string[] args, out QsoRecord? qso, out string? error)
    {
        qso = null;
        error = null;

        if (args.Length < 2 || args.Any(static a => a is "help" or "-?" or "--help"))
        {
            error = "Usage: log <callsign> <band> <mode> [--station call] [--rst-sent 59] [--rst-rcvd 59] [--freq khz]";
            return false;
        }

        try
        {
            qso = new QsoRecord
            {
                WorkedCallsign = callsign,
                Band = EnumHelpers.ParseBand(args[0]),
                Mode = EnumHelpers.ParseMode(args[1]),
                UtcTimestamp = Timestamp.FromDateTime(DateTime.UtcNow)
            };
        }
        catch (ArgumentException ex)
        {
            error = ex.Message;
            return false;
        }

        return TryApplyOptionalArgs(args, qso, out error);
    }

    internal static bool TryApplyOptionalArgs(string[] args, QsoRecord qso, out string? error)
    {
        error = null;

        for (var i = 2; i < args.Length; i++)
        {
            switch (args[i])
            {
                case "--station" when i < args.Length - 1:
                    qso.StationCallsign = args[++i].ToUpperInvariant();
                    break;
                case "--station":
                    error = "Missing value for --station.";
                    return false;
                case "--rst-sent" when i < args.Length - 1:
                    if (!TryParseRst(args[++i], out var rstSent))
                    {
                        error = $"Invalid value for --rst-sent: {args[i]}. Expected 2 or 3 digits.";
                        return false;
                    }

                    qso.RstSent = rstSent;
                    break;
                case "--rst-sent":
                    error = "Missing value for --rst-sent.";
                    return false;
                case "--rst-rcvd" when i < args.Length - 1:
                    if (!TryParseRst(args[++i], out var rstReceived))
                    {
                        error = $"Invalid value for --rst-rcvd: {args[i]}. Expected 2 or 3 digits.";
                        return false;
                    }

                    qso.RstReceived = rstReceived;
                    break;
                case "--rst-rcvd":
                    error = "Missing value for --rst-rcvd.";
                    return false;
                case "--freq" when i < args.Length - 1:
                    var freqValue = args[++i];
                    if (!ulong.TryParse(freqValue, out var freq))
                    {
                        error = $"Invalid value for --freq: {freqValue}";
                        return false;
                    }

                    qso.FrequencyKhz = freq;
                    break;
                case "--freq":
                    error = "Missing value for --freq.";
                    return false;
                default:
                    error = $"Unknown option: {args[i]}";
                    return false;
            }
        }

        return true;
    }

    internal static bool TryParseRst(string value, out RstReport report)
    {
        report = new RstReport();

        if (value.Length is not (2 or 3) || value.Any(static c => !char.IsAsciiDigit(c)))
        {
            return false;
        }

        report.Readability = (uint)(value[0] - '0');
        report.Strength = (uint)(value[1] - '0');

        if (value.Length == 3)
        {
            report.Tone = (uint)(value[2] - '0');
        }

        return true;
    }
}

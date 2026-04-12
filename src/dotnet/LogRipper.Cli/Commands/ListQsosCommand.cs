using Grpc.Net.Client;
using LogRipper.Cli;
using LogRipper.Domain;
using LogRipper.Services;
using static LogRipper.Cli.EnumHelpers;

namespace LogRipper.Cli.Commands;

internal static class ListQsosCommand
{
    public static async Task<int> RunAsync(GrpcChannel channel, string[] args, bool jsonOutput = false)
    {
        if (!TryCreateRequest(args, out var request, out var error))
        {
            Console.Error.WriteLine(error);
            return 1;
        }

        var client = new LogbookService.LogbookServiceClient(channel);
        using var call = client.ListQsos(request);

        if (jsonOutput)
        {
            var records = new List<Google.Protobuf.IMessage>();

            while (await call.ResponseStream.MoveNext(CancellationToken.None))
            {
                var qso = call.ResponseStream.Current.Qso;
                if (qso is not null)
                {
                    records.Add(qso);
                }
            }

            JsonOutput.PrintArray(records);
            return 0;
        }

        Console.WriteLine($"{"UTC",-20} {"ID",-38} {"Callsign",-12} {"Band",-8} {"Mode",-8} {"RST S",-6} {"RST R",-6}");
        Console.WriteLine(new string('-', 102));

        var count = 0u;

        while (await call.ResponseStream.MoveNext(CancellationToken.None))
        {
            var qso = call.ResponseStream.Current.Qso;
            if (qso is null)
            {
                continue;
            }

            var utc = qso.UtcTimestamp?.ToDateTime().ToString("u") ?? "";
            var band = FormatBand(qso.Band);
            var mode = FormatMode(qso.Mode);
            var rstS = FormatRst(qso.RstSent);
            var rstR = FormatRst(qso.RstReceived);

            Console.WriteLine($"{utc,-20} {qso.LocalId,-38} {qso.WorkedCallsign,-12} {band,-8} {mode,-8} {rstS,-6} {rstR,-6}");
            count++;
        }

        Console.WriteLine();
        Console.WriteLine($"{count} QSO(s)");

        return 0;
    }

    internal static bool TryCreateRequest(string[] args, out ListQsosRequest request, out string? error)
    {
        request = new ListQsosRequest { Limit = 20 };
        error = null;

        for (var i = 0; i < args.Length; i++)
        {
            switch (args[i])
            {
                case "--callsign" when i < args.Length - 1:
                    request.CallsignFilter = args[++i].ToUpperInvariant();
                    break;
                case "--callsign":
                    error = "Missing value for --callsign.";
                    return false;
                case "--band" when i < args.Length - 1:
                    try
                    {
                        request.BandFilter = EnumHelpers.ParseBand(args[++i]);
                    }
                    catch (ArgumentException ex)
                    {
                        error = ex.Message;
                        return false;
                    }

                    break;
                case "--band":
                    error = "Missing value for --band.";
                    return false;
                case "--mode" when i < args.Length - 1:
                    try
                    {
                        request.ModeFilter = EnumHelpers.ParseMode(args[++i]);
                    }
                    catch (ArgumentException ex)
                    {
                        error = ex.Message;
                        return false;
                    }

                    break;
                case "--mode":
                    error = "Missing value for --mode.";
                    return false;
                case "--after" when i < args.Length - 1:
                    var after = TimeParser.Parse(args[++i]);
                    if (after is null)
                    {
                        error = "Invalid --after value. Use relative (2.days, 3.hours) or absolute (2026-04-10).";
                        return false;
                    }

                    request.After = after;
                    break;
                case "--after":
                    error = "Missing value for --after.";
                    return false;
                case "--before" when i < args.Length - 1:
                    var before = TimeParser.Parse(args[++i]);
                    if (before is null)
                    {
                        error = "Invalid --before value. Use relative (2.days, 3.hours) or absolute (2026-04-10).";
                        return false;
                    }

                    request.Before = before;
                    break;
                case "--before":
                    error = "Missing value for --before.";
                    return false;
                case "--limit" when i < args.Length - 1:
                    var limitValue = args[++i];
                    if (!uint.TryParse(limitValue, out var limit))
                    {
                        error = $"Invalid value for --limit: {limitValue}";
                        return false;
                    }

                    request.Limit = limit;
                    break;
                case "--limit":
                    error = "Missing value for --limit.";
                    return false;
                default:
                    error = $"Unknown option: {args[i]}";
                    return false;
            }
        }

        return true;
    }

    internal static string FormatRst(RstReport? rst)
    {
        if (rst is null)
        {
            return "";
        }

        return rst.HasTone
            ? $"{rst.Readability}{rst.Strength}{rst.Tone}"
            : $"{rst.Readability}{rst.Strength}";
    }
}

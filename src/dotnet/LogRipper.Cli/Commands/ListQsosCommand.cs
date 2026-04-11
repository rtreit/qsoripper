using Grpc.Core;
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
        var request = new ListQsosRequest { Limit = 20 };

        for (var i = 0; i < args.Length - 1; i++)
        {
            switch (args[i])
            {
                case "--callsign":
                    request.CallsignFilter = args[++i].ToUpperInvariant();
                    break;
                case "--band":
                    try
                    {
                        request.BandFilter = EnumHelpers.ParseBand(args[++i]);
                    }
                    catch (ArgumentException ex)
                    {
                        Console.Error.WriteLine(ex.Message);
                        return 1;
                    }

                    break;
                case "--mode":
                    try
                    {
                        request.ModeFilter = EnumHelpers.ParseMode(args[++i]);
                    }
                    catch (ArgumentException ex)
                    {
                        Console.Error.WriteLine(ex.Message);
                        return 1;
                    }

                    break;
                case "--limit":
                    if (uint.TryParse(args[++i], out var limit))
                    {
                        request.Limit = limit;
                    }

                    break;
            }
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

    private static string FormatRst(RstReport? rst)
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

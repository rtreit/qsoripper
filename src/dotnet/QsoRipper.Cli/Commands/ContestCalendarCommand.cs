using System.Globalization;
using Google.Protobuf.Collections;
using Google.Protobuf.WellKnownTypes;
using Grpc.Net.Client;
using QsoRipper.Domain;
using QsoRipper.Services;

namespace QsoRipper.Cli.Commands;

internal static class ContestCalendarCommand
{
    public static async Task<int> RunAsync(GrpcChannel channel, string[] args, bool refresh, bool jsonOutput)
    {
        if (!TryParseOptions(args, out var options, out var error))
        {
            Console.Error.WriteLine(error);
            return 1;
        }

        var client = new ContestCalendarService.ContestCalendarServiceClient(channel);
        if (options.Refresh || refresh)
        {
            var response = await client.RefreshContestCalendarAsync(new RefreshContestCalendarRequest());
            return HandleRefreshResponse(response, jsonOutput);
        }

        var request = new GetActiveContestsRequest
        {
            IncludePartialMatches = options.IncludePartialMatches,
            LookaheadMinutes = options.LookaheadMinutes
        };

        if (options.AtUtc is not null)
        {
            request.AtUtc = Timestamp.FromDateTimeOffset(options.AtUtc.Value);
        }

        if (options.Band is not null)
        {
            request.Band = options.Band.Value;
        }

        if (options.Mode is not null)
        {
            request.Mode = options.Mode.Value;
        }

        var activeResponse = await client.GetActiveContestsAsync(request);
        return HandleActiveResponse(activeResponse, jsonOutput);
    }

    internal static int HandleActiveResponse(GetActiveContestsResponse response, bool jsonOutput)
    {
        if (jsonOutput)
        {
            JsonOutput.Print(response);
            return StatusExitCode(response.Status);
        }

        PrintHeader(response.Status, response.FetchedAt, response.ValidUntil, response.HasErrorMessage ? response.ErrorMessage : null);

        if (response.Contests.Count == 0)
        {
            Console.WriteLine("No active contests matched.");
            return StatusExitCode(response.Status);
        }

        foreach (var contest in response.Contests)
        {
            PrintContest(contest);
        }

        return StatusExitCode(response.Status);
    }

    internal static int HandleRefreshResponse(RefreshContestCalendarResponse response, bool jsonOutput)
    {
        if (jsonOutput)
        {
            JsonOutput.Print(response);
            return StatusExitCode(response.Status);
        }

        PrintHeader(response.Status, response.FetchedAt, response.ValidUntil, response.HasErrorMessage ? response.ErrorMessage : null);
        Console.WriteLine($"Loaded contests:  {response.Contests.Count.ToString(CultureInfo.InvariantCulture)}");

        foreach (var contest in response.Contests)
        {
            PrintContest(contest);
        }

        return StatusExitCode(response.Status);
    }

    private static bool TryParseOptions(string[] args, out ContestCalendarOptions options, out string error)
    {
        options = new ContestCalendarOptions();
        error = string.Empty;

        for (var i = 0; i < args.Length; i++)
        {
            var arg = args[i];

            switch (arg)
            {
                case "active" when i == 0:
                    break;
                case "refresh" when i == 0:
                    options = options with { Refresh = true };
                    break;
                case "--band":
                    if (!TryReadValue(args, ref i, arg, out var bandValue, out error))
                    {
                        return false;
                    }

                    try
                    {
                        options = options with { Band = EnumHelpers.ParseBand(bandValue) };
                    }
                    catch (ArgumentException ex)
                    {
                        error = ex.Message;
                        return false;
                    }

                    break;
                case "--mode":
                    if (!TryReadValue(args, ref i, arg, out var modeValue, out error))
                    {
                        return false;
                    }

                    try
                    {
                        options = options with { Mode = EnumHelpers.ParseMode(modeValue) };
                    }
                    catch (ArgumentException ex)
                    {
                        error = ex.Message;
                        return false;
                    }

                    break;
                case "--at":
                    if (!TryReadValue(args, ref i, arg, out var atValue, out error))
                    {
                        return false;
                    }

                    if (!DateTimeOffset.TryParse(atValue, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var atUtc))
                    {
                        error = $"Invalid --at value: {atValue}. Use an ISO-8601 UTC timestamp.";
                        return false;
                    }

                    options = options with { AtUtc = atUtc.ToUniversalTime() };
                    break;
                case "--lookahead-hours":
                    if (!TryReadValue(args, ref i, arg, out var lookaheadValue, out error))
                    {
                        return false;
                    }

                    if (!TryParseLookaheadHours(lookaheadValue, out var lookaheadMinutes))
                    {
                        error = $"Invalid --lookahead-hours value: {lookaheadValue}.";
                        return false;
                    }

                    options = options with { LookaheadMinutes = lookaheadMinutes };
                    break;
                case "--exact-matches":
                    options = options with { IncludePartialMatches = false };
                    break;
                case "--include-partial":
                    options = options with { IncludePartialMatches = true };
                    break;
                default:
                    error = $"Unknown contests argument: {arg}";
                    return false;
            }
        }

        return true;
    }

    internal static bool TryParseLookaheadHours(string value, out uint lookaheadMinutes)
    {
        lookaheadMinutes = 0;
        if (!uint.TryParse(value, NumberStyles.None, CultureInfo.InvariantCulture, out var lookaheadHours)
            || lookaheadHours > uint.MaxValue / 60)
        {
            return false;
        }

        lookaheadMinutes = lookaheadHours * 60;
        return true;
    }

    private static bool TryReadValue(string[] args, ref int index, string optionName, out string value, out string error)
    {
        if (index == args.Length - 1)
        {
            value = string.Empty;
            error = $"Missing value for {optionName}.";
            return false;
        }

        value = args[++index];
        error = string.Empty;
        return true;
    }

    private static void PrintHeader(ContestCalendarStatus status, Timestamp? fetchedAt, Timestamp? validUntil, string? error)
    {
        Console.WriteLine($"Status:           {FormatStatus(status)}");
        Console.WriteLine($"Fetched at:       {FormatTimestamp(fetchedAt)}");
        Console.WriteLine($"Valid until:      {FormatTimestamp(validUntil)}");

        if (!string.IsNullOrWhiteSpace(error))
        {
            Console.WriteLine($"Error:            {error}");
        }
    }

    private static void PrintContest(ContestCalendarEntry contest)
    {
        Console.WriteLine();
        Console.WriteLine(contest.Name);
        Console.WriteLine($"  UTC window:     {FormatTimestamp(contest.StartTimeUtc)} to {FormatTimestamp(contest.EndTimeUtc)}");
        Console.WriteLine($"  Local window:   {FormatLocalTimestamp(contest.StartTimeUtc)} to {FormatLocalTimestamp(contest.EndTimeUtc)}");
        Console.WriteLine($"  Bands:          {FormatBands(contest.Bands)}");
        Console.WriteLine($"  Modes:          {FormatModes(contest.Modes)}");
        Console.WriteLine($"  Exchange:       {FormatOptional(contest.HasExchange ? contest.Exchange : null)}");
        Console.WriteLine($"  Details:        {FormatDetailsStatus(contest.DetailsStatus)}");
        Console.WriteLine($"  Source:         {FormatOptional(contest.SourceName)}");

        if (contest.HasRulesUrl)
        {
            Console.WriteLine($"  Rules:          {contest.RulesUrl}");
        }

        if (contest.HasSourceUrl)
        {
            Console.WriteLine($"  Source URL:     {contest.SourceUrl}");
        }
    }

    private static int StatusExitCode(ContestCalendarStatus status)
    {
        return status is ContestCalendarStatus.Error or ContestCalendarStatus.Disabled ? 1 : 0;
    }

    private static string FormatStatus(ContestCalendarStatus status)
    {
        return status switch
        {
            ContestCalendarStatus.Current => "current",
            ContestCalendarStatus.Stale => "stale",
            ContestCalendarStatus.Error => "error",
            ContestCalendarStatus.Disabled => "disabled",
            _ => "unspecified"
        };
    }

    private static string FormatDetailsStatus(ContestDetailsStatus status)
    {
        return status switch
        {
            ContestDetailsStatus.MetadataOnly => "metadata only",
            ContestDetailsStatus.Partial => "partial",
            ContestDetailsStatus.Full => "full",
            _ => "unspecified"
        };
    }

    private static string FormatTimestamp(Timestamp? timestamp)
    {
        return timestamp is null ? "(unavailable)" : timestamp.ToDateTime().ToString("u", CultureInfo.InvariantCulture);
    }

    private static string FormatLocalTimestamp(Timestamp? timestamp)
    {
        return timestamp is null
            ? "(unavailable)"
            : timestamp.ToDateTimeOffset().ToLocalTime().ToString("yyyy-MM-dd HH:mm:ss zzz", CultureInfo.InvariantCulture);
    }

    private static string FormatBands(RepeatedField<Band> bands)
    {
        return bands.Count == 0 ? "(unknown from source)" : string.Join(", ", bands.Select(EnumHelpers.FormatBand));
    }

    private static string FormatModes(RepeatedField<Mode> modes)
    {
        return modes.Count == 0 ? "(unknown from source)" : string.Join(", ", modes.Select(EnumHelpers.FormatMode));
    }

    private static string FormatOptional(string? value)
    {
        return string.IsNullOrWhiteSpace(value) ? "(unavailable)" : value;
    }

    private sealed record ContestCalendarOptions(
        bool Refresh = false,
        Band? Band = null,
        Mode? Mode = null,
        DateTimeOffset? AtUtc = null,
        uint LookaheadMinutes = 0,
        bool IncludePartialMatches = true);
}

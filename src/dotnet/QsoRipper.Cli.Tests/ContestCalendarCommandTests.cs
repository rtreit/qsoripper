using Google.Protobuf.WellKnownTypes;
using QsoRipper.Cli.Commands;
using QsoRipper.Domain;
using QsoRipper.Services;

namespace QsoRipper.Cli.Tests;

#pragma warning disable CA1707
[Collection("ConsoleCapture")]
public sealed class ContestCalendarCommandTests
{
    [Fact]
    public void HandleActiveResponse_writes_contest_details()
    {
        var response = new GetActiveContestsResponse
        {
            Status = ContestCalendarStatus.Current,
            FetchedAt = UtcTimestamp(2026, 5, 24, 15, 0, 0),
            ValidUntil = UtcTimestamp(2026, 5, 24, 16, 0, 0),
        };
        response.Contests.Add(new ContestCalendarEntry
        {
            ContestId = "wa7bnm-test",
            Name = "Example Sprint",
            StartTimeUtc = UtcTimestamp(2026, 5, 24, 16, 0, 0),
            EndTimeUtc = UtcTimestamp(2026, 5, 24, 20, 0, 0),
            Exchange = "RST + serial",
            SourceName = "WA7BNM Contest Calendar",
            SourceUrl = "https://www.contestcalendar.com/",
            DetailsStatus = ContestDetailsStatus.MetadataOnly,
        });

        var output = ConsoleCapture.Out(() => Assert.Equal(0, ContestCalendarCommand.HandleActiveResponse(response, false)));

        Assert.Contains("Status:           current", output, StringComparison.Ordinal);
        Assert.Contains("Example Sprint", output, StringComparison.Ordinal);
        Assert.Contains("UTC window:     2026-05-24 16:00:00Z to 2026-05-24 20:00:00Z", output, StringComparison.Ordinal);
        Assert.Contains("Exchange:       RST + serial", output, StringComparison.Ordinal);
        Assert.Contains("Details:        metadata only", output, StringComparison.Ordinal);
        Assert.Contains("Source URL:     https://www.contestcalendar.com/", output, StringComparison.Ordinal);
    }

    [Fact]
    public void HandleActiveResponse_reports_empty_matches()
    {
        var response = new GetActiveContestsResponse
        {
            Status = ContestCalendarStatus.Current,
            FetchedAt = UtcTimestamp(2026, 5, 24, 15, 0, 0),
            ValidUntil = UtcTimestamp(2026, 5, 24, 16, 0, 0),
        };

        var output = ConsoleCapture.Out(() => Assert.Equal(0, ContestCalendarCommand.HandleActiveResponse(response, false)));

        Assert.Contains("No active contests matched.", output, StringComparison.Ordinal);
    }

    [Fact]
    public void HandleActiveResponse_returns_error_for_disabled_status()
    {
        var response = new GetActiveContestsResponse
        {
            Status = ContestCalendarStatus.Disabled,
            ErrorMessage = "contest calendar disabled",
        };

        var output = ConsoleCapture.Out(() => Assert.Equal(1, ContestCalendarCommand.HandleActiveResponse(response, false)));

        Assert.Contains("Status:           disabled", output, StringComparison.Ordinal);
        Assert.Contains("Error:            contest calendar disabled", output, StringComparison.Ordinal);
    }

    [Fact]
    public void HandleActiveResponse_writes_json_when_requested()
    {
        var response = new GetActiveContestsResponse
        {
            Status = ContestCalendarStatus.Stale,
        };

        var output = ConsoleCapture.Out(() => Assert.Equal(0, ContestCalendarCommand.HandleActiveResponse(response, true)));

        Assert.Contains("\"status\": \"CONTEST_CALENDAR_STATUS_STALE\"", output, StringComparison.Ordinal);
    }

    [Fact]
    public void TryParseLookaheadHours_converts_to_minutes()
    {
        Assert.True(ContestCalendarCommand.TryParseLookaheadHours("12", out var minutes));
        Assert.Equal(720U, minutes);
    }

    [Theory]
    [InlineData("-1")]
    [InlineData("1.5")]
    [InlineData("71582789")]
    public void TryParseLookaheadHours_rejects_invalid_values(string value)
    {
        Assert.False(ContestCalendarCommand.TryParseLookaheadHours(value, out _));
    }

    private static Timestamp UtcTimestamp(int year, int month, int day, int hour, int minute, int second)
    {
        return Timestamp.FromDateTime(DateTime.SpecifyKind(new DateTime(year, month, day, hour, minute, second), DateTimeKind.Utc));
    }
}
#pragma warning restore CA1707

#pragma warning disable CA1707 // xUnit method names use underscores.

using System.Globalization;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar.Tests;

public sealed class ContestCalendarMonitorTests
{
    [Fact]
    public void CurrentSnapshot_NoCacheFetchesFromProvider()
    {
        var provider = new FakeProvider(MakeContest());
        var clock = new TestClock(ParseUtc("2026-05-24T16:00:00Z"));
        var monitor = new ContestCalendarMonitor(provider, TimeSpan.FromHours(1), TimeSpan.FromDays(1), clock);

        var result = monitor.CurrentSnapshot();

        Assert.Equal(ContestCalendarStatus.Current, result.Status);
        Assert.Single(result.Contests);
        Assert.Equal(1, provider.FetchCount);
    }

    [Fact]
    public void CurrentSnapshot_FreshCacheReturnsWithoutFetching()
    {
        var provider = new FakeProvider(MakeContest());
        var clock = new TestClock(ParseUtc("2026-05-24T16:00:00Z"));
        var monitor = new ContestCalendarMonitor(provider, TimeSpan.FromHours(1), TimeSpan.FromDays(1), clock);

        monitor.CurrentSnapshot();
        clock.Advance(TimeSpan.FromMinutes(5));
        monitor.CurrentSnapshot();

        Assert.Equal(1, provider.FetchCount);
    }

    [Fact]
    public void RefreshFailure_WithCacheReturnsStaleCached()
    {
        var provider = new FakeProvider(MakeContest());
        var clock = new TestClock(ParseUtc("2026-05-24T16:00:00Z"));
        var monitor = new ContestCalendarMonitor(provider, TimeSpan.FromMinutes(1), TimeSpan.FromDays(1), clock);

        monitor.CurrentSnapshot();
        provider.FailWith(ContestCalendarProviderException.Transport("connection refused"));
        clock.Advance(TimeSpan.FromMinutes(2));
        var result = monitor.CurrentSnapshot();

        Assert.Equal(ContestCalendarStatus.Stale, result.Status);
        Assert.Contains("connection refused", result.ErrorMessage, StringComparison.Ordinal);
        Assert.Single(result.Contests);
    }

    [Fact]
    public void DisabledProviderWithoutCacheReturnsDisabled()
    {
        var monitor = new ContestCalendarMonitor(
            new DisabledContestCalendarProvider("disabled"),
            TimeSpan.FromHours(1),
            TimeSpan.FromDays(1),
            new TestClock(ParseUtc("2026-05-24T16:00:00Z")));

        var result = monitor.CurrentSnapshot();

        Assert.Equal(ContestCalendarStatus.Disabled, result.Status);
        Assert.Contains("disabled", result.ErrorMessage, StringComparison.Ordinal);
    }

    private static ContestCalendarEntry MakeContest()
    {
        return new ContestCalendarEntry
        {
            ContestId = "contest",
            Name = "Contest",
            StartTimeUtc = Timestamp.FromDateTimeOffset(ParseUtc("2026-05-24T16:00:00Z")),
            EndTimeUtc = Timestamp.FromDateTimeOffset(ParseUtc("2026-05-24T20:00:00Z")),
            SourceName = "WA7BNM Contest Calendar",
            DetailsStatus = ContestDetailsStatus.MetadataOnly,
        };
    }

    private static DateTimeOffset ParseUtc(string value)
    {
        return DateTimeOffset.Parse(value, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal);
    }

    private sealed class FakeProvider(ContestCalendarEntry? contest) : IContestCalendarProvider
    {
        private ContestCalendarProviderException? _exception;

        public int FetchCount { get; private set; }

        public IReadOnlyList<ContestCalendarEntry> FetchContests()
        {
            FetchCount++;
            if (_exception is not null)
            {
                throw _exception;
            }

            return contest is null ? [] : [contest];
        }

        public void FailWith(ContestCalendarProviderException exception)
        {
            _exception = exception;
        }
    }

    private sealed class TestClock(DateTimeOffset now) : TimeProvider
    {
        private DateTimeOffset _now = now;

        public override DateTimeOffset GetUtcNow() => _now;

        public void Advance(TimeSpan elapsed)
        {
            _now = _now.Add(elapsed);
        }
    }
}

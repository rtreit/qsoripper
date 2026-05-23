using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// On-demand cache and refresh layer for contest calendar data.
/// Thread-safe; no background polling.
/// </summary>
public sealed class ContestCalendarMonitor
{
    private readonly IContestCalendarProvider _provider;
    private readonly TimeSpan _refreshInterval;
    private readonly TimeSpan _staleAfter;
    private readonly TimeProvider _clock;
    private readonly Lock _lock = new();
    private ContestCalendarSnapshot? _cached;
    private DateTimeOffset _lastFetchTime;

    public ContestCalendarMonitor(
        IContestCalendarProvider provider,
        TimeSpan refreshInterval,
        TimeSpan staleAfter,
        TimeProvider? clock = null)
    {
        ArgumentNullException.ThrowIfNull(provider);

        _provider = provider;
        _refreshInterval = refreshInterval;
        _staleAfter = staleAfter;
        _clock = clock ?? TimeProvider.System;
    }

    /// <summary>
    /// Return the cached snapshot if still fresh, otherwise fetch a new one.
    /// </summary>
    public ContestCalendarSnapshot CurrentSnapshot()
    {
        lock (_lock)
        {
            if (_cached is not null)
            {
                var elapsed = _clock.GetUtcNow() - _lastFetchTime;
                if (elapsed < _refreshInterval)
                {
                    var cached = _cached.Clone();
                    if (elapsed >= _staleAfter)
                    {
                        cached.Status = ContestCalendarStatus.Stale;
                    }

                    return cached;
                }
            }

            return RefreshNoLock();
        }
    }

    /// <summary>
    /// Force a fresh fetch from the provider, ignoring any cached data.
    /// </summary>
    public ContestCalendarSnapshot RefreshSnapshot()
    {
        lock (_lock)
        {
            return RefreshNoLock();
        }
    }

    private ContestCalendarSnapshot RefreshNoLock()
    {
        try
        {
            var now = _clock.GetUtcNow();
            var snapshot = new ContestCalendarSnapshot
            {
                Status = ContestCalendarStatus.Current,
                FetchedAt = Timestamp.FromDateTimeOffset(now),
                ValidUntil = Timestamp.FromDateTimeOffset(now.Add(_refreshInterval)),
            };

            foreach (var contest in _provider.FetchContests())
            {
                snapshot.Contests.Add(contest.Clone());
            }
            _cached = snapshot.Clone();
            _lastFetchTime = now;
            return snapshot;
        }
        catch (ContestCalendarProviderException ex)
        {
            if (_cached is not null)
            {
                var stale = _cached.Clone();
                stale.Status = ContestCalendarStatus.Stale;
                stale.ErrorMessage = ex.Message;
                return stale;
            }

            return new ContestCalendarSnapshot
            {
                Status = ex.Kind == ContestCalendarProviderErrorKind.Disabled
                    ? ContestCalendarStatus.Disabled
                    : ContestCalendarStatus.Error,
                FetchedAt = Timestamp.FromDateTimeOffset(_clock.GetUtcNow()),
                ErrorMessage = ex.Message,
            };
        }
    }
}

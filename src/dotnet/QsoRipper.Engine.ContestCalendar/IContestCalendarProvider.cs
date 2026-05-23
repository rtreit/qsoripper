using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// Abstraction over an external contest calendar provider.
/// </summary>
public interface IContestCalendarProvider
{
    /// <summary>Fetch fresh normalized contest calendar entries.</summary>
    IReadOnlyList<ContestCalendarEntry> FetchContests();
}

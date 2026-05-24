using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// Provider used when contest calendar fetching is disabled.
/// </summary>
public sealed class DisabledContestCalendarProvider(string reason) : IContestCalendarProvider
{
    public IReadOnlyList<ContestCalendarEntry> FetchContests()
    {
        throw ContestCalendarProviderException.Disabled(reason);
    }
}

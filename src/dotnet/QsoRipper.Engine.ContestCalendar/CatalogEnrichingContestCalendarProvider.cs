using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// Decorates contest calendar metadata with reviewed local contest details.
/// </summary>
public sealed class CatalogEnrichingContestCalendarProvider(
    IContestCalendarProvider inner,
    ContestDetailsCatalog catalog) : IContestCalendarProvider
{
    /// <inheritdoc />
    public IReadOnlyList<ContestCalendarEntry> FetchContests()
    {
        return inner.FetchContests().Select(catalog.Enrich).ToArray();
    }
}

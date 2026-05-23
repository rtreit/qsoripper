using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;
using System.Collections.ObjectModel;

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// Current contest calendar entries plus freshness metadata.
/// </summary>
public sealed class ContestCalendarSnapshot
{
    /// <summary>Gets contest calendar entries.</summary>
    public Collection<ContestCalendarEntry> Contests { get; } = [];

    /// <summary>Gets or sets snapshot freshness/error status.</summary>
    public ContestCalendarStatus Status { get; set; }

    /// <summary>Gets or sets UTC time when this snapshot was fetched.</summary>
    public Timestamp? FetchedAt { get; set; }

    /// <summary>Gets or sets UTC time until which the snapshot should be treated as fresh.</summary>
    public Timestamp? ValidUntil { get; set; }

    /// <summary>Gets or sets provider error text, when present.</summary>
    public string? ErrorMessage { get; set; }

    /// <summary>Create a defensive copy of the snapshot.</summary>
    public ContestCalendarSnapshot Clone()
    {
        var clone = new ContestCalendarSnapshot
        {
            Status = Status,
            FetchedAt = FetchedAt?.Clone(),
            ValidUntil = ValidUntil?.Clone(),
            ErrorMessage = ErrorMessage,
        };

        foreach (var contest in Contests)
        {
            clone.Contests.Add(contest.Clone());
        }

        return clone;
    }
}

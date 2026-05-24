using System.Text.Json;
using System.Text.Json.Serialization;
using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// Reviewed local contest details used to enrich calendar metadata.
/// </summary>
public sealed class ContestDetailsCatalog
{
    private readonly IReadOnlyList<ContestDetailsCatalogEntry> _entries;

    private ContestDetailsCatalog(IReadOnlyList<ContestDetailsCatalogEntry> entries)
    {
        _entries = entries;
    }

    /// <summary>Load a reviewed contest details catalog from JSON.</summary>
    public static ContestDetailsCatalog Load(string path)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);

        try
        {
            using var stream = File.OpenRead(path);
            var document = JsonSerializer.Deserialize(stream, ContestDetailsCatalogJsonContext.Default.ContestDetailsCatalogFile)
                ?? new ContestDetailsCatalogFile();
            return new ContestDetailsCatalog(document.Entries.Select(ToEntry).ToArray());
        }
        catch (IOException ex)
        {
            throw ContestCalendarProviderException.Parse($"Failed to read contest details catalog '{path}': {ex.Message}");
        }
        catch (JsonException ex)
        {
            throw ContestCalendarProviderException.Parse($"Failed to parse contest details catalog '{path}': {ex.Message}");
        }
    }

    /// <summary>Enrich a contest entry with matching local catalog details.</summary>
    public ContestCalendarEntry Enrich(ContestCalendarEntry contest)
    {
        ArgumentNullException.ThrowIfNull(contest);

        var match = _entries.FirstOrDefault(entry => entry.IsMatch(contest));
        if (match is null)
        {
            return contest.Clone();
        }

        var enriched = contest.Clone();
        if (match.Bands.Count > 0)
        {
            enriched.Bands.Clear();
            enriched.Bands.Add(match.Bands);
        }

        if (match.Modes.Count > 0)
        {
            enriched.Modes.Clear();
            enriched.Modes.Add(match.Modes);
        }

        if (!string.IsNullOrWhiteSpace(match.Exchange))
        {
            enriched.Exchange = match.Exchange;
        }

        if (!string.IsNullOrWhiteSpace(match.RulesUrl))
        {
            enriched.RulesUrl = match.RulesUrl;
        }

        enriched.DetailsStatus = match.DetailsStatus;
        return enriched;
    }

    private static ContestDetailsCatalogEntry ToEntry(ContestDetailsCatalogItem item)
    {
        return new ContestDetailsCatalogEntry(
            Normalize(item.ContestId),
            Normalize(item.Name),
            Normalize(item.SourceUrl),
            item.Bands.Select(ParseBand).Where(static band => band != Band.Unspecified).ToArray(),
            item.Modes.Select(ParseMode).Where(static mode => mode != Mode.Unspecified).ToArray(),
            item.Exchange?.Trim(),
            item.RulesUrl?.Trim(),
            ParseDetailsStatus(item.DetailsStatus));
    }

    private static ContestDetailsStatus ParseDetailsStatus(string? value)
    {
        return Normalize(value) switch
        {
            "FULL" => ContestDetailsStatus.Full,
            "METADATAONLY" or "METADATA_ONLY" => ContestDetailsStatus.MetadataOnly,
            "PARTIAL" => ContestDetailsStatus.Partial,
            _ => ContestDetailsStatus.Partial,
        };
    }

    private static Band ParseBand(string value)
    {
        return Normalize(value) switch
        {
            "160M" => Band._160M,
            "80M" => Band._80M,
            "40M" => Band._40M,
            "30M" => Band._30M,
            "20M" => Band._20M,
            "17M" => Band._17M,
            "15M" => Band._15M,
            "12M" => Band._12M,
            "10M" => Band._10M,
            "6M" => Band._6M,
            "2M" => Band._2M,
            "70CM" => Band._70Cm,
            _ => Band.Unspecified,
        };
    }

    private static Mode ParseMode(string value)
    {
        return Normalize(value) switch
        {
            "CW" => Mode.Cw,
            "SSB" => Mode.Ssb,
            "RTTY" => Mode.Rtty,
            "FT8" => Mode.Ft8,
            "FM" => Mode.Fm,
            "AM" => Mode.Am,
            _ => Mode.Unspecified,
        };
    }

    private static string Normalize(string? value)
    {
        return string.IsNullOrWhiteSpace(value) ? string.Empty : value.Trim().ToUpperInvariant();
    }

    private sealed record ContestDetailsCatalogEntry(
        string ContestId,
        string Name,
        string SourceUrl,
        IReadOnlyList<Band> Bands,
        IReadOnlyList<Mode> Modes,
        string? Exchange,
        string? RulesUrl,
        ContestDetailsStatus DetailsStatus)
    {
        public bool IsMatch(ContestCalendarEntry contest)
        {
            if (ContestId.Length > 0 && string.Equals(ContestId, Normalize(contest.ContestId), StringComparison.Ordinal))
            {
                return true;
            }

            if (SourceUrl.Length > 0 && string.Equals(SourceUrl, Normalize(contest.SourceUrl), StringComparison.Ordinal))
            {
                return true;
            }

            return Name.Length > 0 && string.Equals(Name, Normalize(contest.Name), StringComparison.Ordinal);
        }
    }
}

internal sealed class ContestDetailsCatalogFile
{
    public ContestDetailsCatalogItem[] Entries { get; set; } = [];
}

internal sealed class ContestDetailsCatalogItem
{
    public string? ContestId { get; set; }

    public string? Name { get; set; }

    public string? SourceUrl { get; set; }

    public string[] Bands { get; set; } = [];

    public string[] Modes { get; set; } = [];

    public string? Exchange { get; set; }

    public string? RulesUrl { get; set; }

    public string? DetailsStatus { get; set; }
}

[JsonSourceGenerationOptions(PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase, PropertyNameCaseInsensitive = true)]
[JsonSerializable(typeof(ContestDetailsCatalogFile))]
internal sealed partial class ContestDetailsCatalogJsonContext : JsonSerializerContext;

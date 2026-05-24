using System.Globalization;
using System.Xml.Linq;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// WA7BNM RSS contest calendar provider.
/// </summary>
public sealed class Wa7bnmContestCalendarProvider : IContestCalendarProvider
{
    private readonly HttpClient _httpClient;
    private readonly string _rssUrl;

    public Wa7bnmContestCalendarProvider(HttpClient httpClient, Wa7bnmContestCalendarConfig config)
    {
        ArgumentNullException.ThrowIfNull(httpClient);
        ArgumentNullException.ThrowIfNull(config);

        _httpClient = httpClient;
        _rssUrl = config.RssUrl;
    }

    /// <inheritdoc/>
    public IReadOnlyList<ContestCalendarEntry> FetchContests()
    {
        string body;
        try
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, _rssUrl);
            using var response = _httpClient.Send(request, HttpCompletionOption.ResponseContentRead);
            response.EnsureSuccessStatusCode();
            body = response.Content.ReadAsStringAsync().GetAwaiter().GetResult();
        }
        catch (HttpRequestException ex)
        {
            throw ContestCalendarProviderException.Transport($"Failed to fetch WA7BNM contest calendar data: {ex.Message}");
        }
        catch (TaskCanceledException ex)
        {
            throw ContestCalendarProviderException.Transport($"WA7BNM contest calendar request timed out: {ex.Message}");
        }

        return Wa7bnmRssParser.Parse(body, _rssUrl);
    }
}

internal static class Wa7bnmRssParser
{
    public static IReadOnlyList<ContestCalendarEntry> Parse(string xml, string fallbackSourceUrl)
    {
        XDocument document;
        try
        {
            document = XDocument.Parse(xml);
        }
        catch (System.Xml.XmlException ex)
        {
            throw ContestCalendarProviderException.Parse(ex.Message);
        }

        var channel = document.Root?.Element("channel")
            ?? throw ContestCalendarProviderException.Parse("RSS channel is missing.");
        var year = DateTimeOffset.TryParse(
            channel.Element("lastBuildDate")?.Value,
            CultureInfo.InvariantCulture,
            DateTimeStyles.AssumeUniversal,
            out var builtAt)
            ? builtAt.Year
            : DateTimeOffset.UtcNow.Year;

        return channel
            .Elements("item")
            .Select(item => EntryFromItem(item, year, fallbackSourceUrl))
            .ToList();
    }

    private static ContestCalendarEntry EntryFromItem(XElement item, int sourceYear, string fallbackSourceUrl)
    {
        var title = item.Element("title")?.Value?.Trim();
        var description = item.Element("description")?.Value?.Trim();
        if (string.IsNullOrWhiteSpace(title) || string.IsNullOrWhiteSpace(description))
        {
            throw ContestCalendarProviderException.Parse("RSS contest item is missing a title or description.");
        }

        var (start, end) = ParseSchedule(description, sourceYear);
        var sourceUrl = item.Element("link")?.Value?.Trim();
        return new ContestCalendarEntry
        {
            ContestId = StableContestId(title, start),
            Name = title,
            StartTimeUtc = Timestamp.FromDateTimeOffset(start),
            EndTimeUtc = Timestamp.FromDateTimeOffset(end),
            SourceName = "WA7BNM Contest Calendar",
            SourceUrl = string.IsNullOrWhiteSpace(sourceUrl) ? fallbackSourceUrl : sourceUrl,
            DetailsStatus = ContestDetailsStatus.MetadataOnly,
        };
    }

    private static (DateTimeOffset Start, DateTimeOffset End) ParseSchedule(string description, int sourceYear)
    {
        try
        {
            var parts = description.Split(',', 2, StringSplitOptions.TrimEntries);
            if (parts.Length != 2)
            {
                throw ContestCalendarProviderException.Parse($"Missing comma in schedule '{description}'.");
            }

            var times = parts[0].Split('-', 2, StringSplitOptions.TrimEntries);
            if (times.Length != 2)
            {
                throw ContestCalendarProviderException.Parse($"Missing time range in schedule '{description}'.");
            }

            var date = DateTime.ParseExact(parts[1], ["MMM d", "MMMM d"], CultureInfo.InvariantCulture, DateTimeStyles.None);
            var startTime = ParseUtcTime(times[0]);
            var endTime = ParseUtcTime(times[1]);
            var start = new DateTimeOffset(sourceYear, date.Month, date.Day, startTime.Hour, startTime.Minute, 0, TimeSpan.Zero);
            var end = new DateTimeOffset(sourceYear, date.Month, date.Day, endTime.Hour, endTime.Minute, 0, TimeSpan.Zero);
            if (end <= start)
            {
                end = end.AddDays(1);
            }

            return (start, end);
        }
        catch (FormatException ex)
        {
            throw ContestCalendarProviderException.Parse($"Invalid schedule '{description}': {ex.Message}");
        }
    }

    private static TimeOnly ParseUtcTime(string value)
    {
        var trimmed = value.Trim().TrimEnd('Z');
        return TimeOnly.ParseExact(trimmed, "HHmm", CultureInfo.InvariantCulture, DateTimeStyles.None);
    }

    private static string StableContestId(string name, DateTimeOffset start)
    {
        var input = $"{name.Trim().ToUpperInvariant()}\n{start.ToUnixTimeSeconds()}";
        var hash = 14695981039346656037UL;
        foreach (var value in input.Select(character => (byte)character))
        {
            hash ^= value;
            hash *= 1099511628211UL;
        }

        return string.Create(CultureInfo.InvariantCulture, $"wa7bnm-{hash:x16}");
    }
}

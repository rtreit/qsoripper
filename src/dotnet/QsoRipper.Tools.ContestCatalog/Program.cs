using System.Globalization;
using System.Net;
using System.Net.Http.Headers;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Text.RegularExpressions;
using System.Xml.Linq;
using QsoRipper.Engine.ContestCalendar;

namespace QsoRipper.Tools.ContestCatalog;

internal static class Program
{
    private static async Task Main(string[] args)
    {
        var options = ContestCatalogOptions.Parse(args);
        using var httpClient = new HttpClient { Timeout = options.HttpTimeout };
        httpClient.DefaultRequestHeaders.UserAgent.Add(new ProductInfoHeaderValue("QsoRipperContestCatalog", "0.1"));

        var generator = new ContestCatalogGenerator(httpClient, TimeProvider.System);
        var catalog = await generator.Generate(options, CancellationToken.None).ConfigureAwait(false);

        var outputDirectory = Path.GetDirectoryName(Path.GetFullPath(options.OutputPath));
        if (!string.IsNullOrWhiteSpace(outputDirectory))
        {
            Directory.CreateDirectory(outputDirectory);
        }

        await using var output = File.Create(options.OutputPath);
        await JsonSerializer.SerializeAsync(output, catalog, ContestCatalogJsonContext.Default.GeneratedCatalog, CancellationToken.None).ConfigureAwait(false);
        await output.WriteAsync("\n"u8.ToArray()).ConfigureAwait(false);

        Console.WriteLine($"Wrote {catalog.Entries.Length} contest catalog entries to {options.OutputPath}. Review before copying into the engine default catalog.");
    }
}

internal sealed record ContestCatalogOptions(
    string SourceUrl,
    string? SeedPath,
    string OutputPath,
    bool PromoteCandidates,
    bool FetchOfficialRules,
    TimeSpan HttpTimeout)
{
    private const string DefaultCalendarUrl = "https://www.contestcalendar.com/contestcal.php";

    public static ContestCatalogOptions Parse(string[] args)
    {
        var sourceUrl = DefaultCalendarUrl;
        string? seedPath = null;
        var outputPath = Path.Combine("artifacts", "contest-calendar", "contest-details.generated.json");
        var promoteCandidates = false;
        var fetchOfficialRules = true;
        var timeout = TimeSpan.FromSeconds(15);

        for (var index = 0; index < args.Length; index++)
        {
            switch (args[index])
            {
                case "--calendar-url":
                case "--rss-url":
                    sourceUrl = ReadValue(args, ref index);
                    break;
                case "--seed":
                    seedPath = ReadValue(args, ref index);
                    break;
                case "--output":
                    outputPath = ReadValue(args, ref index);
                    break;
                case "--promote-candidates":
                    promoteCandidates = true;
                    break;
                case "--skip-official-rules":
                    fetchOfficialRules = false;
                    break;
                case "--timeout-seconds":
                    timeout = TimeSpan.FromSeconds(int.Parse(ReadValue(args, ref index), CultureInfo.InvariantCulture));
                    break;
                case "--help":
                case "-h":
                    PrintHelp();
                    Environment.Exit(0);
                    break;
                default:
                    throw new InvalidOperationException($"Unknown argument: {args[index]}");
            }
        }

        return new ContestCatalogOptions(sourceUrl, seedPath, outputPath, promoteCandidates, fetchOfficialRules, timeout);
    }

    private static string ReadValue(string[] args, ref int index)
    {
        if (index == args.Length - 1)
        {
            throw new InvalidOperationException($"Missing value for {args[index]}.");
        }

        return args[++index];
    }

    private static void PrintHelp()
    {
        Console.WriteLine(
            """
            QsoRipper contest details catalog generator

            Usage:
              dotnet run --project src\dotnet\QsoRipper.Tools.ContestCatalog -- [options]

            Options:
              --calendar-url URL        Contest calendar URL. Defaults to the 12-month WA7BNM calendar.
              --rss-url URL             Alias for --calendar-url.
              --seed PATH               Optional reviewed seed JSON containing official rules URLs and known fields.
              --output PATH             Output JSON path. Defaults to artifacts\contest-calendar\contest-details.generated.json.
              --promote-candidates      Write scraped candidate fields into canonical catalog fields.
              --skip-official-rules     Use calendar detail pages only; do not fetch official rules URLs.
              --timeout-seconds N       HTTP timeout. Defaults to 15 seconds.
            """);
    }
}

internal sealed class ContestCatalogGenerator(HttpClient httpClient, TimeProvider clock)
{
    public async Task<GeneratedCatalog> Generate(ContestCatalogOptions options, CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(options);

        var source = await httpClient.GetStringAsync(new Uri(options.SourceUrl), cancellationToken).ConfigureAwait(false);
        var contests = ContestSourceReader.Read(source, options.SourceUrl);
        var seeds = options.SeedPath is null ? [] : SeedCatalog.Load(options.SeedPath).Entries;
        using var concurrency = new SemaphoreSlim(8);

        var entries = await Task.WhenAll(contests.Select(contest => GenerateEntry(
            contest,
            seeds,
            options,
            concurrency,
            cancellationToken))).ConfigureAwait(false);

        return new GeneratedCatalog(
            1,
            clock.GetUtcNow(),
            "WA7BNM Contest Calendar",
            options.SourceUrl,
            entries
                .OrderBy(entry => entry.Name, StringComparer.OrdinalIgnoreCase)
                .ToArray());
    }

    private async Task<GeneratedCatalogEntry> GenerateEntry(
        RssContest contest,
        SeedCatalogEntry[] seeds,
        ContestCatalogOptions options,
        SemaphoreSlim concurrency,
        CancellationToken cancellationToken)
    {
        await concurrency.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            var seed = SeedCatalogEntry.FindMatch(seeds, contest);
            CandidateExtraction? candidates = null;
            var rulesUrl = seed?.RulesUrl;
            var rulesUrlFromSeed = !string.IsNullOrWhiteSpace(rulesUrl);
            if (string.IsNullOrWhiteSpace(rulesUrl) && IsContestCalendarDetailUrl(contest.SourceUrl))
            {
                var detailHtml = await httpClient.GetStringAsync(new Uri(contest.SourceUrl), cancellationToken).ConfigureAwait(false);
                candidates = CalendarDetailExtractor.Extract(contest.SourceUrl, detailHtml, clock.GetUtcNow());
                rulesUrl = candidates.RulesUrl;
            }

            if (options.FetchOfficialRules && !string.IsNullOrWhiteSpace(rulesUrl))
            {
                try
                {
                    RulePagePolicy.ThrowIfDenied(rulesUrl);
                    var rules = await httpClient.GetStringAsync(new Uri(rulesUrl), cancellationToken).ConfigureAwait(false);
                    candidates = CandidateExtraction.Merge(
                        RuleTextExtractor.Extract(rulesUrl, rules, clock.GetUtcNow(), rulesUrl),
                        candidates);
                }
                catch (Exception exception) when (!rulesUrlFromSeed && IsRecoverableRulesFetchFailure(exception))
                {
                    Console.Error.WriteLine($"Skipped official rules URL for {contest.Name}: {exception.Message}");
                }
            }

            return GeneratedCatalogEntry.From(contest, seed, candidates, options.PromoteCandidates);
        }
        finally
        {
            concurrency.Release();
        }
    }

    private static bool IsContestCalendarDetailUrl(string sourceUrl)
    {
        return Uri.TryCreate(sourceUrl, UriKind.Absolute, out var uri)
            && RulePagePolicy.IsContestCalendarHost(uri)
            && (uri.AbsolutePath.Contains("contestdetails", StringComparison.OrdinalIgnoreCase)
                || uri.AbsolutePath.Contains("weeklycontdetails", StringComparison.OrdinalIgnoreCase));
    }

    private static bool IsRecoverableRulesFetchFailure(Exception exception)
    {
        return exception is HttpRequestException
            or TaskCanceledException
            or InvalidOperationException;
    }
}

internal static class ContestSourceReader
{
    public static IReadOnlyList<RssContest> Read(string content, string sourceUrl)
    {
        return content.TrimStart().StartsWith("<rss", StringComparison.OrdinalIgnoreCase)
            || content.Contains("<rss", StringComparison.OrdinalIgnoreCase)
            ? ContestRssReader.Read(content, sourceUrl)
            : ContestCalendarPageReader.Read(content, sourceUrl);
    }
}

internal static class ContestRssReader
{
    public static IReadOnlyList<RssContest> Read(string xml, string fallbackSourceUrl)
    {
        var document = XDocument.Parse(xml);
        var channel = document.Root?.Element("channel") ?? throw new InvalidOperationException("RSS channel is missing.");
        return channel
            .Elements("item")
            .Select(item => new RssContest(
                item.Element("title")?.Value?.Trim() ?? throw new InvalidOperationException("RSS item is missing title."),
                item.Element("link")?.Value?.Trim() is { Length: > 0 } link ? link : fallbackSourceUrl))
            .ToArray();
    }
}

internal static partial class ContestCalendarPageReader
{
    public static IReadOnlyList<RssContest> Read(string html, string sourceUrl)
    {
        var baseUri = new Uri(sourceUrl);
        return CalendarEntryRegex()
            .Matches(html)
            .Select(match => new RssContest(
                CleanHtml(match.Groups["name"].Value),
                new Uri(baseUri, WebUtility.HtmlDecode(match.Groups["href"].Value)).AbsoluteUri))
            .Where(contest => !string.IsNullOrWhiteSpace(contest.Name))
            .GroupBy(contest => contest.Name, StringComparer.OrdinalIgnoreCase)
            .Select(group => group.First())
            .ToArray();
    }

    private static string CleanHtml(string html)
    {
        return WebUtility.HtmlDecode(TagRegex().Replace(html, " ")).Trim();
    }

    [GeneratedRegex("<tr[^>]*>\\s*<td[^>]*>\\s*<span[^>]*>\\s*<a[^>]*href=\"(?<href>[^\"]*contestdetails\\.php\\?ref=[^\"]+)\"[^>]*>\\+</a>\\s*</span>\\s*(?<name>.*?)</td>\\s*<td", RegexOptions.IgnoreCase | RegexOptions.Singleline, 100)]
    private static partial Regex CalendarEntryRegex();

    [GeneratedRegex("<[^>]+>", RegexOptions.IgnoreCase, 100)]
    private static partial Regex TagRegex();
}

internal static class RulePagePolicy
{
    private static readonly HashSet<string> DeniedHosts = new(StringComparer.OrdinalIgnoreCase)
    {
        "contestcalendar.com",
        "www.contestcalendar.com",
        "wa7bnm.com",
        "www.wa7bnm.com",
    };

    public static bool IsContestCalendarHost(Uri uri)
    {
        return DeniedHosts.Contains(uri.Host);
    }

    public static void ThrowIfDenied(string rulesUrl)
    {
        if (!Uri.TryCreate(rulesUrl, UriKind.Absolute, out var uri))
        {
            throw new InvalidOperationException($"Rules URL is not absolute: {rulesUrl}");
        }

        if (DeniedHosts.Contains(uri.Host))
        {
            throw new InvalidOperationException($"Refusing to scrape restricted contest calendar host: {uri.Host}");
        }
    }
}

internal static partial class CalendarDetailExtractor
{
    public static CandidateExtraction Extract(string sourceUrl, string html, DateTimeOffset fetchedAtUtc)
    {
        var rulesUrl = ExtractRulesUrl(sourceUrl, html);
        return new CandidateExtraction(
            ExtractBands(ExtractField(html, "Bands")),
            ExtractModes(ExtractField(html, "Mode")),
            ExtractExchange(ExtractField(html, "Exchange")),
            sourceUrl,
            fetchedAtUtc,
            "calendar-detail",
            rulesUrl);
    }

    private static string? ExtractField(string html, string label)
    {
        var match = Regex.Match(
            html,
            $@"<td[^>]*>\s*{Regex.Escape(label)}:\s*</td>\s*<td[^>]*>(?<value>.*?)</td>",
            RegexOptions.IgnoreCase | RegexOptions.Singleline,
            TimeSpan.FromMilliseconds(100));
        return match.Success ? CleanHtml(match.Groups["value"].Value) : null;
    }

    private static string? ExtractRulesUrl(string sourceUrl, string html)
    {
        var match = RulesUrlRegex().Match(html);
        return match.Success
            ? new Uri(new Uri(sourceUrl), WebUtility.HtmlDecode(match.Groups["href"].Value)).AbsoluteUri
            : null;
    }

    private static string[] ExtractBands(string? text)
    {
        if (string.IsNullOrWhiteSpace(text))
        {
            return [];
        }

        var bands = new SortedSet<string>(BandComparer.Instance);
        foreach (Match match in DetailBandRegex().Matches(text))
        {
            bands.Add($"{match.Groups[1].Value}m");
        }

        if (DetailSeventyCentimeterRegex().IsMatch(text))
        {
            bands.Add("70cm");
        }

        return bands.ToArray();
    }

    private static string[] ExtractModes(string? text)
    {
        if (string.IsNullOrWhiteSpace(text))
        {
            return [];
        }

        var modes = new SortedSet<string>(StringComparer.OrdinalIgnoreCase);
        RuleTextExtractor.AddModeIfPresent(text, "CW", "cw", modes);
        RuleTextExtractor.AddModeIfPresent(text, "SSB", "ssb", modes);
        RuleTextExtractor.AddModeIfPresent(text, "PHONE", "ssb", modes);
        RuleTextExtractor.AddModeIfPresent(text, "RTTY", "rtty", modes);
        RuleTextExtractor.AddModeIfPresent(text, "FT8", "ft8", modes);
        RuleTextExtractor.AddModeIfPresent(text, "FT4", "ft4", modes);
        RuleTextExtractor.AddModeIfPresent(text, "DIG", "digital", modes);
        RuleTextExtractor.AddModeIfPresent(text, "DIGITAL", "digital", modes);
        RuleTextExtractor.AddModeIfPresent(text, "FM", "fm", modes);
        RuleTextExtractor.AddModeIfPresent(text, "AM", "am", modes);
        return modes.ToArray();
    }

    private static string? ExtractExchange(string? text)
    {
        return string.IsNullOrWhiteSpace(text) ? null : text;
    }

    private static string CleanHtml(string html)
    {
        var withBreaks = BreakRegex().Replace(html, "; ");
        return WebUtility.HtmlDecode(TagRegex().Replace(withBreaks, " ")).Trim();
    }

    [GeneratedRegex("<br\\s*/?>", RegexOptions.IgnoreCase, 100)]
    private static partial Regex BreakRegex();

    [GeneratedRegex("<[^>]+>", RegexOptions.IgnoreCase, 100)]
    private static partial Regex TagRegex();

    [GeneratedRegex("Find\\s+rules\\s+at:\\s*</td>\\s*<td[^>]*>\\s*<a[^>]*href=\"(?<href>[^\"]+)\"", RegexOptions.IgnoreCase | RegexOptions.Singleline, 100)]
    private static partial Regex RulesUrlRegex();

    [GeneratedRegex("\\b(160|80|40|30|20|17|15|12|10|6|2)\\s*(?:m|meter|meters|metre|metres)?\\b", RegexOptions.IgnoreCase, 100)]
    private static partial Regex DetailBandRegex();

    [GeneratedRegex("\\b70\\s*(?:cm|centimeter|centimeters|centimetre|centimetres)\\b", RegexOptions.IgnoreCase, 100)]
    private static partial Regex DetailSeventyCentimeterRegex();
}

internal static partial class RuleTextExtractor
{
    public static CandidateExtraction Extract(string sourceUrl, string html, DateTimeOffset fetchedAtUtc, string? rulesUrl = null)
    {
        var text = NormalizeText(StripHtml(html));
        return new CandidateExtraction(
            ExtractBands(text),
            ExtractModes(text),
            ExtractExchange(text),
            sourceUrl,
            fetchedAtUtc,
            "official-rules",
            rulesUrl);
    }

    private static string StripHtml(string html)
    {
        var withoutScripts = ScriptOrStyleRegex().Replace(html, " ");
        var withoutTags = TagRegex().Replace(withoutScripts, " ");
        return WebUtility.HtmlDecode(withoutTags);
    }

    private static string NormalizeText(string text)
    {
        return WhitespaceRegex().Replace(text, " ").Trim();
    }

    private static string[] ExtractBands(string text)
    {
        var bands = new SortedSet<string>(BandComparer.Instance);
        foreach (Match match in BandRegex().Matches(text))
        {
            bands.Add($"{match.Groups[1].Value}m");
        }

        if (SeventyCentimeterRegex().IsMatch(text))
        {
            bands.Add("70cm");
        }

        return bands.ToArray();
    }

    private static string[] ExtractModes(string text)
    {
        var modes = new SortedSet<string>(StringComparer.OrdinalIgnoreCase);
        AddModeIfPresent(text, "CW", "cw", modes);
        AddModeIfPresent(text, "SSB", "ssb", modes);
        AddModeIfPresent(text, "PHONE", "ssb", modes);
        AddModeIfPresent(text, "RTTY", "rtty", modes);
        AddModeIfPresent(text, "FT8", "ft8", modes);
        AddModeIfPresent(text, "FT4", "ft4", modes);
        AddModeIfPresent(text, "DIG", "digital", modes);
        AddModeIfPresent(text, "DIGITAL", "digital", modes);
        AddModeIfPresent(text, "FM", "fm", modes);
        AddModeIfPresent(text, "AM", "am", modes);
        return modes.ToArray();
    }

    public static void AddModeIfPresent(string text, string token, string mode, SortedSet<string> modes)
    {
        if (Regex.IsMatch(text, $@"\b{Regex.Escape(token)}\b", RegexOptions.IgnoreCase, TimeSpan.FromMilliseconds(100)))
        {
            modes.Add(mode);
        }
    }

    private static string? ExtractExchange(string text)
    {
        var match = ExchangeRegex().Match(text);
        if (!match.Success)
        {
            return null;
        }

        var exchange = match.Groups[1].Value.Trim(' ', ':', '-', '–');
        return exchange.Length > 160 ? string.Concat(exchange.AsSpan(0, 157), "...") : exchange;
    }

    [GeneratedRegex("<(script|style)\\b[^>]*>.*?</\\1>", RegexOptions.IgnoreCase | RegexOptions.Singleline, 100)]
    private static partial Regex ScriptOrStyleRegex();

    [GeneratedRegex("<[^>]+>", RegexOptions.IgnoreCase, 100)]
    private static partial Regex TagRegex();

    [GeneratedRegex("\\s+", RegexOptions.None, 100)]
    private static partial Regex WhitespaceRegex();

    [GeneratedRegex("\\b(160|80|40|30|20|17|15|12|10|6|2)\\s*(?:m|meter|meters|metre|metres)\\b", RegexOptions.IgnoreCase, 100)]
    private static partial Regex BandRegex();

    [GeneratedRegex("\\b70\\s*(?:cm|centimeter|centimeters|centimetre|centimetres)\\b", RegexOptions.IgnoreCase, 100)]
    private static partial Regex SeventyCentimeterRegex();

    [GeneratedRegex("\\bexchange\\b\\s*(?:is|:)?\\s*([^.;]{1,220})", RegexOptions.IgnoreCase, 100)]
    private static partial Regex ExchangeRegex();
}

internal sealed class BandComparer : IComparer<string>
{
    public static readonly BandComparer Instance = new();

    private static readonly string[] Order = ["160m", "80m", "40m", "30m", "20m", "17m", "15m", "12m", "10m", "6m", "2m", "70cm"];

    public int Compare(string? x, string? y)
    {
        return Array.IndexOf(Order, x) is var left && Array.IndexOf(Order, y) is var right && left != right
            ? left.CompareTo(right)
            : string.Compare(x, y, StringComparison.OrdinalIgnoreCase);
    }
}

internal sealed record RssContest(string Name, string SourceUrl);

internal sealed record GeneratedCatalog(
    int Version,
    DateTimeOffset GeneratedAtUtc,
    string SourceName,
    string SourceUrl,
    GeneratedCatalogEntry[] Entries);

internal sealed record GeneratedCatalogEntry(
    string Name,
    string SourceUrl,
    string? RulesUrl,
    string[] Bands,
    string[] Modes,
    string? Exchange,
    string DetailsStatus,
    string ReviewStatus,
    [property: JsonIgnore]
    CandidateExtraction? CandidateExtraction)
{
    public string[]? CandidateBands => CandidateExtraction?.Bands;

    public string[]? CandidateModes => CandidateExtraction?.Modes;

    public string? CandidateExchange => CandidateExtraction?.Exchange;

    public string? CandidateSourceUrl => CandidateExtraction?.SourceUrl;

    public DateTimeOffset? CandidateFetchedAtUtc => CandidateExtraction?.FetchedAtUtc;

    public string? CandidateConfidence => CandidateExtraction?.Confidence;

    public static GeneratedCatalogEntry From(RssContest contest, SeedCatalogEntry? seed, CandidateExtraction? candidates, bool promoteCandidates)
    {
        var bands = seed?.Bands is { Length: > 0 } seedBands ? seedBands : [];
        var modes = seed?.Modes is { Length: > 0 } seedModes ? seedModes : [];
        var exchange = seed?.Exchange;
        if (promoteCandidates)
        {
            bands = bands.Length > 0 ? bands : candidates?.Bands ?? [];
            modes = modes.Length > 0 ? modes : candidates?.Modes ?? [];
            exchange = string.IsNullOrWhiteSpace(exchange) ? candidates?.Exchange : exchange;
        }

        return new GeneratedCatalogEntry(
            contest.Name,
            contest.SourceUrl,
            seed?.RulesUrl ?? candidates?.RulesUrl,
            bands,
            modes,
            exchange,
            bands.Length > 0 || modes.Length > 0 || !string.IsNullOrWhiteSpace(exchange) ? "partial" : "metadataOnly",
            promoteCandidates ? "generated" : "needsReview",
            promoteCandidates ? null : candidates);
    }
}

internal sealed record CandidateExtraction(
    string[] Bands,
    string[] Modes,
    string? Exchange,
    string SourceUrl,
    DateTimeOffset FetchedAtUtc,
    string Confidence,
    string? RulesUrl)
{
    public static CandidateExtraction? Merge(CandidateExtraction primary, CandidateExtraction? fallback)
    {
        return new CandidateExtraction(
            fallback?.Bands is { Length: > 0 } fallbackBands ? fallbackBands : primary.Bands,
            fallback?.Modes is { Length: > 0 } fallbackModes ? fallbackModes : primary.Modes,
            string.IsNullOrWhiteSpace(fallback?.Exchange) ? primary.Exchange : fallback.Exchange,
            primary.SourceUrl,
            primary.FetchedAtUtc,
            primary.Confidence,
            primary.RulesUrl ?? fallback?.RulesUrl);
    }
}

internal sealed record SeedCatalog(int Version, SeedCatalogEntry[] Entries)
{
    public static SeedCatalog Load(string path)
    {
        using var stream = File.OpenRead(path);
        var catalog = JsonSerializer.Deserialize(stream, ContestCatalogJsonContext.Default.SeedCatalog)
            ?? new SeedCatalog(1, []);
        return catalog.Entries is null ? catalog with { Entries = [] } : catalog;
    }
}

internal sealed record SeedCatalogEntry(
    string? Name,
    string? SourceUrl,
    string[] RssNameAliases,
    string? RulesUrl,
    string[] Bands,
    string[] Modes,
    string? Exchange)
{
    public static SeedCatalogEntry? FindMatch(IEnumerable<SeedCatalogEntry> seeds, RssContest contest)
    {
        return seeds.FirstOrDefault(seed =>
            Matches(seed.Name, contest.Name)
            || Matches(seed.SourceUrl, contest.SourceUrl)
            || (seed.RssNameAliases ?? []).Any(alias => Matches(alias, contest.Name)));
    }

    private static bool Matches(string? left, string right)
    {
        return !string.IsNullOrWhiteSpace(left)
            && string.Equals(left.Trim(), right.Trim(), StringComparison.OrdinalIgnoreCase);
    }
}

[JsonSourceGenerationOptions(
    PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase,
    PropertyNameCaseInsensitive = true,
    DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingDefault,
    WriteIndented = true)]
[JsonSerializable(typeof(GeneratedCatalog))]
[JsonSerializable(typeof(SeedCatalog))]
internal sealed partial class ContestCatalogJsonContext : JsonSerializerContext;

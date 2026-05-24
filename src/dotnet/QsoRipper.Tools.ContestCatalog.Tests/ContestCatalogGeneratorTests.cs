using System.Net;
using System.Text;
using System.Text.Json;
using QsoRipper.Tools.ContestCatalog;

namespace QsoRipper.Tools.ContestCatalog.Tests;

public sealed class ContestCatalogGeneratorTests
{
    [Fact]
    public async Task GenerateKeepsScrapedFieldsAsReviewCandidatesByDefault()
    {
        using var temp = new TempDirectory();
        var seedPath = Path.Combine(temp.Path, "seed.json");
        await File.WriteAllTextAsync(
            seedPath,
            """
            {
              "version": 1,
              "entries": [
                {
                  "name": "Sample CW Sprint",
                  "rulesUrl": "https://sponsor.example.test/rules.html"
                }
              ]
            }
            """);

        using var handler = new StubHttpMessageHandler(request =>
        {
            return request.RequestUri?.AbsoluteUri switch
            {
                "https://calendar.example.test/calendar.rss" => Rss("Sample CW Sprint", "https://contestcalendar.com/weeklycont.php#sample"),
                "https://sponsor.example.test/rules.html" => """
                    <html><body>
                    <p>Bands: 80 meters, 40 meters, 20 meters and 15 meters.</p>
                    <p>Mode: CW only.</p>
                    <p>Exchange: RST and state/province.</p>
                    </body></html>
                    """,
                _ => throw new InvalidOperationException($"Unexpected URL: {request.RequestUri}"),
            };
        });
        using var httpClient = new HttpClient(handler, disposeHandler: false);

        var generator = new ContestCatalogGenerator(
            httpClient,
            new FrozenTimeProvider(new DateTimeOffset(2026, 5, 23, 22, 0, 0, TimeSpan.Zero)));
        var catalog = await generator.Generate(
            new ContestCatalogOptions("https://calendar.example.test/calendar.rss", seedPath, "unused.json", false, true, TimeSpan.FromSeconds(5)),
            CancellationToken.None);

        var entry = Assert.Single(catalog.Entries);
        Assert.Empty(entry.Bands);
        Assert.Empty(entry.Modes);
        Assert.Null(entry.Exchange);
        Assert.Equal("metadataOnly", entry.DetailsStatus);
        Assert.Equal("needsReview", entry.ReviewStatus);
        Assert.NotNull(entry.CandidateExtraction);
        Assert.Equal(["80m", "40m", "20m", "15m"], entry.CandidateExtraction.Bands);
        Assert.Equal(["cw"], entry.CandidateExtraction.Modes);
        Assert.Equal("RST and state/province", entry.CandidateExtraction.Exchange);
    }

    [Fact]
    public async Task GeneratePromotesCandidatesOnlyWhenRequested()
    {
        using var temp = new TempDirectory();
        var seedPath = Path.Combine(temp.Path, "seed.json");
        await File.WriteAllTextAsync(
            seedPath,
            """
            {
              "version": 1,
              "entries": [
                {
                  "rssNameAliases": [ "Sample Phone Sprint" ],
                  "rulesUrl": "https://sponsor.example.test/phone.html"
                }
              ]
            }
            """);

        using var handler = new StubHttpMessageHandler(request =>
        {
            return request.RequestUri?.AbsoluteUri switch
            {
                "https://calendar.example.test/calendar.rss" => Rss("Sample Phone Sprint", "https://contestcalendar.com/weeklycont.php#phone"),
                "https://sponsor.example.test/phone.html" => """
                    <html><body>
                    <p>The event runs on 20m and 10m.</p>
                    <p>Mode: SSB.</p>
                    <p>Exchange: serial number.</p>
                    </body></html>
                    """,
                _ => throw new InvalidOperationException($"Unexpected URL: {request.RequestUri}"),
            };
        });
        using var httpClient = new HttpClient(handler, disposeHandler: false);

        var generator = new ContestCatalogGenerator(
            httpClient,
            new FrozenTimeProvider(new DateTimeOffset(2026, 5, 23, 22, 0, 0, TimeSpan.Zero)));
        var catalog = await generator.Generate(
            new ContestCatalogOptions("https://calendar.example.test/calendar.rss", seedPath, "unused.json", true, true, TimeSpan.FromSeconds(5)),
            CancellationToken.None);

        var entry = Assert.Single(catalog.Entries);
        Assert.Equal(["20m", "10m"], entry.Bands);
        Assert.Equal(["ssb"], entry.Modes);
        Assert.Equal("serial number", entry.Exchange);
        Assert.Equal("partial", entry.DetailsStatus);
        Assert.Equal("generated", entry.ReviewStatus);
        Assert.Null(entry.CandidateExtraction);
    }

    [Fact]
    public async Task GenerateRefusesToFetchContestCalendarRulesUrls()
    {
        using var temp = new TempDirectory();
        var seedPath = Path.Combine(temp.Path, "seed.json");
        await File.WriteAllTextAsync(
            seedPath,
            """
            {
              "version": 1,
              "entries": [
                {
                  "sourceUrl": "https://contestcalendar.com/weeklycont.php#restricted",
                  "rulesUrl": "https://www.contestcalendar.com/weeklycont.php#restricted"
                }
              ]
            }
            """);

        using var handler = new StubHttpMessageHandler(request =>
        {
            Assert.Equal("https://calendar.example.test/calendar.rss", request.RequestUri?.AbsoluteUri);
            return Rss("Restricted Contest", "https://contestcalendar.com/weeklycont.php#restricted");
        });
        using var httpClient = new HttpClient(handler, disposeHandler: false);
        var generator = new ContestCatalogGenerator(
            httpClient,
            new FrozenTimeProvider(new DateTimeOffset(2026, 5, 23, 22, 0, 0, TimeSpan.Zero)));

        var exception = await Assert.ThrowsAsync<InvalidOperationException>(() => generator.Generate(
            new ContestCatalogOptions("https://calendar.example.test/calendar.rss", seedPath, "unused.json", false, true, TimeSpan.FromSeconds(5)),
            CancellationToken.None));
        Assert.Contains("Refusing to scrape restricted contest calendar host", exception.Message, StringComparison.Ordinal);
    }

    [Fact]
    public async Task GenerateDiscoversDetailsFromCalendarPage()
    {
        using var handler = new StubHttpMessageHandler(request =>
        {
            return request.RequestUri?.AbsoluteUri switch
            {
                "https://www.contestcalendar.com/contestcal.php" => """
                    <html><body><table>
                    <tr><td><span class="expandlink"><a href="contestdetails.php?ref=498">+</a></span> CWops Test (CWT)</td><td>1300Z-1400Z, May 6</td></tr>
                    <tr><td><span class="expandlink"><a href="contestdetails.php?ref=498">+</a></span> CWops Test (CWT)</td><td>1900Z-2000Z, May 6</td></tr>
                    </table></body></html>
                    """,
                "https://www.contestcalendar.com/contestdetails.php?ref=498" => """
                    <html><body><table>
                    <tr><td class="bgray" colspan="3"><strong>CWops Test (CWT)</strong></td></tr>
                    <tr><td>&nbsp;</td><td>Mode:</td><td>CW</td></tr>
                    <tr><td>&nbsp;</td><td>Bands:</td><td>160, 80, 40, 20, 15, 10m</td></tr>
                    <tr><td>&nbsp;</td><td>Exchange:</td><td>Member: Name + Member No./"CWA"<br/>non-Member: Name + state/province/country</td></tr>
                    <tr><td>&nbsp;</td><td>Find rules at:</td><td><a href="https://cwops.example.test/cwops-tests/">https://cwops.example.test/cwops-tests/</a></td></tr>
                    </table></body></html>
                    """,
                "https://cwops.example.test/cwops-tests/" => """
                    <html><body><p>Official rules page.</p></body></html>
                    """,
                _ => throw new InvalidOperationException($"Unexpected URL: {request.RequestUri}"),
            };
        });
        using var httpClient = new HttpClient(handler, disposeHandler: false);
        var generator = new ContestCatalogGenerator(
            httpClient,
            new FrozenTimeProvider(new DateTimeOffset(2026, 5, 23, 22, 0, 0, TimeSpan.Zero)));

        var catalog = await generator.Generate(
            new ContestCatalogOptions("https://www.contestcalendar.com/contestcal.php", null, "unused.json", false, true, TimeSpan.FromSeconds(5)),
            CancellationToken.None);

        var entry = Assert.Single(catalog.Entries);
        Assert.Equal("CWops Test (CWT)", entry.Name);
        Assert.Equal("https://www.contestcalendar.com/contestdetails.php?ref=498", entry.SourceUrl);
        Assert.Equal("https://cwops.example.test/cwops-tests/", entry.RulesUrl);
        Assert.NotNull(entry.CandidateExtraction);
        Assert.Equal(["160m", "80m", "40m", "20m", "15m", "10m"], entry.CandidateExtraction.Bands);
        Assert.Equal(["cw"], entry.CandidateExtraction.Modes);
        Assert.Equal("Member: Name + Member No./\"CWA\"; non-Member: Name + state/province/country", entry.CandidateExtraction.Exchange);
        Assert.Equal("official-rules", entry.CandidateExtraction.Confidence);
    }

    [Fact]
    public void ContestCalendarPageReaderReadsDistinctContestDetailLinks()
    {
        var contests = ContestCalendarPageReader.Read(
            """
            <html><body><table>
            <tr><td><span class="expandlink"><a href="contestdetails.php?ref=1">+</a></span> First Contest</td><td>0000Z</td></tr>
            <tr><td><span class="expandlink"><a href="contestdetails.php?ref=2">+</a></span> Second &amp; Contest</td><td>0100Z</td></tr>
            <tr><td><span class="expandlink"><a href="contestdetails.php?ref=1">+</a></span> First Contest</td><td>0200Z</td></tr>
            </table></body></html>
            """,
            "https://www.contestcalendar.com/contestcal.php");

        Assert.Collection(
            contests,
            first =>
            {
                Assert.Equal("First Contest", first.Name);
                Assert.Equal("https://www.contestcalendar.com/contestdetails.php?ref=1", first.SourceUrl);
            },
            second =>
            {
                Assert.Equal("Second & Contest", second.Name);
                Assert.Equal("https://www.contestcalendar.com/contestdetails.php?ref=2", second.SourceUrl);
            });
    }

    [Fact]
    public void SeedCatalogFindsMatchesByNameSourceUrlAndAlias()
    {
        SeedCatalogEntry[] seeds =
        [
            new("Name Match", null, [], null, [], [], null),
            new(null, "https://contestcalendar.com/weeklycont.php#source", [], null, [], [], null),
            new(null, null, ["Alias Match"], null, [], [], null),
        ];

        Assert.Same(seeds[0], SeedCatalogEntry.FindMatch(seeds, new RssContest("name match", "unused")));
        Assert.Same(seeds[1], SeedCatalogEntry.FindMatch(seeds, new RssContest("unused", "https://contestcalendar.com/weeklycont.php#source")));
        Assert.Same(seeds[2], SeedCatalogEntry.FindMatch(seeds, new RssContest("alias match", "unused")));
    }

    [Fact]
    public void GeneratedCatalogSerializesReviewableCandidates()
    {
        var catalog = new GeneratedCatalog(
            1,
            new DateTimeOffset(2026, 5, 23, 22, 0, 0, TimeSpan.Zero),
            "WA7BNM Contest Calendar RSS",
            "https://calendar.example.test/calendar.rss",
            [
                new GeneratedCatalogEntry(
                    "Sample Contest",
                    "https://contestcalendar.com/weeklycont.php#sample",
                    "https://sponsor.example.test/rules.html",
                    [],
                    [],
                    null,
                    "metadataOnly",
                    "needsReview",
                    new CandidateExtraction(["20m"], ["cw"], "RST", "https://sponsor.example.test/rules.html", new DateTimeOffset(2026, 5, 23, 22, 0, 0, TimeSpan.Zero), "candidate", "https://sponsor.example.test/rules.html")),
            ]);

        var json = JsonSerializer.Serialize(catalog, ContestCatalogJsonContext.Default.GeneratedCatalog);

        Assert.Contains("\"candidateBands\"", json, StringComparison.Ordinal);
        Assert.Contains("\"candidateModes\"", json, StringComparison.Ordinal);
        Assert.DoesNotContain("\"candidateExtraction\"", json, StringComparison.Ordinal);
        Assert.Contains("\"detailsStatus\": \"metadataOnly\"", json, StringComparison.Ordinal);
    }

    private static string Rss(string title, string link)
    {
        return $$"""
            <?xml version="1.0" encoding="UTF-8"?>
            <rss version="2.0">
              <channel>
                <title>Contest Calendar</title>
                <item>
                  <title>{{WebUtility.HtmlEncode(title)}}</title>
                  <link>{{WebUtility.HtmlEncode(link)}}</link>
                </item>
              </channel>
            </rss>
            """;
    }

    private sealed class StubHttpMessageHandler(Func<HttpRequestMessage, string> respond) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent(respond(request), Encoding.UTF8, "text/plain"),
            });
        }
    }

    private sealed class FrozenTimeProvider(DateTimeOffset utcNow) : TimeProvider
    {
        public override DateTimeOffset GetUtcNow()
        {
            return utcNow;
        }
    }

    private sealed class TempDirectory : IDisposable
    {
        public TempDirectory()
        {
            Path = System.IO.Path.Combine(System.IO.Path.GetTempPath(), System.IO.Path.GetRandomFileName());
            Directory.CreateDirectory(Path);
        }

        public string Path { get; }

        public void Dispose()
        {
            Directory.Delete(Path, recursive: true);
        }
    }
}

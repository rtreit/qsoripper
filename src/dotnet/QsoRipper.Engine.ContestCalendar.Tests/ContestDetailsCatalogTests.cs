#pragma warning disable CA1707 // xUnit method names use underscores.

using System.Globalization;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar.Tests;

public sealed class ContestDetailsCatalogTests
{
    [Fact]
    public void Enrich_MatchesByNameAndAddsReviewedDetails()
    {
        using var directory = TempDirectory.Create();
        var path = Path.Combine(directory.Path, "contest-details.json");
        File.WriteAllText(path, """
            {
              "entries": [
                {
                  "name": "Real Time Contest",
                  "bands": [ "20m", "40m" ],
                  "modes": [ "cw", "ssb" ],
                  "exchange": "RST + serial",
                  "rulesUrl": "https://example.test/rules",
                  "detailsStatus": "full"
                }
              ]
            }
            """);
        var catalog = ContestDetailsCatalog.Load(path);
        var contest = new ContestCalendarEntry
        {
            ContestId = "contest",
            Name = "Real Time Contest",
            StartTimeUtc = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-05-24T16:00:00Z", CultureInfo.InvariantCulture)),
            EndTimeUtc = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-05-24T20:00:00Z", CultureInfo.InvariantCulture)),
            SourceName = "WA7BNM Contest Calendar",
            DetailsStatus = ContestDetailsStatus.MetadataOnly,
        };

        var enriched = catalog.Enrich(contest);

        Assert.Equal([Band._20M, Band._40M], enriched.Bands);
        Assert.Equal([Mode.Cw, Mode.Ssb], enriched.Modes);
        Assert.Equal("RST + serial", enriched.Exchange);
        Assert.Equal("https://example.test/rules", enriched.RulesUrl);
        Assert.Equal(ContestDetailsStatus.Full, enriched.DetailsStatus);
    }

    [Fact]
    public void Provider_EnrichesFetchedContests()
    {
        using var directory = TempDirectory.Create();
        var path = Path.Combine(directory.Path, "contest-details.json");
        File.WriteAllText(path, """
            {
              "entries": [
                {
                  "contestId": "contest",
                  "bands": [ "20m" ],
                  "modes": [ "cw" ],
                  "exchange": "RST + state",
                  "rulesUrl": "https://example.test/rules"
                }
              ]
            }
            """);
        var provider = new CatalogEnrichingContestCalendarProvider(
            new FakeProvider(),
            ContestDetailsCatalog.Load(path));

        var contest = Assert.Single(provider.FetchContests());

        Assert.Equal([Band._20M], contest.Bands);
        Assert.Equal([Mode.Cw], contest.Modes);
        Assert.Equal("RST + state", contest.Exchange);
        Assert.Equal(ContestDetailsStatus.Partial, contest.DetailsStatus);
    }

    [Fact]
    public void Enrich_PreservesMetadataOnlyDetailsStatus()
    {
        using var directory = TempDirectory.Create();
        var path = Path.Combine(directory.Path, "contest-details.json");
        File.WriteAllText(path, """
            {
              "entries": [
                {
                  "contestId": "contest",
                  "detailsStatus": "metadataOnly"
                }
              ]
            }
            """);
        var catalog = ContestDetailsCatalog.Load(path);
        var contest = new ContestCalendarEntry
        {
            ContestId = "contest",
            DetailsStatus = ContestDetailsStatus.Partial,
        };

        var enriched = catalog.Enrich(contest);

        Assert.Equal(ContestDetailsStatus.MetadataOnly, enriched.DetailsStatus);
    }

    private sealed class FakeProvider : IContestCalendarProvider
    {
        public IReadOnlyList<ContestCalendarEntry> FetchContests()
        {
            return
            [
                new ContestCalendarEntry
                {
                    ContestId = "contest",
                    Name = "Real Time Contest",
                    SourceName = "WA7BNM Contest Calendar",
                    DetailsStatus = ContestDetailsStatus.MetadataOnly,
                },
            ];
        }
    }

    private sealed class TempDirectory : IDisposable
    {
        private TempDirectory(string path)
        {
            Path = path;
        }

        public string Path { get; }

        public static TempDirectory Create()
        {
            var path = System.IO.Path.Combine(
                System.IO.Path.GetTempPath(),
                $"qsoripper-contest-catalog-{Guid.NewGuid():N}");
            Directory.CreateDirectory(path);
            return new TempDirectory(path);
        }

        public void Dispose()
        {
            if (Directory.Exists(Path))
            {
                Directory.Delete(Path, recursive: true);
            }
        }
    }
}

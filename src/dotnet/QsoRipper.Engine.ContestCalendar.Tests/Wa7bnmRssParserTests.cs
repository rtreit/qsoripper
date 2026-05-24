#pragma warning disable CA1707 // xUnit method names use underscores.

using QsoRipper.Domain;

namespace QsoRipper.Engine.ContestCalendar.Tests;

public sealed class Wa7bnmRssParserTests
{
    [Fact]
    public void Parse_ReturnsMetadataOnlyEntries()
    {
        const string xml = """
            <?xml version="1.0" encoding="utf-8" ?>
            <rss version="2.0"><channel>
            <lastBuildDate>Sat, 23 May 2026 00:00:00 +0000</lastBuildDate>
            <item><title>Real Time Contest</title><link>https://www.contestcalendar.com/weeklycontdetails.php?ref=x</link><description>1600Z-2000Z, May 24</description></item>
            </channel></rss>
            """;

        var contests = Wa7bnmRssParser.Parse(xml, Wa7bnmContestCalendarConfig.DefaultRssUrl);

        var contest = Assert.Single(contests);
        Assert.Equal("Real Time Contest", contest.Name);
        Assert.Equal(ContestDetailsStatus.MetadataOnly, contest.DetailsStatus);
        Assert.Empty(contest.Bands);
        Assert.Empty(contest.Modes);
        Assert.Equal(1_779_638_400, contest.StartTimeUtc.Seconds);
        Assert.Equal(1_779_652_800, contest.EndTimeUtc.Seconds);
    }

    [Fact]
    public void Parse_RollsEndTimeToNextDay()
    {
        const string xml = """
            <rss version="2.0"><channel>
            <lastBuildDate>Sat, 23 May 2026 00:00:00 +0000</lastBuildDate>
            <item><title>Overnight Contest</title><description>2300Z-0100Z, May 24</description></item>
            </channel></rss>
            """;

        var contest = Assert.Single(Wa7bnmRssParser.Parse(xml, Wa7bnmContestCalendarConfig.DefaultRssUrl));

        Assert.Equal(7_200, contest.EndTimeUtc.Seconds - contest.StartTimeUtc.Seconds);
    }
}

using QsoRipper.Domain;
using QsoRipper.Engine.DotNet;
using QsoRipper.Services;

namespace QsoRipper.Engine.DotNet.Tests;

public sealed class CwKeyingTests
{
    [Fact]
    public void ExpandMacroUsesDefaultRstAndContext()
    {
        var context = new CwSendContext
        {
            WorkedCallsign = "W1AW",
            Exchange = "WA",
        };

        var expanded = ManagedCwController.ExpandMacro("exchange", context, StationProfile());

        Assert.Equal("W1AW 599 WA", expanded);
    }

    [Fact]
    public void ExpandTemplateSupportsLiteralBracesAndCaseInsensitiveTokens()
    {
        var expanded = ManagedCwController.ExpandTemplate("{{{mycall}}}", null, StationProfile());

        Assert.Equal("{K7ABC}", expanded);
    }

    [Fact]
    public void ExpandTemplateRejectsUnknownTokens()
    {
        var error = Assert.Throws<ArgumentException>(() => ManagedCwController.ExpandTemplate("{NOPE}", null, StationProfile()));

        Assert.Contains("NOPE", error.Message, StringComparison.Ordinal);
    }

    private static StationProfile StationProfile()
    {
        return new StationProfile
        {
            StationCallsign = "K7ABC",
        };
    }
}

using QsoRipper.DebugHost.Utilities;
using QsoRipper.Domain;

namespace QsoRipper.DebugHost.Tests;

#pragma warning disable CA1707 // Remove underscores from member names - xUnit allows underscores in test methods
public class ProtoEnumDisplayTests
{
    [Fact]
    public void ForBand_uses_original_proto_name_without_leading_period()
    {
        Assert.Equal("40M", ProtoEnumDisplay.ForBand(Band._40M));
    }

    [Fact]
    public void ForMode_uses_original_proto_name()
    {
        Assert.Equal("FT8", ProtoEnumDisplay.ForMode(Mode.Ft8));
    }
}
#pragma warning restore CA1707

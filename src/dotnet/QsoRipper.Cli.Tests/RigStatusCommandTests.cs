using System.Text.Json;
using QsoRipper.Cli.Commands;
using QsoRipper.Domain;

namespace QsoRipper.Cli.Tests;

#pragma warning disable CA1707 // Remove underscores from member names - xUnit allows underscores in test methods
public sealed class RigStatusCommandTests
{
    [Fact]
    public void BuildConnectedJsonPayload_escapes_special_characters()
    {
        var snapshot = new RigSnapshot
        {
            Status = RigConnectionStatus.Connected,
            FrequencyHz = 14_074_000,
            Band = Band._20M,
            Mode = Mode.Ft8,
            RawMode = "FT8 \"DX\" \\ narrow",
        };

        var json = RigStatusCommand.BuildConnectedJsonPayload(snapshot, "14.074");
        using var document = JsonDocument.Parse(json);

        Assert.Equal("FT8 \"DX\" \\ narrow", document.RootElement.GetProperty("rawMode").GetString());
    }

    [Fact]
    public void BuildConnectedJsonPayload_preserves_sub_kilohertz_frequency_digits()
    {
        var snapshot = new RigSnapshot
        {
            Status = RigConnectionStatus.Connected,
            FrequencyHz = 14_074_123,
        };

        var json = RigStatusCommand.BuildConnectedJsonPayload(snapshot, "14.074123");
        using var document = JsonDocument.Parse(json);

        Assert.Equal("14.074123", document.RootElement.GetProperty("frequencyMhz").GetString());
        Assert.Equal("14.074123 MHz", document.RootElement.GetProperty("frequencyDisplay").GetString());
    }
}
#pragma warning restore CA1707

using QsoRipper.Gui.ViewModels;
using QsoRipper.Services;

namespace QsoRipper.Gui.Tests;

public sealed class WsjtxIngestStepViewModelTests
{
    [Fact]
    public void ConfigureFromSettingsDefaultsUdpToEnabledWhenUnset()
    {
        var viewModel = new WsjtxIngestStepViewModel();

        viewModel.ConfigureFromSettings(new WsjtxIngestSettings { Enabled = true });

        Assert.True(viewModel.UdpEnabled);
    }

    [Fact]
    public void ConfigureFromSettingsDoesNotRequireSaveWhenUserHasNotChangedSettings()
    {
        var viewModel = new WsjtxIngestStepViewModel();

        viewModel.ConfigureFromSettings(
            new WsjtxIngestSettings
            {
                Enabled = true,
                UdpBind = "127.0.0.1:2237",
                PollIntervalMs = 1000
            });

        Assert.False(viewModel.ShouldSave);
    }

    [Fact]
    public void ValidateLocallyAllowsZeroPollIntervalForEngineDefault()
    {
        var viewModel = new WsjtxIngestStepViewModel
        {
            Enabled = true,
            PollIntervalMs = 0
        };

        Assert.True(viewModel.ValidateLocally());
    }

    [Fact]
    public void ValidateLocallyAllowsSmallPositivePollIntervalAcceptedByEngine()
    {
        var viewModel = new WsjtxIngestStepViewModel
        {
            Enabled = true,
            PollIntervalMs = 50
        };

        Assert.True(viewModel.ValidateLocally());
    }

    [Fact]
    public void ValidateLocallyRejectsInvalidUdpBindPort()
    {
        var viewModel = new WsjtxIngestStepViewModel
        {
            Enabled = true,
            UdpBind = "127.0.0.1:notaport"
        };

        Assert.False(viewModel.ValidateLocally());
        Assert.Equal(
            "UDP bind must be host:port with a port between 1 and 65535.",
            viewModel.ValidationSummary);
    }
}

using QsoRipper.Domain;
using QsoRipper.Engine.DotNet;
using QsoRipper.Services;

namespace QsoRipper.Engine.DotNet.Tests;

public sealed class CwKeyingTests
{
    [Fact]
    public void ConfigDefaultsToSafeNullBackend()
    {
        var config = ManagedCwKeyerConfig.FromValues(null, null, null, null, null, null);

        Assert.Equal(ManagedCwBackendKind.Null, config.Backend);
        Assert.False(config.TransmitEnabled);
        Assert.Equal(25u, config.DefaultSpeedWpm);
        Assert.Equal(120_000u, config.MaxTxMs);
    }

    [Theory]
    [InlineData("4")]
    [InlineData("100")]
    public void ConfigRejectsUnsafeSpeed(string speed)
    {
        Assert.Throws<ArgumentOutOfRangeException>(
            () => ManagedCwKeyerConfig.FromValues(null, null, null, speed, null, null));
    }

    [Fact]
    public void HardwareSendRequiresExplicitTransmitEnable()
    {
        var opens = 0;
        using var keyer = new FakeWinkeyerPort();
        using var controller = new ManagedCwController(
            WinkeyerConfig(transmitEnabled: false),
            (_, _) =>
            {
                opens++;
                return keyer;
            });

        var error = Assert.Throws<InvalidOperationException>(() => controller.SendText("CQ", null));

        Assert.Contains("disabled", error.Message, StringComparison.OrdinalIgnoreCase);
        Assert.Equal(0, opens);
    }

    [Fact]
    public void ControllerReusesSessionAndClosesHostModeOnDispose()
    {
        using var keyer = new FakeWinkeyerPort();
        var opens = 0;
        var controller = new ManagedCwController(
            WinkeyerConfig(),
            (_, _) =>
            {
                opens++;
                return keyer;
            });

        var initialStatus = controller.Status();
        controller.SendText("cq test", 31);
        controller.SetSpeed(27);
        controller.Abort();
        var finalStatus = controller.Status();
        controller.Dispose();

        Assert.Equal(1, opens);
        Assert.Equal(1, keyer.InitializeCount);
        Assert.Equal([31u, 27u], keyer.Speeds);
        Assert.Equal(["cq test"], keyer.Texts);
        Assert.Equal(1, keyer.ClearCount);
        Assert.Equal(1, keyer.CloseCount);
        Assert.True(keyer.Disposed);
        Assert.True(initialStatus.Available);
        Assert.True(initialStatus.TransmitEnabled);
        Assert.Equal(42u, initialStatus.FirmwareRevision);
        Assert.Equal(27u, finalStatus.SpeedWpm);
    }

    [Fact]
    public void WatchdogClearsBufferAndReportsSafetyError()
    {
        using var keyer = new FakeWinkeyerPort();
        using var watchdog = new FakeWatchdog();
        using var controller = new ManagedCwController(
            WinkeyerConfig(),
            (_, _) => keyer,
            callback =>
            {
                watchdog.Callback = callback;
                return watchdog;
            });

        controller.SendText("TEST", null);
        watchdog.Fire();
        var status = controller.Status();

        Assert.Equal(TimeSpan.FromMilliseconds(1_000), watchdog.DueTime);
        Assert.Equal(1, keyer.ClearCount);
        Assert.Contains("safety ceiling", status.ErrorMessage, StringComparison.OrdinalIgnoreCase);
    }

    [Fact]
    public void WatchdogDoesNotReportCompletedTransmissionAsAnError()
    {
        using var keyer = new FakeWinkeyerPort { Busy = false };
        using var watchdog = new FakeWatchdog();
        using var controller = new ManagedCwController(
            WinkeyerConfig(),
            (_, _) => keyer,
            callback =>
            {
                watchdog.Callback = callback;
                return watchdog;
            });

        controller.SendText("TEST", null);
        watchdog.Fire();
        var status = controller.Status();

        Assert.Equal(0, keyer.ClearCount);
        Assert.False(status.HasErrorMessage);
    }

    [Fact]
    public void AbortCancelsWatchdogBeforeClearingBuffer()
    {
        using var keyer = new FakeWinkeyerPort();
        using var watchdog = new FakeWatchdog();
        using var controller = new ManagedCwController(
            WinkeyerConfig(),
            (_, _) => keyer,
            callback =>
            {
                watchdog.Callback = callback;
                return watchdog;
            });

        controller.SendText("TEST", null);
        controller.Abort();

        Assert.True(watchdog.Cancelled);
        Assert.Equal(1, keyer.ClearCount);
    }

    [Fact]
    public void NullBackendRetainsRequestedSpeedWithoutOpeningHardware()
    {
        using var controller = new ManagedCwController(
            new ManagedCwKeyerConfig(ManagedCwBackendKind.Null, null, 1_200, 25, false, 120_000));

        controller.SendText("TEST", 38);

        var status = controller.Status();
        Assert.True(status.Available);
        Assert.Equal(38u, status.SpeedWpm);
        Assert.False(status.TransmitEnabled);
    }

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

    private static ManagedCwKeyerConfig WinkeyerConfig(bool transmitEnabled = true)
    {
        return new ManagedCwKeyerConfig(
            ManagedCwBackendKind.Winkeyer,
            "COM_TEST",
            1_200,
            25,
            transmitEnabled,
            1_000);
    }

    private sealed class FakeWinkeyerPort : IManagedWinkeyerPort
    {
        public int InitializeCount { get; private set; }
        public List<uint> Speeds { get; } = [];
        public List<string> Texts { get; } = [];
        public int ClearCount { get; private set; }
        public int CloseCount { get; private set; }
        public bool Disposed { get; private set; }
        public bool Busy { get; init; } = true;

        public byte Initialize()
        {
            InitializeCount++;
            return 42;
        }

        public void SetSpeed(uint speedWpm) => Speeds.Add(speedWpm);

        public void SendText(string text) => Texts.Add(text);

        public void ClearBuffer() => ClearCount++;

        public bool IsBusy() => Busy;

        public void CloseHostMode() => CloseCount++;

        public void Dispose() => Disposed = true;
    }

    private sealed class FakeWatchdog : IManagedCwWatchdog
    {
        public Action? Callback { get; set; }
        public TimeSpan? DueTime { get; private set; }
        public bool Cancelled { get; private set; }

        public void Arm(TimeSpan dueTime)
        {
            DueTime = dueTime;
            Cancelled = false;
        }

        public void Cancel() => Cancelled = true;

        public void Fire() => Assert.IsType<Action>(Callback).Invoke();

        public void Dispose()
        {
        }
    }
}

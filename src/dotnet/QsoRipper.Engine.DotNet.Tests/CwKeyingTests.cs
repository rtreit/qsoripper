using QsoRipper.Domain;
using QsoRipper.Engine.DotNet;
using QsoRipper.Services;

namespace QsoRipper.Engine.DotNet.Tests;

public sealed class CwKeyingTests
{
    [Fact]
    public void TomlSettingsSupplyCwConfigurationDefaults()
    {
        var persisted = new ManagedCwPersistedSettings
        {
            Backend = "winkeyer",
            WinkeyerPort = "COM9",
            WinkeyerBaud = 2_400,
            SpeedWpm = 31,
            TransmitEnabled = true,
            MaxTxMs = 45_000,
        };

        var config = ManagedCwKeyerConfig.FromSources(persisted, static _ => null);

        Assert.Equal(ManagedCwBackendKind.Winkeyer, config.Backend);
        Assert.Equal("COM9", config.WinkeyerPort);
        Assert.Equal(2_400, config.WinkeyerBaud);
        Assert.Equal(31u, config.DefaultSpeedWpm);
        Assert.True(config.TransmitEnabled);
        Assert.Equal(45_000u, config.MaxTxMs);
    }

    [Fact]
    public void EnvironmentOverridesIndividualTomlSettings()
    {
        var persisted = new ManagedCwPersistedSettings
        {
            Backend = "winkeyer",
            WinkeyerPort = "COM9",
            WinkeyerBaud = 1_200,
            SpeedWpm = 20,
            TransmitEnabled = false,
            MaxTxMs = 30_000,
        };
        var environment = new Dictionary<string, string>(StringComparer.Ordinal)
        {
            [ManagedCwKeyerConfig.SpeedWpmEnvironmentVariable] = "35",
            [ManagedCwKeyerConfig.TransmitEnabledEnvironmentVariable] = "true",
        };

        var config = ManagedCwKeyerConfig.FromSources(
            persisted,
            name => environment.GetValueOrDefault(name));

        Assert.Equal("COM9", config.WinkeyerPort);
        Assert.Equal(35u, config.DefaultSpeedWpm);
        Assert.True(config.TransmitEnabled);
        Assert.Equal(30_000u, config.MaxTxMs);
    }

    [Fact]
    public void CathubConfigurationUsesTypedBrokerWithoutOpeningSerialPort()
    {
        var config = ManagedCwKeyerConfig.FromValues(
            "cathub",
            null,
            null,
            "http://127.0.0.1:50071",
            "dotnet-test",
            "24",
            "true",
            "30000");
        using var keyer = new FakeWinkeyerPort
        {
            BrokerStatus = new ManagedBrokerHardwareStatus(true, 19, "watchdog recovered"),
        };
        var serialOpens = 0;
        var brokerOpens = 0;
        using var controller = new ManagedCwController(
            config,
            (_, _) =>
            {
                serialOpens++;
                return keyer;
            },
            cathubFactory: (endpoint, clientName) =>
            {
                Assert.Equal("http://127.0.0.1:50071", endpoint);
                Assert.Equal("dotnet-test", clientName);
                brokerOpens++;
                return keyer;
            });

        controller.SendText("TEST", null);
        var status = controller.Status();

        Assert.Equal(0, serialOpens);
        Assert.Equal(1, brokerOpens);
        Assert.Equal(CwKeyerBackend.Cathub, status.Backend);
        Assert.Equal("http://127.0.0.1:50071", status.BrokerEndpoint);
        Assert.Equal(19u, status.PotWpm);
        Assert.True(status.Busy);
        Assert.Equal("watchdog recovered", status.LastSafetyAction);
    }

    [Fact]
    public void ConfigDefaultsToSafeNullBackend()
    {
        var config = ManagedCwKeyerConfig.FromValues(null, null, null, null, null, null, null, null);

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
            () => ManagedCwKeyerConfig.FromValues(null, null, null, null, null, speed, null, null));
    }

    [Fact]
    public void ConfigRejectsNonLoopbackCathubEndpoint()
    {
        Assert.Throws<InvalidOperationException>(() => ManagedCwKeyerConfig.FromValues(
            "cathub", null, null, "http://192.168.1.10:50071", null, null, null, null));
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
            new ManagedCwKeyerConfig(
                ManagedCwBackendKind.Null,
                null,
                1_200,
                ManagedCwKeyerConfig.DefaultCathubEndpoint,
                ManagedCwKeyerConfig.DefaultCathubClientName,
                25,
                false,
                120_000));

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
            ManagedCwKeyerConfig.DefaultCathubEndpoint,
            ManagedCwKeyerConfig.DefaultCathubClientName,
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
        public ManagedBrokerHardwareStatus? BrokerStatus { get; init; }

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

        public ManagedBrokerHardwareStatus? GetBrokerStatus() => BrokerStatus;

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

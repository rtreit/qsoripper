using QsoRipper.Cli.Commands;
using QsoRipper.EngineSelection;
using QsoRipper.Services;

namespace QsoRipper.Cli.Tests;

#pragma warning disable CA1707 // Remove underscores from member names - xUnit allows underscores in test methods
[Collection("ConsoleCapture")]
public class SetupWizardTests
{
    [Fact]
    public void PromptField_returns_default_on_empty_input()
    {
        var originalIn = Console.In;
        var originalOut = Console.Out;
        try
        {
            Console.SetIn(new StringReader(Environment.NewLine));
            Console.SetOut(TextWriter.Null);
            var result = SetupCommand.PromptField("Test", "default_value");
            Assert.Equal("default_value", result);
        }
        finally
        {
            Console.SetIn(originalIn);
            Console.SetOut(originalOut);
        }
    }

    [Fact]
    public void PromptField_returns_user_input_when_provided()
    {
        var originalIn = Console.In;
        var originalOut = Console.Out;
        try
        {
            Console.SetIn(new StringReader("user_input" + Environment.NewLine));
            Console.SetOut(TextWriter.Null);
            var result = SetupCommand.PromptField("Test", "default_value");
            Assert.Equal("user_input", result);
        }
        finally
        {
            Console.SetIn(originalIn);
            Console.SetOut(originalOut);
        }
    }

    [Fact]
    public void PromptField_trims_whitespace()
    {
        var originalIn = Console.In;
        var originalOut = Console.Out;
        try
        {
            Console.SetIn(new StringReader("  trimmed  " + Environment.NewLine));
            Console.SetOut(TextWriter.Null);
            var result = SetupCommand.PromptField("Test", "default");
            Assert.Equal("trimmed", result);
        }
        finally
        {
            Console.SetIn(originalIn);
            Console.SetOut(originalOut);
        }
    }

    [Theory]
    [InlineData("y", true, true)]
    [InlineData("Y", true, true)]
    [InlineData("yes", true, true)]
    [InlineData("YES", true, true)]
    [InlineData("n", true, false)]
    [InlineData("N", true, false)]
    [InlineData("no", true, false)]
    [InlineData("anything", true, false)]
    [InlineData("", true, true)]    // default yes
    [InlineData("", false, false)]  // default no
    [InlineData("y", false, true)]
    [InlineData("n", false, false)]
    public void PromptYesNo_handles_inputs(string input, bool defaultYes, bool expected)
    {
        var originalIn = Console.In;
        var originalOut = Console.Out;
        try
        {
            Console.SetIn(new StringReader(input + Environment.NewLine));
            Console.SetOut(TextWriter.Null);
            var result = SetupCommand.PromptYesNo("Question?", defaultYes);
            Assert.Equal(expected, result);
        }
        finally
        {
            Console.SetIn(originalIn);
            Console.SetOut(originalOut);
        }
    }

    [Fact]
    public void CliArguments_defaults_setup_flags_to_false()
    {
        var args = new CliArguments("setup", "http://localhost:50051", EngineCatalog.DefaultProfile);

        Assert.False(args.SetupStatus);
        Assert.False(args.SetupFromEnv);
    }

    [Fact]
    public void CliArguments_can_set_setup_status()
    {
        var args = new CliArguments("setup", "http://localhost:50051", EngineCatalog.DefaultProfile, SetupStatus: true);

        Assert.True(args.SetupStatus);
        Assert.False(args.SetupFromEnv);
    }

    [Fact]
    public void CliArguments_can_set_setup_from_env()
    {
        var args = new CliArguments("setup", "http://localhost:50051", EngineCatalog.DefaultProfile, SetupFromEnv: true);

        Assert.False(args.SetupStatus);
        Assert.True(args.SetupFromEnv);
    }

    [Fact]
    public void WsjtxIngestFromEnvironmentReadsCanonicalRuntimeVariables()
    {
        var snapshot = new Dictionary<string, string?>
        {
            ["QSORIPPER_WSJTX_INGEST_ENABLED"] = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ENABLED"),
            ["QSORIPPER_WSJTX_INGEST_UDP_ENABLED"] = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_UDP_ENABLED"),
            ["QSORIPPER_WSJTX_INGEST_UDP_BIND"] = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_UDP_BIND"),
            ["QSORIPPER_WSJTX_INGEST_ADIF_TAIL_ENABLED"] = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ADIF_TAIL_ENABLED"),
            ["QSORIPPER_WSJTX_INGEST_ADIF_TAIL_PATH"] = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ADIF_TAIL_PATH"),
            ["QSORIPPER_WSJTX_INGEST_POLL_INTERVAL_MS"] = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_POLL_INTERVAL_MS"),
            ["QSORIPPER_WSJTX_INGEST_SYNC_TO_QRZ"] = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_SYNC_TO_QRZ"),
        };

        try
        {
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ENABLED", "true");
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_UDP_ENABLED", "true");
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_UDP_BIND", "0.0.0.0:2237");
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ADIF_TAIL_ENABLED", "true");
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ADIF_TAIL_PATH", @"C:\logs\wsjtx_log.adi");
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_POLL_INTERVAL_MS", "0");
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_SYNC_TO_QRZ", "true");

            var success = SetupCommand.TryBuildWsjtxIngestSettingsFromEnvironment(
                existing: null,
                out var settings,
                out var error);

            Assert.True(success, error);
            Assert.NotNull(settings);
            Assert.True(settings.Enabled);
            Assert.True(settings.UdpEnabled);
            Assert.Equal("0.0.0.0:2237", settings.UdpBind);
            Assert.True(settings.AdifTailEnabled);
            Assert.Equal(@"C:\logs\wsjtx_log.adi", settings.AdifTailPath);
            Assert.Equal(0u, settings.PollIntervalMs);
            Assert.True(settings.SyncToQrz);
        }
        finally
        {
            foreach (var (key, value) in snapshot)
            {
                Environment.SetEnvironmentVariable(key, value);
            }
        }
    }

    [Fact]
    public void WsjtxIngestFromEnvironmentRejectsInvalidUdpBindPort()
    {
        var originalEnabled = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ENABLED");
        var originalBind = Environment.GetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_UDP_BIND");

        try
        {
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ENABLED", "true");
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_UDP_BIND", "127.0.0.1:notaport");

            var success = SetupCommand.TryBuildWsjtxIngestSettingsFromEnvironment(
                existing: null,
                out var settings,
                out var error);

            Assert.False(success);
            Assert.Null(settings);
            Assert.Equal(
                "Error: WSJT-X UDP bind must be host:port with a port between 1 and 65535.",
                error);
        }
        finally
        {
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_ENABLED", originalEnabled);
            Environment.SetEnvironmentVariable("QSORIPPER_WSJTX_INGEST_UDP_BIND", originalBind);
        }
    }
}
#pragma warning restore CA1707

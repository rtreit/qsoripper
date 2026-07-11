using System.Globalization;
using System.Text.Json;
using System.Text.Json.Serialization;
using Grpc.Net.Client;
using QsoRipper.Services;

namespace QsoRipper.Cli.Commands;

internal static class CwCommand
{
    public static async Task<int> RunAsync(GrpcChannel channel, string[] args, bool jsonOutput)
    {
        if (args.Length == 0)
        {
            Console.Error.WriteLine(CliHelpText.GetCommandHelp("cw"));
            return 1;
        }

        var client = new CwService.CwServiceClient(channel);
        var subcommand = args[0].ToLowerInvariant();
        var remaining = args.Skip(1).ToArray();

        try
        {
            return subcommand switch
            {
                "status" => await RunStatusAsync(client, jsonOutput),
                "list" => await RunListAsync(client, jsonOutput),
                "send" => await RunSendAsync(client, remaining, jsonOutput),
                "macro" => await RunMacroAsync(client, remaining, jsonOutput),
                "speed" => await RunSpeedAsync(client, remaining, jsonOutput),
                "abort" => await RunAbortAsync(client, jsonOutput),
                _ => InvalidUsage($"Unknown cw subcommand '{args[0]}'."),
            };
        }
        catch (Exception ex) when (ex is ArgumentException or FormatException or OverflowException)
        {
            return InvalidUsage(ex.Message);
        }
    }

    private static async Task<int> RunStatusAsync(CwService.CwServiceClient client, bool jsonOutput)
    {
        var response = await client.GetCwKeyerStatusAsync(new GetCwKeyerStatusRequest());
        var status = response.Status;
        if (jsonOutput)
        {
            WriteJson(ToStatusJson(status));
            return 0;
        }

        Console.WriteLine($"CW keyer: {BackendName(status.Backend)}");
        Console.WriteLine($"  Available: {status.Available}");
        Console.WriteLine($"  Speed: {status.SpeedWpm} WPM");
        Console.WriteLine($"  Hardware transmit enabled: {status.TransmitEnabled}");
        Console.WriteLine($"  Transmit safety ceiling: {status.MaxTxMs} ms");
        if (status.HasPortName)
        {
            Console.WriteLine($"  Port: {status.PortName}");
        }

        if (status.HasFirmwareRevision)
        {
            Console.WriteLine($"  Firmware revision: {status.FirmwareRevision}");
        }

        if (status.HasErrorMessage)
        {
            Console.WriteLine($"  Error: {status.ErrorMessage}");
        }

        return 0;
    }

    private static async Task<int> RunListAsync(CwService.CwServiceClient client, bool jsonOutput)
    {
        var response = await client.ListCwMacrosAsync(new ListCwMacrosRequest());
        if (jsonOutput)
        {
            WriteJson(response.Macros.Select(static macro => new CwMacroJson(
                macro.Name,
                macro.Label,
                macro.Template)).ToArray());
            return 0;
        }

        foreach (var macro in response.Macros)
        {
            Console.WriteLine($"{macro.Name,-10} {macro.Template}");
        }

        return 0;
    }

    private static async Task<int> RunSendAsync(CwService.CwServiceClient client, string[] args, bool jsonOutput)
    {
        if (args.Length == 0)
        {
            return InvalidUsage("cw send requires text.");
        }

        var text = args[0];
        var context = ParseContext(args.Skip(1).ToArray());
        var response = await client.SendCwTextAsync(new SendCwTextRequest
        {
            Text = text,
            Context = context,
        });

        return WriteSendResult(response.State, response.ExpandedText, jsonOutput);
    }

    private static async Task<int> RunMacroAsync(CwService.CwServiceClient client, string[] args, bool jsonOutput)
    {
        if (args.Length == 0)
        {
            return InvalidUsage("cw macro requires a macro name.");
        }

        var name = args[0];
        var context = ParseContext(args.Skip(1).ToArray());
        var response = await client.SendCwMacroAsync(new SendCwMacroRequest
        {
            Name = name,
            Context = context,
        });

        return WriteSendResult(response.State, response.ExpandedText, jsonOutput);
    }

    private static async Task<int> RunSpeedAsync(CwService.CwServiceClient client, string[] args, bool jsonOutput)
    {
        if (args.Length != 1 || !uint.TryParse(args[0], NumberStyles.None, CultureInfo.InvariantCulture, out var speedWpm))
        {
            return InvalidUsage("cw speed requires a numeric WPM value.");
        }
        ValidateSpeed(speedWpm);

        var response = await client.SetCwSpeedAsync(new SetCwSpeedRequest
        {
            SpeedWpm = speedWpm,
        });

        if (jsonOutput)
        {
            WriteJson(ToStatusJson(response.Status));
            return 0;
        }

        Console.WriteLine($"CW speed accepted: {response.Status.SpeedWpm} WPM");
        return 0;
    }

    private static async Task<int> RunAbortAsync(CwService.CwServiceClient client, bool jsonOutput)
    {
        var response = await client.AbortCwAsync(new AbortCwRequest());
        return WriteSendResult(response.State, string.Empty, jsonOutput);
    }

    private static CwSendContext ParseContext(string[] args)
    {
        var context = new CwSendContext();
        for (var index = 0; index < args.Length; index++)
        {
            var option = args[index];
            if (index == args.Length - 1)
            {
                throw new ArgumentException($"Missing value for {option}.");
            }

            var value = args[++index];
            switch (option)
            {
                case "--his-call":
                    context.WorkedCallsign = value;
                    break;
                case "--rst":
                    context.Rst = value;
                    break;
                case "--exchange":
                case "--exch":
                    context.Exchange = value;
                    break;
                case "--nr":
                    context.Serial = uint.Parse(value, CultureInfo.InvariantCulture);
                    break;
                case "--speed":
                    context.SpeedWpm = uint.Parse(value, CultureInfo.InvariantCulture);
                    ValidateSpeed(context.SpeedWpm);
                    break;
                default:
                    throw new ArgumentException($"Unknown CW option '{option}'.");
            }
        }

        return context;
    }

    private static int WriteSendResult(CwSendState state, string expandedText, bool jsonOutput)
    {
        if (jsonOutput)
        {
            WriteJson(new CwSendResultJson(state.ToString(), expandedText));
            return 0;
        }

        Console.WriteLine($"CW state: {state}");
        if (!string.IsNullOrEmpty(expandedText))
        {
            Console.WriteLine($"Expanded: {expandedText}");
        }

        return 0;
    }

    private static int InvalidUsage(string message)
    {
        Console.Error.WriteLine(message);
        Console.Error.WriteLine(CliHelpText.GetCommandHelp("cw"));
        return 1;
    }

    private static void WriteJson(CwStatusJson value)
    {
        Console.WriteLine(JsonSerializer.Serialize(value, CwCommandJsonContext.Default.CwStatusJson));
    }

    private static void WriteJson(CwMacroJson[] value)
    {
        Console.WriteLine(JsonSerializer.Serialize(value, CwCommandJsonContext.Default.CwMacroJsonArray));
    }

    private static void WriteJson(CwSendResultJson value)
    {
        Console.WriteLine(JsonSerializer.Serialize(value, CwCommandJsonContext.Default.CwSendResultJson));
    }

    private static CwStatusJson ToStatusJson(CwKeyerStatus status)
    {
        return new CwStatusJson(
            BackendName(status.Backend),
            status.Available,
            status.SpeedWpm,
            status.TransmitEnabled,
            status.MaxTxMs,
            status.HasFirmwareRevision ? status.FirmwareRevision : null,
            status.HasPortName ? status.PortName : string.Empty,
            status.HasErrorMessage ? status.ErrorMessage : string.Empty);
    }

    private static void ValidateSpeed(uint speedWpm)
    {
        if (speedWpm is < 5 or > 99)
        {
            throw new ArgumentOutOfRangeException(
                nameof(speedWpm),
                speedWpm,
                "CW speed must be between 5 and 99 WPM.");
        }
    }

    private static string BackendName(CwKeyerBackend backend)
    {
        return backend switch
        {
            CwKeyerBackend.Null => "null",
            CwKeyerBackend.Winkeyer => "winkeyer",
            CwKeyerBackend.Cwdaemon => "cwdaemon",
            _ => "unspecified",
        };
    }
}

internal sealed record CwStatusJson(
    string Backend,
    bool Available,
    uint SpeedWpm,
    bool TransmitEnabled,
    ulong MaxTxMs,
    uint? FirmwareRevision,
    string PortName,
    string ErrorMessage);

internal sealed record CwMacroJson(string Name, string Label, string Template);

internal sealed record CwSendResultJson(string State, string ExpandedText);

[JsonSourceGenerationOptions(WriteIndented = true, PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
[JsonSerializable(typeof(CwStatusJson))]
[JsonSerializable(typeof(CwMacroJson[]))]
[JsonSerializable(typeof(CwSendResultJson))]
internal sealed partial class CwCommandJsonContext : JsonSerializerContext;

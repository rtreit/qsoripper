using System.Globalization;
using System.IO.Ports;
using System.Text;
using Grpc.Core;
using QsoRipper.Domain;
using QsoRipper.Services;

namespace QsoRipper.Engine.DotNet;

internal enum ManagedCwBackendKind
{
    Null,
    Winkeyer,
    Cwdaemon,
}

internal sealed record ManagedCwKeyerConfig(
    ManagedCwBackendKind Backend,
    string? WinkeyerPort,
    int WinkeyerBaud,
    uint DefaultSpeedWpm)
{
    public const string BackendEnvironmentVariable = "QSORIPPER_CW_KEYER_BACKEND";
    public const string WinkeyerPortEnvironmentVariable = "QSORIPPER_CW_WINKEYER_PORT";
    public const string WinkeyerBaudEnvironmentVariable = "QSORIPPER_CW_WINKEYER_BAUD";
    public const string SpeedWpmEnvironmentVariable = "QSORIPPER_CW_SPEED_WPM";
    public const int DefaultWinkeyerBaud = 1200;
    public const uint DefaultCwSpeedWpm = 25;

    public static ManagedCwKeyerConfig FromEnvironment()
    {
        var backend = Environment.GetEnvironmentVariable(BackendEnvironmentVariable);
        var port = Environment.GetEnvironmentVariable(WinkeyerPortEnvironmentVariable);
        var baud = Environment.GetEnvironmentVariable(WinkeyerBaudEnvironmentVariable);
        var speed = Environment.GetEnvironmentVariable(SpeedWpmEnvironmentVariable);

        return FromValues(backend, port, baud, speed);
    }

    internal static ManagedCwKeyerConfig FromValues(string? backend, string? port, string? baud, string? speed)
    {
        var backendKind = (backend ?? "null").Trim().ToUpperInvariant() switch
        {
            "" or "NULL" => ManagedCwBackendKind.Null,
            "WINKEYER" => ManagedCwBackendKind.Winkeyer,
            "CWDAEMON" => ManagedCwBackendKind.Cwdaemon,
            var value => throw new InvalidOperationException($"Unsupported CW keyer backend '{value}'."),
        };

        return new ManagedCwKeyerConfig(
            backendKind,
            string.IsNullOrWhiteSpace(port) ? null : port.Trim(),
            ParseIntOrDefault(baud, DefaultWinkeyerBaud),
            ClampSpeed(ParseUIntOrDefault(speed, DefaultCwSpeedWpm)));
    }

    private static int ParseIntOrDefault(string? value, int defaultValue)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            return defaultValue;
        }

        return int.Parse(value, CultureInfo.InvariantCulture);
    }

    private static uint ParseUIntOrDefault(string? value, uint defaultValue)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            return defaultValue;
        }

        return uint.Parse(value, CultureInfo.InvariantCulture);
    }

    public static uint ClampSpeed(uint speedWpm)
    {
        return Math.Min(99, Math.Max(5, speedWpm));
    }
}

internal sealed class ManagedCwController(ManagedCwKeyerConfig config)
{
    public static IReadOnlyList<CwMacro> BuiltInMacros { get; } =
    [
        new CwMacro { Name = "cq", Label = "CQ", Template = "CQ TEST {MYCALL} {MYCALL}" },
        new CwMacro { Name = "exchange", Label = "Exchange", Template = "{HISCALL} {RST} {EXCH}" },
        new CwMacro { Name = "tu", Label = "TU", Template = "TU {MYCALL}" },
        new CwMacro { Name = "repeat", Label = "Repeat", Template = "{HISCALL} {RST} {EXCH}" },
    ];

    public static string ExpandMacro(string name, CwSendContext? context, StationProfile? stationProfile)
    {
        var macro = BuiltInMacros.FirstOrDefault(candidate => string.Equals(candidate.Name, name, StringComparison.OrdinalIgnoreCase))
            ?? throw new KeyNotFoundException($"Unknown CW macro '{name}'.");
        return ExpandTemplate(macro.Template, context, stationProfile);
    }

    public static string ExpandTemplate(string template, CwSendContext? context, StationProfile? stationProfile)
    {
        var output = new StringBuilder(template.Length);

        for (var index = 0; index < template.Length; index++)
        {
            var ch = template[index];
            if (ch == '{')
            {
                if (index + 1 < template.Length && template[index + 1] == '{')
                {
                    output.Append('{');
                    index++;
                    continue;
                }

                var closeIndex = template.IndexOf('}', index + 1);
                if (closeIndex < 0)
                {
                    throw new ArgumentException("Unmatched '{' in CW macro template.");
                }

                var token = template[(index + 1)..closeIndex];
                if (token.Contains('{', StringComparison.Ordinal))
                {
                    throw new ArgumentException("Unmatched '{' in CW macro template.");
                }

                output.Append(ResolveToken(token, context, stationProfile));
                index = closeIndex;
                continue;
            }

            if (ch == '}')
            {
                if (index + 1 < template.Length && template[index + 1] == '}')
                {
                    output.Append('}');
                    index++;
                    continue;
                }

                throw new ArgumentException("Unmatched '}' in CW macro template.");
            }

            output.Append(ch);
        }

        return output.ToString();
    }

    public void SendText(string text, uint? speedWpm)
    {
        switch (config.Backend)
        {
            case ManagedCwBackendKind.Null:
                return;
            case ManagedCwBackendKind.Winkeyer:
                SendWinkeyerText(text, speedWpm ?? config.DefaultSpeedWpm);
                return;
            case ManagedCwBackendKind.Cwdaemon:
                throw new InvalidOperationException("cwdaemon backend is reserved but not implemented.");
            default:
                throw new InvalidOperationException($"Unsupported CW keyer backend '{config.Backend}'.");
        }
    }

    public void Abort()
    {
        switch (config.Backend)
        {
            case ManagedCwBackendKind.Null:
                return;
            case ManagedCwBackendKind.Winkeyer:
                using (var keyer = OpenWinkeyer())
                {
                    keyer.Initialize();
                    keyer.ClearBuffer();
                }

                return;
            case ManagedCwBackendKind.Cwdaemon:
                throw new InvalidOperationException("cwdaemon backend is reserved but not implemented.");
            default:
                throw new InvalidOperationException($"Unsupported CW keyer backend '{config.Backend}'.");
        }
    }

    public void SetSpeed(uint speedWpm)
    {
        var clamped = ManagedCwKeyerConfig.ClampSpeed(speedWpm);
        switch (config.Backend)
        {
            case ManagedCwBackendKind.Null:
                return;
            case ManagedCwBackendKind.Winkeyer:
                using (var keyer = OpenWinkeyer())
                {
                    keyer.Initialize();
                    keyer.SetSpeed(clamped);
                }

                return;
            case ManagedCwBackendKind.Cwdaemon:
                throw new InvalidOperationException("cwdaemon backend is reserved but not implemented.");
            default:
                throw new InvalidOperationException($"Unsupported CW keyer backend '{config.Backend}'.");
        }
    }

    public CwKeyerStatus Status()
    {
        return new CwKeyerStatus
        {
            Backend = config.Backend switch
            {
                ManagedCwBackendKind.Null => CwKeyerBackend.Null,
                ManagedCwBackendKind.Winkeyer => CwKeyerBackend.Winkeyer,
                ManagedCwBackendKind.Cwdaemon => CwKeyerBackend.Cwdaemon,
                _ => CwKeyerBackend.Unspecified,
            },
            Available = config.Backend == ManagedCwBackendKind.Null || (config.Backend == ManagedCwBackendKind.Winkeyer && !string.IsNullOrWhiteSpace(config.WinkeyerPort)),
            SpeedWpm = config.DefaultSpeedWpm,
            PortName = config.WinkeyerPort ?? string.Empty,
            ErrorMessage = LastStatusError(),
        };
    }

    private string LastStatusError()
    {
        return config.Backend switch
        {
            ManagedCwBackendKind.Winkeyer when string.IsNullOrWhiteSpace(config.WinkeyerPort) => $"{ManagedCwKeyerConfig.WinkeyerPortEnvironmentVariable} is required.",
            ManagedCwBackendKind.Cwdaemon => "cwdaemon backend is not implemented.",
            _ => string.Empty,
        };
    }

    private static string ResolveToken(string token, CwSendContext? context, StationProfile? stationProfile)
    {
        return token.Trim().ToUpperInvariant() switch
        {
            "MYCALL" => NonEmpty(stationProfile?.StationCallsign)
                ?? throw new ArgumentException("CW macro token 'MYCALL' requires an active station callsign."),
            "HISCALL" => NonEmpty(context?.WorkedCallsign)
                ?? throw new ArgumentException("CW macro token 'HISCALL' requires a worked callsign."),
            "RST" => NonEmpty(context?.Rst) ?? "599",
            "EXCH" => NonEmpty(context?.Exchange)
                ?? throw new ArgumentException("CW macro token 'EXCH' requires an exchange."),
            "NR" => context is { HasSerial: true }
                ? context.Serial.ToString(CultureInfo.InvariantCulture)
                : throw new ArgumentException("CW macro token 'NR' requires a serial number."),
            var value => throw new ArgumentException($"Unknown CW macro token '{value}'."),
        };
    }

    private void SendWinkeyerText(string text, uint speedWpm)
    {
        using var keyer = OpenWinkeyer();
        keyer.Initialize();
        keyer.SetSpeed(speedWpm);
        keyer.SendText(text);
    }

    private ManagedWinkeyerPort OpenWinkeyer()
    {
        if (string.IsNullOrWhiteSpace(config.WinkeyerPort))
        {
            throw new InvalidOperationException($"{ManagedCwKeyerConfig.WinkeyerPortEnvironmentVariable} is required.");
        }

        return new ManagedWinkeyerPort(config.WinkeyerPort, config.WinkeyerBaud);
    }

    private static string? NonEmpty(string? value)
    {
        return string.IsNullOrWhiteSpace(value) ? null : value.Trim().ToUpperInvariant();
    }
}

internal sealed class ManagedWinkeyerPort : IDisposable
{
    private readonly SerialPort _port;

    public ManagedWinkeyerPort(string portName, int baudRate)
    {
        _port = new SerialPort(portName, baudRate)
        {
            ReadTimeout = 500,
            WriteTimeout = 500,
        };
        _port.Open();
    }

    public void Initialize()
    {
        Write([0x00, 0x02]);
        var version = _port.ReadByte();
        if (version == 0xFF)
        {
            throw new IOException("WinKeyer returned 0xFF; check serial baud rate.");
        }

        Write([0x00, 0x0B]);
        ClearBuffer();
    }

    public void SetSpeed(uint speedWpm)
    {
        Write([0x02, checked((byte)ManagedCwKeyerConfig.ClampSpeed(speedWpm))]);
    }

    public void SendText(string text)
    {
        var bytes = new byte[text.Length];
        for (var index = 0; index < text.Length; index++)
        {
            var ch = char.ToUpperInvariant(text[index]);
            if (ch > 0x7F)
            {
                throw new InvalidOperationException($"Non-ASCII CW character '{ch}'.");
            }

            bytes[index] = (byte)ch;
        }

        Write(bytes);
    }

    public void ClearBuffer()
    {
        Write([0x0A]);
    }

    public void Dispose()
    {
        _port.Dispose();
    }

    private void Write(byte[] bytes)
    {
        _port.Write(bytes, 0, bytes.Length);
    }
}

internal sealed class ManagedCwGrpcService(ManagedEngineState state)
    : CwService.CwServiceBase
{
    private readonly ManagedCwController _controller = new(ManagedCwKeyerConfig.FromEnvironment());

    public override Task<ListCwMacrosResponse> ListCwMacros(ListCwMacrosRequest request, ServerCallContext context)
    {
        var response = new ListCwMacrosResponse();
        response.Macros.AddRange(ManagedCwController.BuiltInMacros);
        return Task.FromResult(response);
    }

    public override Task<SendCwMacroResponse> SendCwMacro(SendCwMacroRequest request, ServerCallContext context)
    {
        try
        {
            var expanded = ManagedCwController.ExpandMacro(
                request.Name,
                request.Context,
                state.GetActiveStationContext().EffectiveActiveProfile);
            _controller.SendText(expanded, request.Context?.HasSpeedWpm == true ? request.Context.SpeedWpm : null);

            return Task.FromResult(new SendCwMacroResponse
            {
                State = CwSendState.Accepted,
                ExpandedText = expanded,
            });
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or IOException or KeyNotFoundException)
        {
            throw ToRpcException(ex);
        }
    }

    public override Task<SendCwTextResponse> SendCwText(SendCwTextRequest request, ServerCallContext context)
    {
        try
        {
            var expanded = ManagedCwController.ExpandTemplate(
                request.Text,
                request.Context,
                state.GetActiveStationContext().EffectiveActiveProfile);
            _controller.SendText(expanded, request.Context?.HasSpeedWpm == true ? request.Context.SpeedWpm : null);

            return Task.FromResult(new SendCwTextResponse
            {
                State = CwSendState.Accepted,
                ExpandedText = expanded,
            });
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or IOException)
        {
            throw ToRpcException(ex);
        }
    }

    public override Task<AbortCwResponse> AbortCw(AbortCwRequest request, ServerCallContext context)
    {
        try
        {
            _controller.Abort();
            return Task.FromResult(new AbortCwResponse
            {
                State = CwSendState.AbortRequested,
            });
        }
        catch (Exception ex) when (ex is InvalidOperationException or IOException)
        {
            throw ToRpcException(ex);
        }
    }

    public override Task<SetCwSpeedResponse> SetCwSpeed(SetCwSpeedRequest request, ServerCallContext context)
    {
        try
        {
            _controller.SetSpeed(request.SpeedWpm);
            return Task.FromResult(new SetCwSpeedResponse
            {
                Status = _controller.Status(),
            });
        }
        catch (Exception ex) when (ex is InvalidOperationException or IOException)
        {
            throw ToRpcException(ex);
        }
    }

    public override Task<GetCwKeyerStatusResponse> GetCwKeyerStatus(GetCwKeyerStatusRequest request, ServerCallContext context)
    {
        return Task.FromResult(new GetCwKeyerStatusResponse
        {
            Status = _controller.Status(),
        });
    }

    private static RpcException ToRpcException(Exception ex)
    {
        var statusCode = ex switch
        {
            KeyNotFoundException => StatusCode.NotFound,
            ArgumentException when ex.Message.Contains("MYCALL", StringComparison.Ordinal) => StatusCode.FailedPrecondition,
            ArgumentException => StatusCode.InvalidArgument,
            InvalidOperationException => StatusCode.FailedPrecondition,
            IOException => StatusCode.Unavailable,
            _ => StatusCode.Internal,
        };

        return new RpcException(new Status(statusCode, ex.Message));
    }
}

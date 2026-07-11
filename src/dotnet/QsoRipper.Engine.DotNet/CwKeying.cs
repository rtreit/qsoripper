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
    uint DefaultSpeedWpm,
    bool TransmitEnabled,
    uint MaxTxMs)
{
    public const string BackendEnvironmentVariable = "QSORIPPER_CW_KEYER_BACKEND";
    public const string WinkeyerPortEnvironmentVariable = "QSORIPPER_CW_WINKEYER_PORT";
    public const string WinkeyerBaudEnvironmentVariable = "QSORIPPER_CW_WINKEYER_BAUD";
    public const string SpeedWpmEnvironmentVariable = "QSORIPPER_CW_SPEED_WPM";
    public const string TransmitEnabledEnvironmentVariable = "QSORIPPER_CW_TRANSMIT_ENABLED";
    public const string MaxTxMsEnvironmentVariable = "QSORIPPER_CW_MAX_TX_MS";
    public const int DefaultWinkeyerBaud = 1200;
    public const uint DefaultCwSpeedWpm = 25;
    public const uint DefaultMaxTxMs = 120_000;
    public const uint MinimumMaxTxMs = 1_000;
    public const uint MaximumMaxTxMs = 300_000;

    public static ManagedCwKeyerConfig FromEnvironment()
    {
        return FromSources(null, Environment.GetEnvironmentVariable);
    }

    internal static ManagedCwKeyerConfig FromSources(
        ManagedCwPersistedSettings? persisted,
        Func<string, string?> environment)
    {
        string? Effective(string name, string? persistedValue) => environment(name) ?? persistedValue;

        return FromValues(
            Effective(BackendEnvironmentVariable, persisted?.Backend),
            Effective(WinkeyerPortEnvironmentVariable, persisted?.WinkeyerPort),
            Effective(WinkeyerBaudEnvironmentVariable, persisted?.WinkeyerBaud?.ToString(CultureInfo.InvariantCulture)),
            Effective(SpeedWpmEnvironmentVariable, persisted?.SpeedWpm?.ToString(CultureInfo.InvariantCulture)),
            Effective(TransmitEnabledEnvironmentVariable, persisted?.TransmitEnabled?.ToString(CultureInfo.InvariantCulture)),
            Effective(MaxTxMsEnvironmentVariable, persisted?.MaxTxMs?.ToString(CultureInfo.InvariantCulture)));
    }

    internal static ManagedCwKeyerConfig FromValues(
        string? backend,
        string? port,
        string? baud,
        string? speed,
        string? transmitEnabled,
        string? maxTxMs)
    {
        var backendKind = (backend ?? "null").Trim().ToUpperInvariant() switch
        {
            "" or "NULL" => ManagedCwBackendKind.Null,
            "WINKEYER" => ManagedCwBackendKind.Winkeyer,
            "CWDAEMON" => ManagedCwBackendKind.Cwdaemon,
            var value => throw new InvalidOperationException($"Unsupported CW keyer backend '{value}'."),
        };

        var parsedSpeed = ParseUIntOrDefault(speed, DefaultCwSpeedWpm);
        ValidateSpeed(parsedSpeed);
        var parsedMaxTxMs = ParseUIntOrDefault(maxTxMs, DefaultMaxTxMs);
        if (parsedMaxTxMs is < MinimumMaxTxMs or > MaximumMaxTxMs)
        {
            throw new InvalidOperationException(
                $"{MaxTxMsEnvironmentVariable} must be between {MinimumMaxTxMs} and {MaximumMaxTxMs}, got {parsedMaxTxMs}.");
        }

        return new ManagedCwKeyerConfig(
            backendKind,
            string.IsNullOrWhiteSpace(port) ? null : port.Trim(),
            ParseIntOrDefault(baud, DefaultWinkeyerBaud),
            parsedSpeed,
            ParseBoolOrDefault(transmitEnabled, false),
            parsedMaxTxMs);
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

    private static bool ParseBoolOrDefault(string? value, bool defaultValue)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            return defaultValue;
        }

        return value.Trim().ToUpperInvariant() switch
        {
            "TRUE" or "1" or "YES" or "ON" => true,
            "FALSE" or "0" or "NO" or "OFF" => false,
            _ => throw new InvalidOperationException($"Invalid boolean CW configuration '{value}'."),
        };
    }

    public static void ValidateSpeed(uint speedWpm)
    {
        if (speedWpm is < 5 or > 99)
        {
            throw new ArgumentOutOfRangeException(
                nameof(speedWpm),
                speedWpm,
                "CW speed must be between 5 and 99 WPM.");
        }
    }
}

internal interface IManagedWinkeyerPort : IDisposable
{
    byte Initialize();
    void SetSpeed(uint speedWpm);
    void SendText(string text);
    void ClearBuffer();
    bool IsBusy();
    void CloseHostMode();
}

internal interface IManagedCwWatchdog : IDisposable
{
    void Arm(TimeSpan dueTime);
    void Cancel();
}

internal sealed class ManagedCwWatchdog(Action callback) : IManagedCwWatchdog
{
    private readonly Timer _timer = new(static state => ((Action)state!).Invoke(), callback, Timeout.InfiniteTimeSpan, Timeout.InfiniteTimeSpan);

    public void Arm(TimeSpan dueTime) => _timer.Change(dueTime, Timeout.InfiniteTimeSpan);

    public void Cancel() => _timer.Change(Timeout.InfiniteTimeSpan, Timeout.InfiniteTimeSpan);

    public void Dispose() => _timer.Dispose();
}

internal sealed class ManagedCwController : IDisposable
{
    private readonly object _gate = new();
    private readonly ManagedCwKeyerConfig _config;
    private readonly Func<string, int, IManagedWinkeyerPort> _portFactory;
    private readonly Func<Action, IManagedCwWatchdog> _watchdogFactory;
    private IManagedWinkeyerPort? _keyer;
    private IManagedCwWatchdog? _watchdog;
    private uint _activeSpeedWpm;
    private uint? _firmwareRevision;
    private string? _lastError;
    private bool _disposed;

    public ManagedCwController(ManagedCwKeyerConfig config)
        : this(
            config,
            static (portName, baudRate) => new ManagedWinkeyerPort(portName, baudRate),
            static callback => new ManagedCwWatchdog(callback))
    {
    }

    internal ManagedCwController(
        ManagedCwKeyerConfig config,
        Func<string, int, IManagedWinkeyerPort> portFactory,
        Func<Action, IManagedCwWatchdog>? watchdogFactory = null)
    {
        _config = config;
        _portFactory = portFactory;
        _watchdogFactory = watchdogFactory ?? (static callback => new ManagedCwWatchdog(callback));
        _activeSpeedWpm = config.DefaultSpeedWpm;
    }
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
        ValidateText(text);
        lock (_gate)
        {
            ThrowIfDisposed();
            var speed = speedWpm ?? _activeSpeedWpm;
            ManagedCwKeyerConfig.ValidateSpeed(speed);
            switch (_config.Backend)
            {
                case ManagedCwBackendKind.Null:
                    _activeSpeedWpm = speed;
                    _lastError = null;
                    return;
                case ManagedCwBackendKind.Winkeyer:
                    if (!_config.TransmitEnabled)
                    {
                        throw new InvalidOperationException(
                            $"CW hardware transmission is disabled; set {ManagedCwKeyerConfig.TransmitEnabledEnvironmentVariable}=true to enable it.");
                    }

                    ExecuteKeyer(keyer =>
                    {
                        keyer.SetSpeed(speed);
                        keyer.SendText(text);
                    });
                    _activeSpeedWpm = speed;
                    ArmWatchdog();
                    return;
                case ManagedCwBackendKind.Cwdaemon:
                    throw new InvalidOperationException("cwdaemon backend is reserved but not implemented.");
                default:
                    throw new InvalidOperationException($"Unsupported CW keyer backend '{_config.Backend}'.");
            }
        }
    }

    public void Abort()
    {
        lock (_gate)
        {
            ThrowIfDisposed();
            CancelWatchdog();
            switch (_config.Backend)
            {
                case ManagedCwBackendKind.Null:
                    return;
                case ManagedCwBackendKind.Winkeyer:
                    ExecuteKeyer(static keyer => keyer.ClearBuffer());
                    return;
                case ManagedCwBackendKind.Cwdaemon:
                    throw new InvalidOperationException("cwdaemon backend is reserved but not implemented.");
                default:
                    throw new InvalidOperationException($"Unsupported CW keyer backend '{_config.Backend}'.");
            }
        }
    }

    public void SetSpeed(uint speedWpm)
    {
        ManagedCwKeyerConfig.ValidateSpeed(speedWpm);
        lock (_gate)
        {
            ThrowIfDisposed();
            switch (_config.Backend)
            {
                case ManagedCwBackendKind.Null:
                    break;
                case ManagedCwBackendKind.Winkeyer:
                    ExecuteKeyer(keyer => keyer.SetSpeed(speedWpm));
                    break;
                case ManagedCwBackendKind.Cwdaemon:
                    throw new InvalidOperationException("cwdaemon backend is reserved but not implemented.");
                default:
                    throw new InvalidOperationException($"Unsupported CW keyer backend '{_config.Backend}'.");
            }

            _activeSpeedWpm = speedWpm;
            _lastError = null;
        }
    }

    public CwKeyerStatus Status()
    {
        lock (_gate)
        {
            ThrowIfDisposed();
            var available = _config.Backend switch
            {
                ManagedCwBackendKind.Null => true,
                ManagedCwBackendKind.Winkeyer => ProbeWinkeyer(),
                ManagedCwBackendKind.Cwdaemon => RecordUnavailable("cwdaemon backend is not implemented."),
                _ => RecordUnavailable($"Unsupported CW keyer backend '{_config.Backend}'."),
            };
            var status = new CwKeyerStatus
            {
                Backend = _config.Backend switch
                {
                    ManagedCwBackendKind.Null => CwKeyerBackend.Null,
                    ManagedCwBackendKind.Winkeyer => CwKeyerBackend.Winkeyer,
                    ManagedCwBackendKind.Cwdaemon => CwKeyerBackend.Cwdaemon,
                    _ => CwKeyerBackend.Unspecified,
                },
                Available = available,
                SpeedWpm = _activeSpeedWpm,
                TransmitEnabled = _config.TransmitEnabled,
                MaxTxMs = _config.MaxTxMs,
            };
            if (_config.WinkeyerPort is not null)
            {
                status.PortName = _config.WinkeyerPort;
            }

            if (_lastError is not null)
            {
                status.ErrorMessage = _lastError;
            }

            if (_firmwareRevision.HasValue)
            {
                status.FirmwareRevision = _firmwareRevision.Value;
            }

            return status;
        }
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

    public void Dispose()
    {
        lock (_gate)
        {
            if (_disposed)
            {
                return;
            }

            _disposed = true;
            _watchdog?.Dispose();
            _watchdog = null;
            Disconnect();
        }
    }

    private bool ProbeWinkeyer()
    {
        try
        {
            EnsureConnected();
            return true;
        }
        catch (Exception error) when (error is InvalidOperationException or IOException)
        {
            _lastError = error.Message;
            Disconnect();
            return false;
        }
    }

    private bool RecordUnavailable(string message)
    {
        _lastError = message;
        return false;
    }

    private void EnsureConnected()
    {
        if (_keyer is not null)
        {
            return;
        }

        if (string.IsNullOrWhiteSpace(_config.WinkeyerPort))
        {
            throw new InvalidOperationException(
                $"{ManagedCwKeyerConfig.WinkeyerPortEnvironmentVariable} is required.");
        }

        var keyer = _portFactory(_config.WinkeyerPort, _config.WinkeyerBaud);
        try
        {
            _firmwareRevision = keyer.Initialize();
            _keyer = keyer;
            _lastError = null;
        }
        catch
        {
            keyer.Dispose();
            throw;
        }
    }

    private void ExecuteKeyer(Action<IManagedWinkeyerPort> operation)
    {
        EnsureConnected();
        try
        {
            operation(_keyer!);
            _lastError = null;
        }
        catch (Exception error) when (error is InvalidOperationException or IOException)
        {
            _lastError = error.Message;
            Disconnect();
            throw;
        }
    }

    private void ArmWatchdog()
    {
        _watchdog ??= _watchdogFactory(ExpireWatchdog);
        _watchdog.Arm(TimeSpan.FromMilliseconds(_config.MaxTxMs));
    }

    private void CancelWatchdog()
    {
        _watchdog?.Cancel();
    }

    private void ExpireWatchdog()
    {
        lock (_gate)
        {
            if (_disposed || _keyer is null)
            {
                return;
            }

            try
            {
                if (_keyer.IsBusy())
                {
                    _keyer.ClearBuffer();
                    _lastError =
                        $"CW transmit safety ceiling reached after {_config.MaxTxMs} ms; WinKeyer buffer cleared.";
                }
                else
                {
                    _lastError = null;
                }
            }
            catch (Exception error) when (error is InvalidOperationException or IOException)
            {
                _lastError = error.Message;
                Disconnect();
            }
        }
    }

    private void Disconnect()
    {
        if (_keyer is null)
        {
            return;
        }

        try
        {
            _keyer.CloseHostMode();
        }
        catch (Exception error) when (error is InvalidOperationException or IOException)
        {
            _lastError ??= error.Message;
        }
        finally
        {
            _keyer.Dispose();
            _keyer = null;
            _firmwareRevision = null;
        }
    }

    private void ThrowIfDisposed()
    {
        ObjectDisposedException.ThrowIf(_disposed, this);
    }

    private static void ValidateText(string text)
    {
        if (string.IsNullOrEmpty(text))
        {
            throw new ArgumentException("CW text must not be empty.", nameof(text));
        }

        var nonAscii = text.FirstOrDefault(static character => character > 0x7F);
        if (nonAscii != default)
        {
            throw new ArgumentException($"CW text contains non-ASCII character '{nonAscii}'.", nameof(text));
        }
    }

    private static string? NonEmpty(string? value)
    {
        return string.IsNullOrWhiteSpace(value) ? null : value.Trim().ToUpperInvariant();
    }
}

internal sealed class ManagedWinkeyerPort : IManagedWinkeyerPort
{
    private readonly SerialPort _port;
    private bool _hostOpen;

    public ManagedWinkeyerPort(string portName, int baudRate)
    {
        _port = new SerialPort(portName, baudRate, Parity.None, 8, StopBits.One)
        {
            ReadTimeout = 500,
            WriteTimeout = 500,
            Handshake = Handshake.None,
        };
        try
        {
            _port.Open();
        }
        catch (Exception error) when (error is UnauthorizedAccessException or ArgumentException or InvalidOperationException or IOException)
        {
            _port.Dispose();
            throw new IOException($"Open WinKeyer port {portName}: {error.Message}", error);
        }
    }

    public byte Initialize()
    {
        Write([0x00, 0x02]);
        int version;
        try
        {
            version = _port.ReadByte();
        }
        catch (TimeoutException error)
        {
            throw new IOException("Timed out reading WinKeyer Host Open response.", error);
        }
        if (version == 0xFF)
        {
            throw new IOException("WinKeyer returned 0xFF; check serial baud rate.");
        }

        _hostOpen = true;
        ClearBuffer();
        return checked((byte)version);
    }

    public void SetSpeed(uint speedWpm)
    {
        ManagedCwKeyerConfig.ValidateSpeed(speedWpm);
        Write([0x02, checked((byte)speedWpm)]);
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

    public bool IsBusy()
    {
        try
        {
            while (_port.BytesToRead > 0)
            {
                _ = _port.ReadByte();
            }

            Write([0x15]);
            while (true)
            {
                var status = _port.ReadByte();
                if ((status & 0xE8) == 0xC0)
                {
                    return (status & 0x04) != 0;
                }
            }
        }
        catch (TimeoutException error)
        {
            throw new IOException("Timed out reading WinKeyer status response.", error);
        }
    }

    public void CloseHostMode()
    {
        if (!_hostOpen)
        {
            return;
        }

        Write([0x00, 0x03]);
        _hostOpen = false;
    }

    public void Dispose()
    {
        if (_hostOpen)
        {
            try
            {
                CloseHostMode();
            }
            catch (Exception error) when (error is IOException or InvalidOperationException or TimeoutException)
            {
                // The transport is already gone; disposal still releases the handle.
            }
        }

        _port.Dispose();
    }

    private void Write(byte[] bytes)
    {
        try
        {
            _port.Write(bytes, 0, bytes.Length);
        }
        catch (Exception error) when (error is InvalidOperationException or TimeoutException or IOException)
        {
            throw new IOException($"WinKeyer serial write failed: {error.Message}", error);
        }
    }
}

internal sealed class ManagedCwGrpcService : CwService.CwServiceBase
{
    private readonly ManagedEngineState _state;
    private readonly ManagedCwController _controller;

    public ManagedCwGrpcService(ManagedEngineState state, ManagedCwController controller)
    {
        _state = state;
        _controller = controller;
    }

    public override Task<ListCwMacrosResponse> ListCwMacros(ListCwMacrosRequest request, ServerCallContext context)
    {
        var response = new ListCwMacrosResponse();
        response.Macros.AddRange(ManagedCwController.BuiltInMacros);
        return Task.FromResult(response);
    }

    public override async Task<SendCwMacroResponse> SendCwMacro(SendCwMacroRequest request, ServerCallContext context)
    {
        try
        {
            var expanded = ManagedCwController.ExpandMacro(
                request.Name,
                request.Context,
                _state.GetActiveStationContext().EffectiveActiveProfile);
            await Task.Run(
                () => _controller.SendText(
                    expanded,
                    request.Context?.HasSpeedWpm == true ? request.Context.SpeedWpm : null),
                context.CancellationToken);

            return new SendCwMacroResponse
            {
                State = CwSendState.Accepted,
                ExpandedText = expanded,
            };
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or IOException or KeyNotFoundException)
        {
            throw ToRpcException(ex);
        }
    }

    public override async Task<SendCwTextResponse> SendCwText(SendCwTextRequest request, ServerCallContext context)
    {
        try
        {
            var expanded = ManagedCwController.ExpandTemplate(
                request.Text,
                request.Context,
                _state.GetActiveStationContext().EffectiveActiveProfile);
            await Task.Run(
                () => _controller.SendText(
                    expanded,
                    request.Context?.HasSpeedWpm == true ? request.Context.SpeedWpm : null),
                context.CancellationToken);

            return new SendCwTextResponse
            {
                State = CwSendState.Accepted,
                ExpandedText = expanded,
            };
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or IOException)
        {
            throw ToRpcException(ex);
        }
    }

    public override async Task<AbortCwResponse> AbortCw(AbortCwRequest request, ServerCallContext context)
    {
        try
        {
            await Task.Run(_controller.Abort, context.CancellationToken);
            return new AbortCwResponse
            {
                State = CwSendState.AbortRequested,
            };
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or IOException)
        {
            throw ToRpcException(ex);
        }
    }

    public override async Task<SetCwSpeedResponse> SetCwSpeed(SetCwSpeedRequest request, ServerCallContext context)
    {
        try
        {
            await Task.Run(() => _controller.SetSpeed(request.SpeedWpm), context.CancellationToken);
            var status = await Task.Run(_controller.Status, context.CancellationToken);
            return new SetCwSpeedResponse
            {
                Status = status,
            };
        }
        catch (Exception ex) when (ex is ArgumentException or InvalidOperationException or IOException)
        {
            throw ToRpcException(ex);
        }
    }

    public override async Task<GetCwKeyerStatusResponse> GetCwKeyerStatus(GetCwKeyerStatusRequest request, ServerCallContext context)
    {
        var status = await Task.Run(_controller.Status, context.CancellationToken);
        return new GetCwKeyerStatusResponse
        {
            Status = status,
        };
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

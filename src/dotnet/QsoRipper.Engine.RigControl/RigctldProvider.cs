using System.Diagnostics.CodeAnalysis;
using System.Globalization;
using System.Net.Sockets;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;

namespace QsoRipper.Engine.RigControl;

/// <summary>
/// rigctld-backed rig control provider.
/// </summary>
/// <remarks>
/// Connects to a running <c>rigctld</c> daemon over TCP, reads logging-relevant
/// rig state, and normalizes the result into project-owned proto types.
/// Each <see cref="GetSnapshot"/> call opens a TCP connection, issues the required
/// and supported optional commands on that socket, and closes it.
/// </remarks>
public sealed class RigctldProvider : IRigControlProvider
{
    /// <summary>Default rigctld host.</summary>
    public const string DefaultHost = "127.0.0.1";

    /// <summary>Default rigctld TCP port.</summary>
    public const int DefaultPort = 4532;

    /// <summary>Default read timeout in milliseconds.</summary>
    public const int DefaultReadTimeoutMs = 2_000;

    private readonly string _host;
    private readonly int _port;
    private readonly TimeSpan _readTimeout;

    public RigctldProvider(string host, int port, TimeSpan readTimeout)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(host);
        ArgumentOutOfRangeException.ThrowIfLessThanOrEqual(port, 0);
        ArgumentOutOfRangeException.ThrowIfGreaterThan(port, 65535);
        ArgumentOutOfRangeException.ThrowIfLessThanOrEqual(readTimeout, TimeSpan.Zero);

        _host = host;
        _port = port;
        _readTimeout = readTimeout;
    }

    public RigSnapshot GetSnapshot()
    {
        var state = ReadRigState();
        var band = BandMapping.FrequencyHzToBand(state.FrequencyHz);
        var modeMapping = ModeMapping.HamlibModeToProto(state.RawMode);

        var snapshot = new RigSnapshot
        {
            FrequencyHz = state.FrequencyHz,
            Band = band,
            Mode = modeMapping.Mode,
            RawMode = state.RawMode,
            Status = RigConnectionStatus.Connected,
            SampledAt = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
        };

        if (modeMapping.Submode is not null)
        {
            snapshot.Submode = modeMapping.Submode;
        }

        if (state.FrequencyRxHz is { } frequencyRxHz)
        {
            snapshot.FrequencyRxHz = frequencyRxHz;
            snapshot.BandRx = BandMapping.FrequencyHzToBand(frequencyRxHz);
        }

        if (state.TxPowerWatts is { } txPowerWatts)
        {
            snapshot.TxPowerWatts = txPowerWatts;
        }

        return snapshot;
    }

    [SuppressMessage("Reliability", "CA2000:Dispose objects before losing scope", Justification = "TcpClient and StreamReader are disposed via using.")]
    private RigState ReadRigState()
    {
        TcpClient client;
        try
        {
            client = new TcpClient();
            var connectTask = client.ConnectAsync(_host, _port);
            if (!connectTask.Wait(_readTimeout))
            {
                client.Dispose();
                throw new RigControlException(
                    $"Connection to {_host}:{_port} timed out after {_readTimeout.TotalMilliseconds:F0}ms.",
                    RigControlErrorKind.Timeout);
            }
        }
        catch (RigControlException)
        {
            throw;
        }
        catch (AggregateException ex) when (ex.InnerException is SocketException socketEx)
        {
            throw new RigControlException(
                $"Failed to connect to {_host}:{_port}: {socketEx.Message}",
                RigControlErrorKind.Transport,
                socketEx);
        }
        catch (SocketException ex)
        {
            throw new RigControlException(
                $"Failed to connect to {_host}:{_port}: {ex.Message}",
                RigControlErrorKind.Transport,
                ex);
        }

        try
        {
            using (client)
            {
                using var stream = client.GetStream();
                stream.ReadTimeout = (int)_readTimeout.TotalMilliseconds;
                using var writer = new StreamWriter(stream, leaveOpen: true) { NewLine = "\n", AutoFlush = true };
                using var reader = new StreamReader(stream, leaveOpen: true);

                // Read frequency: send "f\n", expect one line with Hz value
                writer.Write("f\n");
                var freqLine = ReadLineOrThrow(reader, "frequency");
                var frequencyHz = ParseFrequency(freqLine);

                // Read mode: send "m\n", expect two lines (mode + passband)
                writer.Write("m\n");
                var modeLine = ReadLineOrThrow(reader, "mode");
                // Read and discard passband line
                _ = reader.ReadLine();

                ulong txFrequencyHz = frequencyHz;
                string txMode = modeLine;
                ulong? frequencyRxHz = null;

                writer.Write("s\n");
                var splitLine = TryReadOptionalLine(reader, "split state");
                if (splitLine is not null)
                {
                    var txVfo = TryReadOptionalLine(reader, "split TX VFO");
                    if (txVfo is not null && splitLine == "1")
                    {
                        writer.Write("i\n");
                        var splitFrequencyLine = TryReadOptionalLine(reader, "split frequency");
                        if (splitFrequencyLine is not null
                            && ulong.TryParse(splitFrequencyLine, NumberStyles.None, CultureInfo.InvariantCulture, out var splitFrequencyHz))
                        {
                            frequencyRxHz = frequencyHz;
                            txFrequencyHz = splitFrequencyHz;

                            writer.Write("x\n");
                            var splitMode = TryReadOptionalLine(reader, "split mode");
                            if (splitMode is not null)
                            {
                                _ = ReadLineOrThrow(reader, "split passband");
                                txMode = splitMode;
                            }
                        }
                    }
                }

                var txPowerWatts = ReadOptionalTxPower(
                    reader,
                    writer,
                    txFrequencyHz,
                    txMode);

                return new RigState(txFrequencyHz, txMode, frequencyRxHz, txPowerWatts);
            }
        }
        catch (RigControlException)
        {
            throw;
        }
        catch (IOException ex)
        {
            throw new RigControlException(
                $"Failed to read from rigctld at {_host}:{_port}: {ex.Message}",
                RigControlErrorKind.Transport,
                ex);
        }
    }

    private static string ReadLineOrThrow(StreamReader reader, string commandDescription)
    {
        string? line;
        try
        {
            line = reader.ReadLine();
        }
        catch (IOException ex)
        {
            throw new RigControlException(
                $"Timed out reading {commandDescription} from rigctld.",
                RigControlErrorKind.Timeout,
                ex);
        }

        if (line is null)
        {
            throw new RigControlException(
                $"rigctld closed the connection before returning {commandDescription}.",
                RigControlErrorKind.Transport);
        }

        var trimmed = line.Trim();

        // rigctld reports errors as "RPRT -N" where N is the error code.
        if (trimmed.StartsWith("RPRT", StringComparison.Ordinal))
        {
            throw new RigControlException(
                $"rigctld error: {trimmed}",
                RigControlErrorKind.Parse);
        }

        return trimmed;
    }

    private static string? TryReadOptionalLine(StreamReader reader, string commandDescription)
    {
        try
        {
            return ReadLineOrThrow(reader, commandDescription);
        }
        catch (RigControlException ex) when (ex.Kind == RigControlErrorKind.Parse)
        {
            return null;
        }
    }

    private static double? ReadOptionalTxPower(
        StreamReader reader,
        StreamWriter writer,
        ulong frequencyHz,
        string rawMode)
    {
        writer.Write("l RFPOWER\n");
        var relativeLine = TryReadOptionalLine(reader, "RF power");
        if (relativeLine is null
            || !double.TryParse(relativeLine, NumberStyles.Float, CultureInfo.InvariantCulture, out var relativePower)
            || !double.IsFinite(relativePower)
            || relativePower is < 0 or > 1)
        {
            return null;
        }

        writer.Write(string.Create(
            CultureInfo.InvariantCulture,
            $"2 {relativePower:R} {frequencyHz} {rawMode}\n"));
        var milliwattsLine = TryReadOptionalLine(reader, "power conversion");
        if (milliwattsLine is null
            || !uint.TryParse(milliwattsLine, NumberStyles.None, CultureInfo.InvariantCulture, out var milliwatts))
        {
            return null;
        }

        return milliwatts / 1_000.0;
    }

    private static ulong ParseFrequency(string line)
    {
        if (ulong.TryParse(line.Trim(), out var hz))
        {
            return hz;
        }

        throw new RigControlException(
            $"Invalid frequency value '{line}'.",
            RigControlErrorKind.Parse);
    }

    private sealed record RigState(
        ulong FrequencyHz,
        string RawMode,
        ulong? FrequencyRxHz,
        double? TxPowerWatts);
}

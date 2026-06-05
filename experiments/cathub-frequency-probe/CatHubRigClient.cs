using System.Diagnostics;
using System.Globalization;
using System.Net.Sockets;
using System.Text;

namespace CatHubFrequencyProbe;

internal sealed class CatHubRigClient : IAsyncDisposable
{
    private static readonly TimeSpan ConnectTimeout = TimeSpan.FromMilliseconds(750);
    private static readonly TimeSpan CommandTimeout = TimeSpan.FromMilliseconds(500);

    private readonly string _host;
    private readonly int _port;
    private TcpClient? _client;
    private StreamReader? _reader;
    private NetworkStream? _stream;

    public CatHubRigClient(string host, int port)
    {
        _host = host;
        _port = port;
    }

    public async Task<FrequencySnapshot> ReadSnapshotAsync(CancellationToken cancellationToken)
    {
        var stopwatch = Stopwatch.StartNew();
        try
        {
            await EnsureConnectedAsync(cancellationToken).ConfigureAwait(false);

            var frequency = await ReadFrequencyAsync(cancellationToken).ConfigureAwait(false);
            var (mode, _) = await ReadModeAsync(cancellationToken).ConfigureAwait(false);
            var vfo = await ReadVfoAsync(cancellationToken).ConfigureAwait(false);

            stopwatch.Stop();
            return new FrequencySnapshot(
                frequency,
                mode,
                vfo,
                stopwatch.ElapsedMilliseconds,
                DateTimeOffset.Now);
        }
        catch
        {
            await DisconnectAsync().ConfigureAwait(false);
            throw;
        }
    }

    public async ValueTask DisposeAsync()
    {
        await DisconnectAsync().ConfigureAwait(false);
    }

    private async Task EnsureConnectedAsync(CancellationToken cancellationToken)
    {
        if (_client?.Connected == true && _stream is not null && _reader is not null)
        {
            return;
        }

        await DisconnectAsync().ConfigureAwait(false);

        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(ConnectTimeout);

        _client = new TcpClient();
        await _client.ConnectAsync(_host, _port, timeout.Token).ConfigureAwait(false);
        _stream = _client.GetStream();
        _reader = new StreamReader(_stream, Encoding.ASCII, detectEncodingFromByteOrderMarks: false, bufferSize: 256, leaveOpen: true);
    }

    private async Task<ulong> ReadFrequencyAsync(CancellationToken cancellationToken)
    {
        var line = await CommandLineAsync("f", cancellationToken).ConfigureAwait(false);
        if (!ulong.TryParse(line, NumberStyles.None, CultureInfo.InvariantCulture, out var frequency))
        {
            throw new InvalidDataException($"Invalid frequency reply: '{line}'");
        }

        return frequency;
    }

    private async Task<(string Mode, int PassbandHz)> ReadModeAsync(CancellationToken cancellationToken)
    {
        await WriteCommandAsync("m", cancellationToken).ConfigureAwait(false);
        var mode = await ReadLineWithTimeoutAsync(cancellationToken).ConfigureAwait(false);
        var passbandLine = await ReadLineWithTimeoutAsync(cancellationToken).ConfigureAwait(false);
        _ = int.TryParse(passbandLine, NumberStyles.Integer, CultureInfo.InvariantCulture, out var passband);
        return (mode, passband);
    }

    private async Task<string> ReadVfoAsync(CancellationToken cancellationToken)
    {
        return await CommandLineAsync("v", cancellationToken).ConfigureAwait(false);
    }

    private async Task<string> CommandLineAsync(string command, CancellationToken cancellationToken)
    {
        await WriteCommandAsync(command, cancellationToken).ConfigureAwait(false);
        return await ReadLineWithTimeoutAsync(cancellationToken).ConfigureAwait(false);
    }

    private async Task WriteCommandAsync(string command, CancellationToken cancellationToken)
    {
        var stream = _stream ?? throw new InvalidOperationException("Not connected.");
        var bytes = Encoding.ASCII.GetBytes(command + "\n");
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(CommandTimeout);
        await stream.WriteAsync(bytes, timeout.Token).ConfigureAwait(false);
        await stream.FlushAsync(timeout.Token).ConfigureAwait(false);
    }

    private async Task<string> ReadLineWithTimeoutAsync(CancellationToken cancellationToken)
    {
        var reader = _reader ?? throw new InvalidOperationException("Not connected.");
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
        timeout.CancelAfter(CommandTimeout);
        var line = await reader.ReadLineAsync(timeout.Token).ConfigureAwait(false);
        return line ?? throw new EndOfStreamException("cathub closed the rigctld connection.");
    }

    private Task DisconnectAsync()
    {
        _reader?.Dispose();
        _stream?.Dispose();
        _client?.Dispose();
        _reader = null;
        _stream = null;
        _client = null;
        return Task.CompletedTask;
    }
}

using System.Globalization;
using System.Net;
using System.Net.Sockets;
using Google.Protobuf.WellKnownTypes;
using Microsoft.Extensions.Hosting;
using QsoRipper.Services;

namespace QsoRipper.Engine.DotNet.Wsjtx;

/// <summary>
/// Background supervisor that mirrors the Rust qsoripper-server WSJT-X ingest runtime. It runs two
/// independent, cancelable loops — a UDP Logged ADIF listener and an ADIF tail recovery poller —
/// that re-read effective settings every iteration, serialize all imports through a shared lock so
/// they never interleave on storage, and publish live diagnostics into
/// <see cref="ManagedEngineState"/> for SetupStatus.wsjtx_ingest_status. QRZ uploads are spawned off
/// the ingest loops and serialized by a dedicated lock so they never block ingestion.
/// </summary>
internal sealed class WsjtxIngestSupervisor : IHostedService, IDisposable
{
    private static readonly TimeSpan UdpReceiveTimeout = TimeSpan.FromMilliseconds(500);
    private static readonly TimeSpan IdlePollInterval = TimeSpan.FromMilliseconds(500);
    private static readonly TimeSpan BindBackoff = TimeSpan.FromMilliseconds(1000);

    private readonly ManagedEngineState _state;
    private readonly object _statusLock = new();
    private readonly WsjtxIngestStatus _status = new();
    private readonly SemaphoreSlim _importLock = new(1, 1);
    private readonly SemaphoreSlim _qrzSyncLock = new(1, 1);

    private CancellationTokenSource? _cts;
    private Task? _udpTask;
    private Task? _tailTask;
    private int _tailCursor;

    public WsjtxIngestSupervisor(ManagedEngineState state)
    {
        _state = state ?? throw new ArgumentNullException(nameof(state));
    }

    public Task StartAsync(CancellationToken cancellationToken)
    {
        _cts = new CancellationTokenSource();
        var token = _cts.Token;
        _udpTask = Task.Run(() => UdpLoopAsync(token), CancellationToken.None);
        _tailTask = Task.Run(() => TailLoopAsync(token), CancellationToken.None);
        return Task.CompletedTask;
    }

    public async Task StopAsync(CancellationToken cancellationToken)
    {
        if (_cts is null)
        {
            return;
        }

        await _cts.CancelAsync().ConfigureAwait(false);

        var pending = new[] { _udpTask, _tailTask }.Where(static task => task is not null).Cast<Task>().ToArray();
        if (pending.Length > 0)
        {
            try
            {
                await Task.WhenAll(pending).WaitAsync(cancellationToken).ConfigureAwait(false);
            }
            catch (OperationCanceledException)
            {
                // Host shutdown timeout elapsed; the loops observe cancellation and exit on their own.
            }
        }
    }

    public void Dispose()
    {
        _cts?.Dispose();
        _importLock.Dispose();
        _qrzSyncLock.Dispose();
    }

    private async Task UdpLoopAsync(CancellationToken token)
    {
        var activeBind = string.Empty;
        UdpClient? socket = null;

        try
        {
            while (!token.IsCancellationRequested)
            {
                var settings = _state.GetWsjtxIngestSettingsSnapshot();
                UpdateConfigStatus(settings);

                if (!settings.Enabled || !settings.UdpEnabled)
                {
                    socket?.Dispose();
                    socket = null;
                    activeBind = string.Empty;
                    MarkUdpStopped();
                    if (await DelayOrCancel(IdlePollInterval, token).ConfigureAwait(false))
                    {
                        break;
                    }

                    continue;
                }

                if (socket is null || !string.Equals(activeBind, settings.UdpBind, StringComparison.Ordinal))
                {
                    socket?.Dispose();
                    socket = null;
                    if (!TryBind(settings.UdpBind, out var bound, out var bindError))
                    {
                        MutateStatus(status =>
                        {
                            status.Enabled = true;
                            status.Running = settings.AdifTailEnabled;
                            status.UdpRunning = false;
                            status.UdpBind = settings.UdpBind;
                            status.LastError = $"Failed to bind WSJT-X UDP listener on {settings.UdpBind}: {bindError}";
                        });
                        activeBind = string.Empty;
                        if (await DelayOrCancel(BindBackoff, token).ConfigureAwait(false))
                        {
                            break;
                        }

                        continue;
                    }

                    socket = bound;
                    activeBind = settings.UdpBind;
                    var localBind = LocalBindOrDefault(socket, settings.UdpBind);
                    MutateStatus(status =>
                    {
                        status.Enabled = true;
                        status.Running = true;
                        status.UdpRunning = true;
                        status.UdpBind = localBind;
                        status.ClearLastError();
                    });
                }

                try
                {
                    using var timeoutCts = CancellationTokenSource.CreateLinkedTokenSource(token);
                    timeoutCts.CancelAfter(UdpReceiveTimeout);
                    UdpReceiveResult result;
                    try
                    {
                        result = await socket.ReceiveAsync(timeoutCts.Token).ConfigureAwait(false);
                    }
                    catch (OperationCanceledException) when (!token.IsCancellationRequested)
                    {
                        // Receive timeout: re-evaluate settings on the next iteration.
                        continue;
                    }

                    await ProcessDatagramAsync(result.Buffer, settings.SyncToQrz, token).ConfigureAwait(false);
                }
                catch (OperationCanceledException) when (token.IsCancellationRequested)
                {
                    break;
                }
                catch (SocketException socketError)
                {
                    MutateStatus(status => status.LastError = $"WSJT-X UDP receive failed: {socketError.Message}");
                    socket.Dispose();
                    socket = null;
                    activeBind = string.Empty;
                }
            }
        }
        finally
        {
            socket?.Dispose();
            MarkUdpStopped();
        }
    }

    private async Task TailLoopAsync(CancellationToken token)
    {
        var activePath = string.Empty;
        _tailCursor = 0;

        try
        {
            while (!token.IsCancellationRequested)
            {
                var settings = _state.GetWsjtxIngestSettingsSnapshot();
                UpdateConfigStatus(settings);

                if (!settings.Enabled || !settings.AdifTailEnabled)
                {
                    _tailCursor = 0;
                    activePath = string.Empty;
                    MarkTailStopped();
                    if (await DelayOrCancel(IdlePollInterval, token).ConfigureAwait(false))
                    {
                        break;
                    }

                    continue;
                }

                var path = settings.HasAdifTailPath ? settings.AdifTailPath?.Trim() : null;
                if (string.IsNullOrWhiteSpace(path))
                {
                    MutateStatus(status => status.LastError = "WSJT-X ADIF tail is enabled but no path is configured.");
                    if (await DelayOrCancel(PollInterval(settings), token).ConfigureAwait(false))
                    {
                        break;
                    }

                    continue;
                }

                if (!string.Equals(activePath, path, StringComparison.Ordinal))
                {
                    _tailCursor = 0;
                    activePath = path;
                }

                var tailPath = path;
                MutateStatus(status =>
                {
                    status.Enabled = true;
                    status.Running = true;
                    status.AdifTailRunning = true;
                    status.AdifTailPath = tailPath;
                });

                await HandleTailImportAsync(path, settings.SyncToQrz, token).ConfigureAwait(false);

                if (await DelayOrCancel(PollInterval(settings), token).ConfigureAwait(false))
                {
                    break;
                }
            }
        }
        finally
        {
            MarkTailStopped();
        }
    }

    /// <summary>
    /// Processes a single WSJT-X UDP datagram exactly as the Rust udp loop does: ignored datagrams
    /// are recorded as a skip, malformed datagrams increment parse_errors, and Logged ADIF datagrams
    /// are imported (refresh=true) under the import lock with optional QRZ sync.
    /// </summary>
    internal async Task ProcessDatagramAsync(byte[] datagram, bool syncToQrz, CancellationToken token)
    {
        ArgumentNullException.ThrowIfNull(datagram);

        var parse = WsjtxDatagram.TryParseLoggedAdif(datagram);
        switch (parse.Status)
        {
            case WsjtxDatagramParseStatus.Ignored:
                RecordImportSummary(new ImportAdifResponse { RecordsSkipped = 1 }, Array.Empty<WsjtxImportedQsoRef>());
                break;

            case WsjtxDatagramParseStatus.Malformed:
                MutateStatus(status =>
                {
                    status.ParseErrors = checked(status.ParseErrors + 1);
                    status.LastError = parse.Error;
                });
                break;

            case WsjtxDatagramParseStatus.Logged:
                await ImportUnderLockAsync(parse.Adif!, refresh: true, syncToQrz, token).ConfigureAwait(false);
                break;

            default:
                break;
        }
    }

    /// <summary>
    /// Performs one ADIF tail poll for the supplied path, mirroring the Rust
    /// <c>ingest_wsjtx_adif_tail</c> cursor semantics (refresh=false, complete-record-only advance).
    /// </summary>
    internal async Task HandleTailImportAsync(string path, bool syncToQrz, CancellationToken token)
    {
        byte[] bytes;
        try
        {
            bytes = await File.ReadAllBytesAsync(path, token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested)
        {
            return;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or NotSupportedException or ArgumentException)
        {
            MutateStatus(status =>
            {
                status.ParseErrors = checked(status.ParseErrors + 1);
                status.LastError = $"failed to read {path}: {error.Message}";
            });
            return;
        }

        if (_tailCursor > bytes.Length)
        {
            _tailCursor = 0;
        }

        if (_tailCursor == bytes.Length)
        {
            return;
        }

        var appended = bytes.AsSpan(_tailCursor);
        var completeLength = WsjtxAdifTail.CompleteAdifPrefixLength(appended);
        if (completeLength is not { } length)
        {
            return;
        }

        var payload = appended[..length].ToArray();
        var start = _tailCursor;

        WsjtxImportDetail detail;
        try
        {
            await _importLock.WaitAsync(token).ConfigureAwait(false);
            try
            {
                detail = _state.ImportAdifDetailed(
                    payload,
                    refresh: false,
                    WsjtxImportDiagnostic.AdifTailSource);
            }
            finally
            {
                _importLock.Release();
            }
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested)
        {
            return;
        }
        catch (Exception error) when (error is FormatException or InvalidOperationException or ArgumentException)
        {
            MutateStatus(status =>
            {
                status.ParseErrors = checked(status.ParseErrors + 1);
                status.LastError = $"WSJT-X ADIF tail import failed: {error.Message}";
            });
            return;
        }

        _tailCursor = start + length;
        RecordImportSummary(detail.Response, detail.AffectedQsos);
        QueueQrzSync(detail.AffectedQsos, syncToQrz);
    }

    private async Task ImportUnderLockAsync(byte[] adif, bool refresh, bool syncToQrz, CancellationToken token)
    {
        WsjtxImportDetail detail;
        try
        {
            await _importLock.WaitAsync(token).ConfigureAwait(false);
            try
            {
                detail = _state.ImportAdifDetailed(
                    adif,
                    refresh,
                    WsjtxImportDiagnostic.UdpSource);
            }
            finally
            {
                _importLock.Release();
            }
        }
        catch (OperationCanceledException) when (token.IsCancellationRequested)
        {
            return;
        }
        catch (Exception error) when (error is FormatException or InvalidOperationException or ArgumentException)
        {
            MutateStatus(status =>
            {
                status.ParseErrors = checked(status.ParseErrors + 1);
                status.LastError = $"WSJT-X import failed: {error.Message}";
            });
            return;
        }

        RecordImportSummary(detail.Response, detail.AffectedQsos);
        QueueQrzSync(detail.AffectedQsos, syncToQrz);
    }

    private void QueueQrzSync(IReadOnlyList<WsjtxImportedQsoRef> affected, bool syncToQrz)
    {
        if (!syncToQrz || affected.Count == 0)
        {
            return;
        }

        var localIds = affected.Select(static qso => qso.LocalId).ToArray();
        _ = Task.Run(() => RunQrzSyncAsync(localIds));
    }

    /// <summary>
    /// Runs the managed QRZ sync for the supplied imported QSOs off the ingest loop, serialized by a
    /// dedicated lock, and folds each per-QSO outcome into live status.
    /// </summary>
    internal async Task RunQrzSyncAsync(IReadOnlyList<string> localIds)
    {
        if (localIds.Count == 0)
        {
            return;
        }

        await _qrzSyncLock.WaitAsync().ConfigureAwait(false);
        try
        {
            IReadOnlyList<WsjtxQrzSyncOutcome> outcomes;
            try
            {
                outcomes = _state.SyncImportedQsosToQrz(localIds);
            }
            catch (Exception error) when (error is InvalidOperationException or ArgumentException)
            {
                MutateStatus(status =>
                {
                    status.LastQrzSyncSuccess = false;
                    status.LastQrzSyncError = $"WSJT-X QRZ sync failed: {error.Message}";
                });
                return;
            }

            foreach (var outcome in outcomes)
            {
                MutateStatus(status =>
                {
                    if (outcome.Success)
                    {
                        status.LastQrzSyncSuccess = true;
                        status.ClearLastQrzSyncError();
                        status.LastImportedCallsign = outcome.WorkedCallsign;
                        status.LastImportedLocalId = outcome.LocalId;
                    }
                    else
                    {
                        status.LastQrzSyncSuccess = false;
                        status.LastQrzSyncError = outcome.Error;
                    }
                });
            }
        }
        finally
        {
            _qrzSyncLock.Release();
        }
    }

    private void RecordImportSummary(ImportAdifResponse response, IReadOnlyList<WsjtxImportedQsoRef> affected)
    {
        MutateStatus(status =>
        {
            status.LastEventAt = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow);
            status.RecordsImported = SaturatingAdd(status.RecordsImported, response.RecordsImported);
            status.RecordsUpdated = SaturatingAdd(status.RecordsUpdated, response.RecordsUpdated);
            status.RecordsSkipped = SaturatingAdd(status.RecordsSkipped, response.RecordsSkipped);
            status.DuplicatesSkipped = SaturatingAdd(status.DuplicatesSkipped, CountDuplicateWarnings(response));
            if (affected.Count > 0)
            {
                var last = affected[^1];
                status.LastImportedCallsign = last.WorkedCallsign;
                status.LastImportedLocalId = last.LocalId;
            }

            status.ClearLastError();
        });
    }

    private void UpdateConfigStatus(WsjtxIngestSettings settings)
    {
        MutateStatus(status =>
        {
            status.Enabled = settings.Enabled;
            status.UdpBind = settings.UdpBind;
            if (settings.HasAdifTailPath && !string.IsNullOrWhiteSpace(settings.AdifTailPath))
            {
                status.AdifTailPath = settings.AdifTailPath;
            }
            else
            {
                status.ClearAdifTailPath();
            }

            status.Running = settings.Enabled && (status.UdpRunning || status.AdifTailRunning);
        });
    }

    private void MarkUdpStopped()
    {
        MutateStatus(status =>
        {
            status.UdpRunning = false;
            status.Running = status.Enabled && status.AdifTailRunning;
        });
    }

    private void MarkTailStopped()
    {
        MutateStatus(status =>
        {
            status.AdifTailRunning = false;
            status.Running = status.Enabled && status.UdpRunning;
        });
    }

    /// <summary>Returns a clone of the supervisor's current live status snapshot.</summary>
    internal WsjtxIngestStatus SnapshotStatus()
    {
        lock (_statusLock)
        {
            return _status.Clone();
        }
    }

    private void MutateStatus(Action<WsjtxIngestStatus> mutate)
    {
        WsjtxIngestStatus snapshot;
        lock (_statusLock)
        {
            mutate(_status);
            snapshot = _status.Clone();
        }

        _state.SetWsjtxIngestLiveStatus(snapshot);
    }

    private static uint CountDuplicateWarnings(ImportAdifResponse response)
    {
        var count = 0L;
        foreach (var warning in response.Warnings)
        {
            if (warning.Contains("duplicate skipped", StringComparison.Ordinal))
            {
                count++;
            }
        }

        return count > uint.MaxValue ? uint.MaxValue : (uint)count;
    }

    private static uint SaturatingAdd(uint current, uint delta)
    {
        var sum = (ulong)current + delta;
        return sum > uint.MaxValue ? uint.MaxValue : (uint)sum;
    }

    private static TimeSpan PollInterval(WsjtxIngestSettings settings)
    {
        var ms = settings.PollIntervalMs == 0 ? 1000 : settings.PollIntervalMs;
        return TimeSpan.FromMilliseconds(ms);
    }

    private static bool TryBind(string bind, out UdpClient socket, out string error)
    {
        socket = null!;
        error = string.Empty;
        if (!TryParseEndpoint(bind, out var endpoint))
        {
            error = "bind address must be in host:port form";
            return false;
        }

        try
        {
            socket = new UdpClient(endpoint);
            return true;
        }
        catch (SocketException socketError)
        {
            error = socketError.Message;
            return false;
        }
        catch (ArgumentException argError)
        {
            error = argError.Message;
            return false;
        }
    }

    private static bool TryParseEndpoint(string bind, out IPEndPoint endpoint)
    {
        if (IPEndPoint.TryParse(bind, out var parsed) && parsed.Port > 0)
        {
            endpoint = parsed;
            return true;
        }

        // Fall back to resolving a host name to a loopback/any address is intentionally not done:
        // the Rust runtime binds the literal host:port. Try a DNS-free host split for "localhost".
        var separator = bind.LastIndexOf(':');
        if (separator > 0 && separator < bind.Length - 1)
        {
            var host = bind[..separator];
            var portText = bind[(separator + 1)..];
            if (int.TryParse(portText, NumberStyles.Integer, CultureInfo.InvariantCulture, out var port)
                && port is > 0 and <= 65535
                && host.Equals("localhost", StringComparison.OrdinalIgnoreCase))
            {
                endpoint = new IPEndPoint(IPAddress.Loopback, port);
                return true;
            }
        }

        endpoint = null!;
        return false;
    }

    private static string LocalBindOrDefault(UdpClient socket, string fallback)
    {
        try
        {
            return socket.Client.LocalEndPoint?.ToString() ?? fallback;
        }
        catch (SocketException)
        {
            return fallback;
        }
        catch (ObjectDisposedException)
        {
            return fallback;
        }
    }

    private static async Task<bool> DelayOrCancel(TimeSpan duration, CancellationToken token)
    {
        try
        {
            await Task.Delay(duration, token).ConfigureAwait(false);
            return false;
        }
        catch (OperationCanceledException)
        {
            return true;
        }
    }
}

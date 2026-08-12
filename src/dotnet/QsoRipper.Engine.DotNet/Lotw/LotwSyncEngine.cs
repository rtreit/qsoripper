using System.Diagnostics;
using System.Globalization;
using System.Net;
using System.Text;
using System.Text.RegularExpressions;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;
using QsoRipper.Engine.Storage;

namespace QsoRipper.Engine.DotNet.Lotw;

internal sealed record LotwReport(IReadOnlyList<QsoRecord> Confirmations, string? HighWater);

internal sealed record LotwSyncResult(
    int TotalRecords,
    int ProcessedRecords,
    int UploadedRecords,
    int ConfirmedRecords,
    int UnmatchedRecords,
    int ConflictRecords,
    int ErrorRecords,
    string? ConfirmationHighWater,
    string? ErrorSummary);

internal interface ILotwApi
{
    Task<int> UploadQsosAsync(IReadOnlyList<QsoRecord> qsos, CancellationToken cancellationToken);

    Task<LotwReport> FetchConfirmationsAsync(string? since, CancellationToken cancellationToken);
}

internal sealed class LotwConfigurationException : InvalidOperationException
{
    public LotwConfigurationException()
    {
    }

    public LotwConfigurationException(string message)
        : base(message)
    {
    }

    public LotwConfigurationException(string message, Exception innerException)
        : base(message, innerException)
    {
    }
}

internal sealed class LotwClient : ILotwApi, IDisposable
{
    internal const string DefaultReportUrl = "https://lotw.arrl.org/lotwuser/lotwreport.adi";

    private static readonly Regex HighWaterRegex = new(
        @"<APP_LOTW_LASTQSL(?::\d+)?(?:\:[^>]*)?>(?<value>[^<\r\n]*)",
        RegexOptions.IgnoreCase | RegexOptions.CultureInvariant);

    private readonly string _username;
    private readonly string _password;
    private readonly string _tqslPath;
    private readonly string _stationLocation;
    private readonly string? _certificatePassword;
    private readonly Uri _reportUrl;
    private readonly TimeSpan _timeout;
    private readonly HttpClient _httpClient;

    public LotwClient(ManagedLotwSettings settings)
    {
        ArgumentNullException.ThrowIfNull(settings);
        _username = Required(settings.Username, "LoTW username is not configured.");
        _password = Required(settings.Password, "LoTW password is not configured.", trim: false);
        _tqslPath = Required(settings.TqslPath, "TQSL executable is not configured.");
        _stationLocation = Required(settings.StationLocation, "TQSL station location is not configured.");
        _certificatePassword = Normalize(settings.CertificatePassword, trim: false);
        if (!Uri.TryCreate(Normalize(settings.ReportUrl) ?? DefaultReportUrl, UriKind.Absolute, out var reportUrl))
        {
            throw new LotwConfigurationException("LoTW report URL is invalid.");
        }
        _reportUrl = reportUrl;

        _timeout = TimeSpan.FromSeconds(settings.TimeoutSeconds is > 0 ? settings.TimeoutSeconds.Value : 60);
        _httpClient = new HttpClient { Timeout = _timeout };
    }

    public async Task<int> UploadQsosAsync(IReadOnlyList<QsoRecord> qsos, CancellationToken cancellationToken)
    {
        ArgumentNullException.ThrowIfNull(qsos);
        if (qsos.Count == 0)
        {
            return 0;
        }

        var stagingDirectory = Path.Combine(Path.GetTempPath(), $"qsoripper-lotw-{Guid.NewGuid():N}");
        Directory.CreateDirectory(stagingDirectory);
        try
        {
            var adifPath = Path.Combine(stagingDirectory, "qsoripper-lotw-upload.adi");
            await File.WriteAllBytesAsync(
                adifPath,
                ManagedAdifCodec.SerializeAdiQsos(qsos, includeHeader: true),
                cancellationToken).ConfigureAwait(false);

            using var process = new Process
            {
                StartInfo = BuildTqslStartInfo(adifPath),
            };
            try
            {
                if (!process.Start())
                {
                    throw new InvalidOperationException("TQSL did not start.");
                }
            }
            catch (Exception exception) when (exception is System.ComponentModel.Win32Exception or InvalidOperationException)
            {
                throw new InvalidOperationException("TQSL did not start.", exception);
            }

            var standardOutput = process.StandardOutput.ReadToEndAsync(cancellationToken);
            var standardError = process.StandardError.ReadToEndAsync(cancellationToken);
            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            timeout.CancelAfter(_timeout);
            try
            {
                await process.WaitForExitAsync(timeout.Token).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (!cancellationToken.IsCancellationRequested)
            {
                TryKill(process);
                throw new TimeoutException("TQSL timed out.");
            }

            var output = await standardOutput.ConfigureAwait(false);
            var error = await standardError.ConfigureAwait(false);
            if (process.ExitCode != 0)
            {
                throw new InvalidOperationException($"TQSL upload failed: {SafeProcessDetail(error, output)}");
            }

            return qsos.Count;
        }
        finally
        {
            try
            {
                Directory.Delete(stagingDirectory, recursive: true);
            }
            catch (IOException)
            {
            }
            catch (UnauthorizedAccessException)
            {
            }
        }
    }

    public async Task<LotwReport> FetchConfirmationsAsync(string? since, CancellationToken cancellationToken)
    {
        var query = new Dictionary<string, string>
        {
            ["login"] = _username,
            ["password"] = _password,
            ["qso_query"] = "1",
            ["qso_qsl"] = "yes",
            ["qso_qsldetail"] = "yes",
            ["qso_mydetail"] = "yes",
            ["qso_withown"] = "yes",
        };
        if (!string.IsNullOrWhiteSpace(since))
        {
            query["qso_qslsince"] = since.Trim();
        }

        var requestUri = BuildRequestUri(query);
        HttpResponseMessage response;
        try
        {
            response = await _httpClient.GetAsync(requestUri, HttpCompletionOption.ResponseHeadersRead, cancellationToken)
                .ConfigureAwait(false);
        }
        catch (Exception exception) when (exception is HttpRequestException or TaskCanceledException)
        {
            throw new InvalidOperationException("LoTW report request failed.", exception);
        }

        using (response)
        {
            if (response.StatusCode is HttpStatusCode.Unauthorized or HttpStatusCode.Forbidden)
            {
                throw new InvalidOperationException("LoTW authentication failed.");
            }
            if (!response.IsSuccessStatusCode)
            {
                throw new InvalidOperationException(
                    $"LoTW report request returned HTTP {(int)response.StatusCode}.");
            }

            var payload = await response.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
            var text = Encoding.UTF8.GetString(payload);
            if (LooksLikeAuthenticationFailure(text))
            {
                throw new InvalidOperationException("LoTW authentication failed.");
            }

            return new LotwReport(
                ManagedAdifCodec.ParseAdiQsos(payload),
                Normalize(HighWaterRegex.Match(text).Groups["value"].Value));
        }
    }

    public void Dispose() => _httpClient.Dispose();

    private ProcessStartInfo BuildTqslStartInfo(string adifPath)
    {
        var startInfo = new ProcessStartInfo
        {
            FileName = _tqslPath,
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        startInfo.ArgumentList.Add("-a");
        startInfo.ArgumentList.Add("compliant");
        startInfo.ArgumentList.Add("-d");
        startInfo.ArgumentList.Add("-l");
        startInfo.ArgumentList.Add(_stationLocation);
        if (_certificatePassword is not null)
        {
            startInfo.ArgumentList.Add("-p");
            startInfo.ArgumentList.Add(_certificatePassword);
        }
        startInfo.ArgumentList.Add("-u");
        startInfo.ArgumentList.Add(adifPath);
        startInfo.ArgumentList.Add("-x");
        return startInfo;
    }

    private Uri BuildRequestUri(IReadOnlyDictionary<string, string> query)
    {
        var builder = new UriBuilder(_reportUrl)
        {
            Query = string.Join("&", query.Select(static pair =>
                $"{Uri.EscapeDataString(pair.Key)}={Uri.EscapeDataString(pair.Value)}")),
        };
        return builder.Uri;
    }

    private static bool LooksLikeAuthenticationFailure(string payload) =>
        payload.Contains("<!DOCTYPE html", StringComparison.OrdinalIgnoreCase)
        || payload.Contains("<html", StringComparison.OrdinalIgnoreCase)
        || payload.Contains("password", StringComparison.OrdinalIgnoreCase)
            && payload.Contains("login", StringComparison.OrdinalIgnoreCase);

    private string SafeProcessDetail(params string[] values)
    {
        var detail = values
            .SelectMany(static value => value.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries))
            .Select(static value => value.Trim())
            .FirstOrDefault(static value => value.Length > 0);
        if (detail is null)
        {
            return "TQSL returned an error.";
        }

        foreach (var secret in new[] { _password, _certificatePassword })
        {
            if (!string.IsNullOrEmpty(secret))
            {
                detail = detail.Replace(secret, "[REDACTED]", StringComparison.Ordinal);
            }
        }
        return detail[..Math.Min(detail.Length, 500)];
    }

    private static void TryKill(Process process)
    {
        try
        {
            process.Kill(entireProcessTree: true);
        }
        catch (InvalidOperationException)
        {
        }
    }

    private static string Required(string? value, string message, bool trim = true) =>
        Normalize(value, trim) ?? throw new LotwConfigurationException(message);

    private static string? Normalize(string? value, bool trim = true)
    {
        if (string.IsNullOrWhiteSpace(value))
        {
            return null;
        }
        return trim ? value.Trim() : value;
    }
}

internal sealed class LotwSyncEngine(ILotwApi api, ILogbookStore store)
{
    private const double MatchToleranceMinutes = 30;

    public async Task<LotwSyncResult> SyncAsync(
        bool fullSync,
        bool upload,
        bool download,
        CancellationToken cancellationToken)
    {
        var localQsos = await store.ListQsosAsync(new QsoListQuery { Sort = QsoSortOrder.OldestFirst });
        var uploaded = 0;
        var confirmed = 0;
        var unmatched = 0;
        var conflicts = 0;
        var errors = 0;
        var processed = 0;
        var errorMessages = new List<string>();

        if (upload)
        {
            var pending = localQsos.Where(IsUploadPending).Select(static qso => qso.Clone()).ToArray();
            if (pending.Length > 0)
            {
                try
                {
                    uploaded = await api.UploadQsosAsync(pending, cancellationToken).ConfigureAwait(false);
                    processed += uploaded;
                    foreach (var qso in pending)
                    {
                        await PatchUploadAsync(qso, success: true).ConfigureAwait(false);
                    }
                }
                catch (Exception exception) when (exception is not OperationCanceledException)
                {
                    errors += pending.Length;
                    errorMessages.Add(exception.Message);
                    foreach (var qso in pending)
                    {
                        await PatchUploadAsync(qso, success: false).ConfigureAwait(false);
                    }
                }
            }
        }

        var metadata = await store.GetSyncMetadataAsync();
        var highWater = metadata.LotwLastQsl;
        if (download)
        {
            try
            {
                var report = await api.FetchConfirmationsAsync(fullSync ? null : metadata.LotwLastQsl, cancellationToken)
                    .ConfigureAwait(false);
                highWater = report.HighWater ?? metadata.LotwLastQsl;
                processed += report.Confirmations.Count;
                foreach (var confirmation in report.Confirmations)
                {
                    var matches = FindMatches(confirmation, localQsos).ToArray();
                    if (matches.Length == 0)
                    {
                        unmatched++;
                    }
                    else if (matches.Length == 1)
                    {
                        if (await ApplyConfirmationAsync(matches[0], confirmation).ConfigureAwait(false))
                        {
                            confirmed++;
                        }
                    }
                    else
                    {
                        conflicts++;
                        foreach (var match in matches)
                        {
                            await PatchStatusAsync(match, LotwSyncStatus.Conflict).ConfigureAwait(false);
                        }
                    }
                }

                await store.UpsertSyncMetadataAsync(metadata with
                {
                    LotwLastSync = DateTimeOffset.UtcNow,
                    LotwLastQsl = highWater,
                });
            }
            catch (Exception exception) when (exception is not OperationCanceledException)
            {
                errors++;
                errorMessages.Add(exception.Message);
            }
        }

        return new LotwSyncResult(
            localQsos.Count,
            processed,
            uploaded,
            confirmed,
            unmatched,
            conflicts,
            errors,
            highWater,
            errorMessages.Count == 0 ? null : string.Join(" ", errorMessages));
    }

    public async Task UploadSingleAsync(QsoRecord snapshot, CancellationToken cancellationToken)
    {
        try
        {
            await api.UploadQsosAsync([snapshot], cancellationToken).ConfigureAwait(false);
            await PatchUploadAsync(snapshot, success: true).ConfigureAwait(false);
        }
        catch
        {
            await PatchUploadAsync(snapshot, success: false).ConfigureAwait(false);
            throw;
        }
    }

    private async Task PatchUploadAsync(QsoRecord snapshot, bool success)
    {
        for (var attempt = 0; attempt < 3; attempt++)
        {
            var current = await store.GetQsoAsync(snapshot.LocalId);
            if (current is null || current.DeletedAt is not null)
            {
                return;
            }

            var replacement = current.Clone();
            if (success)
            {
                replacement.LotwSent = true;
                replacement.LotwSentStatus = QslStatus.Yes;
                replacement.LotwSentDate = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow);
                replacement.LotwSyncStatus = Equals(current.UpdatedAt, snapshot.UpdatedAt)
                    ? LotwSyncStatus.Uploaded
                    : LotwSyncStatus.Modified;
            }
            else
            {
                replacement.LotwSyncStatus = LotwSyncStatus.Failed;
            }

            if (await store.UpdateQsoIfUnchangedAsync(current, replacement))
            {
                return;
            }
        }
    }

    private async Task<bool> ApplyConfirmationAsync(QsoRecord snapshot, QsoRecord confirmation)
    {
        for (var attempt = 0; attempt < 3; attempt++)
        {
            var current = await store.GetQsoAsync(snapshot.LocalId);
            if (current is null || current.DeletedAt is not null)
            {
                return false;
            }

            var replacement = current.Clone();
            replacement.LotwReceived = true;
            replacement.LotwReceivedStatus = QslStatus.Yes;
            replacement.LotwReceivedDate = confirmation.LotwReceivedDate?.Clone()
                ?? confirmation.QslReceivedDate?.Clone()
                ?? Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow);
            replacement.LotwSyncStatus = LotwSyncStatus.Confirmed;
            CopyMissingConfirmationFields(replacement, confirmation);
            if (await store.UpdateQsoIfUnchangedAsync(current, replacement))
            {
                return true;
            }
        }
        return false;
    }

    private Task PatchStatusAsync(QsoRecord snapshot, LotwSyncStatus status) =>
        PatchStatusCoreAsync(snapshot, status);

    private async Task PatchStatusCoreAsync(QsoRecord snapshot, LotwSyncStatus status)
    {
        for (var attempt = 0; attempt < 3; attempt++)
        {
            var current = await store.GetQsoAsync(snapshot.LocalId);
            if (current is null)
            {
                return;
            }
            var replacement = current.Clone();
            replacement.LotwSyncStatus = status;
            if (await store.UpdateQsoIfUnchangedAsync(current, replacement))
            {
                return;
            }
        }
    }

    private static bool IsUploadPending(QsoRecord qso) => qso.LotwSyncStatus is
        LotwSyncStatus.LocalOnly or LotwSyncStatus.Queued or LotwSyncStatus.Modified or LotwSyncStatus.Failed;

    private static IEnumerable<QsoRecord> FindMatches(QsoRecord confirmation, IEnumerable<QsoRecord> localQsos)
    {
        if (confirmation.UtcTimestamp is null)
        {
            return [];
        }

        var confirmationTime = confirmation.UtcTimestamp.ToDateTimeOffset();
        return localQsos.Where(local =>
            local.UtcTimestamp is not null
            && string.Equals(local.WorkedCallsign, confirmation.WorkedCallsign, StringComparison.OrdinalIgnoreCase)
            && StationCallMatches(local, confirmation)
            && local.Band == confirmation.Band
            && local.Mode == confirmation.Mode
            && Math.Abs((local.UtcTimestamp.ToDateTimeOffset() - confirmationTime).TotalMinutes) <= MatchToleranceMinutes);
    }

    private static bool StationCallMatches(QsoRecord local, QsoRecord confirmation)
    {
        confirmation.ExtraFields.TryGetValue("OPERATOR", out var reportOperator);
        var reportCall = FirstValue(confirmation.StationCallsign, confirmation.OwnerCallsign, reportOperator);
        return reportCall is not null
            && (string.Equals(local.StationCallsign, reportCall, StringComparison.OrdinalIgnoreCase)
            || string.Equals(local.OwnerCallsign, reportCall, StringComparison.OrdinalIgnoreCase)
            || local.ExtraFields.TryGetValue("OPERATOR", out var localOperator)
                && string.Equals(localOperator, reportCall, StringComparison.OrdinalIgnoreCase));
    }

    private static string? FirstValue(params string[] values) =>
        values.FirstOrDefault(static value => !string.IsNullOrWhiteSpace(value))?.Trim();

    private static void CopyMissingConfirmationFields(QsoRecord target, QsoRecord source)
    {
        if (string.IsNullOrWhiteSpace(target.WorkedGrid) && !string.IsNullOrWhiteSpace(source.WorkedGrid))
        {
            target.WorkedGrid = source.WorkedGrid;
        }
        if (target.WorkedDxcc == 0 && source.WorkedDxcc != 0)
        {
            target.WorkedDxcc = source.WorkedDxcc;
        }
        if (string.IsNullOrWhiteSpace(target.WorkedCountry) && !string.IsNullOrWhiteSpace(source.WorkedCountry))
        {
            target.WorkedCountry = source.WorkedCountry;
        }
        if (string.IsNullOrWhiteSpace(target.WorkedState) && !string.IsNullOrWhiteSpace(source.WorkedState))
        {
            target.WorkedState = source.WorkedState;
        }
        if (string.IsNullOrWhiteSpace(target.WorkedCounty) && !string.IsNullOrWhiteSpace(source.WorkedCounty))
        {
            target.WorkedCounty = source.WorkedCounty;
        }
        if (target.WorkedCqZone == 0 && source.WorkedCqZone != 0)
        {
            target.WorkedCqZone = source.WorkedCqZone;
        }
        if (target.WorkedItuZone == 0 && source.WorkedItuZone != 0)
        {
            target.WorkedItuZone = source.WorkedItuZone;
        }
    }
}

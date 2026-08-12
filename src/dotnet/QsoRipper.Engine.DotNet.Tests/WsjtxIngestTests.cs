using System.Buffers.Binary;
using System.Text;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;
using QsoRipper.Engine.DotNet;
using QsoRipper.Engine.DotNet.Wsjtx;
using QsoRipper.Engine.QrzLogbook;
using QsoRipper.Engine.Storage.Memory;
using QsoRipper.Services;

namespace QsoRipper.Engine.DotNet.Tests;

#pragma warning disable CA1707 // Remove underscores from member names - xUnit allows underscores in test methods
public sealed class WsjtxIngestTests : IDisposable
{
    private const uint WsjtxMagic = 0xADBC_CBDA;
    private const string SampleAdif =
        "<CALL:4>W1AW<STATION_CALLSIGN:5>K7RND<QSO_DATE:8>20250102<TIME_ON:4>0102<QSO_DATE_OFF:8>20250102<TIME_OFF:6>010300<BAND:3>15M<MODE:3>FT8<FREQ:8>21.02830<NOTES:13>Operator note<EOR>\n";

    private readonly string _tempDirectory;

    public WsjtxIngestTests()
    {
        _tempDirectory = Path.Combine(
            Path.GetTempPath(),
            "qsoripper-wsjtx-tests",
            Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_tempDirectory);
    }

    public void Dispose()
    {
        try
        {
            Directory.Delete(_tempDirectory, recursive: true);
        }
        catch (IOException)
        {
            // Best-effort cleanup.
        }
    }

    // ---- Datagram parser ------------------------------------------------

    [Fact]
    public void Datagram_parse_extracts_logged_adif_payload()
    {
        var datagram = BuildLoggedAdifDatagram("WSJT-X", SampleAdif);

        var result = WsjtxDatagram.TryParseLoggedAdif(datagram);

        Assert.Equal(WsjtxDatagramParseStatus.Logged, result.Status);
        Assert.NotNull(result.Adif);
        Assert.Equal(SampleAdif, Encoding.UTF8.GetString(result.Adif!));
    }

    [Fact]
    public void Datagram_parse_ignores_non_logged_message_types()
    {
        // Message type 1 (Status) is not Logged ADIF and must be ignored, not errored.
        var datagram = BuildDatagram(messageType: 1, id: "WSJT-X", adif: SampleAdif);

        var result = WsjtxDatagram.TryParseLoggedAdif(datagram);

        Assert.Equal(WsjtxDatagramParseStatus.Ignored, result.Status);
        Assert.Null(result.Adif);
    }

    [Fact]
    public void Datagram_parse_ignores_non_magic_datagrams()
    {
        var datagram = Encoding.UTF8.GetBytes("{\"adif\":\"whatever\"}");

        var result = WsjtxDatagram.TryParseLoggedAdif(datagram);

        Assert.Equal(WsjtxDatagramParseStatus.Ignored, result.Status);
    }

    [Fact]
    public void Datagram_parse_reports_malformed_when_string_extends_past_packet()
    {
        var datagram = new byte[16];
        BinaryPrimitives.WriteUInt32BigEndian(datagram.AsSpan(0), WsjtxMagic);
        BinaryPrimitives.WriteUInt32BigEndian(datagram.AsSpan(4), 2); // schema
        BinaryPrimitives.WriteUInt32BigEndian(datagram.AsSpan(8), 12); // Logged ADIF
        BinaryPrimitives.WriteUInt32BigEndian(datagram.AsSpan(12), 50); // id length far exceeds buffer

        var result = WsjtxDatagram.TryParseLoggedAdif(datagram);

        Assert.Equal(WsjtxDatagramParseStatus.Malformed, result.Status);
        Assert.NotNull(result.Error);
    }

    [Fact]
    public void Datagram_parse_reports_malformed_for_empty_adif_payload()
    {
        var datagram = BuildLoggedAdifDatagram("WSJT-X", "   ");

        var result = WsjtxDatagram.TryParseLoggedAdif(datagram);

        Assert.Equal(WsjtxDatagramParseStatus.Malformed, result.Status);
    }

    // ---- ADIF tail prefix length ----------------------------------------

    [Fact]
    public void Complete_prefix_returns_length_through_last_eor()
    {
        var bytes = Encoding.UTF8.GetBytes(SampleAdif);

        var length = WsjtxAdifTail.CompleteAdifPrefixLength(bytes);

        // The complete prefix ends just after "<EOR>" but before the trailing newline.
        var expected = SampleAdif.IndexOf("<EOR>", StringComparison.Ordinal) + "<EOR>".Length;
        Assert.Equal(expected, length);
    }

    [Fact]
    public void Complete_prefix_withholds_incomplete_trailing_record()
    {
        // First record is complete; the second is missing its <EOR> terminator.
        var text = SampleAdif + "<CALL:4>K1AB<STATION_CALLSIGN:5>K7RND<QSO_DATE:8>20250102<TIME_ON:4>0103<BAND:3>15M<MODE:2>CW";
        var bytes = Encoding.UTF8.GetBytes(text);

        var length = WsjtxAdifTail.CompleteAdifPrefixLength(bytes);

        var expected = SampleAdif.IndexOf("<EOR>", StringComparison.Ordinal) + "<EOR>".Length;
        Assert.Equal(expected, length);
    }

    [Fact]
    public void Complete_prefix_ignores_literal_eor_inside_field_value()
    {
        // A comment field whose value literally contains "<EOR>" must not terminate the record early.
        // Value "see <EOR>" is exactly 9 characters, so the field length consumes the literal tag.
        const string record = "<CALL:4>W1AW<STATION_CALLSIGN:5>K7RND<COMMENT:9>see <EOR><QSO_DATE:8>20250102<TIME_ON:4>0102<BAND:3>15M<MODE:2>CW<EOR>";
        var bytes = Encoding.UTF8.GetBytes(record);

        var length = WsjtxAdifTail.CompleteAdifPrefixLength(bytes);

        Assert.Equal(record.Length, length);

        // And there is exactly one real record terminator: the prefix ends at the final <EOR>.
        var lastEor = record.LastIndexOf("<EOR>", StringComparison.Ordinal) + "<EOR>".Length;
        Assert.Equal(lastEor, length);
    }

    [Fact]
    public void Complete_prefix_returns_null_when_no_record_terminator()
    {
        var bytes = Encoding.UTF8.GetBytes("<CALL:4>W1AW<STATION_CALLSIGN:5>K7RND");

        var length = WsjtxAdifTail.CompleteAdifPrefixLength(bytes);

        Assert.Null(length);
    }

    // ---- ImportAdifDetailed affected ids --------------------------------

    [Fact]
    public void Import_adif_detailed_reports_inserted_and_refreshed_ids()
    {
        var state = CreateStateWithProfile();

        var inserted = state.ImportAdifDetailed(Encoding.UTF8.GetBytes(SampleAdif), refresh: false);
        Assert.Equal(1u, inserted.Response.RecordsImported);
        var affectedId = Assert.Single(inserted.AffectedQsos);
        Assert.Equal("W1AW", affectedId.WorkedCallsign);
        Assert.False(string.IsNullOrWhiteSpace(affectedId.LocalId));

        // Re-importing the same record with refresh should refresh and report the same local id.
        var refreshed = state.ImportAdifDetailed(Encoding.UTF8.GetBytes(SampleAdif), refresh: true);
        Assert.Equal(1u, refreshed.Response.RecordsUpdated);
        var refreshedId = Assert.Single(refreshed.AffectedQsos);
        Assert.Equal(affectedId.LocalId, refreshedId.LocalId);
    }

    // ---- Supervisor end-to-end (driven imports, no real sockets) --------

    [Fact]
    public void Import_diagnostic_includes_qso_end_delta_milliseconds()
    {
        var qso = new QsoRecord
        {
            UtcEndTimestamp = Timestamp.FromDateTimeOffset(
                new DateTimeOffset(2026, 8, 8, 2, 51, 30, 250, TimeSpan.Zero)),
        };
        var importedAt = new DateTimeOffset(2026, 8, 8, 2, 51, 34, 75, TimeSpan.Zero);

        var note = WsjtxImportDiagnostic.Create(WsjtxImportDiagnostic.UdpSource, qso, importedAt);

        Assert.Contains("qso_end_to_import_ms=3825", note, StringComparison.Ordinal);
    }

    [Fact]
    public void Setup_status_wsjtx_ingest_status_is_null_before_supervisor_publishes()
    {
        var state = CreateStateWithProfile();
        using var supervisor = new WsjtxIngestSupervisor(state);

        var status = state.GetSetupStatus();

        Assert.Null(status.WsjtxIngestStatus);
    }

    [Fact]
    public async Task Process_logged_datagram_imports_and_publishes_live_status()
    {
        var state = CreateStateWithProfile();
        using var supervisor = new WsjtxIngestSupervisor(state);
        var datagram = BuildLoggedAdifDatagram("WSJT-X", SampleAdif);

        await supervisor.ProcessDatagramAsync(datagram, syncToQrz: false, CancellationToken.None);

        var live = supervisor.SnapshotStatus();
        Assert.Equal(1u, live.RecordsImported);
        Assert.Equal("W1AW", live.LastImportedCallsign);
        Assert.False(string.IsNullOrWhiteSpace(live.LastImportedLocalId));
        Assert.NotNull(live.LastEventAt);
        var stored = state.GetQso(live.LastImportedLocalId);
        Assert.NotNull(stored);
        AssertWsjtxDiagnostic(stored!, WsjtxImportDiagnostic.UdpSource, "dotnet");

        var setupStatus = state.GetSetupStatus();
        Assert.NotNull(setupStatus.WsjtxIngestStatus);
        Assert.Equal(1u, setupStatus.WsjtxIngestStatus.RecordsImported);
        Assert.Equal("W1AW", setupStatus.WsjtxIngestStatus.LastImportedCallsign);
    }

    [Fact]
    public async Task Process_ignored_datagram_counts_as_skip()
    {
        var state = CreateStateWithProfile();
        using var supervisor = new WsjtxIngestSupervisor(state);
        var datagram = BuildDatagram(messageType: 1, id: "WSJT-X", adif: SampleAdif);

        await supervisor.ProcessDatagramAsync(datagram, syncToQrz: false, CancellationToken.None);

        var live = supervisor.SnapshotStatus();
        Assert.Equal(0u, live.RecordsImported);
        Assert.Equal(1u, live.RecordsSkipped);
        Assert.Equal(0u, live.ParseErrors);
    }

    [Fact]
    public async Task Process_malformed_datagram_increments_parse_errors()
    {
        var state = CreateStateWithProfile();
        using var supervisor = new WsjtxIngestSupervisor(state);
        var datagram = BuildLoggedAdifDatagram("WSJT-X", "   ");

        await supervisor.ProcessDatagramAsync(datagram, syncToQrz: false, CancellationToken.None);

        var live = supervisor.SnapshotStatus();
        Assert.Equal(0u, live.RecordsImported);
        Assert.Equal(1u, live.ParseErrors);
        Assert.False(string.IsNullOrWhiteSpace(live.LastError));
    }

    [Fact]
    public async Task Tail_import_advances_cursor_and_withholds_incomplete_records()
    {
        var state = CreateStateWithProfile();
        using var supervisor = new WsjtxIngestSupervisor(state);
        var path = Path.Combine(_tempDirectory, "wsjtx_log.adi");

        // One complete record plus an incomplete trailing record.
        var incompleteTail = "<CALL:4>K1AB<STATION_CALLSIGN:5>K7RND<QSO_DATE:8>20250102<TIME_ON:4>0103<BAND:3>15M<MODE:2>CW";
        await File.WriteAllTextAsync(path, SampleAdif + incompleteTail);

        await supervisor.HandleTailImportAsync(path, syncToQrz: false, CancellationToken.None);
        var afterFirst = supervisor.SnapshotStatus();
        Assert.Equal(1u, afterFirst.RecordsImported);
        var firstStored = state.GetQso(afterFirst.LastImportedLocalId);
        Assert.NotNull(firstStored);
        AssertWsjtxDiagnostic(firstStored!, WsjtxImportDiagnostic.AdifTailSource, "dotnet");

        // A second poll without new complete data must not re-import.
        await supervisor.HandleTailImportAsync(path, syncToQrz: false, CancellationToken.None);
        Assert.Equal(1u, supervisor.SnapshotStatus().RecordsImported);

        // Completing the second record makes it importable on the next poll.
        await File.WriteAllTextAsync(
            path,
            SampleAdif + "<CALL:4>K1AB<STATION_CALLSIGN:5>K7RND<QSO_DATE:8>20250102<TIME_ON:4>0103<BAND:3>15M<MODE:2>CW<EOR>\n");

        await supervisor.HandleTailImportAsync(path, syncToQrz: false, CancellationToken.None);
        Assert.Equal(2u, supervisor.SnapshotStatus().RecordsImported);
    }

    [Fact]
    public async Task First_wsjtx_ingest_source_remains_authoritative()
    {
        var state = CreateStateWithProfile();
        using var supervisor = new WsjtxIngestSupervisor(state);
        var path = Path.Combine(_tempDirectory, "wsjtx_log.adi");
        await File.WriteAllTextAsync(path, SampleAdif);

        await supervisor.HandleTailImportAsync(path, syncToQrz: false, CancellationToken.None);
        await supervisor.ProcessDatagramAsync(
            BuildLoggedAdifDatagram("WSJT-X", SampleAdif),
            syncToQrz: false,
            CancellationToken.None);

        var localId = supervisor.SnapshotStatus().LastImportedLocalId;
        var stored = state.GetQso(localId);
        Assert.NotNull(stored);
        AssertWsjtxDiagnostic(stored!, WsjtxImportDiagnostic.AdifTailSource, "dotnet");
        Assert.DoesNotContain(WsjtxImportDiagnostic.UdpSource, stored!.Notes, StringComparison.Ordinal);
    }

    [Fact]
    public async Task Tail_import_reports_read_error_as_parse_error()
    {
        var state = CreateStateWithProfile();
        using var supervisor = new WsjtxIngestSupervisor(state);
        var missingPath = Path.Combine(_tempDirectory, "does-not-exist.adi");

        await supervisor.HandleTailImportAsync(missingPath, syncToQrz: false, CancellationToken.None);

        var live = supervisor.SnapshotStatus();
        Assert.Equal(1u, live.ParseErrors);
        Assert.False(string.IsNullOrWhiteSpace(live.LastError));
    }

    [Fact]
    public async Task Qrz_sync_marks_success_when_api_key_configured()
    {
        var state = CreateStateWithProfile(apiKey: "qrz-api-key");
        using var supervisor = new WsjtxIngestSupervisor(state);
        var detail = state.ImportAdifDetailed(Encoding.UTF8.GetBytes(SampleAdif), refresh: false);
        var localId = Assert.Single(detail.AffectedQsos).LocalId;

        await supervisor.RunQrzSyncAsync(new[] { localId });

        var live = supervisor.SnapshotStatus();
        Assert.True(live.LastQrzSyncSuccess);
        Assert.False(live.HasLastQrzSyncError);
        Assert.Equal("W1AW", live.LastImportedCallsign);
        Assert.Equal(localId, live.LastImportedLocalId);

        var stored = state.GetQso(localId);
        Assert.NotNull(stored);
        Assert.Equal(SyncStatus.Synced, stored!.SyncStatus);
    }

    [Fact]
    public async Task Qrz_sync_marks_failure_when_api_key_missing()
    {
        var state = CreateStateWithProfile();
        using var supervisor = new WsjtxIngestSupervisor(state);
        var detail = state.ImportAdifDetailed(Encoding.UTF8.GetBytes(SampleAdif), refresh: false);
        var localId = Assert.Single(detail.AffectedQsos).LocalId;

        await supervisor.RunQrzSyncAsync(new[] { localId });

        var live = supervisor.SnapshotStatus();
        Assert.False(live.LastQrzSyncSuccess);
        Assert.Equal("QRZ logbook is not configured.", live.LastQrzSyncError);

        var stored = state.GetQso(localId);
        Assert.NotNull(stored);
        Assert.Equal(SyncStatus.LocalOnly, stored!.SyncStatus);
    }

    // ---- Helpers --------------------------------------------------------

    private ManagedEngineState CreateStateWithProfile(string? apiKey = null)
    {
        var storage = new MemoryStorage();
        var syncEngine = apiKey is null ? null : new QrzSyncEngine(new SuccessfulQrzLogbookApi());
        var state = new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            storage,
            lookupCoordinator: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: syncEngine);
        var request = new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        };

        if (apiKey is not null)
        {
            request.QrzLogbookApiKey = apiKey;
        }

        state.SaveSetup(request);
        return state;
    }

    private static void AssertWsjtxDiagnostic(QsoRecord qso, string source, string engine)
    {
        Assert.True(qso.ExtraFields.TryGetValue(WsjtxImportDiagnostic.ExtraFieldKey, out var diagnostic));
        Assert.StartsWith(WsjtxImportDiagnostic.NotePrefix, diagnostic, StringComparison.Ordinal);
        Assert.Contains("imported_at_utc=", diagnostic, StringComparison.Ordinal);
        Assert.Contains("qso_end_to_import_ms=", diagnostic, StringComparison.Ordinal);
        Assert.DoesNotContain("qso_end_to_import_ms=unavailable", diagnostic, StringComparison.Ordinal);
        Assert.Contains($"source={source}", diagnostic, StringComparison.Ordinal);
        Assert.Contains($"engine={engine}", diagnostic, StringComparison.Ordinal);
        Assert.Contains(diagnostic, qso.Notes, StringComparison.Ordinal);
        Assert.Contains("Operator note", qso.Notes, StringComparison.Ordinal);
    }

    private sealed class SuccessfulQrzLogbookApi : IQrzLogbookApi
    {
        public Task<List<QsoRecord>> FetchQsosAsync(string? sinceDateYmd) => Task.FromResult(new List<QsoRecord>());

        public Task<string> UploadQsoAsync(QsoRecord qso, string? bookOwner = null) => Task.FromResult("WSJTX-1");

        public Task<string> UploadQsoWithReplaceAsync(QsoRecord qso, string? bookOwner = null) => Task.FromResult("WSJTX-1");

        public Task<string> UpdateQsoAsync(QsoRecord qso, string? bookOwner = null) => Task.FromResult("WSJTX-1");

        public Task<QrzLogbookStatus> GetStatusAsync() => Task.FromResult(new QrzLogbookStatus("K7RND", 0));

        public Task DeleteQsoAsync(string logid) => Task.CompletedTask;
    }

    private static byte[] BuildLoggedAdifDatagram(string id, string adif)
    {
        return BuildDatagram(messageType: 12, id, adif);
    }

    private static byte[] BuildDatagram(uint messageType, string id, string adif)
    {
        using var stream = new MemoryStream();
        WriteBeU32(stream, WsjtxMagic);
        WriteBeU32(stream, 2); // schema
        WriteBeU32(stream, messageType);
        WriteQString(stream, id);
        WriteQString(stream, adif);
        return stream.ToArray();
    }

    private static void WriteBeU32(Stream stream, uint value)
    {
        Span<byte> buffer = stackalloc byte[4];
        BinaryPrimitives.WriteUInt32BigEndian(buffer, value);
        stream.Write(buffer);
    }

    private static void WriteQString(Stream stream, string value)
    {
        var bytes = Encoding.UTF8.GetBytes(value);
        WriteBeU32(stream, (uint)bytes.Length);
        stream.Write(bytes);
    }
}

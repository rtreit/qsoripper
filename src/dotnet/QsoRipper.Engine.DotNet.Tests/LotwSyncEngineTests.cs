using System.Globalization;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;
using QsoRipper.Engine.DotNet.Lotw;
using QsoRipper.Engine.Storage.Memory;

namespace QsoRipper.Engine.DotNet.Tests;

public sealed class LotwSyncEngineTests
{
    [Fact]
    public void ManagedAdifPreservesFalseAndNonBooleanLotwValues()
    {
        var payload = "<CALL:4>W1AW<QSO_DATE:8>20260808<TIME_ON:6>031000<BAND:3>20M<MODE:3>FT8"
            + "<LOTW_QSL_SENT:1>N<LOTW_QSL_RCVD:1>R<EOR>";

        var qso = Assert.Single(ManagedAdifCodec.ParseAdiQsos(System.Text.Encoding.ASCII.GetBytes(payload)));

        Assert.True(qso.HasLotwSent);
        Assert.False(qso.LotwSent);
        Assert.Equal(QslStatus.No, qso.LotwSentStatus);
        Assert.Equal(QslStatus.Requested, qso.LotwReceivedStatus);
        var exported = System.Text.Encoding.ASCII.GetString(ManagedAdifCodec.SerializeAdiQsos([qso], false));
        Assert.Contains("<LOTW_QSL_SENT:1>N", exported, StringComparison.Ordinal);
        Assert.Contains("<LOTW_QSL_RCVD:1>R", exported, StringComparison.Ordinal);
    }

    [Fact]
    public async Task SyncAsyncUploadsAndAppliesConfirmationWithoutReplacingNotes()
    {
        var storage = new MemoryStorage();
        var local = CreateQso("local-1", "KC7AVA", "N7XAK", ParseUtc("2026-08-08T02:50:00Z"));
        local.Notes = "Keep this operator note.";
        await storage.Logbook.InsertQsoAsync(local);

        var fixture = await File.ReadAllBytesAsync(Path.Combine(AppContext.BaseDirectory, "lotw_confirmations.adi"));
        var confirmation = Assert.Single(ManagedAdifCodec.ParseAdiQsos(fixture));
        var api = new FakeLotwApi(new LotwReport([confirmation], "2026-08-09 12:34:56"));
        var engine = new LotwSyncEngine(api, storage.Logbook);

        var result = await engine.SyncAsync(fullSync: false, upload: true, download: true, CancellationToken.None);

        Assert.Equal(1, result.UploadedRecords);
        Assert.Equal(1, result.ConfirmedRecords);
        Assert.Equal("2026-08-09 12:34:56", result.ConfirmationHighWater);
        var saved = await storage.Logbook.GetQsoAsync("local-1");
        Assert.NotNull(saved);
        Assert.Equal(LotwSyncStatus.Confirmed, saved.LotwSyncStatus);
        Assert.Equal(QslStatus.Yes, saved.LotwSentStatus);
        Assert.Equal(QslStatus.Yes, saved.LotwReceivedStatus);
        Assert.Equal("DM34", saved.WorkedGrid);
        Assert.Equal("Keep this operator note.", saved.Notes);
        var metadata = await storage.Logbook.GetSyncMetadataAsync();
        Assert.Equal("2026-08-09 12:34:56", metadata.LotwLastQsl);
    }

    [Fact]
    public async Task SyncAsyncMarksAmbiguousMatchesAsConflict()
    {
        var storage = new MemoryStorage();
        var time = ParseUtc("2026-08-08T02:50:00Z");
        await storage.Logbook.InsertQsoAsync(CreateQso("local-1", "KC7AVA", "N7XAK", time));
        await storage.Logbook.InsertQsoAsync(CreateQso("local-2", "KC7AVA", "N7XAK", time.AddMinutes(2)));
        var report = new LotwReport([CreateQso("report", "KC7AVA", "N7XAK", time.AddMinutes(1))], null);
        var engine = new LotwSyncEngine(new FakeLotwApi(report), storage.Logbook);

        var result = await engine.SyncAsync(fullSync: true, upload: false, download: true, CancellationToken.None);

        Assert.Equal(1, result.ConflictRecords);
        Assert.Equal(LotwSyncStatus.Conflict, (await storage.Logbook.GetQsoAsync("local-1"))!.LotwSyncStatus);
        Assert.Equal(LotwSyncStatus.Conflict, (await storage.Logbook.GetQsoAsync("local-2"))!.LotwSyncStatus);
    }

    [Fact]
    public async Task IncrementalSyncUsesPersistedHighWater()
    {
        var storage = new MemoryStorage();
        await storage.Logbook.UpsertSyncMetadataAsync(new QsoRipper.Engine.Storage.SyncMetadata
        {
            LotwLastQsl = "2026-08-01 00:00:00",
        });
        var api = new FakeLotwApi(new LotwReport([], null));
        var engine = new LotwSyncEngine(api, storage.Logbook);

        await engine.SyncAsync(fullSync: false, upload: false, download: true, CancellationToken.None);

        Assert.Equal("2026-08-01 00:00:00", api.LastSince);
    }

    [Fact]
    public void SharedConfigPersistsLotwSettingsWithoutPasswords()
    {
        var configPath = Path.Combine(Path.GetTempPath(), $"qsoripper-lotw-{Guid.NewGuid():N}.toml");
        try
        {
            var config = SharedPersistedSetupConfig.CreateDefault();
            config.Lotw = new ManagedLotwSettings
            {
                Username = "KC7AVA",
                Password = "website-secret",
                TqslPath = "tqsl",
                StationLocation = "Home",
                CertificatePassword = "certificate-secret",
                TimeoutSeconds = 45,
            };

            SharedSetupConfigPersistence.Save(configPath, config);

            var content = File.ReadAllText(configPath);
            Assert.Contains("[lotw]", content, StringComparison.Ordinal);
            Assert.Contains("station_location = \"Home\"", content, StringComparison.Ordinal);
            Assert.DoesNotContain("website-secret", content, StringComparison.Ordinal);
            Assert.DoesNotContain("certificate-secret", content, StringComparison.Ordinal);
            var loaded = SharedSetupConfigPersistence.Load(configPath).Config.Lotw;
            Assert.NotNull(loaded);
            Assert.Equal("KC7AVA", loaded.Username);
            Assert.Equal((uint)45, loaded.TimeoutSeconds);
        }
        finally
        {
            File.Delete(configPath);
        }
    }

    private static QsoRecord CreateQso(
        string localId,
        string stationCallsign,
        string workedCallsign,
        DateTimeOffset timestamp) => new()
        {
            LocalId = localId,
            StationCallsign = stationCallsign,
            WorkedCallsign = workedCallsign,
            UtcTimestamp = Timestamp.FromDateTimeOffset(timestamp),
            CreatedAt = Timestamp.FromDateTimeOffset(timestamp),
            UpdatedAt = Timestamp.FromDateTimeOffset(timestamp),
            Band = Band._20M,
            Mode = Mode.Ft8,
            LotwSyncStatus = LotwSyncStatus.LocalOnly,
        };

    private static DateTimeOffset ParseUtc(string value) =>
        DateTimeOffset.Parse(value, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal);

    private sealed class FakeLotwApi(LotwReport report) : ILotwApi
    {
        public string? LastSince { get; private set; }

        public Task<int> UploadQsosAsync(IReadOnlyList<QsoRecord> qsos, CancellationToken cancellationToken) =>
            Task.FromResult(qsos.Count);

        public Task<LotwReport> FetchConfirmationsAsync(string? since, CancellationToken cancellationToken)
        {
            LastSince = since;
            return Task.FromResult(report);
        }
    }
}

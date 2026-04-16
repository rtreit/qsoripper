using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;
using QsoRipper.Engine.DotNet;
using QsoRipper.EngineSelection;
using QsoRipper.Services;

namespace QsoRipper.Engine.DotNet.Tests;

#pragma warning disable CA1707 // Remove underscores from member names - xUnit allows underscores in test methods
public sealed class ManagedEngineStateTests : IDisposable
{
    private readonly string _tempDirectory;

    public ManagedEngineStateTests()
    {
        _tempDirectory = Path.Combine(
            Path.GetTempPath(),
            "qsoripper-managed-engine-tests",
            Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_tempDirectory);
    }

    [Fact]
    public void Build_engine_info_reports_managed_engine_identity()
    {
        var info = ManagedEngineState.BuildEngineInfo();

        Assert.Equal(EngineCatalog.GetEngineId(EngineImplementation.DotNet), info.EngineId);
        Assert.Equal(EngineCatalog.GetDisplayName(EngineImplementation.DotNet), info.DisplayName);
        Assert.Contains("logbook-crud", info.Capabilities);
    }

    [Fact]
    public void Save_setup_persists_active_profile_and_redacted_runtime_values()
    {
        var state = CreateState();

        var response = state.SaveSetup(new SaveSetupRequest
        {
            LogFilePath = Path.Combine(_tempDirectory, "portable-log.db"),
            QrzXmlUsername = "k7rnd",
            QrzXmlPassword = "secret",
            QrzLogbookApiKey = "api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var runtime = state.GetRuntimeConfigSnapshot();
        var profiles = state.ListStationProfiles();

        Assert.True(response.Status.SetupComplete);
        Assert.True(response.Status.ConfigFileExists);
        Assert.Equal(Path.Combine(_tempDirectory, "portable-log.db"), response.Status.LogFilePath);
        Assert.Equal("K7RND", response.Status.StationProfile.StationCallsign);
        Assert.Single(profiles.Profiles);
        Assert.NotEmpty(profiles.ActiveProfileId);
        Assert.Contains(response.Status.Warnings, warning => warning.Contains("in-memory logbook", StringComparison.Ordinal));

        var storageValue = runtime.Values.Single(value => value.Key == "QSORIPPER_STORAGE_BACKEND");
        var sqlitePathValue = runtime.Values.Single(value => value.Key == "QSORIPPER_SQLITE_PATH");
        var passwordValue = runtime.Values.Single(value => value.Key == "QSORIPPER_QRZ_XML_PASSWORD");
        Assert.Equal("memory", storageValue.DisplayValue);
        Assert.Equal(Path.Combine(_tempDirectory, "portable-log.db"), sqlitePathValue.DisplayValue);
        Assert.Equal("***", passwordValue.DisplayValue);
        Assert.True(passwordValue.Secret);
        Assert.True(passwordValue.Redacted);
    }

    [Fact]
    public void Log_qso_uses_active_station_context_and_sync_updates_status()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            LogFilePath = Path.Combine(_tempDirectory, "qsoripper-managed.db"),
            QrzLogbookApiKey = "api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var logged = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = false,
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Ft8,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-04-12T01:51:00Z", System.Globalization.CultureInfo.InvariantCulture))
            }
        });

        var stored = state.GetQso(logged.LocalId);
        var beforeSync = state.GetSyncStatus();
        var syncResult = state.SyncWithQrz();
        var afterSync = state.GetSyncStatus();

        Assert.NotNull(stored);
        Assert.Equal("K7RND", stored!.StationCallsign);
        Assert.Equal("K7RND", stored.StationSnapshot.StationCallsign);
        Assert.Equal("CN87", stored.StationSnapshot.Grid);
        Assert.Equal(1u, beforeSync.PendingUpload);
        Assert.Equal(1u, syncResult.UploadedRecords);
        Assert.True(syncResult.Complete);
        Assert.Equal(0u, afterSync.PendingUpload);
        Assert.Equal(1u, afterSync.QrzQsoCount);
        Assert.Equal("K7RND", afterSync.QrzLogbookOwner);
    }

    [Fact]
    public void Apply_runtime_config_rejects_non_memory_storage()
    {
        var state = CreateState();

        var exception = Assert.Throws<InvalidOperationException>(() => state.ApplyRuntimeConfig(
        [
            new RuntimeConfigMutation
            {
                Key = "QSORIPPER_STORAGE_BACKEND",
                Kind = RuntimeConfigMutationKind.Set,
                Value = "sqlite"
            }
        ]));

        Assert.Equal("The managed .NET engine currently supports only memory storage.", exception.Message);
    }

    public void Dispose()
    {
        if (Directory.Exists(_tempDirectory))
        {
            Directory.Delete(_tempDirectory, recursive: true);
        }
    }

    private ManagedEngineState CreateState()
    {
        return new ManagedEngineState(Path.Combine(_tempDirectory, "managed-engine.json"));
    }
}
#pragma warning restore CA1707

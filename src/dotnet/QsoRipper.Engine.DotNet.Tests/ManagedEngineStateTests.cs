using System.Net.Http;
using System.Reflection;
using System.Text;
using Google.Protobuf.WellKnownTypes;
using Grpc.Core;
using QsoRipper.Domain;
using QsoRipper.Engine.DotNet;
using QsoRipper.Engine.QrzLogbook;
using QsoRipper.Engine.RigControl;
using QsoRipper.Engine.Storage.Memory;
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

        Assert.Equal(EngineCatalog.DotNetProfile.EngineId, info.EngineId);
        Assert.Equal(EngineCatalog.DotNetProfile.DisplayName, info.DisplayName);
        Assert.Contains("engine-info", info.Capabilities);
        Assert.Contains("logbook", info.Capabilities);
        Assert.Contains("lookup-callsign", info.Capabilities);
        Assert.Contains("lookup-stream", info.Capabilities);
        Assert.Contains("lookup-cache", info.Capabilities);
        Assert.Contains("rig-control", info.Capabilities);
    }

    [Fact]
    public async Task Startup_repairs_legacy_qrz_logids_and_collapses_duplicates()
    {
        var storage = new MemoryStorage();
        var older = new QsoRecord
        {
            LocalId = "older",
            StationCallsign = "K7RND",
            WorkedCallsign = "W1AW",
            Band = Band._20M,
            Mode = Mode.Ft8,
            UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-01-01T00:00:00Z", System.Globalization.CultureInfo.InvariantCulture)),
            CreatedAt = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-01-01T00:00:00Z", System.Globalization.CultureInfo.InvariantCulture)),
        };
        older.ExtraFields["APP_QRZLOG_LOGID"] = "12345";
        var newer = older.Clone();
        newer.LocalId = "newer";
        newer.CreatedAt = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-01-02T00:00:00Z", System.Globalization.CultureInfo.InvariantCulture));
        newer.QrzLogid = "12345";
        newer.Notes = "preserve me";

        await storage.Logbook.InsertQsoAsync(older);
        await storage.Logbook.InsertQsoAsync(newer);

        _ = new ManagedEngineState(Path.Combine(_tempDirectory, "config.toml"), storage);

        var saved = Assert.Single(await storage.Logbook.ListQsosAsync(new QsoRipper.Engine.Storage.QsoListQuery
        {
            DeletedFilter = QsoRipper.Engine.Storage.DeletedRecordsFilter.All,
        }));
        Assert.Equal("older", saved.LocalId);
        Assert.Equal("12345", saved.QrzLogid);
        Assert.Equal("preserve me", saved.Notes);
        Assert.DoesNotContain("APP_QRZLOG_LOGID", saved.ExtraFields.Keys);
    }

    [Fact]
    public async Task Station_profile_service_validates_profiles_and_distinguishes_delete_failures()
    {
        var state = CreateState();
        var service = new ManagedStationProfileGrpcService(state);
        var context = new TestServerCallContext();

        var invalid = await Assert.ThrowsAsync<RpcException>(() => service.SaveStationProfile(
            new SaveStationProfileRequest { Profile = new StationProfile { StationCallsign = "K7RND" } },
            context));
        Assert.Equal(StatusCode.InvalidArgument, invalid.StatusCode);

        var missing = await Assert.ThrowsAsync<RpcException>(() => service.DeleteStationProfile(
            new DeleteStationProfileRequest { ProfileId = "missing" },
            context));
        Assert.Equal(StatusCode.NotFound, missing.StatusCode);

        var saved = await service.SaveStationProfile(
            new SaveStationProfileRequest
            {
                MakeActive = true,
                Profile = new StationProfile
                {
                    ProfileName = " Home ",
                    StationCallsign = " k7rnd ",
                },
            },
            context);
        Assert.Equal("Home", saved.Profile.Profile.ProfileName);
        Assert.Equal("K7RND", saved.Profile.Profile.StationCallsign);

        var active = await Assert.ThrowsAsync<RpcException>(() => service.DeleteStationProfile(
            new DeleteStationProfileRequest { ProfileId = saved.Profile.ProfileId },
            context));
        Assert.Equal(StatusCode.FailedPrecondition, active.StatusCode);
    }

    [Fact]
    public async Task Setup_service_rejects_unknown_wizard_steps()
    {
        var service = new ManagedSetupGrpcService(CreateState());

        var exception = await Assert.ThrowsAsync<RpcException>(() => service.ValidateSetupStep(
            new ValidateSetupStepRequest { Step = (SetupWizardStep)999 },
            new TestServerCallContext()));

        Assert.Equal(StatusCode.InvalidArgument, exception.StatusCode);
    }

    [Fact]
    public async Task Setup_credential_tests_use_the_live_test_adapter()
    {
        var tester = new FakeQrzCredentialTester();
        var state = new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            new MemoryStorage(),
            lookupCoordinator: null,
            contestCalendarMonitor: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: null,
            currentPersistenceLocation: null,
            loadedPersistedSetup: null,
            qrzCredentialTester: tester);
        var service = new ManagedSetupGrpcService(state);
        var context = new TestServerCallContext();

        var xml = await service.TestQrzCredentials(
            new TestQrzCredentialsRequest { QrzXmlUsername = "K7RND", QrzXmlPassword = "secret" },
            context);
        var logbook = await service.TestQrzLogbookCredentials(
            new TestQrzLogbookCredentialsRequest { ApiKey = "key" },
            context);

        Assert.True(xml.Success);
        Assert.True(logbook.Success);
        Assert.Equal("K7RND", logbook.LogbookOwner);
        Assert.Equal(42U, logbook.QsoCount);
        Assert.Equal(1, tester.XmlCalls);
        Assert.Equal(1, tester.LogbookCalls);
    }

    [Fact]
    public async Task Rig_connection_test_rejects_an_invalid_request_port()
    {
        var service = new ManagedRigControlGrpcService(CreateState());

        var exception = await Assert.ThrowsAsync<RpcException>(() => service.TestRigConnection(
            new TestRigConnectionRequest { Host = "example.invalid", Port = 70_000 },
            new TestServerCallContext()));

        Assert.Equal(StatusCode.InvalidArgument, exception.StatusCode);
    }

    [Fact]
    public async Task Services_reject_unknown_enum_filters_and_report_missing_delete()
    {
        var state = CreateState();
        var context = new TestServerCallContext();
        var logbook = new ManagedLogbookGrpcService(state);

        var missingDelete = await Assert.ThrowsAsync<RpcException>(() => logbook.DeleteQso(
            new DeleteQsoRequest { LocalId = "missing" },
            context));
        Assert.Equal(StatusCode.NotFound, missingDelete.StatusCode);

        var invalidList = await Assert.ThrowsAsync<RpcException>(() => logbook.ListQsos(
            new ListQsosRequest { BandFilter = (Band)999 },
            new TestServerStreamWriter<ListQsosResponse>(),
            context));
        Assert.Equal(StatusCode.InvalidArgument, invalidList.StatusCode);

        var contests = new ManagedContestCalendarGrpcService(state);
        var invalidContest = await Assert.ThrowsAsync<RpcException>(() => contests.GetActiveContests(
            new GetActiveContestsRequest { Band = (Band)999 },
            context));
        Assert.Equal(StatusCode.InvalidArgument, invalidContest.StatusCode);
    }

    [Fact]
    public async Task Sync_preflight_fails_before_streaming_when_qrz_is_unconfigured()
    {
        var writer = new TestServerStreamWriter<SyncWithQrzResponse>();
        var service = new ManagedLogbookGrpcService(CreateState());

        var exception = await Assert.ThrowsAsync<RpcException>(() => service.SyncWithQrz(
            new SyncWithQrzRequest(),
            writer,
            new TestServerCallContext()));

        Assert.Equal(StatusCode.FailedPrecondition, exception.StatusCode);
        Assert.Empty(writer.Messages);
    }

    [Fact]
    public async Task Sync_stream_emits_initial_and_terminal_lifecycle_messages()
    {
        var state = new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            new MemoryStorage(),
            lookupCoordinator: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: new QrzSyncEngine(new FakeQrzLogbookApi()));
        state.SaveSetup(new SaveSetupRequest { QrzLogbookApiKey = "api-key" });
        var writer = new TestServerStreamWriter<SyncWithQrzResponse>();
        var service = new ManagedLogbookGrpcService(state);

        await service.SyncWithQrz(
            new SyncWithQrzRequest(),
            writer,
            new TestServerCallContext());

        Assert.Equal(2, writer.Messages.Count);
        Assert.False(writer.Messages[0].Complete);
        Assert.True(writer.Messages[1].Complete);
    }

    [Fact]
    public void Save_setup_ignores_persistence_paths_and_redacts_runtime_values()
    {
        var state = CreateState();

        var response = state.SaveSetup(new SaveSetupRequest
        {
            LogFilePath = Path.Combine(_tempDirectory, "portable-log.db"),
            PersistenceValues =
            {
                new SetupFieldValue
                {
                    Key = "persistence.path",
                    Value = Path.Combine(_tempDirectory, "portable-from-contract.db")
                }
            },
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
        var persistedConfig = File.ReadAllText(Path.Combine(_tempDirectory, "config.toml"));

        Assert.True(response.Status.SetupComplete);
        Assert.True(response.Status.ConfigFileExists);
        Assert.True(response.Status.PersistenceContractExplicit);
        Assert.True(string.IsNullOrWhiteSpace(response.Status.LogFilePath));
        Assert.True(string.IsNullOrWhiteSpace(response.Status.SuggestedLogFilePath));
        Assert.Equal("K7RND", response.Status.StationProfile.StationCallsign);
        Assert.Single(profiles.Profiles);
        Assert.NotEmpty(profiles.ActiveProfileId);
        Assert.Contains(response.Status.Warnings, warning => warning.Contains("in-memory logbook", StringComparison.Ordinal));

        var passwordValue = runtime.Values.Single(value => value.Key == "QSORIPPER_QRZ_XML_PASSWORD");
        var rigDefinition = runtime.Definitions.Single(value => value.Key == "QSORIPPER_RIGCTLD_ENABLED");
        var rigValue = runtime.Values.Single(value => value.Key == "QSORIPPER_RIGCTLD_ENABLED");
        Assert.DoesNotContain(runtime.Definitions, value => value.Key == "QSORIPPER_STORAGE_BACKEND");
        Assert.DoesNotContain(runtime.Values, value => value.Key == "QSORIPPER_STORAGE_BACKEND");
        Assert.Equal("In-memory logbook", runtime.PersistenceSummary);
        Assert.True(string.IsNullOrWhiteSpace(runtime.PersistenceLocation));
        Assert.DoesNotContain(runtime.Values, value => value.Key == "QSORIPPER_SQLITE_PATH");
        Assert.Equal("***", passwordValue.DisplayValue);
        Assert.True(passwordValue.Secret);
        Assert.True(passwordValue.Redacted);
        Assert.Equal(RuntimeConfigValueSource.BaseConfig, passwordValue.Source);
        Assert.Equal("false", rigDefinition.DefaultValue);
        Assert.Equal(RuntimeConfigValueSource.Default, rigValue.Source);
        Assert.DoesNotContain("log_file_path", persistedConfig, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("secret", persistedConfig, StringComparison.Ordinal);
        Assert.DoesNotContain("api-key", persistedConfig, StringComparison.Ordinal);
    }

    [Fact]
    public void Save_setup_preserves_unknown_shared_config_tables()
    {
        // The unified config.toml is shared with the CAT hub daemon ([cat_hub]) and launcher
        // ([launcher]); an engine setup save must not clobber those sections.
        var configPath = Path.Combine(_tempDirectory, "config.toml");
        File.WriteAllText(
            configPath,
            """
            [cat_hub.radio]
            backend = "ts590"
            port = "COM4"

            [[cat_hub.serial_endpoint]]
            name = "n1mm"
            transport = "COM11"
            dialect = "ts590"

            [[cat_hub.hamlib_net]]
            name = "engine"
            bind = "127.0.0.1:4532"

            [launcher]
            engines = [1]
            """);

        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var persistedConfig = File.ReadAllText(configPath);
        Assert.Contains("[cat_hub.radio]", persistedConfig, StringComparison.Ordinal);
        Assert.Contains("port = \"COM4\"", persistedConfig, StringComparison.Ordinal);
        Assert.Contains("[[cat_hub.serial_endpoint]]", persistedConfig, StringComparison.Ordinal);
        Assert.Contains("[[cat_hub.hamlib_net]]", persistedConfig, StringComparison.Ordinal);
        Assert.Contains("[launcher]", persistedConfig, StringComparison.Ordinal);
        Assert.Contains("K7RND", persistedConfig, StringComparison.Ordinal);
    }

    [Fact]
    public void Cw_keying_toml_loads_and_survives_setup_save_verbatim()
    {
        var configPath = Path.Combine(_tempDirectory, "config.toml");
        File.WriteAllText(
            configPath,
            """
            [cw_keying]
            backend = "winkeyer"
            winkeyer_port = "COM3"
            winkeyer_baud = 1200
            cathub_endpoint = "http://127.0.0.1:50071"
            cathub_client_name = "dotnet-engine"
            speed_wpm = 20
            transmit_enabled = true
            max_tx_ms = 30000
            future_key = "preserve-me"
            """);

        var loaded = SharedSetupConfigPersistence.Load(configPath);
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
            }
        });
        var saved = File.ReadAllText(configPath);

        Assert.Equal("winkeyer", loaded.Config.CwKeying.Backend);
        Assert.Equal("COM3", loaded.Config.CwKeying.WinkeyerPort);
        Assert.Equal(1_200, loaded.Config.CwKeying.WinkeyerBaud);
        Assert.Equal("http://127.0.0.1:50071", loaded.Config.CwKeying.CathubEndpoint);
        Assert.Equal("dotnet-engine", loaded.Config.CwKeying.CathubClientName);
        Assert.Equal(20u, loaded.Config.CwKeying.SpeedWpm);
        Assert.True(loaded.Config.CwKeying.TransmitEnabled);
        Assert.Equal(30_000u, loaded.Config.CwKeying.MaxTxMs);
        Assert.Contains("future_key = \"preserve-me\"", saved, StringComparison.Ordinal);
    }

    [Fact]
    public void Log_qso_uses_active_station_context_and_sync_updates_status()
    {
        var state = CreateStateWithSync();
        state.SaveSetup(new SaveSetupRequest
        {
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
        Assert.True(string.IsNullOrEmpty(syncResult.Error), $"Sync error: [{syncResult.Error}]");
        Assert.Equal(1u, syncResult.UploadedRecords);
        Assert.True(syncResult.Complete);
        Assert.Equal(0u, afterSync.PendingUpload);
        // STATUS is fetched once before upload (issue #337 fix), so the count
        // it reports is the pre-upload count (0). The next sync cycle will
        // see the post-upload count.
        Assert.Equal(0u, afterSync.QrzQsoCount);
        Assert.Equal("K7RND", afterSync.QrzLogbookOwner);
    }

    [Fact]
    public void Per_operation_qrz_sync_uses_the_qrz_api_and_persists_its_logid()
    {
        var storage = new MemoryStorage();
        var fakeApi = new FakeQrzLogbookApi
        {
            UploadValidator = async qso =>
                await storage.GetQsoAsync(qso.LocalId).ConfigureAwait(false) is not null,
        };
        var state = new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            storage,
            lookupCoordinator: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: new QrzSyncEngine(fakeApi));
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });

        var response = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = true,
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Cw,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });

        var stored = state.GetQso(response.LocalId)!;
        Assert.Equal(1, fakeApi.InsertCalls);
        Assert.Equal("FAKE-1", response.QrzLogid);
        Assert.Equal("FAKE-1", stored.QrzLogid);
        Assert.Equal(SyncStatus.Synced, stored.SyncStatus);
        Assert.True(fakeApi.UploadValidationPassed);
    }

    [Fact]
    public void Per_operation_update_persists_local_edit_before_calling_qrz()
    {
        var storage = new MemoryStorage();
        var fakeApi = new FakeQrzLogbookApi();
        var state = new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            storage,
            lookupCoordinator: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: new QrzSyncEngine(fakeApi));
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
            },
        });
        var logged = state.LogQso(new LogQsoRequest
        {
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Cw,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
        var edit = state.GetQso(logged.LocalId)!;
        edit.Comment = "persist before remote";
        fakeApi.UploadValidator = async qso =>
            (await storage.GetQsoAsync(qso.LocalId).ConfigureAwait(false))?.Comment == "persist before remote";

        var response = state.UpdateQso(new UpdateQsoRequest { Qso = edit, SyncToQrz = true });

        Assert.True(response.SyncSuccess);
        Assert.True(fakeApi.UploadValidationPassed);
        Assert.Equal("persist before remote", state.GetQso(logged.LocalId)!.Comment);
    }

    [Fact]
    public void Update_without_qrz_configuration_preserves_local_only_status()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var logged = LogSampleQso(state, "W1AW");
        var edit = state.GetQso(logged.LocalId)!;
        edit.Comment = "offline edit";

        var response = state.UpdateQso(new UpdateQsoRequest { Qso = edit, SyncToQrz = true });

        Assert.False(response.SyncSuccess);
        Assert.True(response.HasSyncError);
        Assert.Equal(SyncStatus.LocalOnly, state.GetQso(logged.LocalId)!.SyncStatus);
    }

    [Fact]
    public void Save_setup_normalizes_unspecified_conflict_policy_to_flag_for_review()
    {
        var state = CreateState();

        var response = state.SaveSetup(new SaveSetupRequest
        {
            SyncConfig = new SyncConfig
            {
                AutoSyncEnabled = true,
                SyncIntervalSeconds = 300,
                ConflictPolicy = ConflictPolicy.Unspecified
            }
        });

        Assert.Equal(ConflictPolicy.FlagForReview, response.Status.SyncConfig.ConflictPolicy);
    }

    [Fact]
    public void Sync_with_qrz_unexpected_exception_does_not_include_stack_trace()
    {
        var storage = new MemoryStorage();
        var syncEngine = new QrzSyncEngine(new FakeMalformedQrzLogbookApi());
        var state = new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            storage,
            lookupCoordinator: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: syncEngine);

        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var response = state.SyncWithQrz();

        Assert.True(response.Complete);
        Assert.False(string.IsNullOrWhiteSpace(response.Error));
        Assert.DoesNotContain("\n", response.Error, StringComparison.Ordinal);
        Assert.DoesNotContain(" at ", response.Error, StringComparison.Ordinal);
    }

    [Fact]
    public void Apply_runtime_config_rejects_unadvertised_storage_key()
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

        Assert.Equal("Unsupported runtime config key: QSORIPPER_STORAGE_BACKEND", exception.Message);
    }

    [Fact]
    public void Runtime_overrides_and_session_override_do_not_persist_across_restart()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
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

        state.ApplyRuntimeConfig(
        [
            new RuntimeConfigMutation
            {
                Key = "QSORIPPER_QRZ_XML_USERNAME",
                Kind = RuntimeConfigMutationKind.Set,
                Value = "runtime-user"
            },
            new RuntimeConfigMutation
            {
                Key = "QSORIPPER_QRZ_XML_PASSWORD",
                Kind = RuntimeConfigMutationKind.Set,
                Value = "runtime-secret"
            },
            new RuntimeConfigMutation
            {
                Key = "QSORIPPER_QRZ_LOGBOOK_API_KEY",
                Kind = RuntimeConfigMutationKind.Clear
            }
        ]);

        state.SetSessionStationProfileOverride(new StationProfile
        {
            ProfileName = "Field Day",
            StationCallsign = "W7FD",
            OperatorCallsign = "W7FD",
            Grid = "CN85"
        });

        var restarted = CreateState();
        var status = restarted.GetSetupStatus();
        var runtime = restarted.GetRuntimeConfigSnapshot();
        var context = restarted.GetActiveStationContext();

        Assert.Equal("k7rnd", status.QrzXmlUsername);
        Assert.False(status.HasQrzXmlPassword);
        Assert.False(status.HasQrzLogbookApiKey);
        Assert.False(context.HasSessionOverride);
        Assert.Equal("K7RND", context.EffectiveActiveProfile.StationCallsign);
        Assert.Equal(
            "k7rnd",
            runtime.Values.Single(value => value.Key == "QSORIPPER_QRZ_XML_USERNAME").DisplayValue);
        Assert.False(
            runtime.Values.Single(value => value.Key == "QSORIPPER_QRZ_LOGBOOK_API_KEY").HasValue);
    }

    [Fact]
    public void Save_setup_rebuilds_owned_sync_client_without_leaking_previous_http_client()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "api-key"
        });

        var originalSyncEngine = GetRequiredPrivateField<QrzSyncEngine>(state, "_syncEngine");
        var originalHttpClient = GetOwnedSyncHttpClient(originalSyncEngine);
        Assert.False(IsHttpClientDisposed(originalHttpClient));

        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "replacement-key"
        });

        Assert.True(IsHttpClientDisposed(originalHttpClient));
    }

    [Fact]
    public void Migrates_legacy_json_config_to_shared_toml()
    {
        var legacyPath = Path.Combine(_tempDirectory, "dotnet-engine.json");
        var configPath = Path.Combine(_tempDirectory, "config.toml");
        File.WriteAllText(
            legacyPath,
            """
            {
              "qrzXmlUsername": "k7rnd",
              "qrzXmlPassword": "secret",
              "hasQrzXmlPassword": true,
              "activeProfileId": "home",
              "stationProfiles": [
                {
                  "profileId": "home",
                  "profileJson": "{ \"profileName\": \"Home\", \"stationCallsign\": \"K7RND\", \"operatorCallsign\": \"K7RND\", \"grid\": \"CN87\" }"
                }
              ],
              "runtimeOverrides": {
                "QSORIPPER_QRZ_XML_USERNAME": "runtime-user"
              },
              "sessionOverrideProfileJson": "{ \"profileName\": \"Field\", \"stationCallsign\": \"W7FD\", \"operatorCallsign\": \"W7FD\", \"grid\": \"CN85\" }"
            }
            """);

        var state = CreateState();
        var status = state.GetSetupStatus();
        var context = state.GetActiveStationContext();
        var persistedConfig = File.ReadAllText(configPath);

        Assert.True(File.Exists(configPath));
        Assert.Contains("active_profile_id = \"home\"", persistedConfig, StringComparison.Ordinal);
        Assert.Contains("station_callsign = \"K7RND\"", persistedConfig, StringComparison.Ordinal);
        Assert.DoesNotContain("runtimeOverrides", persistedConfig, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("sessionOverrideProfileJson", persistedConfig, StringComparison.OrdinalIgnoreCase);
        Assert.DoesNotContain("secret", persistedConfig, StringComparison.Ordinal);
        Assert.Equal("k7rnd", status.QrzXmlUsername);
        Assert.True(status.HasQrzXmlPassword);
        Assert.Equal("K7RND", status.StationProfile.StationCallsign);
        Assert.False(context.HasSessionOverride);
    }

    [Fact]
    public async Task Delete_qso_grpc_success_omits_optional_error_fields()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
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
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-04-16T22:48:00Z", System.Globalization.CultureInfo.InvariantCulture))
            }
        });

        var service = new ManagedLogbookGrpcService(state);
        var response = await service.DeleteQso(
            new DeleteQsoRequest
            {
                LocalId = logged.LocalId,
                DeleteFromQrz = false
            },
            null!);

        Assert.True(response.Success);
        Assert.True(string.IsNullOrEmpty(response.Error));
        Assert.True(string.IsNullOrEmpty(response.QrzDeleteError));
    }

    [Fact]
    public void Test_rig_connection_connected_omits_error_message()
    {
        var state = CreateStateWithRigSnapshot(new RigSnapshot
        {
            FrequencyHz = 14_074_000,
            Band = Band._20M,
            Mode = Mode.Ft8
        });

        var response = state.TestRigConnection(new TestRigConnectionRequest());

        Assert.True(response.Success);
        Assert.True(string.IsNullOrEmpty(response.ErrorMessage));
        Assert.NotNull(response.Snapshot);
        Assert.Equal(14_074_000UL, response.Snapshot.FrequencyHz);
        Assert.Equal(RigConnectionStatus.Connected, response.Snapshot.Status);
    }

    [Fact]
    public void Build_rig_snapshot_connected_omits_error_message_without_monitor()
    {
        var state = CreateStateWithRigSnapshot(new RigSnapshot
        {
            Status = RigConnectionStatus.Connected,
            FrequencyHz = 14_074_000,
            Band = Band._20M,
            Mode = Mode.Ft8,
            SampledAt = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
        });

        var snapshot = state.BuildRigSnapshot();

        Assert.Equal(RigConnectionStatus.Connected, snapshot.Status);
        Assert.False(snapshot.HasErrorMessage);
        Assert.Equal(14_074_000UL, snapshot.FrequencyHz);
    }

    [Fact]
    public void Log_qso_requires_timestamp_band_and_mode()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var exception = Assert.Throws<InvalidOperationException>(() => state.LogQso(new LogQsoRequest
        {
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW"
            }
        }));

        Assert.Equal("utc_timestamp is required.", exception.Message);
    }

    [Fact]
    public void Import_adif_applies_active_profile_and_skips_duplicates()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var payload = Utf8("<CALL:4>W1AW\n<QSO_DATE:8>20260115\n<TIME_ON:4>1523\n<BAND:3>20M\n<MODE:4>RTTY\n<EOR>\n");

        var first = state.ImportAdif(payload, refresh: false);
        var second = state.ImportAdif(payload, refresh: false);
        var stored = state.ListQsos(new ListQsosRequest()).Single();

        Assert.Equal(1u, first.RecordsImported);
        Assert.Contains(first.Warnings, warning => warning.Contains("applied active station profile 'Home'.", StringComparison.Ordinal));
        Assert.Equal(1u, second.RecordsSkipped);
        Assert.Contains(second.Warnings, warning => warning.Contains("duplicate skipped", StringComparison.Ordinal));
        Assert.Equal("K7RND", stored.StationCallsign);
        Assert.Equal("CN87", stored.StationSnapshot.Grid);
    }

    [Fact]
    public void Import_adif_treats_empty_rst_fields_as_absent()
    {
        var state = CreateState();
        var payload = Utf8(
            "<CALL:4>W1AW<STATION_CALLSIGN:5>K7RND<QSO_DATE:8>20260115<TIME_ON:4>1523<BAND:3>20M<MODE:2>CW" +
            "<RST_SENT:0><RST_RCVD:0><EOR>\n");

        var response = state.ImportAdif(payload, refresh: false);
        var stored = state.ListQsos(new ListQsosRequest()).Single();

        Assert.Equal(1u, response.RecordsImported);
        Assert.Null(stored.RstSent);
        Assert.Null(stored.RstReceived);
    }

    [Fact]
    public void Import_adif_skips_minute_precision_duplicate_with_small_frequency_drift()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        state.LogQso(new LogQsoRequest
        {
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._15M,
                Mode = Mode.Cw,
                FrequencyHz = 21_028_340,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2025-01-02T01:02:32Z", System.Globalization.CultureInfo.InvariantCulture)),
                WorkedCountry = "United States",
                WorkedGrid = "FN31pr"
            }
        });

        var response = state.ImportAdif(
            Utf8("<CALL:4>W1AW<STATION_CALLSIGN:5>K7RND<QSO_DATE:8>20250102<TIME_ON:4>0102<BAND:3>15M<MODE:2>CW<FREQ:8>21.02830<EOR>\n"),
            refresh: false);
        var stored = state.ListQsos(new ListQsosRequest()).Single();

        Assert.Equal(0u, response.RecordsImported);
        Assert.Equal(1u, response.RecordsSkipped);
        Assert.Contains(response.Warnings, warning => warning.Contains("duplicate skipped", StringComparison.Ordinal));
        Assert.Equal("United States", stored.WorkedCountry);
        Assert.Equal("FN31pr", stored.WorkedGrid);
    }

    [Fact]
    public void Import_adif_refresh_updates_existing_record_and_preserves_absent_fields()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
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
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Rtty,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-01-15T15:23:00Z", System.Globalization.CultureInfo.InvariantCulture)),
                Comment = "Keep comment",
                Notes = "Old notes"
            }
        });

        var response = state.ImportAdif(
            Utf8("<CALL:4>W1AW\n<QSO_DATE:8>20260115\n<TIME_ON:6>152300\n<BAND:3>20M\n<MODE:4>RTTY\n<NOTES:9>New notes\n<EOR>\n"),
            refresh: true);

        var stored = state.GetQso(logged.LocalId);

        Assert.NotNull(stored);
        Assert.Equal(1u, response.RecordsUpdated);
        Assert.Contains(response.Warnings, warning => warning.Contains("refreshed existing record", StringComparison.Ordinal));
        Assert.Equal("New notes", stored!.Notes);
        Assert.Equal("Keep comment", stored.Comment);
    }

    [Fact]
    public void Import_adif_skips_invalid_time_on_length_with_warning()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var response = state.ImportAdif(
            Utf8("<CALL:4>W1AW\n<QSO_DATE:8>20260115\n<TIME_ON:1>1\n<BAND:3>20M\n<MODE:4>RTTY\n<EOR>\n"),
            refresh: false);

        Assert.Equal(0u, response.RecordsImported);
        Assert.Equal(1u, response.RecordsSkipped);
        Assert.Contains(response.Warnings, warning => warning.Contains("invalid ADIF date/time '20260115/1'. Skipped.", StringComparison.Ordinal));
    }

    [Fact]
    public void Import_adif_skips_invalid_band_with_warning()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var response = state.ImportAdif(
            Utf8("<CALL:4>W1AW\n<QSO_DATE:8>20260115\n<TIME_ON:4>1523\n<BAND:5>BOGUS\n<MODE:4>RTTY\n<EOR>\n"),
            refresh: false);

        Assert.Equal(0u, response.RecordsImported);
        Assert.Equal(1u, response.RecordsSkipped);
        Assert.Contains(response.Warnings, warning => warning.Contains("unrecognized ADIF band 'BOGUS'. Skipped.", StringComparison.Ordinal));
    }

    [Fact]
    public void Export_adif_filters_by_contest_and_orders_oldest_first()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        state.LogQso(new LogQsoRequest
        {
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1NEW",
                Band = Band._20M,
                Mode = Mode.Ft8,
                ContestId = "WWDX",
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-01-16T01:00:00Z", System.Globalization.CultureInfo.InvariantCulture))
            }
        });
        state.LogQso(new LogQsoRequest
        {
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1OLD",
                Band = Band._20M,
                Mode = Mode.Ft8,
                ContestId = "WWDX",
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-01-15T01:00:00Z", System.Globalization.CultureInfo.InvariantCulture))
            }
        });
        state.LogQso(new LogQsoRequest
        {
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1OFF",
                Band = Band._20M,
                Mode = Mode.Ft8,
                ContestId = "STATEQP",
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-01-14T01:00:00Z", System.Globalization.CultureInfo.InvariantCulture))
            }
        });

        var payload = state.ExportAdif(new ExportAdifRequest
        {
            ContestId = "WWDX",
            IncludeHeader = true
        });
        var text = Encoding.UTF8.GetString(payload);

        Assert.Contains("<ADIF_VER:5>3.1.7", text, StringComparison.Ordinal);
        Assert.Contains("<PROGRAMID:9>QsoRipper", text, StringComparison.Ordinal);
        Assert.DoesNotContain("W1OFF", text, StringComparison.Ordinal);
        Assert.True(text.IndexOf("W1OLD", StringComparison.Ordinal) < text.IndexOf("W1NEW", StringComparison.Ordinal));
    }

    [Fact]
    public void Adif_round_trips_normalized_split_and_geo_fields()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var payload = Utf8(
            "<STATION_CALLSIGN:5>K7RND<CALL:4>W1AW<QSO_DATE:8>20260115<TIME_ON:4>1523" +
            "<BAND:3>20M<MODE:3>SSB<BAND_RX:3>40M<FREQ_RX:5>7.075" +
            "<LAT:11>N041 30.000<LON:11>W071 45.500<ALTITUDE:3>150" +
            "<GRIDSQUARE_EXT:2>ab<OWNER_CALLSIGN:4>W1AW<QSO_COMPLETE:1>Y" +
            "<APP_QSORIPPER_RX_WPM:2>28" +
            "<MY_ALTITUDE:3>550<MY_GRIDSQUARE_EXT:2>bb<EOR>\n");

        var imported = state.ImportAdif(payload, refresh: false);
        Assert.Equal(1u, imported.RecordsImported);

        var stored = state.ListQsos(new ListQsosRequest()).Single();

        Assert.Equal(Band._40M, stored.BandRx);
        Assert.Equal(7_075_000ul, stored.FrequencyRxHz);
        Assert.Equal(150.0, stored.WorkedAltitudeMeters);
        Assert.Equal("ab", stored.WorkedGridsquareExt);
        Assert.Equal("W1AW", stored.OwnerCallsign);
        Assert.Equal(QsoCompletion.Yes, stored.QsoComplete);
        Assert.True(stored.HasCwDecodeRxWpm);
        Assert.Equal(28u, stored.CwDecodeRxWpm);
        Assert.True(stored.HasWorkedLatitude);
        Assert.True(stored.HasWorkedLongitude);
        Assert.NotNull(stored.StationSnapshot);
        Assert.Equal(550.0, stored.StationSnapshot.AltitudeMeters);
        Assert.Equal("bb", stored.StationSnapshot.GridsquareExt);

        var exported = Encoding.UTF8.GetString(state.ExportAdif(new ExportAdifRequest()));

        Assert.Contains("<BAND_RX:3>40M", exported, StringComparison.Ordinal);
        Assert.Contains("<FREQ_RX:5>7.075", exported, StringComparison.Ordinal);
        Assert.Contains("<ALTITUDE:3>150", exported, StringComparison.Ordinal);
        Assert.Contains("<GRIDSQUARE_EXT:2>ab", exported, StringComparison.Ordinal);
        Assert.Contains("<OWNER_CALLSIGN:4>W1AW", exported, StringComparison.Ordinal);
        Assert.Contains("<QSO_COMPLETE:1>Y", exported, StringComparison.Ordinal);
        Assert.Contains("<APP_QSORIPPER_RX_WPM:2>28", exported, StringComparison.Ordinal);
        Assert.Contains("<MY_ALTITUDE:3>550", exported, StringComparison.Ordinal);
        Assert.Contains("<MY_GRIDSQUARE_EXT:2>bb", exported, StringComparison.Ordinal);
        Assert.Contains("<LAT:11>N041 30.000", exported, StringComparison.Ordinal);
        Assert.Contains("<LON:11>W071 45.500", exported, StringComparison.Ordinal);

        // Each new field should be emitted exactly once (no duplicates from extra_fields).
        AssertSingleAdifField(exported, "BAND_RX");
        AssertSingleAdifField(exported, "FREQ_RX");
        AssertSingleAdifField(exported, "LAT");
        AssertSingleAdifField(exported, "LON");
        AssertSingleAdifField(exported, "ALTITUDE");
        AssertSingleAdifField(exported, "GRIDSQUARE_EXT");
        AssertSingleAdifField(exported, "OWNER_CALLSIGN");
        AssertSingleAdifField(exported, "QSO_COMPLETE");
        AssertSingleAdifField(exported, "APP_QSORIPPER_RX_WPM");
        AssertSingleAdifField(exported, "MY_ALTITUDE");
        AssertSingleAdifField(exported, "MY_GRIDSQUARE_EXT");
    }

    private static void AssertSingleAdifField(string adif, string key)
    {
        var matches = System.Text.RegularExpressions.Regex.Matches(adif, $"<{key}:", System.Text.RegularExpressions.RegexOptions.IgnoreCase);
        Assert.True(matches.Count == 1, $"Expected exactly one <{key}:...> tag, found {matches.Count}");
    }

    [Fact]
    public void Update_qso_replaces_caller_owned_fields_and_can_clear_values()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
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
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-06-01T12:00:00Z", System.Globalization.CultureInfo.InvariantCulture)),
                RstSent = new RstReport { Raw = "59" },
                RstReceived = new RstReport { Raw = "57" },
                Notes = "Initial notes",
                FrequencyHz = 14_074_000,
            }
        });

        // Update is a full replacement of caller-owned fields. Engine-owned
        // identity, QRZ linkage, timestamps, and station snapshot are preserved.
        var updateResponse = state.UpdateQso(new UpdateQsoRequest
        {
            SyncToQrz = false,
            Qso = new QsoRecord
            {
                LocalId = logged.LocalId,
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Ft8,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2026-06-01T12:00:00Z", System.Globalization.CultureInfo.InvariantCulture)),
                Comment = "Updated comment",
                WorkedLatitude = 47.5,
                WorkedLongitude = -122.3,
                FrequencyRxHz = 14_075_000,
                OwnerCallsign = "K7RND",
                QsoComplete = QsoCompletion.Yes,
                CwDecodeRxWpm = 24,
                CwDecodeTranscript = "CQ TEST",
            }
        });

        var stored = state.GetQso(logged.LocalId);

        Assert.True(updateResponse.Success);
        Assert.NotNull(stored);
        Assert.Equal("Updated comment", stored!.Comment);
        Assert.Null(stored.RstSent);
        Assert.Null(stored.RstReceived);
        Assert.False(stored.HasNotes);
        Assert.False(stored.HasFrequencyHz);
        Assert.Equal(47.5, stored.WorkedLatitude);
        Assert.Equal(-122.3, stored.WorkedLongitude);
        Assert.Equal(14_075_000UL, stored.FrequencyRxHz);
        Assert.Equal("K7RND", stored.OwnerCallsign);
        Assert.Equal(QsoCompletion.Yes, stored.QsoComplete);
        Assert.Equal(24u, stored.CwDecodeRxWpm);
        Assert.Equal("CQ TEST", stored.CwDecodeTranscript);
    }

    [Fact]
    public void Log_qso_replaces_caller_owned_identity_and_sync_metadata()
    {
        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "N7OP",
                Grid = "CN87",
            },
        });
        var callerCreatedAt = Timestamp.FromDateTimeOffset(DateTimeOffset.Parse("2000-01-01T00:00:00Z", System.Globalization.CultureInfo.InvariantCulture));

        var response = state.LogQso(new LogQsoRequest
        {
            Qso = new QsoRecord
            {
                LocalId = "caller-owned-id",
                StationCallsign = "N0FAKE",
                WorkedCallsign = " w1aw ",
                Band = Band._20M,
                Mode = Mode.Cw,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
                CreatedAt = callerCreatedAt,
                SyncStatus = SyncStatus.Synced,
                QrzLogid = "caller-logid",
                QrzBookid = "caller-bookid",
            },
        });

        var stored = state.GetQso(response.LocalId)!;
        Assert.NotEqual("caller-owned-id", stored.LocalId);
        Assert.Equal("W1AW", stored.WorkedCallsign);
        Assert.Equal("K7RND", stored.StationCallsign);
        Assert.Equal("K7RND", stored.StationSnapshot!.StationCallsign);
        Assert.Equal("N7OP", stored.StationSnapshot.OperatorCallsign);
        Assert.NotEqual(callerCreatedAt, stored.CreatedAt);
        Assert.Equal(SyncStatus.LocalOnly, stored.SyncStatus);
        Assert.False(stored.HasQrzLogid);
        Assert.False(stored.HasQrzBookid);
    }

    [Fact]
    public async Task Import_adif_grpc_converts_post_await_validation_errors_to_invalid_argument()
    {
        var state = CreateState();
        var service = new ManagedLogbookGrpcService(state);
        var stream = new TestAsyncStreamReader<ImportAdifRequest>([
            new ImportAdifRequest()
        ]);

        var ex = await Assert.ThrowsAsync<RpcException>(
            () => service.ImportAdif(stream, new TestServerCallContext()));

        Assert.Equal(StatusCode.InvalidArgument, ex.StatusCode);
        Assert.Equal("chunk is required.", ex.Status.Detail);
    }

    public void Dispose()
    {
        if (Directory.Exists(_tempDirectory))
        {
            Directory.Delete(_tempDirectory, recursive: true);
        }
    }

    [Fact]
    public void Soft_delete_marks_row_with_tombstone_and_keeps_it_retrievable()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var loggedResp = LogSampleQso(state, "W1AW");
        var logged = state.GetQso(loggedResp.LocalId)!;

        var outcome = state.DeleteQso(logged.LocalId, queueRemoteDelete: false);

        Assert.True(outcome.Found);
        Assert.False(outcome.RemoteDeleteQueued);

        var fetched = state.GetQso(logged.LocalId);
        Assert.NotNull(fetched);
        Assert.NotNull(fetched!.DeletedAt);
        Assert.False(fetched.PendingRemoteDelete);
    }

    [Fact]
    public void Soft_delete_with_qrz_logid_queues_remote_delete()
    {
        var state = CreateStateWithSync();
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "test-api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });
        var loggedResp = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = true,
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Ft8,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
        Assert.False(string.IsNullOrEmpty(loggedResp.QrzLogid));
        var logged = state.GetQso(loggedResp.LocalId)!;

        var outcome = state.DeleteQso(logged.LocalId, queueRemoteDelete: true);

        Assert.True(outcome.Found);
        Assert.True(outcome.RemoteDeleteQueued);
        Assert.False(outcome.MissingQrzLogid);

        var fetched = state.GetQso(logged.LocalId);
        Assert.NotNull(fetched!.DeletedAt);
        Assert.True(fetched.PendingRemoteDelete);
    }

    [Fact]
    public void Soft_delete_without_qrz_logid_reports_missing_logid_when_remote_requested()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var loggedResp = LogSampleQso(state, "W1AW");
        var logged = state.GetQso(loggedResp.LocalId)!;

        var outcome = state.DeleteQso(logged.LocalId, queueRemoteDelete: true);

        Assert.True(outcome.Found);
        Assert.False(outcome.RemoteDeleteQueued);
        Assert.True(outcome.MissingQrzLogid);
    }

    [Fact]
    public void Update_on_soft_deleted_row_throws_QsoSoftDeletedException()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var loggedResp = LogSampleQso(state, "W1AW");
        var logged = state.GetQso(loggedResp.LocalId)!;
        state.DeleteQso(logged.LocalId, queueRemoteDelete: false);

        Assert.Throws<QsoSoftDeletedException>(() => state.UpdateQso(new UpdateQsoRequest
        {
            Qso = new QsoRecord(logged) { Notes = "should not apply" },
        }));
    }

    [Fact]
    public void Update_qso_flips_synced_to_modified_so_next_sync_uses_replace()
    {
        // Regression for the .NET parity of the Rust bug fixed in this change:
        // editing a previously-synced QSO must leave the row marked Modified
        // (with its qrz_logid intact) so the next bulk sync issues REPLACE
        // instead of stranding the local correction. See
        // docs/architecture/engine-specification.md §UpdateQso step 4.
        var state = CreateStateWithSync();
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "test-api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });
        var loggedResp = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = true,
            Qso = new QsoRecord
            {
                WorkedCallsign = "WG0Y",
                Band = Band._20M,
                Mode = Mode.Cw,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
        var logged = state.GetQso(loggedResp.LocalId)!;
        Assert.Equal(SyncStatus.Synced, logged.SyncStatus);
        var originalLogid = logged.QrzLogid;
        Assert.False(string.IsNullOrEmpty(originalLogid));

        // Operator edits a field without re-syncing.
        var edit = new QsoRecord(logged) { Notes = "Corrected state to CO" };
        state.UpdateQso(new UpdateQsoRequest { Qso = edit, SyncToQrz = false });

        var reloaded = state.GetQso(loggedResp.LocalId)!;
        Assert.Equal(SyncStatus.Modified, reloaded.SyncStatus);
        Assert.Equal(originalLogid, reloaded.QrzLogid);
    }

    [Fact]
    public void Update_qso_trims_local_id_before_lookup()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var loggedResp = LogSampleQso(state, "W1AW");
        var logged = state.GetQso(loggedResp.LocalId)!;

        var updateResponse = state.UpdateQso(new UpdateQsoRequest
        {
            SyncToQrz = false,
            Qso = new QsoRecord(logged)
            {
                LocalId = $"  {logged.LocalId}  ",
                Comment = "Trimmed local id update",
            },
        });

        Assert.True(updateResponse.Success);
        var stored = state.GetQso(logged.LocalId);
        Assert.NotNull(stored);
        Assert.Equal("Trimmed local id update", stored!.Comment);
    }

    [Fact]
    public void Update_qso_does_not_fall_back_to_qrz_logid_when_local_id_misses()
    {
        var state = CreateStateWithSync();
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "test-api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });
        var loggedResp = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = true,
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Cw,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
        var logged = state.GetQso(loggedResp.LocalId)!;
        Assert.False(string.IsNullOrWhiteSpace(logged.QrzLogid));

        var exception = Assert.Throws<KeyNotFoundException>(() => state.UpdateQso(new UpdateQsoRequest
        {
            SyncToQrz = false,
            Qso = new QsoRecord(logged)
            {
                LocalId = "missing-local-id",
                Comment = "Must not recover via QRZ logid",
            },
        }));

        Assert.Contains("missing-local-id", exception.Message, StringComparison.Ordinal);
        var stored = state.GetQso(logged.LocalId);
        Assert.NotNull(stored);
        Assert.NotEqual("Must not recover via QRZ logid", stored!.Comment);
    }

    [Fact]
    public void Restore_clears_tombstone_and_pending_flag()
    {
        var state = CreateStateWithSync();
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "test-api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });
        var loggedResp = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = true,
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Ft8,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
        var logged = state.GetQso(loggedResp.LocalId)!;
        var originalLogid = logged.QrzLogid;
        Assert.False(string.IsNullOrEmpty(originalLogid));
        state.DeleteQso(logged.LocalId, queueRemoteDelete: true);

        var outcome = state.RestoreQso(logged.LocalId);

        Assert.True(outcome.Found);
        Assert.NotNull(outcome.Restored);
        Assert.Null(outcome.Restored!.DeletedAt);
        Assert.False(outcome.Restored.PendingRemoteDelete);
        Assert.Equal(originalLogid, outcome.Restored.QrzLogid);
        Assert.Equal(SyncStatus.Synced, outcome.Restored.SyncStatus);
    }

    [Fact]
    public void Restore_unknown_local_id_returns_not_found()
    {
        var state = CreateState();

        var outcome = state.RestoreQso("does-not-exist");

        Assert.False(outcome.Found);
        Assert.Null(outcome.Restored);
    }

    [Fact]
    public void List_qsos_excludes_soft_deleted_by_default()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var keepResp = LogSampleQso(state, "W1AW");
        var keep = state.GetQso(keepResp.LocalId)!;
        var trashResp = LogSampleQso(state, "K7RND");
        var trash = state.GetQso(trashResp.LocalId)!;
        state.DeleteQso(trash.LocalId, queueRemoteDelete: false);

        var active = state.ListQsos(new ListQsosRequest());

        Assert.Contains(active, q => q.LocalId == keep.LocalId);
        Assert.DoesNotContain(active, q => q.LocalId == trash.LocalId);
    }

    [Fact]
    public void List_qsos_with_deleted_only_filter_returns_trash()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var keepResp = LogSampleQso(state, "W1AW");
        var keep = state.GetQso(keepResp.LocalId)!;
        var trashResp = LogSampleQso(state, "K7RND");
        var trash = state.GetQso(trashResp.LocalId)!;
        state.DeleteQso(trash.LocalId, queueRemoteDelete: false);

        var deleted = state.ListQsos(new ListQsosRequest { DeletedFilter = DeletedRecordsFilter.DeletedOnly });

        Assert.DoesNotContain(deleted, q => q.LocalId == keep.LocalId);
        Assert.Contains(deleted, q => q.LocalId == trash.LocalId);
    }

    [Fact]
    public async Task Restore_qso_grpc_returns_restored_record()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var loggedResp = LogSampleQso(state, "W1AW");
        var logged = state.GetQso(loggedResp.LocalId)!;
        state.DeleteQso(logged.LocalId, queueRemoteDelete: false);

        var service = new ManagedLogbookGrpcService(state);
        var response = await service.RestoreQso(
            new RestoreQsoRequest { LocalId = logged.LocalId },
            null!);

        Assert.True(response.Success);
        Assert.NotNull(response.Restored);
        Assert.Null(response.Restored!.DeletedAt);
    }

    [Fact]
    public async Task Restore_qso_grpc_throws_not_found_for_unknown_id()
    {
        var state = CreateState();
        var service = new ManagedLogbookGrpcService(state);

        var ex = await Assert.ThrowsAsync<RpcException>(() => service.RestoreQso(
            new RestoreQsoRequest { LocalId = "missing-id" },
            null!));
        Assert.Equal(StatusCode.NotFound, ex.StatusCode);
    }

    [Fact]
    public async Task Update_qso_grpc_on_soft_deleted_row_returns_failed_precondition()
    {
        var state = CreateState();
        EnsureStationConfigured(state);
        var loggedResp = LogSampleQso(state, "W1AW");
        var logged = state.GetQso(loggedResp.LocalId)!;
        state.DeleteQso(logged.LocalId, queueRemoteDelete: false);

        var service = new ManagedLogbookGrpcService(state);
        var ex = await Assert.ThrowsAsync<RpcException>(() => service.UpdateQso(
            new UpdateQsoRequest
            {
                Qso = new QsoRecord(logged) { Notes = "blocked" },
            },
            null!));
        Assert.Equal(StatusCode.FailedPrecondition, ex.StatusCode);
    }

    private static LogQsoResponse LogSampleQso(ManagedEngineState state, string callsign)
    {
        return state.LogQso(new LogQsoRequest
        {
            SyncToQrz = false,
            Qso = new QsoRecord
            {
                WorkedCallsign = callsign,
                Band = Band._20M,
                Mode = Mode.Ft8,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
    }

    [Fact]
    public void SyncWithQrz_populates_remote_deletes_pushed_counter()
    {
        var state = CreateStateWithSync();
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });
        var loggedResp = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = false,
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Ft8,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
        state.SyncWithQrz();
        var synced = state.GetQso(loggedResp.LocalId)!;
        Assert.False(string.IsNullOrEmpty(synced.QrzLogid));

        state.DeleteQso(synced.LocalId, queueRemoteDelete: true);
        var secondSync = state.SyncWithQrz();

        Assert.True(secondSync.Complete);
        Assert.Equal(1u, secondSync.RemoteDeletesPushed);
        Assert.Equal(0u, secondSync.DeletesSkippedRemote);
    }

    [Fact]
    public async Task Purge_deleted_qsos_pushes_remote_delete_before_local_hard_delete()
    {
        var storage = new MemoryStorage();
        var fakeApi = new FakeQrzLogbookApi();
        var state = new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            storage,
            lookupCoordinator: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: new QrzSyncEngine(fakeApi));
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });
        var logged = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = true,
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Cw,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
        state.DeleteQso(logged.LocalId, queueRemoteDelete: true);

        var response = await new ManagedLogbookGrpcService(state).PurgeDeletedQsos(
            new PurgeDeletedQsosRequest
            {
                Confirm = true,
                IncludePendingRemoteDeletes = true,
                LocalIds = { logged.LocalId },
            },
            null!);

        Assert.Equal(1u, response.RemoteDeletesPushed);
        Assert.Equal(0u, response.RemoteDeletesFailed);
        Assert.Equal(1u, response.PurgedCount);
        Assert.Equal(1, fakeApi.DeleteCalls);
        Assert.Null(state.GetQso(logged.LocalId));
    }

    [Fact]
    public async Task Purge_deleted_qsos_preserves_local_row_when_remote_delete_fails()
    {
        var storage = new MemoryStorage();
        var fakeApi = new FakeQrzLogbookApi { FailDeletes = true };
        var state = new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            storage,
            lookupCoordinator: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: new QrzSyncEngine(fakeApi));
        state.SaveSetup(new SaveSetupRequest
        {
            QrzLogbookApiKey = "api-key",
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });
        var logged = state.LogQso(new LogQsoRequest
        {
            SyncToQrz = true,
            Qso = new QsoRecord
            {
                WorkedCallsign = "W1AW",
                Band = Band._20M,
                Mode = Mode.Cw,
                UtcTimestamp = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow),
            },
        });
        state.DeleteQso(logged.LocalId, queueRemoteDelete: true);

        var response = await new ManagedLogbookGrpcService(state).PurgeDeletedQsos(
            new PurgeDeletedQsosRequest
            {
                Confirm = true,
                IncludePendingRemoteDeletes = true,
                LocalIds = { logged.LocalId },
            },
            null!);

        Assert.Equal(0u, response.RemoteDeletesPushed);
        Assert.Equal(1u, response.RemoteDeletesFailed);
        Assert.Equal(0u, response.PurgedCount);
        Assert.NotNull(state.GetQso(logged.LocalId));
    }

    private static void EnsureStationConfigured(ManagedEngineState state)
    {
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });
    }

    [Fact]
    public void Save_setup_writes_cat_hub_section_and_round_trips()
    {
        var state = CreateState();

        state.SaveSetup(new SaveSetupRequest
        {
            CatHub = BuildValidCatHub(),
        });

        var configPath = Path.Combine(_tempDirectory, "config.toml");
        var content = File.ReadAllText(configPath);
        Assert.Contains("[cat_hub.radio]", content, StringComparison.Ordinal);
        Assert.Contains("[[cat_hub.serial_endpoint]]", content, StringComparison.Ordinal);
        Assert.Contains("[[cat_hub.hamlib_net]]", content, StringComparison.Ordinal);
        Assert.Contains("[cat_hub.winkeyer]", content, StringComparison.Ordinal);
        Assert.Contains("[[cat_hub.winkeyer_endpoint]]", content, StringComparison.Ordinal);

        // Re-load from disk to confirm the lenient projection round-trips the written values.
        var reloaded = CreateState();
        var status = reloaded.GetSetupStatus();
        Assert.NotNull(status.CatHub);
        Assert.Equal("ts590", status.CatHub.Radio.Backend);
        Assert.Equal(9600u, status.CatHub.Radio.Baud);
        Assert.Equal("serial", status.CatHub.Radio.Transport);
        var serialEndpoint = Assert.Single(status.CatHub.SerialEndpoints);
        Assert.Equal("HDSDR", serialEndpoint.Name);
        Assert.Equal("CNCB0", serialEndpoint.Transport);
        Assert.Equal("CNCA0", serialEndpoint.ApplicationTransport);
        Assert.Equal("ts590", serialEndpoint.Dialect);
        Assert.Contains(CatHubPermission.Read, serialEndpoint.Perms);
        Assert.Contains(CatHubPermission.Write, serialEndpoint.Perms);
        var hamlibNetEndpoint = Assert.Single(status.CatHub.HamlibNet);
        Assert.Equal("engine", hamlibNetEndpoint.Name);
        Assert.Equal("127.0.0.1:4532", hamlibNetEndpoint.Bind);
        Assert.NotNull(status.CatHub.Winkeyer);
        Assert.Equal("COM3", status.CatHub.Winkeyer.Port);
        Assert.Equal("127.0.0.1:50071", status.CatHub.Winkeyer.ApiBind);
        var winkeyerEndpoint = Assert.Single(status.CatHub.WinkeyerEndpoints);
        Assert.Equal("n1mm-cw", winkeyerEndpoint.Name);
        Assert.Equal("COM41", winkeyerEndpoint.ApplicationTransport);
        Assert.True(winkeyerEndpoint.Primary);
        Assert.Contains(WinkeyerEndpointPermission.Send, winkeyerEndpoint.Perms);
    }

    [Fact]
    public void Save_setup_reflects_cat_hub_in_status_immediately()
    {
        var state = CreateState();

        var response = state.SaveSetup(new SaveSetupRequest
        {
            CatHub = BuildValidCatHub(),
        });

        Assert.NotNull(response.Status.CatHub);
        Assert.Equal("ts590", response.Status.CatHub.Radio.Backend);
    }

    [Fact]
    public void Save_setup_writes_wsjtx_ingest_section_and_round_trips()
    {
        var state = CreateState();

        var response = state.SaveSetup(new SaveSetupRequest
        {
            WsjtxIngest = new WsjtxIngestSettings
            {
                Enabled = true,
                UdpBind = "127.0.0.1:2237",
                AdifTailEnabled = true,
                AdifTailPath = Path.Combine(_tempDirectory, "wsjtx_log.adi"),
                PollIntervalMs = 250,
                SyncToQrz = true,
            },
        });

        var configPath = Path.Combine(_tempDirectory, "config.toml");
        var content = File.ReadAllText(configPath);
        var reloaded = CreateState();
        var status = reloaded.GetSetupStatus();

        Assert.NotNull(response.Status.WsjtxIngest);
        Assert.Contains("[wsjtx_ingest]", content, StringComparison.Ordinal);
        Assert.NotNull(status.WsjtxIngest);
        Assert.True(status.WsjtxIngest.Enabled);
        Assert.True(status.WsjtxIngest.UdpEnabled);
        Assert.Equal("127.0.0.1:2237", status.WsjtxIngest.UdpBind);
        Assert.True(status.WsjtxIngest.AdifTailEnabled);
        Assert.Equal(Path.Combine(_tempDirectory, "wsjtx_log.adi"), status.WsjtxIngest.AdifTailPath);
        Assert.Equal(250u, status.WsjtxIngest.PollIntervalMs);
        Assert.True(status.WsjtxIngest.SyncToQrz);
    }

    [Fact]
    public void Loaded_wsjtx_ingest_section_applies_runtime_defaults()
    {
        var configPath = Path.Combine(_tempDirectory, "config.toml");
        File.WriteAllText(
            configPath,
            """
            [wsjtx_ingest]
            enabled = true
            """);

        var state = CreateState();
        var status = state.GetSetupStatus();

        Assert.NotNull(status.WsjtxIngest);
        Assert.True(status.WsjtxIngest.Enabled);
        Assert.True(status.WsjtxIngest.UdpEnabled);
        Assert.Equal("127.0.0.1:2237", status.WsjtxIngest.UdpBind);
        Assert.Equal(1000u, status.WsjtxIngest.PollIntervalMs);
        Assert.False(status.WsjtxIngest.HasAdifTailPath);
    }

    [Fact]
    public void Save_setup_without_wsjtx_ingest_preserves_existing_wsjtx_section()
    {
        var configPath = Path.Combine(_tempDirectory, "config.toml");
        File.WriteAllText(
            configPath,
            """
            [wsjtx_ingest]
            # keep operator comment
            enabled = true
            future_key = "preserve-me"
            """);
        var state = CreateState();

        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87"
            }
        });

        var content = File.ReadAllText(configPath);
        Assert.Contains("# keep operator comment", content, StringComparison.Ordinal);
        Assert.Contains("future_key = \"preserve-me\"", content, StringComparison.Ordinal);
    }

    [Fact]
    public void Save_setup_rejects_invalid_wsjtx_ingest()
    {
        var state = CreateState();

        var exception = Assert.Throws<InvalidOperationException>(() => state.SaveSetup(new SaveSetupRequest
        {
            WsjtxIngest = new WsjtxIngestSettings
            {
                Enabled = true,
                UdpEnabled = false,
                AdifTailEnabled = true,
            },
        }));

        Assert.Contains("WSJT-X ADIF tail path is required", exception.Message, StringComparison.Ordinal);
    }

    [Theory]
    [MemberData(nameof(InvalidCatHubCases))]
    public void Save_setup_rejects_invalid_cat_hub(CatHubSettings catHub, string expectedMessage)
    {
        var state = CreateState();

        var exception = Assert.Throws<InvalidOperationException>(() => state.SaveSetup(new SaveSetupRequest
        {
            CatHub = catHub,
        }));

        Assert.Equal(expectedMessage, exception.Message);
    }

    public static IEnumerable<object[]> InvalidCatHubCases()
    {
        // Missing radio message entirely.
        yield return new object[]
        {
            new CatHubSettings { HamlibNet = { ValidEndpoint() } },
            "CAT hub radio settings are required.",
        };

        // Missing backend.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Transport = "tcp", Host = "127.0.0.1", TcpPort = 4532 },
                HamlibNet = { ValidEndpoint() },
            },
            "CAT hub radio backend is required.",
        };

        // Unsupported backend.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "flex", Transport = "tcp", Host = "127.0.0.1", TcpPort = 4532 },
                HamlibNet = { ValidEndpoint() },
            },
            "CAT hub radio backend 'flex' is not supported (expected one of: ts590, rigctld, loopback).",
        };

        // Serial backend missing a serial port.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "ts590" },
                HamlibNet = { ValidEndpoint() },
            },
            "CAT hub radio requires a serial port for the selected backend.",
        };

        // tcp_port out of range.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "loopback", TcpPort = 70000 },
                HamlibNet = { ValidEndpoint() },
            },
            "CAT hub radio tcp_port must be between 1 and 65535.",
        };

        // tcp_port explicit zero.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "loopback", TcpPort = 0 },
                HamlibNet = { ValidEndpoint() },
            },
            "CAT hub radio tcp_port must be between 1 and 65535.",
        };

        // No endpoints at all.
        yield return new object[]
        {
            new CatHubSettings { Radio = new CatHubRadioSettings { Backend = "loopback" } },
            "CAT hub configuration requires at least one serial endpoint or hamlib_net endpoint.",
        };

        // Duplicate names across serial and Hamlib NET endpoints.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "ts590", Port = "COM4" },
                SerialEndpoints = { new CatHubSerialEndpoint { Name = "shared", Transport = "CNCB0", Dialect = "ts590" } },
                HamlibNet = { new CatHubHamlibNetEndpoint { Name = "shared", Bind = "127.0.0.1:4532" } },
            },
            "CAT hub endpoint names must be unique: 'shared'.",
        };

        // Duplicate endpoint transports.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "ts590", Port = "COM4" },
                SerialEndpoints =
                {
                    new CatHubSerialEndpoint { Name = "a", Transport = "CNCB0", Dialect = "ts590" },
                    new CatHubSerialEndpoint { Name = "b", Transport = "cncb0", Dialect = "ts590" },
                },
            },
            "CAT hub serial endpoints must use distinct transports: 'cncb0'.",
        };

        // Application transport must be the other side of the virtual pair.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "ts590", Port = "COM4" },
                SerialEndpoints =
                {
                    new CatHubSerialEndpoint
                    {
                        Name = "n1mm",
                        Transport = "COM20",
                        ApplicationTransport = "com20",
                        Dialect = "ts590",
                    },
                },
            },
            "CAT hub serial endpoint 'n1mm' application transport must differ from its hub transport.",
        };

        // Endpoint reusing the radio port.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "ts590", Port = "COM4" },
                SerialEndpoints = { new CatHubSerialEndpoint { Name = "a", Transport = "com4", Dialect = "ts590" } },
            },
            "CAT hub serial endpoint 'a' cannot reuse the radio port 'COM4'.",
        };

        // Unsupported dialect.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "ts590", Port = "COM4" },
                SerialEndpoints = { new CatHubSerialEndpoint { Name = "a", Transport = "CNCB0", Dialect = "kenwood" } },
            },
            "CAT hub serial endpoint 'a' dialect 'kenwood' is not supported (expected one of: ts590, ts2000).",
        };

        // Bind without a port.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "loopback" },
                HamlibNet = { new CatHubHamlibNetEndpoint { Name = "engine", Bind = "127.0.0.1" } },
            },
            "CAT hub hamlib_net endpoint 'engine' bind must be in host:port form.",
        };

        // Bind port out of range.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "loopback" },
                HamlibNet = { new CatHubHamlibNetEndpoint { Name = "engine", Bind = "127.0.0.1:70000" } },
            },
            "CAT hub hamlib_net endpoint 'engine' bind port must be between 1 and 65535.",
        };

        // WinKeyer application transport must be the other side of the virtual pair.
        yield return new object[]
        {
            new CatHubSettings
            {
                Radio = new CatHubRadioSettings { Backend = "loopback" },
                HamlibNet = { ValidEndpoint() },
                Winkeyer = new CatHubWinkeyerSettings { Port = "COM3" },
                WinkeyerEndpoints =
                {
                    new CatHubWinkeyerEndpoint
                    {
                        Name = "wktools",
                        Transport = "COM42",
                        ApplicationTransport = "com42",
                    },
                },
            },
            "CAT hub WinKeyer endpoint 'wktools' application transport must differ from its hub transport.",
        };
    }

    [Fact]
    public void Load_does_not_throw_on_malformed_cat_hub_section()
    {
        var configPath = Path.Combine(_tempDirectory, "config.toml");
        File.WriteAllText(
            configPath,
            """
            [qrz_xml]
            username = "k7rnd"

            [cat_hub]
            radio = "this should be a table, not a string"
            endpoint = 42
            """);

        // Loading must succeed and the malformed CAT hub section must project to null.
        var state = CreateState();
        var status = state.GetSetupStatus();

        Assert.Null(status.CatHub);
        Assert.Equal("k7rnd", status.QrzXmlUsername);
    }

    [Fact]
    public void Save_setup_without_cat_hub_override_preserves_existing_section()
    {
        var configPath = Path.Combine(_tempDirectory, "config.toml");
        File.WriteAllText(
            configPath,
            """
            [cat_hub]
            # operator-authored comment that must survive engine saves
            mystery_key = "keep-me"

            [cat_hub.radio]
            backend = "rigctld"
            host = "192.168.1.50"
            """);

        var state = CreateState();
        state.SaveSetup(new SaveSetupRequest
        {
            StationProfile = new StationProfile
            {
                ProfileName = "Home",
                StationCallsign = "K7RND",
                OperatorCallsign = "K7RND",
                Grid = "CN87",
            },
        });

        var content = File.ReadAllText(configPath);
        Assert.Contains("# operator-authored comment that must survive engine saves", content, StringComparison.Ordinal);
        Assert.Contains("mystery_key = \"keep-me\"", content, StringComparison.Ordinal);
        Assert.Contains("backend = \"rigctld\"", content, StringComparison.Ordinal);
        Assert.Contains("[station_profile]", content, StringComparison.Ordinal);
    }

    [Fact]
    public void Get_setup_wizard_state_orders_cat_hub_before_review()
    {
        var state = CreateState();

        var response = state.GetSetupWizardState();

        Assert.Equal(5, response.Steps.Count);
        Assert.Equal(SetupWizardStep.LogFile, response.Steps[0].Step);
        Assert.Equal(SetupWizardStep.StationProfiles, response.Steps[1].Step);
        Assert.Equal(SetupWizardStep.QrzIntegration, response.Steps[2].Step);
        Assert.Equal(SetupWizardStep.CatHub, response.Steps[3].Step);
        Assert.Equal(SetupWizardStep.Review, response.Steps[4].Step);
        Assert.True(response.Steps[3].Complete);
    }

    private static CatHubSettings BuildValidCatHub()
    {
        return new CatHubSettings
        {
            Radio = new CatHubRadioSettings
            {
                Backend = "ts590",
                Transport = "serial",
                Port = "COM4",
                Baud = 9600,
            },
            Poll = new CatHubPollSettings { BaselineMs = 250 },
            Ptt = new CatHubPttSettings { MaxTxMs = 60000 },
            Events = new CatHubEventSettings { NativePush = true },
            Winkeyer = new CatHubWinkeyerSettings
            {
                Port = "COM3",
                Baud = 1200,
                MaxTxMs = 30000,
                ApiBind = "127.0.0.1:50071",
            },
            WinkeyerEndpoints =
            {
                new CatHubWinkeyerEndpoint
                {
                    Name = "n1mm-cw",
                    Transport = "COM40",
                    ApplicationTransport = "COM41",
                    Baud = 1200,
                    Primary = true,
                    Perms =
                    {
                        WinkeyerEndpointPermission.Status,
                        WinkeyerEndpointPermission.Send,
                        WinkeyerEndpointPermission.Control,
                    },
                },
            },
            SerialEndpoints =
            {
                new CatHubSerialEndpoint
                {
                    Name = "HDSDR",
                    Transport = "CNCB0",
                    ApplicationTransport = "CNCA0",
                    Baud = 9600,
                    Dialect = "ts590",
                    Perms = { CatHubPermission.Read, CatHubPermission.Write },
                },
            },
            HamlibNet = { ValidEndpoint() },
        };
    }

    private static CatHubHamlibNetEndpoint ValidEndpoint()
    {
        return new CatHubHamlibNetEndpoint
        {
            Name = "engine",
            Bind = "127.0.0.1:4532",
            Perms = { CatHubPermission.Read, CatHubPermission.Ptt },
        };
    }

    private ManagedEngineState CreateState()
    {
        return new ManagedEngineState(Path.Combine(_tempDirectory, "config.toml"), new MemoryStorage());
    }

    private ManagedEngineState CreateStateWithSync()
    {
        var storage = new MemoryStorage();
        var fakeApi = new FakeQrzLogbookApi();
        var syncEngine = new QrzSyncEngine(fakeApi);
        return new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            storage,
            lookupCoordinator: null,
            rigControlMonitor: null,
            spaceWeatherMonitor: null,
            syncEngine: syncEngine);
    }

    private ManagedEngineState CreateStateWithRigSnapshot(RigSnapshot snapshot)
    {
        var storage = new MemoryStorage();
        var monitor = new RigControlMonitor(
            new FakeRigControlProvider(() => snapshot.Clone()),
            TimeSpan.Zero);
        return new ManagedEngineState(
            Path.Combine(_tempDirectory, "config.toml"),
            storage,
            lookupCoordinator: null,
            rigControlMonitor: monitor,
            spaceWeatherMonitor: null,
            syncEngine: null);
    }

    private static byte[] Utf8(string value)
    {
        return Encoding.UTF8.GetBytes(value);
    }

    private static HttpClient GetOwnedSyncHttpClient(QrzSyncEngine syncEngine)
    {
        var api = GetRequiredPrivateFieldValue(syncEngine, "_client");
        return GetRequiredPrivateField<HttpClient>(api, "_httpClient");
    }

    private static bool IsHttpClientDisposed(HttpClient client)
    {
        var disposedField = typeof(HttpMessageInvoker).GetField("_disposed", BindingFlags.Instance | BindingFlags.NonPublic)
            ?? throw new InvalidOperationException("Could not locate HttpMessageInvoker._disposed.");

        return disposedField.GetValue(client) is true;
    }

    private static T GetRequiredPrivateField<T>(object instance, string fieldName)
        where T : class
    {
        return Assert.IsType<T>(GetRequiredPrivateFieldValue(instance, fieldName));
    }

    private static object GetRequiredPrivateFieldValue(object instance, string fieldName)
    {
        ArgumentNullException.ThrowIfNull(instance);
        ArgumentException.ThrowIfNullOrWhiteSpace(fieldName);

        var field = instance.GetType().GetField(fieldName, BindingFlags.Instance | BindingFlags.NonPublic)
            ?? throw new InvalidOperationException(
                $"Could not locate field '{fieldName}' on {instance.GetType().FullName}.");

        return field.GetValue(instance)
            ?? throw new InvalidOperationException(
                $"Field '{fieldName}' on {instance.GetType().FullName} was null.");
    }

    private sealed class FakeRigControlProvider(Func<RigSnapshot> factory) : IRigControlProvider
    {
        public RigSnapshot GetSnapshot() => factory();
    }

    private sealed class FakeQrzCredentialTester : IQrzCredentialTester
    {
        public int XmlCalls { get; private set; }

        public int LogbookCalls { get; private set; }

        public Task<TestQrzCredentialsResponse> TestXmlCredentialsAsync(
            string username,
            string password,
            CancellationToken cancellationToken)
        {
            XmlCalls++;
            return Task.FromResult(new TestQrzCredentialsResponse { Success = true });
        }

        public Task<TestQrzLogbookCredentialsResponse> TestLogbookCredentialsAsync(
            string apiKey,
            CancellationToken cancellationToken)
        {
            LogbookCalls++;
            return Task.FromResult(new TestQrzLogbookCredentialsResponse
            {
                Success = true,
                LogbookOwner = "K7RND",
                QsoCount = 42,
            });
        }
    }

    private sealed class TestAsyncStreamReader<T>(IReadOnlyList<T> items)
        : IAsyncStreamReader<T>
    {
        private int _index = -1;

        public T Current => items[_index];

        public Task<bool> MoveNext(CancellationToken cancellationToken)
        {
            cancellationToken.ThrowIfCancellationRequested();
            _index++;
            return Task.FromResult(_index < items.Count);
        }
    }

    private sealed class TestServerStreamWriter<T> : IServerStreamWriter<T>
    {
        public List<T> Messages { get; } = [];

        public WriteOptions? WriteOptions { get; set; }

        public Task WriteAsync(T message)
        {
            Messages.Add(message);
            return Task.CompletedTask;
        }
    }

    private sealed class TestServerCallContext : ServerCallContext
    {
        private readonly Metadata _responseTrailers = [];
        private readonly Dictionary<object, object> _userState = [];
        private WriteOptions? _writeOptions;
        private Status _status;

        protected override string MethodCore => "test";
        protected override string HostCore => "localhost";
        protected override string PeerCore => "test-peer";
        protected override DateTime DeadlineCore => DateTime.UtcNow.AddMinutes(1);
        protected override Metadata RequestHeadersCore => [];
        protected override CancellationToken CancellationTokenCore => CancellationToken.None;
        protected override Metadata ResponseTrailersCore => _responseTrailers;
        protected override Status StatusCore { get => _status; set => _status = value; }
        protected override WriteOptions? WriteOptionsCore { get => _writeOptions; set => _writeOptions = value; }
        protected override AuthContext AuthContextCore => new("none", []);

        protected override ContextPropagationToken CreatePropagationTokenCore(ContextPropagationOptions? options) =>
            throw new NotSupportedException();

        protected override Task WriteResponseHeadersAsyncCore(Metadata responseHeaders) => Task.CompletedTask;

        protected override IDictionary<object, object> UserStateCore => _userState;
    }

    /// <summary>
    /// Minimal in-memory fake for <see cref="IQrzLogbookApi"/> that records uploads and returns empty fetches.
    /// </summary>
    private sealed class FakeQrzLogbookApi : IQrzLogbookApi
    {
        private int _logIdCounter;

        public int InsertCalls { get; private set; }

        public int ReplaceCalls { get; private set; }

        public int UpdateCalls { get; private set; }

        public int DeleteCalls { get; private set; }

        public bool FailDeletes { get; init; }

        public Func<QsoRecord, Task<bool>>? UploadValidator { get; set; }

        public bool UploadValidationPassed { get; private set; }

        public Task<List<QsoRecord>> FetchQsosAsync(string? sinceDateYmd) =>
            Task.FromResult(new List<QsoRecord>());

        public async Task<string> UploadQsoAsync(QsoRecord qso, string? bookOwner = null)
        {
            InsertCalls++;
            if (UploadValidator is not null)
            {
                UploadValidationPassed = await UploadValidator(qso).ConfigureAwait(false);
            }

            var logId = $"FAKE-{Interlocked.Increment(ref _logIdCounter)}";
            return logId;
        }

        public async Task<string> UploadQsoWithReplaceAsync(QsoRecord qso, string? bookOwner = null)
        {
            ReplaceCalls++;
            if (UploadValidator is not null)
            {
                UploadValidationPassed = await UploadValidator(qso).ConfigureAwait(false);
            }

            var logId = $"FAKE-{Interlocked.Increment(ref _logIdCounter)}";
            return logId;
        }

        public async Task<string> UpdateQsoAsync(QsoRecord qso, string? bookOwner = null)
        {
            UpdateCalls++;
            if (UploadValidator is not null)
            {
                UploadValidationPassed = await UploadValidator(qso).ConfigureAwait(false);
            }

            var logId = $"FAKE-{Interlocked.Increment(ref _logIdCounter)}";
            return logId;
        }

        public Task<QrzLogbookStatus> GetStatusAsync() =>
            Task.FromResult(new QrzLogbookStatus("K7RND", (uint)_logIdCounter));

        public Task DeleteQsoAsync(string logid)
        {
            DeleteCalls++;
            return FailDeletes
                ? Task.FromException(new QrzLogbookException("remote delete failed"))
                : Task.CompletedTask;
        }
    }

    private sealed class FakeMalformedQrzLogbookApi : IQrzLogbookApi
    {
        public Task<List<QsoRecord>> FetchQsosAsync(string? sinceDateYmd)
            => Task.FromResult(new List<QsoRecord> { null! });

        public Task<string> UploadQsoAsync(QsoRecord qso, string? bookOwner = null) => Task.FromResult("FAKE-1");

        public Task<string> UploadQsoWithReplaceAsync(QsoRecord qso, string? bookOwner = null) => Task.FromResult("FAKE-1");

        public Task<string> UpdateQsoAsync(QsoRecord qso, string? bookOwner = null) => Task.FromResult("FAKE-1");

        public Task<QrzLogbookStatus> GetStatusAsync() =>
            Task.FromResult(new QrzLogbookStatus("K7RND", 0));

        public Task DeleteQsoAsync(string logid) => Task.CompletedTask;
    }
}
#pragma warning restore CA1707

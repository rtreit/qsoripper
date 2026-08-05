using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;
using QsoRipper.Gui.Services;
using QsoRipper.Gui.Utilities;
using QsoRipper.Gui.ViewModels;
using QsoRipper.Services;

namespace QsoRipper.Gui.Tests;

public sealed class MainWindowViewModelTests
{
    [Fact]
    public void FocusLoggerCommandDoesNotRequestGridFocus()
    {
        using var viewModel = new MainWindowViewModel(new FakeEngineClient());
        var loggerFocusRequests = 0;
        var gridFocusRequests = 0;
        viewModel.LoggerFocusRequested += (_, _) => loggerFocusRequests++;
        viewModel.GridFocusRequested += (_, _) => gridFocusRequests++;

        viewModel.FocusLoggerCommand.Execute(null);

        Assert.Equal(1, loggerFocusRequests);
        Assert.Equal(0, gridFocusRequests);
    }

    [Fact]
    public void FocusSearchCommandDoesNotRequestGridFocus()
    {
        using var viewModel = new MainWindowViewModel(new FakeEngineClient());
        var searchFocusRequests = 0;
        var gridFocusRequests = 0;
        viewModel.SearchFocusRequested += (_, _) => searchFocusRequests++;
        viewModel.GridFocusRequested += (_, _) => gridFocusRequests++;

        viewModel.FocusSearchCommand.Execute(null);

        Assert.Equal(1, searchFocusRequests);
        Assert.Equal(0, gridFocusRequests);
    }

    [Fact]
    public async Task CheckFirstRunAsyncCompletesBeforeSlowSyncStatusFinishes()
    {
        var syncStatusSource = new TaskCompletionSource<GetSyncStatusResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
        var engine = new FakeEngineClient
        {
            SetupStatus = new GetSetupStatusResponse
            {
                Status = new SetupStatus
                {
                    SetupComplete = true,
                    IsFirstRun = false,
                },
            },
            RecentQsos =
            [
                CreateQso("qso-1", "W1AW"),
            ],
            SyncStatusTask = syncStatusSource.Task,
        };

        using var viewModel = new MainWindowViewModel(engine);

        await viewModel.CheckFirstRunAsync();

        Assert.True(viewModel.RecentQsos.HasLoaded);
        Assert.Equal("Ready", viewModel.StatusMessage);
        Assert.Equal("Sync: never", viewModel.SyncStatusText);

        syncStatusSource.SetResult(new GetSyncStatusResponse
        {
            LastSync = Timestamp.FromDateTimeOffset(DateTimeOffset.UtcNow - TimeSpan.FromMinutes(3)),
        });

        await WaitUntilAsync(
            () => viewModel.SyncStatusText.Contains("Last sync", StringComparison.Ordinal),
            TimeSpan.FromSeconds(1));

        Assert.Contains("Last sync", viewModel.SyncStatusText, StringComparison.Ordinal);
    }

    [Fact]
    public async Task CheckFirstRunAsyncDisplaysWindowsLogFileNameOnLinux()
    {
        var engine = new FakeEngineClient
        {
            SetupStatus = new GetSetupStatusResponse
            {
                Status = new SetupStatus
                {
                    SetupComplete = true,
                    LogFilePath = "C:\\logs\\kc7ava-debug-log.db",
                },
            },
        };

        using var viewModel = new MainWindowViewModel(engine);

        await viewModel.CheckFirstRunAsync();

        Assert.Equal("Log: kc7ava-debug-log", viewModel.ActiveLogText);
    }

    [Fact]
    public async Task SyncNowShowsConflictCountWhenQrzDiffersFromLocal()
    {
        var engine = new FakeEngineClient
        {
            SyncResponse = new SyncWithQrzResponse
            {
                Complete = true,
                UploadedRecords = 0,
                DownloadedRecords = 0,
                ConflictRecords = 1,
            },
        };
        using var viewModel = new MainWindowViewModel(engine);

        await viewModel.SyncNowCommand.ExecuteAsync(null);

        Assert.Contains("conflict", viewModel.SyncStatusText, StringComparison.OrdinalIgnoreCase);
        Assert.Contains("1", viewModel.SyncStatusText, StringComparison.Ordinal);
    }

    [Fact]
    public async Task SyncNowShowsResolvedMismatchCountWhenQrzWasUpdated()
    {
        var engine = new FakeEngineClient
        {
            SyncResponse = new SyncWithQrzResponse
            {
                Complete = true,
                UploadedRecords = 2,
                DownloadedRecords = 0,
                ConflictRecords = 0,
            },
        };
        using var viewModel = new MainWindowViewModel(engine);

        await viewModel.SyncNowCommand.ExecuteAsync(null);

        Assert.Contains("↑2", viewModel.SyncStatusText, StringComparison.Ordinal);
    }

    [Fact]
    public void ApplyPreferencesIgnoresPersistedEngineWhenQsoripperEngineEnvIsSet()
    {
        const string envKey = "QSORIPPER_ENGINE";
        var prior = Environment.GetEnvironmentVariable(envKey);
        Environment.SetEnvironmentVariable(envKey, "local-dotnet");
        try
        {
            using var viewModel = new MainWindowViewModel(new FakeEngineClient());

            viewModel.ApplyPreferences(new UiPreferences
            {
                EngineProfileId = "local-rust",
                EngineEndpoint = "http://127.0.0.1:50051",
            });

            var captured = viewModel.CapturePreferences();

            // When env overrides the engine selection, the persisted preference
            // must be preserved (not replaced by the runtime/test fixture value)
            // so a later env-less launch falls back to what the user actually chose.
            Assert.Equal("local-rust", captured.EngineProfileId);
            Assert.Equal("http://127.0.0.1:50051", captured.EngineEndpoint);
        }
        finally
        {
            Environment.SetEnvironmentVariable(envKey, prior);
        }
    }

    [Fact]
    public void ApplyPreferencesUsesPersistedEngineWhenEnvIsUnset()
    {
        const string profileKey = "QSORIPPER_ENGINE";
        const string legacyKey = "QSORIPPER_ENGINE_IMPLEMENTATION";
        const string endpointKey = "QSORIPPER_ENDPOINT";
        var priorProfile = Environment.GetEnvironmentVariable(profileKey);
        var priorLegacy = Environment.GetEnvironmentVariable(legacyKey);
        var priorEndpoint = Environment.GetEnvironmentVariable(endpointKey);
        Environment.SetEnvironmentVariable(profileKey, null);
        Environment.SetEnvironmentVariable(legacyKey, null);
        Environment.SetEnvironmentVariable(endpointKey, null);
        try
        {
            using var viewModel = new MainWindowViewModel(new FakeEngineClient());

            viewModel.ApplyPreferences(new UiPreferences
            {
                EngineProfileId = "local-dotnet",
                EngineEndpoint = "http://127.0.0.1:50052",
            });

            var captured = viewModel.CapturePreferences();

            Assert.Equal("local-dotnet", captured.EngineProfileId);
            Assert.Equal("http://127.0.0.1:50052", captured.EngineEndpoint);
        }
        finally
        {
            Environment.SetEnvironmentVariable(profileKey, priorProfile);
            Environment.SetEnvironmentVariable(legacyKey, priorLegacy);
            Environment.SetEnvironmentVariable(endpointKey, priorEndpoint);
        }
    }

    private static async Task WaitUntilAsync(Func<bool> predicate, TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (DateTime.UtcNow < deadline)
        {
            if (predicate())
            {
                return;
            }

            await Task.Delay(10);
        }

        Assert.True(predicate(), "Condition was not satisfied before the timeout elapsed.");
    }

    private static QsoRecord CreateQso(string localId, string workedCallsign)
    {
        return new QsoRecord
        {
            LocalId = localId,
            WorkedCallsign = workedCallsign,
            StationCallsign = "K7RND",
            UtcTimestamp = Timestamp.FromDateTimeOffset(new DateTimeOffset(2026, 4, 13, 22, 15, 0, TimeSpan.Zero)),
            Band = Band._20M,
            Mode = Mode.Cw,
            FrequencyHz = 14_025_000,
            WorkedGrid = "CN87",
            Comment = "Loaded",
            Notes = string.Empty,
            WorkedCountry = "United States",
            WorkedOperatorName = "Alice",
            WorkedState = "WA",
            RstSent = new RstReport { Raw = "59" },
            RstReceived = new RstReport { Raw = "57" },
        };
    }

    private sealed class FakeEngineClient : IEngineClient
    {
        public GetSetupStatusResponse SetupStatus { get; init; } = new();

        public IReadOnlyList<QsoRecord> RecentQsos { get; init; } = [];

        public Task<GetSyncStatusResponse> SyncStatusTask { get; init; } = Task.FromResult(new GetSyncStatusResponse());

        public SyncWithQrzResponse SyncResponse { get; init; } = new();

        public Task<GetSetupWizardStateResponse> GetWizardStateAsync(CancellationToken ct = default) =>
            Task.FromResult(new GetSetupWizardStateResponse { Status = SetupStatus.Status ?? new SetupStatus() });

        public Task<ValidateSetupStepResponse> ValidateStepAsync(ValidateSetupStepRequest request, CancellationToken ct = default) =>
            throw new NotImplementedException();

        public Task<TestQrzCredentialsResponse> TestQrzCredentialsAsync(string username, string password, CancellationToken ct = default) =>
            throw new NotImplementedException();

        public Task<SaveSetupResponse> SaveSetupAsync(SaveSetupRequest request, CancellationToken ct = default) =>
            throw new NotImplementedException();

        public Task<GetSetupStatusResponse> GetSetupStatusAsync(CancellationToken ct = default) =>
            Task.FromResult(SetupStatus);

        public Task<TestQrzLogbookCredentialsResponse> TestQrzLogbookCredentialsAsync(string apiKey, CancellationToken ct = default) =>
            throw new NotImplementedException();

        public Task<IReadOnlyList<QsoRecord>> ListQsosAsync(CancellationToken ct = default) =>
            Task.FromResult(RecentQsos);

        public Task<UpdateQsoResponse> UpdateQsoAsync(QsoRecord qso, bool syncToQrz = false, CancellationToken ct = default) =>
            throw new NotImplementedException();

        public Task<SyncWithQrzResponse> SyncWithQrzAsync(CancellationToken ct = default) =>
            Task.FromResult(SyncResponse);

        public Task<GetSyncStatusResponse> GetSyncStatusAsync(CancellationToken ct = default) =>
            SyncStatusTask;

        public Task<LookupResponse> LookupCallsignAsync(string callsign, CancellationToken ct = default) =>
            throw new NotImplementedException();

        public Task<DeleteQsoResponse> DeleteQsoAsync(string localId, bool deleteFromQrz = false, CancellationToken ct = default) =>
            throw new NotImplementedException();

        public Task<LogQsoResponse> LogQsoAsync(QsoRecord qso, bool syncToQrz = false, CancellationToken ct = default) =>
            throw new NotImplementedException();

        public Task<GetRigSnapshotResponse> GetRigSnapshotAsync(CancellationToken ct = default) =>
            Task.FromResult(new GetRigSnapshotResponse());

        public Task<GetRigStatusResponse> GetRigStatusAsync(CancellationToken ct = default) =>
            Task.FromResult(new GetRigStatusResponse());

        public Task<GetCurrentSpaceWeatherResponse> GetCurrentSpaceWeatherAsync(CancellationToken ct = default) =>
            Task.FromResult(new GetCurrentSpaceWeatherResponse());
        public Task<ComputeGreatCircleResponse> ComputeGreatCircleAsync(ComputeGreatCircleRequest request, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<GetActiveStationContextResponse> GetActiveStationContextAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<PurgeDeletedQsosResponse> PurgeDeletedQsosAsync(IReadOnlyList<string>? localIds = null, Timestamp? olderThan = null, bool includePendingRemoteDeletes = false, CancellationToken ct = default) => throw new NotImplementedException();
    }
}

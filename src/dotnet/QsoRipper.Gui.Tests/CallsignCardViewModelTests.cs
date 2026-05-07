using Google.Protobuf.WellKnownTypes;
using QsoRipper.Domain;
using QsoRipper.Gui.Services;
using QsoRipper.Gui.ViewModels;
using QsoRipper.Services;

namespace QsoRipper.Gui.Tests;

public sealed class CallsignCardViewModelTests
{
    [Fact]
    public async Task LoadAsyncSetsQrzPageUrlFromCallsign()
    {
        var engine = new LookupEngineClient("k7abc");
        var viewModel = new CallsignCardViewModel(engine);

        await viewModel.LoadAsync("k7abc");

        Assert.True(viewModel.HasQrzPageUrl);
        Assert.Equal("https://www.qrz.com/db/K7ABC", viewModel.QrzPageUrl);
    }

    [Fact]
    public async Task OpenQrzPageCommandRaisesExternalUrlEvent()
    {
        var engine = new LookupEngineClient("N0CALL/P");
        var viewModel = new CallsignCardViewModel(engine);
        string? openedUrl = null;
        viewModel.OpenExternalUrlRequested += (_, url) => openedUrl = url;

        await viewModel.LoadAsync("N0CALL/P");
        viewModel.OpenQrzPageCommand.Execute(null);

        Assert.Equal("https://www.qrz.com/db/N0CALL%2FP", openedUrl);
    }

    private sealed class LookupEngineClient(string callsign) : IEngineClient
    {
        public Task<LookupResponse> LookupCallsignAsync(string inputCallsign, CancellationToken ct = default)
        {
            var record = new CallsignRecord
            {
                Callsign = callsign,
            };
            return Task.FromResult(new LookupResponse
            {
                Result = new LookupResult
                {
                    State = LookupState.Found,
                    Record = record,
                },
            });
        }

        public Task<GetSetupWizardStateResponse> GetWizardStateAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<ValidateSetupStepResponse> ValidateStepAsync(ValidateSetupStepRequest request, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<TestQrzCredentialsResponse> TestQrzCredentialsAsync(string username, string password, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<SaveSetupResponse> SaveSetupAsync(SaveSetupRequest request, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<GetSetupStatusResponse> GetSetupStatusAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<TestQrzLogbookCredentialsResponse> TestQrzLogbookCredentialsAsync(string apiKey, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<IReadOnlyList<QsoRecord>> ListRecentQsosAsync(int limit = 200, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<UpdateQsoResponse> UpdateQsoAsync(QsoRecord qso, bool syncToQrz = false, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<SyncWithQrzResponse> SyncWithQrzAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<GetSyncStatusResponse> GetSyncStatusAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<DeleteQsoResponse> DeleteQsoAsync(string localId, bool deleteFromQrz = false, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<LogQsoResponse> LogQsoAsync(QsoRecord qso, bool syncToQrz = false, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<GetRigSnapshotResponse> GetRigSnapshotAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<GetRigStatusResponse> GetRigStatusAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<GetCurrentSpaceWeatherResponse> GetCurrentSpaceWeatherAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<ComputeGreatCircleResponse> ComputeGreatCircleAsync(ComputeGreatCircleRequest request, CancellationToken ct = default) => throw new NotImplementedException();
        public Task<GetActiveStationContextResponse> GetActiveStationContextAsync(CancellationToken ct = default) => throw new NotImplementedException();
        public Task<PurgeDeletedQsosResponse> PurgeDeletedQsosAsync(IReadOnlyList<string>? localIds = null, Timestamp? olderThan = null, bool includePendingRemoteDeletes = false, CancellationToken ct = default) => throw new NotImplementedException();
    }
}

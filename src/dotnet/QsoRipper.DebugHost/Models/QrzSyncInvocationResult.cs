using QsoRipper.Services;

namespace QsoRipper.DebugHost.Models;

internal sealed record QrzSyncInvocationResult(
    bool FullSync,
    IReadOnlyList<SyncWithQrzResponse> ProgressUpdates,
    GetSyncStatusResponse? SyncStatus,
    string? ErrorMessage,
    DateTimeOffset CompletedAtUtc)
{
    public bool Succeeded => string.IsNullOrWhiteSpace(ErrorMessage);

    public SyncWithQrzResponse? LatestUpdate => ProgressUpdates.Count == 0 ? null : ProgressUpdates[^1];
}

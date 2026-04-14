using QsoRipper.Domain;
using QsoRipper.Services;

namespace QsoRipper.DebugHost.Models;

internal sealed record StorageSmokeTestResult(
    QsoRecord RequestedQso,
    LogQsoResponse? LogResponse,
    GetQsoResponse? LoadedResponse,
    UpdateQsoResponse? UpdateResponse,
    GetQsoResponse? UpdatedResponse,
    IReadOnlyList<QsoRecord> ListedQsos,
    GetSyncStatusResponse? SyncStatus,
    DeleteQsoResponse? DeleteResponse,
    bool RetainedRecord,
    bool UpdateVerified,
    bool DeleteVerified,
    string? ErrorMessage,
    DateTimeOffset CompletedAtUtc)
{
    public bool Succeeded => string.IsNullOrWhiteSpace(ErrorMessage);

    public string? LocalId => LogResponse?.LocalId;

    public bool UpdateSucceeded => UpdateResponse?.Success == true && UpdateVerified;
}

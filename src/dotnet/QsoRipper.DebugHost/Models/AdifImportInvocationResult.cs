using QsoRipper.Services;

namespace QsoRipper.DebugHost.Models;

internal sealed record AdifImportInvocationResult(
    ImportAdifResponse? Response,
    string SourceDescription,
    int ByteCount,
    int ChunkCount,
    bool Refresh,
    string? ErrorMessage,
    DateTimeOffset CompletedAtUtc)
{
    public bool Succeeded => string.IsNullOrWhiteSpace(ErrorMessage);

    public int WarningCount => Response?.Warnings.Count ?? 0;
}

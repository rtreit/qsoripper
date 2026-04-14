using QsoRipper.Services;

namespace QsoRipper.DebugHost.Models;

internal sealed record AdifExportInvocationResult(
    ExportAdifRequest Request,
    string AdifText,
    int ByteCount,
    int ChunkCount,
    int RecordCountEstimate,
    string? ErrorMessage,
    DateTimeOffset CompletedAtUtc)
{
    public bool Succeeded => string.IsNullOrWhiteSpace(ErrorMessage);
}

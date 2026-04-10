namespace LogRipper.DebugHost.Models;

internal sealed record ProtoSampleDefinition(
    string Id,
    string Label,
    Type MessageType,
    bool SupportsQsoOptions);

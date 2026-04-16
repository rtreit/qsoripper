namespace QsoRipper.EngineSelection;

public sealed record EngineTargetProfile(
    string ProfileId,
    EngineImplementation Implementation,
    string EngineId,
    string DisplayName,
    string Endpoint);

namespace CatHubFrequencyProbe;

internal sealed record EngineFrequencySnapshot(
    ulong FrequencyHz,
    string Mode,
    long QueryMilliseconds,
    DateTimeOffset SampledAt);

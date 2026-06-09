namespace CatHubFrequencyProbe;

internal sealed record FrequencySnapshot(
    ulong FrequencyHz,
    string Mode,
    string Vfo,
    long QueryMilliseconds,
    DateTimeOffset SampledAt);

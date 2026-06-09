using System.Diagnostics;
using Grpc.Net.Client;
using QsoRipper.Services;

namespace CatHubFrequencyProbe;

internal sealed class EngineRigClient : IDisposable
{
    private readonly GrpcChannel _channel;
    private readonly RigControlService.RigControlServiceClient _client;

    public EngineRigClient(string endpoint)
    {
        _channel = GrpcChannel.ForAddress(endpoint);
        _client = new RigControlService.RigControlServiceClient(_channel);
    }

    public async Task<EngineFrequencySnapshot> ReadSnapshotAsync(CancellationToken cancellationToken)
    {
        var stopwatch = Stopwatch.StartNew();
        var response = await _client.GetRigSnapshotAsync(
            new GetRigSnapshotRequest(),
            cancellationToken: cancellationToken);
        stopwatch.Stop();

        var snapshot = response.Snapshot
            ?? throw new InvalidDataException("Engine returned no rig snapshot.");
        if (snapshot.ErrorMessage.Length > 0)
        {
            throw new InvalidDataException($"Engine rig snapshot error: {snapshot.ErrorMessage}");
        }

        return new EngineFrequencySnapshot(
            snapshot.FrequencyHz,
            snapshot.RawMode.Length == 0 ? snapshot.Mode.ToString() : snapshot.RawMode,
            stopwatch.ElapsedMilliseconds,
            DateTimeOffset.Now);
    }

    public void Dispose()
    {
        _channel.Dispose();
    }
}

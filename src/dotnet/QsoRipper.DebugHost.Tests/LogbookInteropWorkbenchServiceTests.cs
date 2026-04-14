using QsoRipper.DebugHost.Models;
using QsoRipper.DebugHost.Services;
using QsoRipper.Domain;
using QsoRipper.Services;

namespace QsoRipper.DebugHost.Tests;

#pragma warning disable CA1707 // Remove underscores from member names - xUnit allows underscores in test methods
public class LogbookInteropWorkbenchServiceTests
{
    [Fact]
    public async Task CreateImportRequestsAsync_splits_stream_and_sets_refresh_only_on_first_chunk()
    {
        var bytes = Enumerable.Range(0, 131077).Select(static i => (byte)(i % 251)).ToArray();
        await using var stream = new MemoryStream(bytes);

        var requests = new List<ImportAdifRequest>();
        await foreach (var request in LogbookInteropWorkbenchService.CreateImportRequestsAsync(stream, refresh: true))
        {
            requests.Add(request);
        }

        Assert.Equal(3, requests.Count);
        Assert.True(requests[0].Refresh);
        Assert.False(requests[1].Refresh);
        Assert.False(requests[2].Refresh);
        Assert.Equal(65536, requests[0].Chunk!.Data.Length);
        Assert.Equal(65536, requests[1].Chunk!.Data.Length);
        Assert.Equal(5, requests[2].Chunk!.Data.Length);

        var reconstructed = requests
            .SelectMany(static request => request.Chunk!.Data.ToByteArray())
            .ToArray();
        Assert.Equal(bytes, reconstructed);
    }

    [Theory]
    [InlineData("", 0)]
    [InlineData("<EOH>\n", 0)]
    [InlineData("<CALL:4>W1AW<EOR>", 1)]
    [InlineData("<CALL:4>W1AW<eor>\n<CALL:4>K7RND<EOr>", 2)]
    public void CountAdifRecords_counts_case_insensitive_end_of_record_markers(string payload, int expected)
    {
        Assert.Equal(expected, LogbookInteropWorkbenchService.CountAdifRecords(payload));
    }

    [Fact]
    public void BuildUpdatedQso_preserves_identity_and_sets_verification_comment()
    {
        var original = new QsoRecord
        {
            LocalId = "local-123",
            WorkedCallsign = "W1AW",
            Band = Band._20M,
            Mode = Mode.Ssb,
            Comment = "Original"
        };

        var updated = StorageWorkbenchService.BuildUpdatedQso(original);

        Assert.NotSame(original, updated);
        Assert.Equal("local-123", updated.LocalId);
        Assert.Equal("Original", original.Comment);
        Assert.StartsWith("DebugHost storage smoke update ", updated.Comment, StringComparison.Ordinal);
    }
}
#pragma warning restore CA1707

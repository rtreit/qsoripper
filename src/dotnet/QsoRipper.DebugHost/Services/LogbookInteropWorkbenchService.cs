using System.Runtime.CompilerServices;
using System.Text;
using Google.Protobuf;
using Grpc.Core;
using QsoRipper.DebugHost.Models;
using QsoRipper.Services;

namespace QsoRipper.DebugHost.Services;

internal sealed class LogbookInteropWorkbenchService
{
    private const int ChunkSize = 65536;
    private static readonly Encoding AdifEncoding = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false);

    private readonly GrpcClientFactory _clientFactory;

    public LogbookInteropWorkbenchService(GrpcClientFactory clientFactory)
    {
        ArgumentNullException.ThrowIfNull(clientFactory);

        _clientFactory = clientFactory;
    }

    public async Task<AdifImportInvocationResult> ImportAdifAsync(
        string adifText,
        bool refresh,
        string sourceDescription,
        CancellationToken cancellationToken = default)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(adifText);
        ArgumentException.ThrowIfNullOrWhiteSpace(sourceDescription);

        var adifBytes = AdifEncoding.GetBytes(adifText);
        var chunkCount = Math.Max(1, (adifBytes.Length + ChunkSize - 1) / ChunkSize);

        try
        {
            var client = _clientFactory.CreateLogbookClient();
            using var call = client.ImportAdif(cancellationToken: cancellationToken);
            await using var input = new MemoryStream(adifBytes, writable: false);

            await foreach (var request in CreateImportRequestsAsync(input, refresh, cancellationToken))
            {
                await WriteImportRequestAsync(call.RequestStream, request).WaitAsync(cancellationToken);
            }

            await call.RequestStream.CompleteAsync();
            var response = await call.ResponseAsync;

            return new AdifImportInvocationResult(
                response,
                sourceDescription,
                adifBytes.Length,
                chunkCount,
                refresh,
                ErrorMessage: null,
                DateTimeOffset.UtcNow);
        }
        catch (RpcException ex)
        {
            return new AdifImportInvocationResult(
                Response: null,
                sourceDescription,
                adifBytes.Length,
                chunkCount,
                refresh,
                ex.Status.Detail,
                DateTimeOffset.UtcNow);
        }
        catch (OperationCanceledException ex)
        {
            return new AdifImportInvocationResult(
                Response: null,
                sourceDescription,
                adifBytes.Length,
                chunkCount,
                refresh,
                ex.Message,
                DateTimeOffset.UtcNow);
        }
    }

    public async Task<AdifExportInvocationResult> ExportAdifAsync(
        ExportAdifRequest request,
        CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(request);

        var chunkCount = 0;
        var bytes = Array.Empty<byte>();
        var adifText = string.Empty;

        try
        {
            var client = _clientFactory.CreateLogbookClient();
            using var call = client.ExportAdif(request, cancellationToken: cancellationToken);
            await using var output = new MemoryStream();

            await foreach (var response in call.ResponseStream.ReadAllAsync(cancellationToken))
            {
                var chunk = response.Chunk;
                if (chunk is null)
                {
                    continue;
                }

                chunkCount++;
                await output.WriteAsync(chunk.Data.Memory, cancellationToken);
            }

            bytes = output.ToArray();
            adifText = AdifEncoding.GetString(bytes);

            return new AdifExportInvocationResult(
                request,
                adifText,
                bytes.Length,
                chunkCount,
                CountAdifRecords(adifText),
                ErrorMessage: null,
                DateTimeOffset.UtcNow);
        }
        catch (RpcException ex)
        {
            return new AdifExportInvocationResult(
                request,
                adifText,
                bytes.Length,
                chunkCount,
                CountAdifRecords(adifText),
                ex.Status.Detail,
                DateTimeOffset.UtcNow);
        }
        catch (OperationCanceledException ex)
        {
            return new AdifExportInvocationResult(
                request,
                adifText,
                bytes.Length,
                chunkCount,
                CountAdifRecords(adifText),
                ex.Message,
                DateTimeOffset.UtcNow);
        }
    }

    public async Task<QrzSyncInvocationResult> SyncWithQrzAsync(
        bool fullSync,
        CancellationToken cancellationToken = default)
    {
        var progressUpdates = new List<SyncWithQrzResponse>();
        GetSyncStatusResponse? syncStatus = null;

        try
        {
            var client = _clientFactory.CreateLogbookClient();
            using var call = client.SyncWithQrz(
                new SyncWithQrzRequest { FullSync = fullSync },
                cancellationToken: cancellationToken);

            await foreach (var response in call.ResponseStream.ReadAllAsync(cancellationToken))
            {
                progressUpdates.Add(response);
            }

            syncStatus = await client.GetSyncStatusAsync(
                new GetSyncStatusRequest(),
                cancellationToken: cancellationToken);

            var errorMessage = progressUpdates
                .LastOrDefault(update => !string.IsNullOrWhiteSpace(update.Error))
                ?.Error;

            return new QrzSyncInvocationResult(
                fullSync,
                progressUpdates,
                syncStatus,
                errorMessage,
                DateTimeOffset.UtcNow);
        }
        catch (RpcException ex)
        {
            return new QrzSyncInvocationResult(
                fullSync,
                progressUpdates,
                syncStatus,
                ex.Status.Detail,
                DateTimeOffset.UtcNow);
        }
        catch (OperationCanceledException ex)
        {
            return new QrzSyncInvocationResult(
                fullSync,
                progressUpdates,
                syncStatus,
                ex.Message,
                DateTimeOffset.UtcNow);
        }
    }

    public async Task<(GetSyncStatusResponse? Response, string? ErrorMessage)> GetSyncStatusAsync(
        CancellationToken cancellationToken = default)
    {
        try
        {
            var client = _clientFactory.CreateLogbookClient();
            var response = await client.GetSyncStatusAsync(
                new GetSyncStatusRequest(),
                cancellationToken: cancellationToken);
            return (response, null);
        }
        catch (RpcException ex)
        {
            return (null, ex.Status.Detail);
        }
        catch (OperationCanceledException ex)
        {
            return (null, ex.Message);
        }
    }

    public async Task<(TestQrzLogbookCredentialsResponse? Response, string? ErrorMessage)> TestLogbookCredentialsAsync(
        string apiKey,
        CancellationToken cancellationToken = default)
    {
        try
        {
            var client = _clientFactory.CreateSetupClient();
            var response = await client.TestQrzLogbookCredentialsAsync(
                new TestQrzLogbookCredentialsRequest { ApiKey = apiKey },
                cancellationToken: cancellationToken);
            return (response, null);
        }
        catch (RpcException ex)
        {
            return (null, ex.Status.Detail);
        }
        catch (OperationCanceledException ex)
        {
            return (null, ex.Message);
        }
    }

    internal static async IAsyncEnumerable<ImportAdifRequest> CreateImportRequestsAsync(
        Stream input,
        bool refresh = false,
        [EnumeratorCancellation] CancellationToken cancellationToken = default)
    {
        ArgumentNullException.ThrowIfNull(input);

        var buffer = new byte[ChunkSize];
        var isFirst = true;

        while (true)
        {
            var bytesRead = await input.ReadAsync(buffer.AsMemory(0, buffer.Length), cancellationToken);
            if (bytesRead == 0)
            {
                yield break;
            }

            var request = new ImportAdifRequest
            {
                Chunk = new AdifChunk
                {
                    Data = ByteString.CopyFrom(buffer, 0, bytesRead)
                }
            };

            if (isFirst && refresh)
            {
                request.Refresh = true;
            }

            isFirst = false;
            yield return request;
        }
    }

    internal static int CountAdifRecords(string adifText)
    {
        if (string.IsNullOrWhiteSpace(adifText))
        {
            return 0;
        }

        var count = 0;
        var start = 0;

        while (true)
        {
            var markerIndex = adifText.IndexOf("<EOR>", start, StringComparison.OrdinalIgnoreCase);
            if (markerIndex < 0)
            {
                return count;
            }

            count++;
            start = markerIndex + "<EOR>".Length;
        }
    }

    private static Task WriteImportRequestAsync(
        IClientStreamWriter<ImportAdifRequest> requestStream,
        ImportAdifRequest request)
    {
        ArgumentNullException.ThrowIfNull(requestStream);
        ArgumentNullException.ThrowIfNull(request);

        return requestStream.WriteAsync(request);
    }
}

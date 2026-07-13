using System.Diagnostics.CodeAnalysis;
using System.Net;
using System.Net.Sockets;
using QsoRipper.Domain;

namespace QsoRipper.Engine.RigControl.Tests;

public sealed class RigctldProviderTests
{
    [Fact]
    [SuppressMessage("Reliability", "CA2025", Justification = "The loopback server task is awaited before the listener leaves scope.")]
    public async Task GetSnapshotReadsSplitTxStateAndPowerWhenSupported()
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var endpoint = (IPEndPoint)listener.LocalEndpoint;
        var server = ServeAsync(listener,
            ("f", "14074000\n"),
            ("m", "USB\n2400\n"),
            ("s", "1\nVFOB\n"),
            ("i", "14250000\n"),
            ("x", "CW\n500\n"),
            ("l RFPOWER", "0.5\n"),
            ("2 0.5 14250000 CW", "50000\n"));
        var provider = new RigctldProvider(
            endpoint.Address.ToString(),
            endpoint.Port,
            TimeSpan.FromSeconds(2));

        var snapshot = provider.GetSnapshot();
        await server;

        Assert.Equal(14_250_000ul, snapshot.FrequencyHz);
        Assert.Equal(Band._20M, snapshot.Band);
        Assert.Equal(Mode.Cw, snapshot.Mode);
        Assert.Equal(14_074_000ul, snapshot.FrequencyRxHz);
        Assert.Equal(Band._20M, snapshot.BandRx);
        Assert.Equal(50.0, snapshot.TxPowerWatts);
    }

    [Fact]
    [SuppressMessage("Reliability", "CA2025", Justification = "The loopback server task is awaited before the listener leaves scope.")]
    public async Task GetSnapshotIgnoresUnsupportedOptionalQueries()
    {
        using var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var endpoint = (IPEndPoint)listener.LocalEndpoint;
        var server = ServeAsync(listener,
            ("f", "7074000\n"),
            ("m", "USB\n2400\n"),
            ("s", "RPRT -11\n"),
            ("l RFPOWER", "RPRT -11\n"));
        var provider = new RigctldProvider(
            endpoint.Address.ToString(),
            endpoint.Port,
            TimeSpan.FromSeconds(2));

        var snapshot = provider.GetSnapshot();
        await server;

        Assert.Equal(7_074_000ul, snapshot.FrequencyHz);
        Assert.False(snapshot.HasFrequencyRxHz);
        Assert.False(snapshot.HasTxPowerWatts);
    }

    private static async Task ServeAsync(
        TcpListener listener,
        params (string Command, string Response)[] exchanges)
    {
        try
        {
            using var client = await listener.AcceptTcpClientAsync();
            await using var stream = client.GetStream();
            using var reader = new StreamReader(stream, leaveOpen: true);
            await using var writer = new StreamWriter(stream, leaveOpen: true)
            {
                AutoFlush = true,
                NewLine = "\n",
            };

            foreach (var (command, response) in exchanges)
            {
                Assert.Equal(command, await reader.ReadLineAsync());
                await writer.WriteAsync(response);
            }
        }
        finally
        {
            listener.Stop();
        }
    }
}

using Grpc.Net.Client;
using QsoRipper.Cli;
using QsoRipper.Domain;
using QsoRipper.Services;
using QsoRipper.Shared.Formatting;

namespace QsoRipper.Cli.Commands;

internal static class GetQsoCommand
{
    public static async Task<int> RunAsync(GrpcChannel channel, string localId, bool jsonOutput = false)
    {
        var client = new LogbookService.LogbookServiceClient(channel);
        var response = await client.GetQsoAsync(new GetQsoRequest { LocalId = localId });

        if (response.Qso is not { } qso)
        {
            Console.Error.WriteLine($"QSO not found: {localId}");
            return 1;
        }

        if (jsonOutput)
        {
            JsonOutput.Print(response);
            return 0;
        }

        Console.WriteLine($"Local ID:         {qso.LocalId}");
        Console.WriteLine($"Callsign:         {qso.WorkedCallsign}");
        Console.WriteLine($"Station:          {qso.StationCallsign}");
        Console.WriteLine($"Band:             {EnumHelpers.FormatBand(qso.Band)}");
        Console.WriteLine($"Mode:             {EnumHelpers.FormatMode(qso.Mode)}");

        if (qso.UtcTimestamp is not null)
        {
            Console.WriteLine($"UTC:              {qso.UtcTimestamp.ToDateTime():u}");
        }

        if (qso.UtcEndTimestamp is not null)
        {
            Console.WriteLine($"UTC End:          {qso.UtcEndTimestamp.ToDateTime():u}");
        }

        if (ListQsosCommand.FormatDuration(qso) is { } duration)
        {
            Console.WriteLine($"Duration:         {duration}");
        }

        {
            ulong? freqHz = qso.HasFrequencyHz ? qso.FrequencyHz
#pragma warning disable CS0612
                : qso.HasFrequencyKhz ? qso.FrequencyKhz * 1000
#pragma warning restore CS0612
                : null;
            if (freqHz.HasValue)
            {
                Console.WriteLine($"Frequency:        {FrequencyFormatter.FormatMhzWithUnit(freqHz.Value)}");
            }
        }

        if (qso.RstSent is not null)
        {
            Console.WriteLine($"RST Sent:         {ListQsosCommand.FormatRst(qso.RstSent)}");
        }

        if (qso.RstReceived is not null)
        {
            Console.WriteLine($"RST Rcvd:         {ListQsosCommand.FormatRst(qso.RstReceived)}");
        }

        if (qso.HasQrzLogid)
        {
            Console.WriteLine($"QRZ Log ID:       {qso.QrzLogid}");
        }

        return 0;
    }

}

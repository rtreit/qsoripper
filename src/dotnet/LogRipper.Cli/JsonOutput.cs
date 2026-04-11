using Google.Protobuf;

namespace LogRipper.Cli;

internal static class JsonOutput
{
    private static readonly JsonFormatter Formatter = new(JsonFormatter.Settings.Default.WithIndentation());

    public static void Print(IMessage message)
    {
        Console.WriteLine(Formatter.Format(message));
    }
}

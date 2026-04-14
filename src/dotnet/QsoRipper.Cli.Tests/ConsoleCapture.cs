using System.Text;

namespace QsoRipper.Cli.Tests;

internal static class ConsoleCapture
{
    internal static string Out(Action action)
    {
        var builder = new StringBuilder();
        using var writer = new StringWriter(builder);
        var original = Console.Out;

        try
        {
            Console.SetOut(writer);
            action();
        }
        finally
        {
            Console.SetOut(original);
        }

        return builder.ToString();
    }

    internal static string Error(Action action)
    {
        var builder = new StringBuilder();
        using var writer = new StringWriter(builder);
        var original = Console.Error;

        try
        {
            Console.SetError(writer);
            action();
        }
        finally
        {
            Console.SetError(original);
        }

        return builder.ToString();
    }
}

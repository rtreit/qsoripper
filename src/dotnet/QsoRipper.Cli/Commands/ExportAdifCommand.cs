using Grpc.Core;
using Grpc.Net.Client;
using QsoRipper.EngineSelection;
using QsoRipper.Services;

namespace QsoRipper.Cli.Commands;

internal static class ExportAdifCommand
{
    private const string FallbackLogbookName = "qsoripper-logbook";
    private const int MaxCollisionSuffix = 999;

    public static async Task<int> RunAsync(
        GrpcChannel channel,
        string[] args,
        CancellationToken cancellationToken = default)
    {
        string? outputFile = null;
        var includeHeader = false;

        for (var i = 0; i < args.Length; i++)
        {
            switch (args[i])
            {
                case "--file" when i < args.Length - 1:
                    outputFile = args[++i];
                    break;
                case "--file":
                    Console.Error.WriteLine("Missing value for --file.");
                    return 1;
                case "--include-header":
                    includeHeader = true;
                    break;
            }
        }

        string? resolvedOutputFile;
        try
        {
            resolvedOutputFile = ResolveOutputFile(
                outputFile,
                ResolveConfiguredLogPath(),
                DateOnly.FromDateTime(DateTime.Now));
        }
        catch (Exception ex) when (ex is ArgumentException
                                      or DirectoryNotFoundException
                                      or IOException
                                      or NotSupportedException
                                      or UnauthorizedAccessException)
        {
            Console.Error.WriteLine(ex.Message);
            return 1;
        }

        if (resolvedOutputFile is not null)
        {
            try
            {
                using var output = OpenOutputFile(resolvedOutputFile);
                await WriteExportAsync(channel, includeHeader, output, cancellationToken);
            }
            catch (Exception ex) when (ex is ArgumentException
                                          or DirectoryNotFoundException
                                          or InvalidOperationException
                                          or IOException
                                          or NotSupportedException
                                          or UnauthorizedAccessException)
            {
                Console.Error.WriteLine(ex.Message);
                return 1;
            }

            Console.WriteLine($"Exported to {resolvedOutputFile}");
            return 0;
        }

        using var stdout = Console.OpenStandardOutput();
        await WriteExportAsync(channel, includeHeader, stdout, cancellationToken);
        return 0;
    }

    private static async Task WriteExportAsync(
        GrpcChannel channel,
        bool includeHeader,
        Stream output,
        CancellationToken cancellationToken)
    {
        var client = new LogbookService.LogbookServiceClient(channel);
        using var call = client.ExportAdif(
            new ExportAdifRequest { IncludeHeader = includeHeader },
            cancellationToken: cancellationToken);

        while (await call.ResponseStream.MoveNext(cancellationToken))
        {
            var chunk = call.ResponseStream.Current.Chunk;
            if (chunk is not null)
            {
                await output.WriteAsync(chunk.Data.Memory, cancellationToken);
            }
        }
    }

    internal static string? ResolveOutputFile(string? outputFile, string? configuredLogPath, DateOnly exportDate)
    {
        if (string.IsNullOrWhiteSpace(outputFile))
        {
            return null;
        }

        var trimmed = outputFile.Trim();
        if (Directory.Exists(trimmed) || Path.EndsInDirectorySeparator(trimmed))
        {
            Directory.CreateDirectory(trimmed);
            var baseName = ResolveLogbookBaseName(configuredLogPath);
            var datedName = $"{baseName}-{exportDate:yyyy-MM-dd}";
            return ResolveNonCollidingFilePath(trimmed, datedName);
        }

        return trimmed;
    }

    private static FileStream OpenOutputFile(string outputFile)
    {
        try
        {
            var parent = Path.GetDirectoryName(Path.GetFullPath(outputFile));
            if (!string.IsNullOrWhiteSpace(parent) && !Directory.Exists(parent))
            {
                throw new DirectoryNotFoundException(
                    $"Export destination directory does not exist: {parent}");
            }

            return new FileStream(outputFile, FileMode.Create, FileAccess.Write);
        }
        catch (Exception ex) when (ex is ArgumentException
                                      or DirectoryNotFoundException
                                      or IOException
                                      or NotSupportedException
                                      or UnauthorizedAccessException)
        {
            throw new InvalidOperationException($"Unable to open ADIF export file '{outputFile}': {ex.Message}", ex);
        }
    }

    private static string? ResolveConfiguredLogPath()
    {
        return FirstNonEmptyEnvironmentValue(
            "QSORIPPER_SQLITE_PATH",
            "QSORIPPER_STORAGE_PATH",
            PersistenceSetup.LegacyPathEnvironmentVariable);
    }

    private static string? FirstNonEmptyEnvironmentValue(params string[] names)
    {
        foreach (var name in names)
        {
            var value = Environment.GetEnvironmentVariable(name);
            if (!string.IsNullOrWhiteSpace(value))
            {
                return value.Trim();
            }
        }

        return null;
    }

    private static string ResolveLogbookBaseName(string? configuredLogPath)
    {
        var fileName = string.IsNullOrWhiteSpace(configuredLogPath)
            ? null
            : GetFileNameWithoutExtensionPortable(configuredLogPath);
        return SanitizeFileName(string.IsNullOrWhiteSpace(fileName) ? FallbackLogbookName : fileName);
    }

    private static string? GetFileNameWithoutExtensionPortable(string path)
    {
        var trimmed = path.Trim().TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar, '\\', '/');
        var fileName = trimmed
            .Split(['\\', '/'], StringSplitOptions.RemoveEmptyEntries)
            .LastOrDefault();
        return string.IsNullOrWhiteSpace(fileName) ? null : Path.GetFileNameWithoutExtension(fileName);
    }

    private static string SanitizeFileName(string value)
    {
        var invalid = Path.GetInvalidFileNameChars()
            .Concat(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
            .ToHashSet();
        var chars = value
            .Trim()
            .Select(ch => invalid.Contains(ch) ? '-' : ch)
            .ToArray();
        var sanitized = new string(chars).Trim('.', ' ', '-');
        return string.IsNullOrWhiteSpace(sanitized) ? FallbackLogbookName : sanitized;
    }

    private static string ResolveNonCollidingFilePath(string directory, string datedName)
    {
        var first = Path.Combine(directory, $"{datedName}.adi");
        if (!File.Exists(first))
        {
            return first;
        }

        for (var suffix = 1; suffix <= MaxCollisionSuffix; suffix++)
        {
            var candidate = Path.Combine(directory, $"{datedName}-{suffix:000}.adi");
            if (!File.Exists(candidate))
            {
                return candidate;
            }
        }

        throw new IOException($"Could not find an available export filename in '{directory}' for '{datedName}'.");
    }
}

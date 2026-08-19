using System.Globalization;
using QsoRipper.Domain;

namespace QsoRipper.Engine.DotNet.Wsjtx;

internal static class WsjtxImportDiagnostic
{
    internal const string ExtraFieldKey = "APP_QSORIPPER_WSJTX_IMPORT";
    internal const string NotePrefix = "QsoRipper WSJT-X import:";
    internal const string UdpSource = "udp_logged_adif";
    internal const string AdifTailSource = "adif_tail";

    internal static string Create(string source, QsoRecord qso, DateTimeOffset importedAt)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(source);
        ArgumentNullException.ThrowIfNull(qso);

        var importedAtText = importedAt.ToUniversalTime().ToString(
            "yyyy-MM-dd'T'HH:mm:ss.fff'Z'",
            CultureInfo.InvariantCulture);
        var endDelta = QsoEndToImportMilliseconds(qso, importedAt);
        return $"{NotePrefix} imported_at_utc={importedAtText}, qso_end_to_import_ms={endDelta}, source={source}, engine=dotnet";
    }

    internal static string Resolve(QsoRecord existing, string current)
    {
        ArgumentNullException.ThrowIfNull(existing);
        ArgumentException.ThrowIfNullOrWhiteSpace(current);

        return existing.ExtraFields.TryGetValue(ExtraFieldKey, out var stored)
            && !string.IsNullOrWhiteSpace(stored)
                ? stored
                : current;
    }

    internal static void Apply(QsoRecord qso, string diagnostic)
    {
        ArgumentNullException.ThrowIfNull(qso);
        ArgumentException.ThrowIfNullOrWhiteSpace(diagnostic);

        var retainedLines = qso.HasNotes
            ? qso.Notes
                .Replace("\r\n", "\n", StringComparison.Ordinal)
                .Split('\n')
                .Where(static line => !line.StartsWith(NotePrefix, StringComparison.Ordinal))
            : Array.Empty<string>();
        var notes = string.Join('\n', retainedLines);
        qso.Notes = string.IsNullOrEmpty(notes)
            ? diagnostic
            : $"{notes}\n{diagnostic}";
        qso.ExtraFields[ExtraFieldKey] = diagnostic;
    }

    private static string QsoEndToImportMilliseconds(QsoRecord qso, DateTimeOffset importedAt)
    {
        if (qso.UtcEndTimestamp is null)
        {
            return "unavailable";
        }

        try
        {
            var milliseconds = Math.Truncate(
                (importedAt.ToUniversalTime() - qso.UtcEndTimestamp.ToDateTimeOffset()).TotalMilliseconds);
            return checked((long)milliseconds).ToString(CultureInfo.InvariantCulture);
        }
        catch (ArgumentOutOfRangeException)
        {
            return "unavailable";
        }
        catch (OverflowException)
        {
            return "unavailable";
        }
    }
}

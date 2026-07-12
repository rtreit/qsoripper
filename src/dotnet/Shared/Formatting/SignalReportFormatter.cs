using System.Globalization;
using QsoRipper.Domain;

namespace QsoRipper.Shared.Formatting;

internal static class SignalReportFormatter
{
    public static string Format(RstReport? report, string missingValue = "")
    {
        if (report is null)
        {
            return missingValue;
        }

        var raw = report.Raw?.Trim();
        if (!string.IsNullOrEmpty(raw))
        {
            return raw;
        }

        if (!report.HasReadability || !report.HasStrength)
        {
            return missingValue;
        }

        return report.HasTone
            ? $"{report.Readability}{report.Strength}{report.Tone}"
            : $"{report.Readability}{report.Strength}";
    }

    public static bool TryParse(string? value, out RstReport report)
    {
        var normalized = value?.Trim() ?? string.Empty;
        report = new RstReport();
        if (normalized.Length == 0)
        {
            return false;
        }

        if (IsSignedDigitalReport(normalized))
        {
            report.Raw = normalized;
            return true;
        }

        if (normalized.Length is not (2 or 3)
            || normalized.Any(static character => !char.IsAsciiDigit(character)))
        {
            return false;
        }

        report.Raw = normalized;
        report.Readability = (uint)(normalized[0] - '0');
        report.Strength = (uint)(normalized[1] - '0');
        if (normalized.Length == 3)
        {
            report.Tone = (uint)(normalized[2] - '0');
        }

        return true;
    }

    private static bool IsSignedDigitalReport(string value)
    {
        if (value.Length is not (2 or 3)
            || value[0] is not ('+' or '-')
            || value[1..].Any(static character => !char.IsAsciiDigit(character)))
        {
            return false;
        }

        return int.TryParse(value, NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture, out var report)
               && report is >= -50 and <= 49;
    }
}

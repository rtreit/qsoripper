using System.Globalization;

namespace QsoRipper.Shared.Formatting;

internal static class FrequencyFormatter
{
    public static string FormatMhz(ulong hz)
    {
        var whole = hz / 1_000_000;
        var frac = hz % 1_000_000;
        var full = string.Create(
            CultureInfo.InvariantCulture,
            $"{whole}.{frac:000000}");
        var dotPos = full.IndexOf('.', StringComparison.Ordinal);
        var minLen = dotPos + 4;
        var trimmed = full.AsSpan().TrimEnd('0');
        var end = Math.Max(trimmed.Length, minLen);
        return full[..end];
    }

    public static string FormatMhzWithUnit(ulong hz) => $"{FormatMhz(hz)} MHz";
}

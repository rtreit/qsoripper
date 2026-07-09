using System.Globalization;

namespace QsoRipper.Shared.Formatting;

internal static class FrequencyFormatter
{
    public static string FormatMhz(ulong hz)
    {
        var whole = hz / 1_000_000;
        var khz = hz % 1_000_000 / 1_000;
        var fractionalHz = hz % 1_000;
        return $"{whole}.{khz:000}.{fractionalHz:000}";
    }

    public static string FormatMhzWithUnit(ulong hz) => $"{FormatMhz(hz)} MHz";

    public static string FormatDecimalMhz(ulong hz)
    {
        var whole = hz / 1_000_000;
        var frac = hz % 1_000_000;
        return $"{whole}.{frac:000000}";
    }

    public static bool TryParseMhzToHz(string? value, out ulong hz)
    {
        hz = 0;
        var normalized = value?.Trim();
        if (string.IsNullOrWhiteSpace(normalized))
        {
            return false;
        }

        if (TryParseRadioStyle(normalized, out hz))
        {
            return true;
        }

        if (decimal.TryParse(normalized, NumberStyles.Float, CultureInfo.InvariantCulture, out var mhz) && mhz > 0)
        {
            var hzDecimal = decimal.Round(mhz * 1_000_000m, 0, MidpointRounding.AwayFromZero);
            if (hzDecimal > 0 && hzDecimal <= ulong.MaxValue)
            {
                hz = (ulong)hzDecimal;
                return true;
            }
        }

        return false;
    }

    private static bool TryParseRadioStyle(string value, out ulong hz)
    {
        hz = 0;
        var parts = value.Split('.');
        if (parts.Length != 3
            || !ulong.TryParse(parts[0], NumberStyles.None, CultureInfo.InvariantCulture, out var mhz)
            || !ulong.TryParse(parts[1], NumberStyles.None, CultureInfo.InvariantCulture, out var khz)
            || !ulong.TryParse(parts[2], NumberStyles.None, CultureInfo.InvariantCulture, out var fractionalHz)
            || parts[2].Length > 3
            || khz > 999
            || fractionalHz > 999)
        {
            return false;
        }

        if (parts[2].Length <= 2)
        {
            fractionalHz *= 10;
        }

        var remainder = khz * 1_000 + fractionalHz;
        if (mhz > (ulong.MaxValue - remainder) / 1_000_000)
        {
            return false;
        }

        hz = mhz * 1_000_000 + remainder;
        return hz > 0;
    }
}

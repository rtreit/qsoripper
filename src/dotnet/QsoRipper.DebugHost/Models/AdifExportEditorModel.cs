using System.ComponentModel.DataAnnotations;
using System.Globalization;
using Google.Protobuf.WellKnownTypes;
using QsoRipper.Services;

namespace QsoRipper.DebugHost.Models;

internal sealed class AdifExportEditorModel : IValidatableObject
{
    public string? AfterUtc { get; set; }

    public string? BeforeUtc { get; set; }

    public string? ContestId { get; set; }

    public bool IncludeHeader { get; set; } = true;

    public ExportAdifRequest ToRequest()
    {
        var request = new ExportAdifRequest
        {
            IncludeHeader = IncludeHeader
        };

        if (TryParseTimestamp(AfterUtc, out var after))
        {
            request.After = after;
        }

        if (TryParseTimestamp(BeforeUtc, out var before))
        {
            request.Before = before;
        }

        var contestId = NormalizeOptional(ContestId);
        if (contestId is not null)
        {
            request.ContestId = contestId;
        }

        return request;
    }

    public IEnumerable<ValidationResult> Validate(ValidationContext validationContext)
    {
        var afterRaw = NormalizeOptional(AfterUtc);
        var beforeRaw = NormalizeOptional(BeforeUtc);

        var afterParsed = afterRaw is null
            ? null
            : TryParseTimestamp(afterRaw, out var after)
                ? after
                : null;
        if (afterRaw is not null && afterParsed is null)
        {
            yield return new ValidationResult(
                "After must be a valid UTC timestamp such as 2026-04-13T00:00:00Z.",
                [nameof(AfterUtc)]);
        }

        var beforeParsed = beforeRaw is null
            ? null
            : TryParseTimestamp(beforeRaw, out var before)
                ? before
                : null;
        if (beforeRaw is not null && beforeParsed is null)
        {
            yield return new ValidationResult(
                "Before must be a valid UTC timestamp such as 2026-04-13T23:59:59Z.",
                [nameof(BeforeUtc)]);
        }

        if (afterParsed is not null && beforeParsed is not null && beforeParsed.ToDateTime() <= afterParsed.ToDateTime())
        {
            yield return new ValidationResult(
                "Before must be later than After.",
                [nameof(BeforeUtc)]);
        }
    }

    private static bool TryParseTimestamp(string? raw, out Timestamp? timestamp)
    {
        timestamp = null;

        var normalized = NormalizeOptional(raw);
        if (normalized is null)
        {
            return false;
        }

        if (!DateTimeOffset.TryParse(
                normalized,
                CultureInfo.InvariantCulture,
                DateTimeStyles.AssumeUniversal | DateTimeStyles.AdjustToUniversal,
                out var parsed))
        {
            return false;
        }

        timestamp = Timestamp.FromDateTime(parsed.UtcDateTime);
        return true;
    }

    private static string? NormalizeOptional(string? value)
    {
        return string.IsNullOrWhiteSpace(value)
            ? null
            : value.Trim();
    }
}

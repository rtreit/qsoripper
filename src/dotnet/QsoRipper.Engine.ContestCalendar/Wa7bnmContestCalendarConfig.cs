using System.Diagnostics.CodeAnalysis;

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// Configuration for the WA7BNM contest calendar provider.
/// </summary>
public sealed class Wa7bnmContestCalendarConfig
{
    /// <summary>Environment variable controlling whether fetching is enabled.</summary>
    public const string EnabledEnvVar = "QSORIPPER_CONTEST_CALENDAR_ENABLED";

    /// <summary>Environment variable overriding the RSS URL.</summary>
    public const string RssUrlEnvVar = "QSORIPPER_CONTEST_CALENDAR_RSS_URL";

    /// <summary>Environment variable overriding the HTTP timeout in seconds.</summary>
    public const string TimeoutEnvVar = "QSORIPPER_CONTEST_CALENDAR_HTTP_TIMEOUT_SECONDS";

    /// <summary>Environment variable overriding the refresh interval in seconds.</summary>
    public const string RefreshIntervalEnvVar = "QSORIPPER_CONTEST_CALENDAR_REFRESH_INTERVAL_SECONDS";

    /// <summary>Environment variable overriding the stale-after threshold in seconds.</summary>
    public const string StaleAfterEnvVar = "QSORIPPER_CONTEST_CALENDAR_STALE_AFTER_SECONDS";

    /// <summary>Environment variable pointing at a reviewed local contest details catalog.</summary>
    public const string DetailsPathEnvVar = "QSORIPPER_CONTEST_CALENDAR_DETAILS_PATH";

    /// <summary>Default reviewed local contest details catalog path.</summary>
    public static readonly string DefaultDetailsPath = Path.Combine("data", "contest-calendar", "contest-details.json");

    /// <summary>Default WA7BNM RSS endpoint.</summary>
    public const string DefaultRssUrl = "https://www.contestcalendar.com/calendar.rss";

    /// <summary>Default HTTP timeout.</summary>
    public const int DefaultTimeoutSeconds = 8;

    /// <summary>Default refresh interval.</summary>
    public const int DefaultRefreshIntervalSeconds = 3600;

    /// <summary>Default stale-after threshold.</summary>
    public const int DefaultStaleAfterSeconds = 86400;

    /// <summary>Gets or sets whether contest calendar fetching is enabled.</summary>
    public bool Enabled { get; init; } = true;

    /// <summary>Gets or sets the RSS endpoint URL.</summary>
    [SuppressMessage("Design", "CA1056:URI-like properties should not be strings", Justification = "Stored as string from env vars; passed directly to HttpRequestMessage.")]
    public string RssUrl { get; init; } = DefaultRssUrl;

    /// <summary>Gets or sets the HTTP request timeout.</summary>
    public TimeSpan HttpTimeout { get; init; } = TimeSpan.FromSeconds(DefaultTimeoutSeconds);

    /// <summary>Gets or sets the snapshot refresh interval.</summary>
    public TimeSpan RefreshInterval { get; init; } = TimeSpan.FromSeconds(DefaultRefreshIntervalSeconds);

    /// <summary>Gets or sets the stale-after threshold.</summary>
    public TimeSpan StaleAfter { get; init; } = TimeSpan.FromSeconds(DefaultStaleAfterSeconds);

    /// <summary>Gets or sets the optional reviewed local details catalog path.</summary>
    public string DetailsPath { get; init; } = DefaultDetailsPath;

    /// <summary>Gets or sets whether the details path was explicitly configured.</summary>
    public bool DetailsPathIsExplicit { get; init; }

    /// <summary>Load provider configuration from environment variables.</summary>
    public static Wa7bnmContestCalendarConfig FromEnvironment()
    {
        return new Wa7bnmContestCalendarConfig
        {
            Enabled = ParseBool(Environment.GetEnvironmentVariable(EnabledEnvVar), defaultValue: true),
            RssUrl = Environment.GetEnvironmentVariable(RssUrlEnvVar)?.Trim() is { Length: > 0 } rssUrl
                ? rssUrl
                : DefaultRssUrl,
            HttpTimeout = TimeSpan.FromSeconds(ParseSeconds(Environment.GetEnvironmentVariable(TimeoutEnvVar), DefaultTimeoutSeconds)),
            RefreshInterval = TimeSpan.FromSeconds(ParseSeconds(Environment.GetEnvironmentVariable(RefreshIntervalEnvVar), DefaultRefreshIntervalSeconds)),
            StaleAfter = TimeSpan.FromSeconds(ParseSeconds(Environment.GetEnvironmentVariable(StaleAfterEnvVar), DefaultStaleAfterSeconds)),
            DetailsPath = Environment.GetEnvironmentVariable(DetailsPathEnvVar)?.Trim() is { Length: > 0 } detailsPath
                ? detailsPath
                : DefaultDetailsPath,
            DetailsPathIsExplicit = Environment.GetEnvironmentVariable(DetailsPathEnvVar)?.Trim() is { Length: > 0 },
        };
    }

    private static bool ParseBool(string? raw, bool defaultValue)
    {
        if (string.IsNullOrWhiteSpace(raw))
        {
            return defaultValue;
        }

        return raw.Trim().ToUpperInvariant() switch
        {
            "1" or "TRUE" or "YES" or "Y" or "ON" => true,
            "0" or "FALSE" or "NO" or "N" or "OFF" => false,
            _ => defaultValue,
        };
    }

    private static int ParseSeconds(string? raw, int defaultValue)
    {
        return string.IsNullOrWhiteSpace(raw) || !int.TryParse(raw.Trim(), System.Globalization.NumberStyles.Integer, System.Globalization.CultureInfo.InvariantCulture, out var value)
            ? defaultValue
            : value;
    }
}

namespace QsoRipper.Engine.ContestCalendar;

/// <summary>
/// Stable error categories for the contest calendar provider layer.
/// </summary>
public enum ContestCalendarProviderErrorKind
{
    /// <summary>Provider is disabled by configuration.</summary>
    Disabled,

    /// <summary>Transport failed before a valid response was received.</summary>
    Transport,

    /// <summary>Provider returned a payload that could not be parsed.</summary>
    Parse,
}

/// <summary>
/// Exception surfaced by the contest calendar provider layer.
/// </summary>
#pragma warning disable CA1032, RCS1194 // Standard constructors intentionally omitted; use factory methods.
public sealed class ContestCalendarProviderException : Exception
#pragma warning restore CA1032, RCS1194
{
    /// <summary>Gets the stable error category.</summary>
    public ContestCalendarProviderErrorKind Kind { get; }

    /// <summary>Gets whether the error class is suitable for retry handling.</summary>
    public bool IsRetryable => Kind == ContestCalendarProviderErrorKind.Transport;

    private ContestCalendarProviderException(ContestCalendarProviderErrorKind kind, string message)
        : base(message)
    {
        Kind = kind;
    }

    /// <summary>Create an exception indicating the provider is disabled.</summary>
    public static ContestCalendarProviderException Disabled(string message) =>
        new(ContestCalendarProviderErrorKind.Disabled, message);

    /// <summary>Create an exception indicating a transport failure.</summary>
    public static ContestCalendarProviderException Transport(string message) =>
        new(ContestCalendarProviderErrorKind.Transport, message);

    /// <summary>Create an exception indicating a parse failure.</summary>
    public static ContestCalendarProviderException Parse(string message) =>
        new(ContestCalendarProviderErrorKind.Parse, message);
}

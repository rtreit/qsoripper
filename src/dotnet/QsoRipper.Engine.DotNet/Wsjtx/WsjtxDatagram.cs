using System.Buffers.Binary;
using System.Text;

namespace QsoRipper.Engine.DotNet.Wsjtx;

/// <summary>
/// Classification of a WSJT-X UDP datagram parse attempt, mirroring the Rust
/// <c>extract_wsjtx_logged_adif</c> contract in qsoripper-core.
/// </summary>
internal enum WsjtxDatagramParseStatus
{
    /// <summary>A Logged ADIF datagram with a usable ADIF payload was parsed.</summary>
    Logged,

    /// <summary>A non-magic or non-Logged-ADIF datagram that should be ignored (skipped, not an error).</summary>
    Ignored,

    /// <summary>A magic-framed Logged ADIF datagram that was malformed and should count as a parse error.</summary>
    Malformed,
}

/// <summary>
/// Result of attempting to parse a WSJT-X Logged ADIF UDP datagram.
/// </summary>
internal readonly record struct WsjtxDatagramParseResult(
    WsjtxDatagramParseStatus Status,
    byte[]? Adif,
    string? Error);

/// <summary>
/// Parser for WSJT-X UDP datagrams. Matches the Rust production entry point
/// <c>extract_wsjtx_logged_adif</c>: only the magic-framed Logged ADIF message
/// (message type 12) is accepted; other message types are ignored.
/// </summary>
internal static class WsjtxDatagram
{
    private const uint WsjtxMagic = 0xADBC_CBDA;
    private const uint WsjtxLoggedAdifMessageType = 12;
    private const uint WsjtxNullStringLength = uint.MaxValue;

    private static readonly UTF8Encoding StrictUtf8 = new(encoderShouldEmitUTF8Identifier: false, throwOnInvalidBytes: true);

    /// <summary>
    /// Attempts to extract the ADIF payload from a WSJT-X Logged ADIF datagram.
    /// </summary>
    /// <remarks>
    /// Returns <see cref="WsjtxDatagramParseStatus.Ignored"/> for datagrams that are too short for the
    /// magic header, do not carry the WSJT-X magic, or are not a Logged ADIF message. Returns
    /// <see cref="WsjtxDatagramParseStatus.Malformed"/> for magic-framed Logged ADIF datagrams whose
    /// fields are truncated, non-UTF-8, or whose ADIF payload is empty. Returns
    /// <see cref="WsjtxDatagramParseStatus.Logged"/> with the decoded ADIF bytes on success.
    /// </remarks>
    public static WsjtxDatagramParseResult TryParseLoggedAdif(ReadOnlySpan<byte> datagram)
    {
        if (!TryReadBeU32(datagram, 0, out var magic))
        {
            return Ignored();
        }

        if (magic != WsjtxMagic)
        {
            return Ignored();
        }

        if (!TryReadBeU32(datagram, 4, out _))
        {
            return Malformed("WSJT-X datagram is missing schema field");
        }

        if (!TryReadBeU32(datagram, 8, out var messageType))
        {
            return Malformed("WSJT-X datagram is missing message type field");
        }

        if (messageType != WsjtxLoggedAdifMessageType)
        {
            return Ignored();
        }

        var cursor = 12;
        if (!TryReadWsjtxUtf8(datagram, ref cursor, out _, out var idError))
        {
            return Malformed(idError!);
        }

        if (!TryReadWsjtxUtf8(datagram, ref cursor, out var adif, out var adifError))
        {
            return Malformed(adifError!);
        }

        if (string.IsNullOrWhiteSpace(adif))
        {
            return Malformed("WSJT-X Logged ADIF datagram has an empty ADIF payload");
        }

        return new WsjtxDatagramParseResult(
            WsjtxDatagramParseStatus.Logged,
            Encoding.UTF8.GetBytes(adif),
            Error: null);
    }

    private static WsjtxDatagramParseResult Ignored()
    {
        return new WsjtxDatagramParseResult(WsjtxDatagramParseStatus.Ignored, Adif: null, Error: null);
    }

    private static WsjtxDatagramParseResult Malformed(string error)
    {
        return new WsjtxDatagramParseResult(WsjtxDatagramParseStatus.Malformed, Adif: null, error);
    }

    private static bool TryReadBeU32(ReadOnlySpan<byte> bytes, int offset, out uint value)
    {
        value = 0;
        if (offset < 0)
        {
            return false;
        }

        var end = (long)offset + 4;
        if (end > bytes.Length)
        {
            return false;
        }

        value = BinaryPrimitives.ReadUInt32BigEndian(bytes.Slice(offset, 4));
        return true;
    }

    private static bool TryReadWsjtxUtf8(ReadOnlySpan<byte> bytes, ref int cursor, out string value, out string? error)
    {
        value = string.Empty;
        error = null;

        if (!TryReadBeU32(bytes, cursor, out var length))
        {
            error = "WSJT-X datagram is missing string length";
            return false;
        }

        cursor += 4;
        if (length == WsjtxNullStringLength)
        {
            value = string.Empty;
            return true;
        }

        var end = (long)cursor + length;
        if (end > bytes.Length)
        {
            error = "WSJT-X datagram string extends past packet end";
            return false;
        }

        var slice = bytes.Slice(cursor, (int)length);
        try
        {
            value = StrictUtf8.GetString(slice);
        }
        catch (DecoderFallbackException ex)
        {
            error = $"WSJT-X datagram string is not UTF-8: {ex.Message}";
            return false;
        }

        cursor = (int)end;
        return true;
    }
}

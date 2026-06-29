using System.Text;

namespace QsoRipper.Engine.DotNet.Wsjtx;

/// <summary>
/// Byte-offset cursor helpers for tailing a WSJT-X <c>wsjtx_log.adi</c> file.
/// Mirrors the Rust <c>complete_adif_prefix_len</c> family in qsoripper-core so
/// the .NET engine advances its cursor identically (recovery-only, dedupe-safe).
/// </summary>
internal static class WsjtxAdifTail
{
    /// <summary>
    /// Returns the byte length of the complete ADIF prefix that ends at the last
    /// <c>&lt;eor&gt;</c> (case-insensitive) record terminator, or <see langword="null"/> when no
    /// complete record is present.
    /// </summary>
    /// <remarks>
    /// A field tag <c>&lt;name:len&gt;</c> advances the cursor by <paramref name="bytes"/> measured in
    /// Unicode scalar values (computed over UTF-8), NOT bytes, so a literal <c>&lt;EOR&gt;</c> appearing
    /// inside a field value cannot prematurely terminate a record.
    /// </remarks>
    public static int? CompleteAdifPrefixLength(ReadOnlySpan<byte> bytes)
    {
        int? lastEnd = null;
        var cursor = 0;

        while (cursor < bytes.Length)
        {
            var remaining = bytes[cursor..];
            var tagStartOffset = remaining.IndexOf((byte)'<');
            if (tagStartOffset < 0)
            {
                break;
            }

            var tagStart = cursor + tagStartOffset;
            var tagAndRest = bytes[tagStart..];
            var tagEndOffset = tagAndRest.IndexOf((byte)'>');
            if (tagEndOffset < 0)
            {
                break;
            }

            var tagEnd = tagStart + tagEndOffset;
            var tagBody = bytes[(tagStart + 1)..tagEnd];
            var fieldName = AdifTagName(tagBody);
            cursor = tagEnd + 1;

            if (EqualsIgnoreAsciiCase(fieldName, "eor"))
            {
                lastEnd = cursor;
                continue;
            }

            if (AdifFieldLength(tagBody) is { } fieldLen)
            {
                var nextCursor = CursorAfterUtf8Chars(bytes, cursor, fieldLen);
                if (nextCursor is null)
                {
                    break;
                }

                cursor = nextCursor.Value;
            }
        }

        return lastEnd;
    }

    private static int? CursorAfterUtf8Chars(ReadOnlySpan<byte> bytes, int start, int charCount)
    {
        if (start > bytes.Length)
        {
            return null;
        }

        var slice = bytes[start..];

        // Mirror Rust's std::str::from_utf8(...).ok()? — the remaining slice must be valid UTF-8.
        if (!Utf8IsValid(slice))
        {
            return null;
        }

        var endOffset = 0;
        for (var index = 0; index < charCount; index++)
        {
            var status = System.Text.Rune.DecodeFromUtf8(slice[endOffset..], out var rune, out var consumed);
            if (status != System.Buffers.OperationStatus.Done)
            {
                return null;
            }

            endOffset += consumed;
        }

        return start + endOffset;
    }

    private static bool Utf8IsValid(ReadOnlySpan<byte> bytes)
    {
        var span = bytes;
        while (!span.IsEmpty)
        {
            var status = System.Text.Rune.DecodeFromUtf8(span, out _, out var consumed);
            if (status != System.Buffers.OperationStatus.Done)
            {
                return false;
            }

            span = span[consumed..];
        }

        return true;
    }

    private static ReadOnlySpan<byte> AdifTagName(ReadOnlySpan<byte> tagBody)
    {
        for (var index = 0; index < tagBody.Length; index++)
        {
            var b = tagBody[index];
            if (b == (byte)':' || IsAsciiWhitespace(b))
            {
                return tagBody[..index];
            }
        }

        return tagBody;
    }

    private static int? AdifFieldLength(ReadOnlySpan<byte> tagBody)
    {
        var colon = tagBody.IndexOf((byte)':');
        if (colon < 0)
        {
            return null;
        }

        long length = 0;
        var sawDigit = false;
        for (var index = colon + 1; index < tagBody.Length; index++)
        {
            var b = tagBody[index];
            if (b == (byte)':')
            {
                break;
            }

            if (b < (byte)'0' || b > (byte)'9')
            {
                return null;
            }

            sawDigit = true;
            length = (length * 10) + (b - (byte)'0');
            if (length > int.MaxValue)
            {
                return null;
            }
        }

        return sawDigit ? (int)length : null;
    }

    private static bool EqualsIgnoreAsciiCase(ReadOnlySpan<byte> value, string ascii)
    {
        if (value.Length != ascii.Length)
        {
            return false;
        }

        for (var index = 0; index < value.Length; index++)
        {
            if (ToAsciiLower(value[index]) != ToAsciiLower((byte)ascii[index]))
            {
                return false;
            }
        }

        return true;
    }

    private static byte ToAsciiLower(byte b)
    {
        return b is >= (byte)'A' and <= (byte)'Z' ? (byte)(b + 32) : b;
    }

    private static bool IsAsciiWhitespace(byte b)
    {
        // Matches Rust u8::is_ascii_whitespace: space, tab, line feed, form feed, carriage return.
        return b is (byte)' ' or (byte)'\t' or (byte)'\n' or 0x0C or (byte)'\r';
    }
}

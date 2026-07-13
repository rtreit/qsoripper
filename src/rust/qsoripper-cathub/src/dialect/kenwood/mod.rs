//! Kenwood-family client dialects and shared wire helpers.
//!
//! [`Ts590Dialect`](ts590::Ts590Dialect) is the native pass-through dialect (N1MM, ARCP-590,
//! Log4OM-as-TS590). [`Ts2000Dialect`](ts2000::Ts2000Dialect) is the OmniRig/HDSDR translator.
//! Both share the small frame helpers below.

pub(crate) mod transparent;
pub(crate) mod ts2000;
pub(crate) mod ts590;

use crate::model::Mode;

/// The Kenwood error reply.
pub(crate) const ERR: &[u8] = b"?;";

/// Split a `;`-terminated command frame into its leading alphabetic verb and payload.
///
/// `b"FA00007050000;"` becomes `(b"FA", b"00007050000")`; `b"TX;"` becomes `(b"TX", b"")`.
/// Returns `None` when there is no alphabetic verb.
pub(crate) fn parse_command(frame: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let body = match frame.iter().position(|&b| b == b';') {
        Some(end) => frame.get(..end)?,
        None => frame,
    };
    let verb_len = body
        .iter()
        .position(|b| !b.is_ascii_alphabetic())
        .unwrap_or(body.len());
    if verb_len == 0 {
        return None;
    }
    let verb = body.get(..verb_len)?.to_vec();
    let payload = body.get(verb_len..).unwrap_or(&[]).to_vec();
    Some((verb, payload))
}

/// The Kenwood `MD` mode digit (ASCII byte) for a mode.
pub(crate) fn mode_to_digit(mode: Mode) -> u8 {
    mode.to_kenwood_digit()
}

/// The mode for a Kenwood `MD` mode digit (ASCII byte).
pub(crate) fn mode_from_digit(digit: u8) -> Mode {
    Mode::from_kenwood_digit(digit)
}

/// Build a Kenwood `AI` auto-information status frame (`AI0;` or `AI2;`).
///
/// `AI;` is a *read* on Kenwood radios: a native client (ARCP-590, N1MM) queries the
/// current auto-information mode during connection and waits for a valid `AI<n>;` answer
/// before it proceeds. The reply must report the endpoint's current virtualized state without
/// changing it; only an `AI<n>;` *write* toggles auto-information.
pub(crate) fn ai_frame(on: bool) -> Vec<u8> {
    vec![b'A', b'I', if on { b'2' } else { b'0' }, b';']
}

/// Build a Kenwood frequency frame: `verb` + 11 zero-padded digits + `;`.
pub(crate) fn freq_frame(verb: &[u8], hz: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(verb.len() + 12);
    out.extend_from_slice(verb);
    out.extend_from_slice(format!("{hz:011}").as_bytes());
    out.push(b';');
    out
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn parse_command_splits_verb_and_payload() {
        assert_eq!(
            parse_command(b"FA00007050000;"),
            Some((b"FA".to_vec(), b"00007050000".to_vec()))
        );
        assert_eq!(parse_command(b"TX;"), Some((b"TX".to_vec(), b"".to_vec())));
        assert_eq!(
            parse_command(b"AI2;"),
            Some((b"AI".to_vec(), b"2".to_vec()))
        );
        assert_eq!(parse_command(b"FA;"), Some((b"FA".to_vec(), b"".to_vec())));
        assert_eq!(parse_command(b";"), None);
    }

    #[test]
    fn freq_frame_is_eleven_digits() {
        assert_eq!(freq_frame(b"FA", 7_050_000), b"FA00007050000;".to_vec());
        assert_eq!(freq_frame(b"FB", 0), b"FB00000000000;".to_vec());
    }

    #[test]
    fn mode_digit_helpers_round_trip() {
        assert_eq!(mode_from_digit(mode_to_digit(Mode::Cw)), Mode::Cw);
        assert_eq!(mode_to_digit(Mode::Usb), b'2');
    }
}

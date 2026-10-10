//! Input datagrams: the page's controller state, sent unreliably.
//!
//! Media datagrams run streamer → page and start with the 16-byte
//! [`DatagramHeader`](crate::DatagramHeader). A datagram the other way is
//! input, and is told apart by its first byte, the same one a media datagram
//! has: wire version in the high nibble, [`Kind::Input`] in the low one. What
//! follows is not the media header but one byte naming the payload, then the
//! payload:
//!
//! ```text
//!  0        1         2..
//! +--------+---------+-----------------------------+
//! |ver|kind| payload | payload                     |
//! |  0x12  | format  |                             |
//! +--------+---------+-----------------------------+
//! ```
//!
//! Format [`PAD_JSON`] is one `{"t":"input","k":"pad",...}` message exactly as
//! the control channel carries it (docs/controllers.md), UTF-8 JSON with no
//! newline. Its `q` field is the pad's sequence number: the receiver applies a
//! message only if it is newer than the newest it applied for that pad, which
//! is what makes a lost, duplicated or reordered datagram harmless
//! ([`seq_newer`]). A receiver ignores formats it doesn't know.

use crate::{Kind, WIRE_VERSION};

/// First byte of every input datagram.
pub const INPUT_BYTE0: u8 = (WIRE_VERSION << 4) | Kind::Input as u8;

/// Payload format: one pad message as JSON (the control channel's line, without the newline).
pub const PAD_JSON: u8 = 0;

/// Bytes before the payload.
pub const INPUT_PREFIX_LEN: usize = 2;

/// The datagram for `payload` in `format`.
pub fn encode_input(format: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(INPUT_PREFIX_LEN + payload.len());
    out.push(INPUT_BYTE0);
    out.push(format);
    out.extend_from_slice(payload);
    out
}

/// The format and payload of an input datagram, or `None` if `datagram`
/// isn't one (too short, or another version or kind).
pub fn decode_input(datagram: &[u8]) -> Option<(u8, &[u8])> {
    match datagram {
        [INPUT_BYTE0, format, payload @ ..] => Some((*format, payload)),
        _ => None,
    }
}

/// Whether sequence number `new` is newer than `last`, comparing modulo 2^32
/// (a number up to 2^31 ahead is newer; equal and behind are not).
pub fn seq_newer(new: u32, last: u32) -> bool {
    let d = new.wrapping_sub(last);
    d != 0 && d < 1 << 31
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_datagram_round_trips() {
        let d = encode_input(PAD_JSON, br#"{"k":"pad"}"#);
        assert_eq!(d[0], 0x12);
        assert_eq!(decode_input(&d), Some((PAD_JSON, &br#"{"k":"pad"}"#[..])));
    }

    #[test]
    fn other_datagrams_are_not_input() {
        assert_eq!(decode_input(&[]), None);
        assert_eq!(decode_input(&[INPUT_BYTE0]), None);
        // A video header's first byte (version 1, kind 0).
        assert_eq!(decode_input(&[0x10, 0, 1, 2]), None);
        // Another wire version.
        assert_eq!(decode_input(&[0x22, 0, 1]), None);
        // An empty payload is still input.
        assert_eq!(decode_input(&[INPUT_BYTE0, 7]), Some((7, &[][..])));
    }

    #[test]
    fn sequence_numbers_compare_across_the_wrap() {
        assert!(seq_newer(5, 4));
        assert!(!seq_newer(4, 4), "a resend of the same state isn't newer");
        assert!(!seq_newer(3, 4));
        assert!(seq_newer(0, u32::MAX), "wraps");
        assert!(!seq_newer(u32::MAX, 0));
        assert!(seq_newer(10, u32::MAX - 5));
    }
}

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
//!
//! Format [`HID_REPORT`] is one raw HID input report from a controller the
//! streamer passes through to the app (docs/controllers.md, "Raw HID relay"):
//! the pad's slot (1 byte), the report's sequence number `q` (4 bytes, big
//! endian; the receiver keeps the newest per slot and report id, as for pad
//! state), then the report as the device sent it, its id first.

use crate::{Kind, WIRE_VERSION};

/// First byte of every input datagram.
pub const INPUT_BYTE0: u8 = (WIRE_VERSION << 4) | Kind::Input as u8;

/// Payload format: one pad message as JSON (the control channel's line, without the newline).
pub const PAD_JSON: u8 = 0;

/// Payload format: one raw HID input report (`slot`, `q`, then the report, id first).
pub const HID_REPORT: u8 = 1;

/// Bytes of a [`HID_REPORT`] payload before the report: the slot and `q`.
pub const HID_REPORT_HEADER_LEN: usize = 5;

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

/// The payload of a [`HID_REPORT`] datagram: `report` leads with its id.
pub fn encode_hid_report(slot: u8, q: u32, report: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HID_REPORT_HEADER_LEN + report.len());
    out.push(slot);
    out.extend_from_slice(&q.to_be_bytes());
    out.extend_from_slice(report);
    out
}

/// The slot, sequence number and report (id first, never empty) of a
/// [`HID_REPORT`] payload, or `None` if it is too short.
pub fn decode_hid_report(payload: &[u8]) -> Option<(u8, u32, &[u8])> {
    match payload {
        [slot, q0, q1, q2, q3, report @ ..] if !report.is_empty() => {
            Some((*slot, u32::from_be_bytes([*q0, *q1, *q2, *q3]), report))
        }
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
    fn a_hid_report_round_trips() {
        let payload = encode_hid_report(2, 0x0102_0304, &[0x42, 9, 8, 7]);
        assert_eq!(payload, [2, 1, 2, 3, 4, 0x42, 9, 8, 7]);
        assert_eq!(
            decode_hid_report(&payload),
            Some((2, 0x0102_0304, &[0x42, 9, 8, 7][..]))
        );
        // Inside a datagram, behind the format byte.
        let d = encode_input(HID_REPORT, &payload);
        let (format, inner) = decode_input(&d).unwrap();
        assert_eq!(format, HID_REPORT);
        assert_eq!(decode_hid_report(inner).map(|(s, _, _)| s), Some(2));
        // Too short, or no report at all.
        assert_eq!(decode_hid_report(&[]), None);
        assert_eq!(decode_hid_report(&[1, 0, 0, 0, 1]), None);
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

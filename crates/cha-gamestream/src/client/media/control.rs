//! The client's side of the control stream: the messages it sends (start,
//! ping, keyframe and reference-frame requests, input), the host's
//! messages it decodes (rumble, LEDs, motion, adaptive triggers, HDR mode,
//! termination) and the AES-GCM wrapper, in the direction the host's
//! `media::control` doesn't use.
//!
//! The framing, the type numbers and the sealing of the host-to-client
//! direction are the host's own (`media::control::{messages, crypto,
//! feedback}`): one vocabulary for both sides of the wire.

use crate::backend::{Feedback, HdrMetadata};
use crate::media::control::crypto;
use crate::media::control::messages::{FrameError, frame, ty};

/// The termination code for the host closing the session on purpose
/// (`NVST_DISCONN_SERVER_TERMINATED_CLOSED`).
pub(crate) const TERMINATED_CLOSED: u32 = 0x8003_0023;
/// The short termination message's "intended" reason (type 0x0100).
const SHORT_TERMINATED_INTENDED: u32 = 0x0100;
/// The short termination message (type 0x0100, a 16-bit reason, per Moonlight's control
/// stream), for hosts that predate the extended one.
const TERMINATION_SHORT: u16 = 0x0100;

/// Seals client-to-host messages with the session key and a counting sequence
/// number, which is also their IV.
pub(crate) struct Sealer {
    key: [u8; 16],
    sequence: u32,
}

impl Sealer {
    pub fn new(key: [u8; 16]) -> Self {
        Self { key, sequence: 0 }
    }

    pub fn seal(&mut self, inner: &[u8]) -> Vec<u8> {
        let wire = crypto::seal_as_client(&self.key, self.sequence, inner);
        self.sequence = self.sequence.wrapping_add(1);
        wire
    }
}

// What the client sends, as `type length body`, ready to seal.

/// The first message of a stream is a keyframe request with a two-byte body
/// (message ids per moonlight-common-c's `ControlStream.c`).
pub(crate) fn start_a() -> Vec<u8> {
    frame(ty::REQUEST_IDR, &[0, 0])
}

pub(crate) fn start_b() -> Vec<u8> {
    frame(ty::START_B, &[0])
}

pub(crate) fn request_idr() -> Vec<u8> {
    frame(ty::REQUEST_IDR, &[0, 0])
}

/// The periodic ping (body per Moonlight's control stream): the 4 in the first two bytes is "length of payload".
pub(crate) fn ping() -> Vec<u8> {
    frame(ty::PING, &[4, 0, 0, 0, 0, 0, 0, 0])
}

/// Reference frame invalidation (`SS_RFI_REQUEST`): first, reserved, last,
/// 12 reserved bytes, little-endian.
pub(crate) fn invalidate_refs(first: u32, last: u32) -> Vec<u8> {
    let mut body = [0u8; 24];
    body[0..4].copy_from_slice(&first.to_le_bytes());
    body[8..12].copy_from_slice(&last.to_le_bytes());
    frame(ty::INVALIDATE_REFS, &body)
}

/// An input packet (magic and fields) in its control message: the packet's
/// length first, big-endian.
pub(crate) fn input(packet: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(4 + packet.len());
    body.extend_from_slice(&(packet.len() as u32).to_be_bytes());
    body.extend_from_slice(packet);
    frame(ty::INPUT, &body)
}

/// What a decrypted host message means to the client.
#[derive(Debug, PartialEq)]
pub(crate) enum HostMessage {
    Feedback(Feedback),
    /// The host ends the stream. `graceful` is the host closing on purpose.
    Terminated {
        code: u32,
        graceful: bool,
    },
    /// A type the client has no use for.
    Ignored(u16),
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum HostMessageError {
    #[error(transparent)]
    Framing(#[from] FrameError),
    #[error("message {0:#06x} is shorter than its fields")]
    Short(u16),
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

/// Decodes one decrypted host-to-client message. A message that is cut short
/// or has an unknown type is an error or `Ignored`; the caller drops it.
pub(crate) fn parse_host(inner: &[u8]) -> Result<HostMessage, HostMessageError> {
    if inner.len() < 4 {
        return Err(FrameError::TooShort.into());
    }
    let t = u16::from_le_bytes([inner[0], inner[1]]);
    let len = usize::from(u16::from_le_bytes([inner[2], inner[3]]));
    if len != inner.len() - 4 {
        return Err(FrameError::LengthMismatch.into());
    }
    let b = &inner[4..];
    let need = |n: usize| {
        if b.len() < n {
            Err(HostMessageError::Short(t))
        } else {
            Ok(())
        }
    };
    Ok(match t {
        ty::RUMBLE => {
            // Four bytes of padding, then pad, low, high.
            need(10)?;
            HostMessage::Feedback(Feedback::Rumble {
                pad: le16(b, 4),
                low: le16(b, 6),
                high: le16(b, 8),
            })
        }
        ty::RUMBLE_TRIGGERS => {
            need(6)?;
            HostMessage::Feedback(Feedback::RumbleTriggers {
                pad: le16(b, 0),
                left: le16(b, 2),
                right: le16(b, 4),
            })
        }
        ty::RGB_LED => {
            need(5)?;
            HostMessage::Feedback(Feedback::Led {
                pad: le16(b, 0),
                rgb: (b[2], b[3], b[4]),
            })
        }
        ty::MOTION_EVENT => {
            need(5)?;
            HostMessage::Feedback(Feedback::MotionEnable {
                pad: le16(b, 0),
                rate_hz: le16(b, 2),
                kind: b[4],
            })
        }
        ty::TRIGGER_EFFECT => {
            need(25)?;
            HostMessage::Feedback(Feedback::TriggerEffect {
                pad: le16(b, 0),
                event_flags: b[2],
                type_left: b[3],
                type_right: b[4],
                left: b[5..15].try_into().expect("10 bytes"),
                right: b[15..25].try_into().expect("10 bytes"),
            })
        }
        ty::HDR_MODE => {
            need(1)?;
            HostMessage::Feedback(Feedback::Hdr {
                enabled: b[0] != 0,
                metadata: (b[0] != 0 && b.len() >= 31).then(|| hdr_metadata(&b[1..31])),
            })
        }
        ty::TERMINATION => {
            if b.len() >= 4 {
                // The extended message: a big-endian status code.
                let code = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                HostMessage::Terminated {
                    code,
                    graceful: code == TERMINATED_CLOSED,
                }
            } else if b.len() >= 2 {
                let reason = u32::from(le16(b, 0));
                HostMessage::Terminated {
                    code: reason,
                    graceful: reason == SHORT_TERMINATED_INTENDED,
                }
            } else {
                return Err(HostMessageError::Short(t));
            }
        }
        TERMINATION_SHORT => {
            need(2)?;
            let reason = u32::from(le16(b, 0));
            HostMessage::Terminated {
                code: reason,
                graceful: reason == SHORT_TERMINATED_INTENDED,
            }
        }
        other => HostMessage::Ignored(other),
    })
}

/// `SS_HDR_METADATA` as the host writes it: primaries, white point, then
/// maximum display luminance in nits, minimum in the SEI's 0.0001 cd/m2,
/// light levels. The inverse of `feedback::hdr_mode`, which rounds the
/// maximum down to whole nits.
fn hdr_metadata(b: &[u8]) -> HdrMetadata {
    HdrMetadata {
        display_primaries: [
            (le16(b, 0), le16(b, 2)),
            (le16(b, 4), le16(b, 6)),
            (le16(b, 8), le16(b, 10)),
        ],
        white_point: (le16(b, 12), le16(b, 14)),
        max_luminance: u32::from(le16(b, 16)) * 10_000,
        min_luminance: u32::from(le16(b, 18)),
        max_cll: le16(b, 20),
        max_fall: le16(b, 22),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::control::feedback;
    use crate::media::control::messages::{Outer, parse_inner, parse_outer};

    const KEY: [u8; 16] = *b"0123456789abcdef";

    /// Whatever the host's encoder writes, the client reads back as the same feedback.
    #[test]
    fn host_feedback_round_trips() {
        let all = [
            Feedback::Rumble {
                pad: 2,
                low: 0xAABB,
                high: 0x1234,
            },
            Feedback::RumbleTriggers {
                pad: 1,
                left: 0x100,
                right: 0x200,
            },
            Feedback::Led {
                pad: 3,
                rgb: (1, 2, 3),
            },
            Feedback::MotionEnable {
                pad: 3,
                rate_hz: 100,
                kind: 2,
            },
            Feedback::TriggerEffect {
                pad: 1,
                event_flags: 3,
                type_left: 4,
                type_right: 5,
                left: [6; 10],
                right: [7; 10],
            },
        ];
        for fb in all {
            let wire = feedback::encode(&fb).expect("encodable");
            assert_eq!(parse_host(&wire), Ok(HostMessage::Feedback(fb)));
        }
    }

    #[test]
    fn hdr_mode_round_trips() {
        let on = feedback::hdr_mode(true, Some(&HdrMetadata::fallback()));
        assert_eq!(
            parse_host(&on),
            Ok(HostMessage::Feedback(Feedback::Hdr {
                enabled: true,
                metadata: Some(HdrMetadata::fallback())
            }))
        );
        let off = feedback::hdr_mode(false, None);
        assert_eq!(
            parse_host(&off),
            Ok(HostMessage::Feedback(Feedback::Hdr {
                enabled: false,
                metadata: None
            }))
        );
        // A host without metadata in the message: on, and nothing to say about it.
        assert_eq!(
            parse_host(&frame(ty::HDR_MODE, &[1])),
            Ok(HostMessage::Feedback(Feedback::Hdr {
                enabled: true,
                metadata: None
            }))
        );
    }

    #[test]
    fn terminations() {
        assert_eq!(
            parse_host(&feedback::termination(feedback::TERMINATED_BY_SERVER)),
            Ok(HostMessage::Terminated {
                code: 0x8003_0023,
                graceful: true
            })
        );
        assert_eq!(
            parse_host(&feedback::termination(0x800e_9302)),
            Ok(HostMessage::Terminated {
                code: 0x800e_9302,
                graceful: false
            })
        );
        // The short form of old hosts.
        assert_eq!(
            parse_host(&frame(0x0100, &[0x00, 0x01])),
            Ok(HostMessage::Terminated {
                code: 0x0100,
                graceful: true
            })
        );
        assert_eq!(
            parse_host(&frame(ty::TERMINATION, &[0x05, 0x00])),
            Ok(HostMessage::Terminated {
                code: 5,
                graceful: false
            })
        );
        assert_eq!(
            parse_host(&frame(ty::TERMINATION, &[1])),
            Err(HostMessageError::Short(ty::TERMINATION))
        );
    }

    #[test]
    fn short_and_unknown_messages_are_errors_or_ignored() {
        assert_eq!(
            parse_host(&frame(ty::RUMBLE, &[0; 9])),
            Err(HostMessageError::Short(ty::RUMBLE))
        );
        assert_eq!(
            parse_host(&frame(ty::TRIGGER_EFFECT, &[0; 24])),
            Err(HostMessageError::Short(ty::TRIGGER_EFFECT))
        );
        assert_eq!(
            parse_host(&frame(0x0777, &[1, 2, 3])),
            Ok(HostMessage::Ignored(0x0777))
        );
        assert_eq!(parse_host(&[1, 2]), Err(FrameError::TooShort.into()));
        assert_eq!(
            parse_host(&[0x0b, 0x01, 9, 0]),
            Err(FrameError::LengthMismatch.into())
        );
    }

    /// What the client sends parses as the host's parser expects.
    #[test]
    fn client_messages_parse_on_the_host() {
        use crate::media::control::messages::Inbound;
        assert_eq!(parse_inner(&ping()), Ok(Inbound::Ping));
        assert_eq!(parse_inner(&start_a()), Ok(Inbound::RequestIdr));
        assert_eq!(parse_inner(&start_b()), Ok(Inbound::StartB));
        assert_eq!(parse_inner(&request_idr()), Ok(Inbound::RequestIdr));
        assert_eq!(
            parse_inner(&invalidate_refs(40, 42)),
            Ok(Inbound::InvalidateRefs {
                first: 40,
                last: 42
            })
        );
        assert_eq!(
            parse_inner(&input(&[3, 0, 0, 0, 9])),
            Ok(Inbound::Input(&[3, 0, 0, 0, 9]))
        );
    }

    #[test]
    fn sealed_messages_open_with_the_session_key_and_count() {
        let mut s = Sealer::new(KEY);
        for expected in 0..3u32 {
            let wire = s.seal(&ping());
            let Ok(Outer::Encrypted {
                sequence,
                tag,
                ciphertext,
            }) = parse_outer(&wire)
            else {
                panic!("not encrypted")
            };
            assert_eq!(sequence, expected);
            assert_eq!(
                crypto::open(&KEY, sequence, &tag, ciphertext).unwrap(),
                ping()
            );
        }
    }

    /// The host's messages open with the host-direction IV and no other.
    #[test]
    fn host_messages_open_in_the_host_direction_only() {
        let inner = feedback::termination(1);
        let wire = crypto::seal(&KEY, 5, &inner).unwrap();
        let Ok(Outer::Encrypted {
            sequence,
            tag,
            ciphertext,
        }) = parse_outer(&wire)
        else {
            panic!()
        };
        assert_eq!(
            crypto::open_from_host(&KEY, sequence, &tag, ciphertext).unwrap(),
            inner
        );
        assert!(crypto::open(&KEY, sequence, &tag, ciphertext).is_err());
        assert!(crypto::open_from_host(&[0; 16], sequence, &tag, ciphertext).is_err());
    }

    /// Whatever bytes arrive, decoding returns; it never panics.
    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0x0BAD_C0DE_1234_5678u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let types = [
            ty::RUMBLE,
            ty::RUMBLE_TRIGGERS,
            ty::RGB_LED,
            ty::MOTION_EVENT,
            ty::TRIGGER_EFFECT,
            ty::HDR_MODE,
            ty::TERMINATION,
            TERMINATION_SHORT,
            0x1234,
        ];
        for i in 0..60_000 {
            let len = (next() % 48) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if i % 2 == 0 && buf.len() >= 4 {
                let body = (buf.len() - 4) as u16;
                buf[2..4].copy_from_slice(&body.to_le_bytes());
                let t = types[(next() % types.len() as u64) as usize];
                buf[..2].copy_from_slice(&t.to_le_bytes());
            }
            let _ = parse_host(&buf);
        }
    }
}

// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: one encoder for the backend's neutral Feedback, plus the HDR mode and termination messages that
// lived in control/mod.rs, and the trigger-rumble message Moonshine did not send.

//! Host-to-client control messages: rumble, LEDs, motion enable, adaptive
//! triggers, HDR mode and termination. Each is `type length body`, ready to
//! be encrypted.

use crate::backend::{Feedback, HdrMetadata};

use super::messages::{frame, ty};

/// The message for a backend's feedback. `Hdr` is built with
/// [`hdr_mode`], which needs the metadata the host holds.
pub(crate) fn encode(feedback: &Feedback) -> Option<Vec<u8>> {
    Some(match *feedback {
        Feedback::Rumble { pad, low, high } => {
            let mut body = [0u8; 10];
            // 4 bytes of padding first.
            body[4..6].copy_from_slice(&pad.to_le_bytes());
            body[6..8].copy_from_slice(&low.to_le_bytes());
            body[8..10].copy_from_slice(&high.to_le_bytes());
            frame(ty::RUMBLE, &body)
        }
        Feedback::RumbleTriggers { pad, left, right } => {
            let mut body = [0u8; 6];
            body[0..2].copy_from_slice(&pad.to_le_bytes());
            body[2..4].copy_from_slice(&left.to_le_bytes());
            body[4..6].copy_from_slice(&right.to_le_bytes());
            frame(ty::RUMBLE_TRIGGERS, &body)
        }
        Feedback::Led { pad, rgb } => {
            let mut body = [0u8; 5];
            body[0..2].copy_from_slice(&pad.to_le_bytes());
            body[2..5].copy_from_slice(&[rgb.0, rgb.1, rgb.2]);
            frame(ty::RGB_LED, &body)
        }
        Feedback::MotionEnable { pad, rate_hz, kind } => {
            let mut body = [0u8; 5];
            body[0..2].copy_from_slice(&pad.to_le_bytes());
            body[2..4].copy_from_slice(&rate_hz.to_le_bytes());
            body[4] = kind;
            frame(ty::MOTION_EVENT, &body)
        }
        Feedback::TriggerEffect {
            pad,
            event_flags,
            type_left,
            type_right,
            left,
            right,
        } => {
            let mut body = [0u8; 25];
            body[0..2].copy_from_slice(&pad.to_le_bytes());
            body[2] = event_flags;
            body[3] = type_left;
            body[4] = type_right;
            body[5..15].copy_from_slice(&left);
            body[15..25].copy_from_slice(&right);
            frame(ty::TRIGGER_EFFECT, &body)
        }
        Feedback::Hdr { .. } => return None,
    })
}

/// HDR mode (0x010e): one byte enabled, then 30 bytes of `SS_HDR_METADATA`,
/// fifteen little-endian `u16`: primaries (6), white point (2), max and min
/// display luminance, max CLL, max FALL, max full-frame luminance, padding.
pub(crate) fn hdr_mode(enabled: bool, metadata: Option<&HdrMetadata>) -> Vec<u8> {
    let mut body = Vec::with_capacity(31);
    body.push(u8::from(enabled));
    match metadata {
        Some(m) => {
            for &(x, y) in &m.display_primaries {
                body.extend(x.to_le_bytes());
                body.extend(y.to_le_bytes());
            }
            body.extend(m.white_point.0.to_le_bytes());
            body.extend(m.white_point.1.to_le_bytes());
            // Maximum in nits (the SEI has 0.0001 cd/m2 units), minimum as is.
            body.extend(((m.max_luminance / 10_000).min(u32::from(u16::MAX)) as u16).to_le_bytes());
            body.extend((m.min_luminance.min(u32::from(u16::MAX)) as u16).to_le_bytes());
            body.extend(m.max_cll.to_le_bytes());
            body.extend(m.max_fall.to_le_bytes());
            body.extend(0u16.to_le_bytes());
            body.extend([0u8; 4]);
        }
        None => body.extend([0u8; 30]),
    }
    frame(ty::HDR_MODE, &body)
}

/// The extended termination message, with an `NVST_DISCONN` code; 0x80030023
/// is the server closing on purpose, which the client takes as no error.
pub(crate) fn termination(code: u32) -> Vec<u8> {
    frame(ty::TERMINATION, &code.to_be_bytes())
}

pub(crate) const TERMINATED_BY_SERVER: u32 = 0x8003_0023;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rumble() {
        let m = encode(&Feedback::Rumble {
            pad: 2,
            low: 0xAABB,
            high: 0x1234,
        })
        .unwrap();
        assert_eq!(
            m,
            [0x0b, 0x01, 10, 0, 0, 0, 0, 0, 2, 0, 0xBB, 0xAA, 0x34, 0x12]
        );
    }

    #[test]
    fn trigger_rumble_and_led_and_motion() {
        assert_eq!(
            encode(&Feedback::RumbleTriggers {
                pad: 1,
                left: 0x100,
                right: 0x200
            })
            .unwrap(),
            [0x00, 0x55, 6, 0, 1, 0, 0, 1, 0, 2]
        );
        assert_eq!(
            encode(&Feedback::Led {
                pad: 0,
                rgb: (1, 2, 3)
            })
            .unwrap(),
            [0x02, 0x55, 5, 0, 0, 0, 1, 2, 3]
        );
        assert_eq!(
            encode(&Feedback::MotionEnable {
                pad: 3,
                rate_hz: 100,
                kind: 2
            })
            .unwrap(),
            [0x01, 0x55, 5, 0, 3, 0, 100, 0, 2]
        );
    }

    #[test]
    fn trigger_effect_is_29_bytes() {
        let m = encode(&Feedback::TriggerEffect {
            pad: 1,
            event_flags: 3,
            type_left: 4,
            type_right: 5,
            left: [6; 10],
            right: [7; 10],
        })
        .unwrap();
        assert_eq!(m.len(), 4 + 25);
        assert_eq!(&m[..4], &[0x03, 0x55, 25, 0]);
        assert_eq!(&m[4..9], &[1, 0, 3, 4, 5]);
        assert_eq!(&m[9..19], &[6; 10]);
        assert_eq!(&m[19..], &[7; 10]);
    }

    #[test]
    fn hdr_mode_layout() {
        let off = hdr_mode(false, None);
        assert_eq!(off.len(), 4 + 31);
        assert_eq!(&off[..5], &[0x0e, 0x01, 31, 0, 0]);
        let on = hdr_mode(true, Some(&HdrMetadata::fallback()));
        assert_eq!(on[4], 1);
        // Red x then y, little-endian.
        assert_eq!(&on[5..9], &[0xD0, 0x84, 0x80, 0x3E]);
        // White point after the six primaries, then max luminance 1000 nits and min 10.
        assert_eq!(&on[17..21], &[0x13, 0x3D, 0x42, 0x40]);
        assert_eq!(&on[21..25], &[0xE8, 0x03, 10, 0]);
    }

    #[test]
    fn termination_code_is_big_endian() {
        assert_eq!(
            termination(TERMINATED_BY_SERVER),
            [0x09, 0x01, 4, 0, 0x80, 0x03, 0x00, 0x23]
        );
    }

    #[test]
    fn hdr_feedback_has_its_own_builder() {
        assert!(
            encode(&Feedback::Hdr {
                enabled: true,
                metadata: None
            })
            .is_none()
        );
    }
}

// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: parsing returns typed framing errors, unknown message types are ignored rather than fatal,
// no tracing in the parser (the caller counts what it drops).

//! Control message framing, below the encryption: `type(u16 LE) length(u16 LE)
//! body`, where length counts the body. Input messages carry a further
//! big-endian `u32` length of the input packet that follows.

pub(crate) mod ty {
    pub const ENCRYPTED: u16 = 0x0001;
    pub const TERMINATION: u16 = 0x0109;
    pub const RUMBLE: u16 = 0x010b;
    pub const HDR_MODE: u16 = 0x010e;
    pub const PING: u16 = 0x0200;
    pub const INPUT: u16 = 0x0206;
    pub const INVALIDATE_REFS: u16 = 0x0301;
    pub const REQUEST_IDR: u16 = 0x0302;
    /// The request older, unencrypted clients send; accepted all the same.
    pub const REQUEST_IDR_LEGACY: u16 = 0x0305;
    pub const START_B: u16 = 0x0307;
    pub const RUMBLE_TRIGGERS: u16 = 0x5500;
    pub const MOTION_EVENT: u16 = 0x5501;
    pub const RGB_LED: u16 = 0x5502;
    pub const TRIGGER_EFFECT: u16 = 0x5503;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum FrameError {
    #[error("shorter than a message header")]
    TooShort,
    #[error("the length field disagrees with the packet")]
    LengthMismatch,
    #[error("the input length disagrees with the message")]
    InputLength,
    #[error("an encrypted message too short to hold its sequence number and tag")]
    ShortEncrypted,
}

/// What arrives on the wire: an encrypted message, or a plain one (which
/// the host refuses).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outer<'a> {
    Encrypted {
        sequence: u32,
        tag: [u8; 16],
        ciphertext: &'a [u8],
    },
    Plain {
        ty: u16,
    },
}

/// A decrypted message, as far as the host cares.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Inbound<'a> {
    Ping,
    StartB,
    RequestIdr,
    /// Frames the client could not decode, inclusive, in wire frame numbers.
    InvalidateRefs {
        first: u32,
        last: u32,
    },
    /// One input packet (event type and body).
    Input(&'a [u8]),
    /// A message this host has no use for (loss stats, frame stats, ...).
    Ignored(u16),
}

fn header(buf: &[u8]) -> Result<(u16, &[u8]), FrameError> {
    if buf.len() < 4 {
        return Err(FrameError::TooShort);
    }
    let ty = u16::from_le_bytes([buf[0], buf[1]]);
    let len = usize::from(u16::from_le_bytes([buf[2], buf[3]]));
    if len != buf.len() - 4 {
        return Err(FrameError::LengthMismatch);
    }
    Ok((ty, &buf[4..]))
}

/// Splits the outer framing of one ENet packet.
pub(crate) fn parse_outer(buf: &[u8]) -> Result<Outer<'_>, FrameError> {
    let (ty, body) = header(buf)?;
    if ty != ty::ENCRYPTED {
        return Ok(Outer::Plain { ty });
    }
    // sequence(4) tag(16) ciphertext
    if body.len() < 4 + 16 {
        return Err(FrameError::ShortEncrypted);
    }
    Ok(Outer::Encrypted {
        sequence: u32::from_le_bytes([body[0], body[1], body[2], body[3]]),
        tag: body[4..20].try_into().expect("16 bytes"),
        ciphertext: &body[20..],
    })
}

/// Parses a decrypted message.
pub(crate) fn parse_inner(buf: &[u8]) -> Result<Inbound<'_>, FrameError> {
    let (t, body) = header(buf)?;
    Ok(match t {
        ty::PING => Inbound::Ping,
        ty::START_B => Inbound::StartB,
        ty::REQUEST_IDR | ty::REQUEST_IDR_LEGACY => Inbound::RequestIdr,
        ty::INVALIDATE_REFS => {
            // first(4) reserved(4) last(4) reserved(12), little-endian. A body
            // too short to say is an invalidation of everything, which the
            // backend answers with a keyframe.
            if body.len() >= 12 {
                Inbound::InvalidateRefs {
                    first: u32::from_le_bytes([body[0], body[1], body[2], body[3]]),
                    last: u32::from_le_bytes([body[8], body[9], body[10], body[11]]),
                }
            } else {
                Inbound::InvalidateRefs {
                    first: 0,
                    last: u32::MAX,
                }
            }
        }
        ty::INPUT => {
            if body.len() < 4 {
                return Err(FrameError::InputLength);
            }
            let len = u32::from_be_bytes([body[0], body[1], body[2], body[3]]) as usize;
            if len != body.len() - 4 {
                return Err(FrameError::InputLength);
            }
            Inbound::Input(&body[4..])
        }
        other => Inbound::Ignored(other),
    })
}

/// `type length body`, the plain layout both directions use under encryption.
pub(crate) fn frame(ty: u16, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&ty.to_le_bytes());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_messages_parse() {
        assert_eq!(parse_inner(&[0x00, 0x02, 0, 0]), Ok(Inbound::Ping));
        assert_eq!(parse_inner(&[0x07, 0x03, 1, 0, 0]), Ok(Inbound::StartB));
        assert_eq!(
            parse_inner(&frame(ty::REQUEST_IDR, &[0, 0])),
            Ok(Inbound::RequestIdr)
        );
        assert_eq!(
            parse_inner(&frame(0x0201, &[1, 2, 3])),
            Ok(Inbound::Ignored(0x0201))
        );
    }

    #[test]
    fn invalidation_ranges() {
        let mut body = Vec::new();
        body.extend(40u32.to_le_bytes());
        body.extend(0u32.to_le_bytes());
        body.extend(42u32.to_le_bytes());
        body.extend([0u8; 12]);
        assert_eq!(
            parse_inner(&frame(ty::INVALIDATE_REFS, &body)),
            Ok(Inbound::InvalidateRefs {
                first: 40,
                last: 42
            })
        );
        // A body that can't say what was lost means everything was.
        assert_eq!(
            parse_inner(&frame(ty::INVALIDATE_REFS, &[1, 2])),
            Ok(Inbound::InvalidateRefs {
                first: 0,
                last: u32::MAX
            })
        );
    }

    #[test]
    fn input_carries_its_own_length() {
        let mut body = 5u32.to_be_bytes().to_vec();
        body.extend([3, 0, 0, 0, 9]);
        let msg = frame(ty::INPUT, &body);
        assert_eq!(parse_inner(&msg), Ok(Inbound::Input(&[3, 0, 0, 0, 9])));
        // Claims more than is there.
        let mut body = 9u32.to_be_bytes().to_vec();
        body.extend([3, 0]);
        assert_eq!(
            parse_inner(&frame(ty::INPUT, &body)),
            Err(FrameError::InputLength)
        );
        assert_eq!(
            parse_inner(&frame(ty::INPUT, &[0, 0])),
            Err(FrameError::InputLength)
        );
    }

    #[test]
    fn framing_errors() {
        assert_eq!(parse_inner(&[1, 2, 3]), Err(FrameError::TooShort));
        assert_eq!(parse_inner(&[0, 2, 5, 0]), Err(FrameError::LengthMismatch));
        assert_eq!(
            parse_outer(&[1, 0, 8, 0, 1, 2, 3, 4, 5, 6, 7, 8]),
            Err(FrameError::ShortEncrypted)
        );
    }

    /// Whatever bytes arrive, framing returns; it never panics.
    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0x0123_4567_89AB_CDEFu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for i in 0..50_000 {
            let len = (next() % 64) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            // Half of them with a believable header, so the bodies get parsed.
            if i % 2 == 0 && buf.len() >= 4 {
                let body = (buf.len() - 4) as u16;
                buf[2..4].copy_from_slice(&body.to_le_bytes());
                let ty = [
                    ty::ENCRYPTED,
                    ty::PING,
                    ty::START_B,
                    ty::REQUEST_IDR,
                    ty::INVALIDATE_REFS,
                    ty::INPUT,
                    0x5502,
                ][(next() % 7) as usize];
                buf[..2].copy_from_slice(&ty.to_le_bytes());
            }
            let _ = parse_outer(&buf);
            let _ = parse_inner(&buf);
        }
    }

    #[test]
    fn outer_splits_the_encrypted_wrapper() {
        let mut body = 7u32.to_le_bytes().to_vec();
        body.extend([0xAA; 16]);
        body.extend([1, 2, 3, 4]);
        let msg = frame(ty::ENCRYPTED, &body);
        assert_eq!(
            parse_outer(&msg),
            Ok(Outer::Encrypted {
                sequence: 7,
                tag: [0xAA; 16],
                ciphertext: &[1, 2, 3, 4]
            })
        );
        assert_eq!(
            parse_outer(&frame(ty::PING, &[])),
            Ok(Outer::Plain { ty: ty::PING })
        );
    }
}

/// Wire version carried in the high nibble of byte 0.
pub const WIRE_VERSION: u8 = 1;

/// Size of [`DatagramHeader`] on the wire.
pub const HEADER_LEN: usize = 16;

/// What a datagram carries. Low nibble of byte 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Video = 0,
    Audio = 1,
    Input = 2,
    Feedback = 3,
    /// Synthetic traffic for bandwidth probes and benchmarks.
    Probe = 4,
}

impl Kind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Kind::Video,
            1 => Kind::Audio,
            2 => Kind::Input,
            3 => Kind::Feedback,
            4 => Kind::Probe,
            _ => return None,
        })
    }
}

/// Per-datagram flags (byte 1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags(pub u8);

impl Flags {
    /// Fragment belongs to a frame that does not depend on earlier frames.
    pub const KEYFRAME: u8 = 1 << 0;
    /// Fragment carries data the decoder cannot do without (e.g. PyroWave's
    /// coarsest wavelet band). Candidates for duplication or FEC.
    pub const CRITICAL: u8 = 1 << 1;
    /// Fragment is FEC parity rather than frame data.
    pub const PARITY: u8 = 1 << 2;

    pub fn has(self, bit: u8) -> bool {
        self.0 & bit != 0
    }
}

/// Fixed 16-byte little-endian header at the start of every media datagram.
///
/// ```text
///  0        1       2         3         4..8       8..10       10..12      12..16
/// +--------+-------+---------+---------+----------+-----------+-----------+------------+
/// |ver|kind| flags | stream  | reserved| frame_id | frag_index| frag_count| send_ts_us |
/// +--------+-------+---------+---------+----------+-----------+-----------+------------+
/// ```
///
/// `send_ts_us` is microseconds since the sender's session epoch and wraps
/// every ~71 minutes; receivers compare it with wrapping arithmetic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatagramHeader {
    pub kind: Kind,
    pub flags: Flags,
    pub stream: u8,
    pub frame_id: u32,
    pub frag_index: u16,
    pub frag_count: u16,
    pub send_ts_us: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    TooShort,
    UnsupportedVersion(u8),
    UnknownKind(u8),
    BadFragment { index: u16, count: u16 },
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::TooShort => write!(f, "datagram shorter than header"),
            DecodeError::UnsupportedVersion(v) => write!(f, "unsupported wire version {v}"),
            DecodeError::UnknownKind(k) => write!(f, "unknown datagram kind {k}"),
            DecodeError::BadFragment { index, count } => {
                write!(f, "fragment index {index} out of range for count {count}")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

impl DatagramHeader {
    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[0] = (WIRE_VERSION << 4) | (self.kind as u8 & 0x0f);
        out[1] = self.flags.0;
        out[2] = self.stream;
        out[3] = 0;
        out[4..8].copy_from_slice(&self.frame_id.to_le_bytes());
        out[8..10].copy_from_slice(&self.frag_index.to_le_bytes());
        out[10..12].copy_from_slice(&self.frag_count.to_le_bytes());
        out[12..16].copy_from_slice(&self.send_ts_us.to_le_bytes());
    }

    /// Parses the header and returns it with the remaining payload.
    pub fn decode(datagram: &[u8]) -> Result<(Self, &[u8]), DecodeError> {
        if datagram.len() < HEADER_LEN {
            return Err(DecodeError::TooShort);
        }
        let version = datagram[0] >> 4;
        if version != WIRE_VERSION {
            return Err(DecodeError::UnsupportedVersion(version));
        }
        let kind_raw = datagram[0] & 0x0f;
        let kind = Kind::from_u8(kind_raw).ok_or(DecodeError::UnknownKind(kind_raw))?;
        let u16_at = |i: usize| u16::from_le_bytes([datagram[i], datagram[i + 1]]);
        let u32_at = |i: usize| {
            u32::from_le_bytes([
                datagram[i],
                datagram[i + 1],
                datagram[i + 2],
                datagram[i + 3],
            ])
        };
        let header = DatagramHeader {
            kind,
            flags: Flags(datagram[1]),
            stream: datagram[2],
            frame_id: u32_at(4),
            frag_index: u16_at(8),
            frag_count: u16_at(10),
            send_ts_us: u32_at(12),
        };
        if header.frag_count == 0 || header.frag_index >= header.frag_count {
            return Err(DecodeError::BadFragment {
                index: header.frag_index,
                count: header.frag_count,
            });
        }
        Ok((header, &datagram[HEADER_LEN..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> DatagramHeader {
        DatagramHeader {
            kind: Kind::Video,
            flags: Flags(Flags::KEYFRAME | Flags::CRITICAL),
            stream: 3,
            frame_id: 0xdead_beef,
            frag_index: 7,
            frag_count: 900,
            send_ts_us: 123_456_789,
        }
    }

    #[test]
    fn roundtrip() {
        let mut buf = [0u8; HEADER_LEN + 4];
        let mut head = [0u8; HEADER_LEN];
        sample().encode(&mut head);
        buf[..HEADER_LEN].copy_from_slice(&head);
        buf[HEADER_LEN..].copy_from_slice(b"data");
        let (decoded, payload) = DatagramHeader::decode(&buf).unwrap();
        assert_eq!(decoded, sample());
        assert_eq!(payload, b"data");
        assert!(decoded.flags.has(Flags::CRITICAL));
        assert!(!decoded.flags.has(Flags::PARITY));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(
            DatagramHeader::decode(&[0u8; 4]),
            Err(DecodeError::TooShort)
        );

        let mut head = [0u8; HEADER_LEN];
        sample().encode(&mut head);
        head[0] = (2 << 4) | Kind::Video as u8;
        assert_eq!(
            DatagramHeader::decode(&head),
            Err(DecodeError::UnsupportedVersion(2))
        );

        sample().encode(&mut head);
        head[0] = (WIRE_VERSION << 4) | 0x0f;
        assert_eq!(
            DatagramHeader::decode(&head),
            Err(DecodeError::UnknownKind(0x0f))
        );

        let mut bad = sample();
        bad.frag_index = 900;
        bad.encode(&mut head);
        assert_eq!(
            DatagramHeader::decode(&head),
            Err(DecodeError::BadFragment {
                index: 900,
                count: 900
            })
        );
    }
}

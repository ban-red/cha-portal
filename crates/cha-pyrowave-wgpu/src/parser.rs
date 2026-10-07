//! Turns packets into the two flat arrays the dequant shader reads: a per-32x32-block
//! offset table and the concatenated payload words.
//!
//! Port of PyroWave's `BitstreamParser` (`metal/pyrowave_bitstream.cpp`, imbcmdth/pyrowave,
//! MIT; see `LICENSE-PYROWAVE`), through our TypeScript port.
//!
//! Wire layout (little-endian bitfields, 8 bytes each):
//! - block header: `w0 = ballot:16 | payload_words:12 | sequence:3 | extended:1`,
//!   `w1 = quant_code:8 | block_index:24`
//! - sequence header: `w0 = width-1:14 | height-1:14 | sequence:3 | extended:1`,
//!   `w1 = total_blocks:24 | code:2 | chroma_resolution:1 | primaries:1 | transfer:1 |
//!   ycbcr_transform:1 | range:1 | chroma_siting:1`

use crate::PacketError;
use crate::layout::{BlockLayout, Chroma, DECOMPOSITION_LEVELS};

pub(crate) const NOT_RECEIVED: u32 = 0xffff_ffff;
const HEADER_BYTES: usize = 8;
const SEQUENCE_MASK: i32 = 0x7;
const CODE_START_OF_FRAME: u32 = 0;

/// Colour signalling from the most recent sequence header.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ColorInfo {
    /// 0 = BT.709, 1 = BT.2020.
    pub primaries: u8,
    /// 0 = BT.709, 1 = PQ.
    pub transfer: u8,
    /// 0 = BT.709, 1 = BT.2020 (non-constant luminance).
    pub ycbcr_transform: u8,
    pub full_range: bool,
    /// 0 = centre, 1 = left.
    pub chroma_siting: u8,
}

/// What a frame's first packet says about the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SequenceHeader {
    pub width: u32,
    pub height: u32,
    pub chroma: Chroma,
    pub total_blocks: u32,
    pub color: ColorInfo,
}

fn word(bytes: &[u8], index: usize) -> u32 {
    u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
}

/// Reads the sequence header at the start of a frame (its first 8 bytes), or `None`
/// if `data` does not start with one. Use it to pick the size and chroma for
/// [`crate::Decoder::new`].
pub fn parse_sequence_header(data: &[u8]) -> Option<SequenceHeader> {
    if data.len() < HEADER_BYTES {
        return None;
    }
    let (w0, w1) = (word(data, 0), word(data, 1));
    if w0 >> 31 == 0 {
        return None; // not an extended (start of frame) header
    }
    Some(SequenceHeader {
        width: (w0 & 0x3fff) + 1,
        height: ((w0 >> 14) & 0x3fff) + 1,
        chroma: if (w1 >> 26) & 1 == 1 {
            Chroma::C444
        } else {
            Chroma::C420
        },
        total_blocks: w1 & 0xff_ffff,
        color: color_from(w1),
    })
}

fn color_from(w1: u32) -> ColorInfo {
    ColorInfo {
        primaries: ((w1 >> 27) & 1) as u8,
        transfer: ((w1 >> 28) & 1) as u8,
        ycbcr_transform: ((w1 >> 29) & 1) as u8,
        full_range: (w1 >> 30) & 1 == 1,
        chroma_siting: (w1 >> 31) as u8,
    }
}

/// Splits a frame's bytes into its records: the sequence header (8 bytes) and then
/// one record per coded block. Each is a unit a transport may drop or deliver whole.
/// Stops at the first record that does not fit.
pub fn split_records(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while data.len() - pos >= HEADER_BYTES {
        let w0 = word(data, pos / 4);
        let len = if w0 >> 31 == 1 {
            HEADER_BYTES
        } else {
            ((w0 >> 16) & 0xfff) as usize * 4
        };
        if len < HEADER_BYTES || pos + len > data.len() {
            break;
        }
        out.push(&data[pos..pos + len]);
        pos += len;
    }
    out
}

pub struct BitstreamParser {
    layout: BlockLayout,
    pub(crate) dequant_offsets: Vec<u32>,
    payload: Vec<u32>,
    decoded_blocks: u32,
    total_blocks_in_sequence: u32,
    last_seq: i32,
    decoded_frame_for_current_sequence: bool,
    color: Option<ColorInfo>,
}

impl BitstreamParser {
    pub fn new(layout: BlockLayout) -> Self {
        let mut p = BitstreamParser {
            dequant_offsets: vec![NOT_RECEIVED; layout.block_count_32x32 as usize],
            payload: Vec::with_capacity(256 * 1024),
            decoded_blocks: 0,
            total_blocks_in_sequence: layout.block_count_32x32,
            last_seq: -1,
            decoded_frame_for_current_sequence: false,
            color: None,
            layout,
        };
        p.clear();
        p
    }

    pub fn clear(&mut self) {
        self.dequant_offsets.fill(NOT_RECEIVED);
        self.decoded_blocks = 0;
        self.last_seq = -1;
        self.decoded_frame_for_current_sequence = false;
        self.total_blocks_in_sequence = self.layout.block_count_32x32;
        self.payload.clear();
    }

    /// Colour signalling from the most recent sequence header, if one arrived.
    pub fn color(&self) -> Option<ColorInfo> {
        self.color
    }

    /// Payload words received so far, valid until the next push or clear.
    pub fn payload_words(&self) -> &[u32] {
        &self.payload
    }

    pub fn blocks_received(&self) -> u32 {
        self.decoded_blocks
    }

    pub fn blocks_expected(&self) -> u32 {
        self.total_blocks_in_sequence
    }

    /// Parses one packet (any number of concatenated records). Stale packets from an
    /// older sequence are ignored (`Ok`). Records parsed before an error stay applied.
    pub fn push_packet(&mut self, data: &[u8]) -> Result<(), PacketError> {
        let mut pos = 0; // in words
        let mut remaining = data.len();

        while remaining >= HEADER_BYTES {
            let (w0, w1) = (word(data, pos), word(data, pos + 1));
            let extended = w0 >> 31 == 1;
            let sequence = ((w0 >> 28) & SEQUENCE_MASK as u32) as i32;

            if extended {
                let is444 = (w1 >> 26) & 1 == 1;
                if is444 != (self.layout.chroma == Chroma::C444) {
                    return Err(PacketError::ChromaMismatch);
                }
                let diff = (sequence - self.last_seq) & SEQUENCE_MASK;
                if self.last_seq != -1 && diff > SEQUENCE_MASK / 2 {
                    return Ok(());
                }
                if self.last_seq == -1 || diff != 0 {
                    self.clear();
                    self.last_seq = sequence;
                }
                let code = (w1 >> 24) & 3;
                if code != CODE_START_OF_FRAME {
                    return Err(PacketError::UnknownMode(code));
                }
                let width = (w0 & 0x3fff) + 1;
                let height = ((w0 >> 14) & 0x3fff) + 1;
                if width != self.layout.width || height != self.layout.height {
                    return Err(PacketError::DimensionMismatch { width, height });
                }
                self.total_blocks_in_sequence = w1 & 0xff_ffff;
                self.color = Some(color_from(w1));
                pos += 2;
                remaining -= HEADER_BYTES;
                continue;
            }

            let payload_words = ((w0 >> 16) & 0xfff) as usize;
            let packet_bytes = payload_words * 4;
            if packet_bytes > remaining {
                return Err(PacketError::Truncated {
                    claimed: packet_bytes,
                    left: remaining,
                });
            }

            if self.last_seq == -1 {
                self.clear();
                self.last_seq = sequence;
            } else {
                let diff = (sequence - self.last_seq) & SEQUENCE_MASK;
                if diff > SEQUENCE_MASK / 2 {
                    return Ok(());
                }
                if diff != 0 {
                    self.clear();
                    self.last_seq = sequence;
                }
            }

            let block_index = w1 >> 8;
            if block_index >= self.layout.block_count_32x32 {
                return Err(PacketError::BlockOutOfBounds(block_index));
            }
            if payload_words < HEADER_BYTES / 4 {
                return Err(PacketError::ShortPayload);
            }

            if self.dequant_offsets[block_index as usize] == NOT_RECEIVED {
                self.decoded_blocks += 1;
                self.dequant_offsets[block_index as usize] = self.payload.len() as u32;
                self.payload.extend(
                    data[pos * 4..(pos + payload_words) * 4]
                        .chunks_exact(4)
                        .map(|c| u32::from_le_bytes(c.try_into().unwrap())),
                );
            }

            pos += payload_words;
            remaining -= packet_bytes;
        }

        if remaining != 0 {
            return Err(PacketError::Trailing);
        }
        Ok(())
    }

    /// Whether a decode should be submitted. Partial frames need the coarsest two
    /// levels complete and more than 90% of the blocks.
    pub fn is_ready(&self, allow_partial_frame: bool) -> bool {
        self.is_ready_with(allow_partial_frame, 2, 0.9)
    }

    pub fn is_ready_with(
        &self,
        allow_partial_frame: bool,
        pristine_bands: usize,
        minimum_packet_ratio: f64,
    ) -> bool {
        if self.decoded_frame_for_current_sequence || self.last_seq == -1 {
            return false;
        }
        if self.decoded_blocks < self.total_blocks_in_sequence {
            if !allow_partial_frame || !self.has_pristine_bands(pristine_bands) {
                return false;
            }
            if f64::from(self.decoded_blocks)
                <= f64::from(self.total_blocks_in_sequence) * minimum_packet_ratio
            {
                return false;
            }
        }
        true
    }

    /// Call once a frame has been submitted, so a sequence is not decoded twice.
    pub fn mark_frame_decoded(&mut self) {
        self.decoded_frame_for_current_sequence = true;
    }

    fn has_pristine_bands(&self, bands: usize) -> bool {
        let missing = |index: u32| self.dequant_offsets[index as usize] == NOT_RECEIVED;
        for band in 0..bands {
            for component in &self.layout.block_meta {
                let metas: Vec<_> = if band == 0 {
                    vec![component[DECOMPOSITION_LEVELS - 1][0]]
                } else {
                    (1..4)
                        .map(|b| component[DECOMPOSITION_LEVELS - band][b])
                        .collect()
                };
                for meta in metas {
                    if (0..meta.block_count_32x32).any(|i| missing(meta.block_offset_32x32 + i)) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

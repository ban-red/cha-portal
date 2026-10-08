//! A PyroWave frame as Vibepollo's host sends it, parsed into the packets our
//! decoders take.
//!
//! Written from `docs/plans/vibepollo-pyrowave.md` (section 3.2: the two
//! framings, the receiver's rejection rules and the resync rule) and, for the
//! bit layout of the two header kinds and the block layout, from the MIT
//! PyroWave bitstream our own `pyrowave-webgpu` and `cha-pyrowave-wgpu` port.
//! Nothing here comes from Vibepollo, Apollo, Sunshine or Moonlight code.
//!
//! The video receiver ([`super::video`]) reassembles a frame's packets (with
//! per-block Reed-Solomon recovery), zero-fills the data packets that never
//! came and notes which they were; [`parse_frame`] then turns that byte
//! stream into the sequence header and the whole block records, skipping the
//! records that lost bytes.
//!
//! **Record framing** (the default for clients that send `pyrowaveFeatures`
//! bit 0): a stream of 32-bit little-endian records.
//!
//! - The sequence header, 8 bytes, first. `w0` has `extended` (bit 31) set.
//! - Block records: an 8-byte header (`payload_words` counts the header's own
//!   two words) and the payload. This is exactly an upstream PyroWave
//!   "packet" holding one block, and what `BitstreamParser::pushPacket`
//!   takes, so the output needs no conversion.
//! - Padding: the word `0xFFFFFFFF`, a word count N, N zero words. (It can't
//!   pass for a sequence header: that would be 16384 pixels wide.)
//!
//! **Length-prefixed framing**: a u32 packet count, then each packet behind a
//! u32 size. Told from record framing by bit 31 of the first word, which a
//! count never has. No loss is tolerated.
//!
//! What is not known without a capture (spec section 6): the byte that carries
//! the per-packet record-start flag. [`RECORD_START_FLAG`] is its place; while
//! it is `None` the parser uses the documented fallback.

use std::ops::Range;

use crate::client::{PyrowaveFrame, PyrowaveFraming, PyrowaveRecord};
use crate::handoff::Chroma;

const HEADER_BYTES: usize = 8;
/// Starts a padding record.
const PADDING_WORD: u32 = 0xFFFF_FFFF;
/// The most packets a length-prefixed frame may claim (a frame has 4000 video packets at most).
const MAX_PREFIXED_PACKETS: u32 = 1 << 16;
const DECOMPOSITION_LEVELS: usize = 5;

/// Where a video packet says it begins a record: a bit of one byte of its NV
/// video packet header (offset from the start of that 16-byte header). The
/// protocol document says packets are "flagged `0x80`" and the reference
/// client exposes them as record-start, but not which header byte carries the
/// flag. **Open question, to settle with a capture.**
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordStartFlag {
    pub nv_header_byte: usize,
    pub mask: u8,
}

/// The record-start flag's place. `None` until a capture of a real host says
/// where it is; the parser then resyncs by its fallback rule (a payload whose
/// first record is a finer one that fits the payload with 8 bytes to spare).
/// The most likely home is the NV header's flags byte (offset 8), mask `0x80`,
/// where the stock bits are `0x1` picture data, `0x2` end of frame and `0x4`
/// start of frame; that is a guess, so it is not assumed.
pub const RECORD_START_FLAG: Option<RecordStartFlag> = None;

/// What the negotiated stream is: a sequence header that says otherwise is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamShape {
    pub width: u32,
    pub height: u32,
    pub chroma: Chroma,
}

/// Block counts of a picture's wavelet pyramid: all 32x32 blocks, and those of
/// the coarsest level (which come first and are the "critical" group).
/// From the upstream layout: five decomposition levels, level 4 the coarsest
/// with four bands, the others three, and no top-level chroma at 4:2:0.
pub fn block_counts(shape: StreamShape) -> Option<(u32, u32)> {
    if !(1..=16384).contains(&shape.width) || !(1..=16384).contains(&shape.height) {
        return None;
    }
    let aligned = |v: u32| ((v + 31) & !31).max(128);
    let (aw, ah) = (aligned(shape.width), aligned(shape.height));
    let (mut total, mut coarse) = (0u32, 0u32);
    for level in (0..DECOMPOSITION_LEVELS).rev() {
        let lw = ((aw / 2) >> level).max(1);
        let lh = ((ah / 2) >> level).max(1);
        let per_band = lw.div_ceil(8).div_ceil(4) * lh.div_ceil(8).div_ceil(4);
        let bands = if level == DECOMPOSITION_LEVELS - 1 {
            4
        } else {
            3
        };
        for component in 0..3 {
            if level == 0 && component != 0 && shape.chroma == Chroma::Yuv420 {
                continue;
            }
            total += per_band * bands;
            if level == DECOMPOSITION_LEVELS - 1 {
                coarse += per_band * bands;
            }
        }
    }
    Some((total, coarse))
}

/// A frame as the receiver hands it over.
pub struct RecordInput<'a> {
    /// The frame's payload bytes in order, the 8-byte frame header off and
    /// the last packet cut to its declared length, with lost data packets as
    /// zeros.
    pub bytes: &'a [u8],
    /// Which byte ranges of `bytes` were never received (sorted, disjoint).
    pub lost: &'a [Range<usize>],
    /// Where each video packet's payload begins in `bytes` (ascending; the
    /// first is 0).
    pub payload_starts: &'a [usize],
    /// Where packets flagged as record starts begin, when the flag is known
    /// ([`RECORD_START_FLAG`]); `None` to use the fallback rule.
    pub record_starts: Option<&'a [usize]>,
    /// Bytes of payload in a full video packet.
    pub payload_size: usize,
    /// For the report: the frame's video packets, and those lost.
    pub packets: u32,
    pub packets_lost: u32,
}

/// Why a frame is refused (and dropped; PyroWave never asks the host to repair).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reject {
    /// The first packet, with the sequence header, is lost (or the frame is empty).
    FirstPacketLost,
    /// The first record is not a sequence header.
    NoSequenceHeader,
    /// The sequence header's size or chroma differs from the negotiated stream.
    SequenceMismatch,
    /// A sequence header that is not a start of frame (conditional replenishment, which the host never sends).
    UnsupportedSequenceCode,
    /// A second sequence header in one frame.
    SecondSequenceHeader,
    /// A record with `payload_words` below 2.
    ShortRecord,
    /// A record running past the end of the frame.
    RecordPastEnd,
    /// A block whose `sequence` differs from the sequence header's.
    WrongSequence,
    /// A `block_index` outside the picture.
    BlockOutOfRange,
    /// Length-prefixed framing with any loss.
    LossInLengthPrefixed,
    /// Length-prefixed framing whose sizes don't add up.
    BadLengthPrefix,
}

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Reject::FirstPacketLost => "the first packet (the sequence header) is lost",
            Reject::NoSequenceHeader => "the frame doesn't start with a sequence header",
            Reject::SequenceMismatch => "the sequence header's size or chroma isn't the stream's",
            Reject::UnsupportedSequenceCode => "a sequence header that isn't a start of frame",
            Reject::SecondSequenceHeader => "a second sequence header",
            Reject::ShortRecord => "a record with payload_words below 2",
            Reject::RecordPastEnd => "a record running past the end of the frame",
            Reject::WrongSequence => "a block of another sequence",
            Reject::BlockOutOfRange => "a block_index outside the picture",
            Reject::LossInLengthPrefixed => "a loss in a length-prefixed frame",
            Reject::BadLengthPrefix => "a length-prefixed frame whose sizes don't add up",
        })
    }
}

impl std::error::Error for Reject {}

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

/// Whether any byte of `a..b` is lost.
fn lost_in(lost: &[Range<usize>], a: usize, b: usize) -> bool {
    let i = lost.partition_point(|r| r.end <= a);
    lost.get(i).is_some_and(|r| r.start < b)
}

/// What the sequence header fixed for the rest of the frame.
struct Sequence {
    number: u32,
    total_blocks: u32,
    layout_blocks: u32,
    coarse: u32,
}

/// Parses a frame into the sequence header and the block records that
/// arrived whole: the bytes (the packets a decoder takes, back to back) and
/// the frame's description. Records that lost bytes are skipped; the frame is
/// refused only by the rules of [`Reject`].
pub fn parse_frame(
    input: &RecordInput<'_>,
    shape: StreamShape,
) -> Result<(Vec<u8>, PyrowaveFrame), Reject> {
    let (layout_blocks, coarse) = block_counts(shape).ok_or(Reject::SequenceMismatch)?;
    let bytes = input.bytes;
    if bytes.len() < 4 || lost_in(input.lost, 0, 4) {
        return Err(Reject::FirstPacketLost);
    }
    if word(bytes, 0) >> 31 == 1 {
        return walk(input, shape, layout_blocks, coarse, PyrowaveFraming::Record);
    }

    // Length-prefixed: the first word is a packet count, whose bit 31 is clear.
    if !input.lost.is_empty() {
        return Err(Reject::LossInLengthPrefixed);
    }
    let packets = unprefix(bytes)?;
    let flat = RecordInput {
        bytes: &packets,
        lost: &[],
        payload_starts: &[],
        record_starts: None,
        payload_size: input.payload_size,
        packets: input.packets,
        packets_lost: 0,
    };
    walk(
        &flat,
        shape,
        layout_blocks,
        coarse,
        PyrowaveFraming::LengthPrefixed,
    )
}

/// The packets of a length-prefixed frame, back to back.
fn unprefix(bytes: &[u8]) -> Result<Vec<u8>, Reject> {
    let count = word(bytes, 0);
    if count == 0 || count > MAX_PREFIXED_PACKETS {
        return Err(Reject::BadLengthPrefix);
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut pos = 4usize;
    for _ in 0..count {
        let Some(size) = bytes.get(pos..pos + 4) else {
            return Err(Reject::BadLengthPrefix);
        };
        let size = u32::from_le_bytes(size.try_into().expect("4 bytes")) as usize;
        pos += 4;
        let Some(packet) = pos.checked_add(size).and_then(|end| bytes.get(pos..end)) else {
            return Err(Reject::BadLengthPrefix);
        };
        out.extend_from_slice(packet);
        pos += size;
    }
    // Whatever follows is the last payload's padding.
    if bytes[pos..].iter().any(|&b| b != 0) {
        return Err(Reject::BadLengthPrefix);
    }
    Ok(out)
}

fn walk(
    input: &RecordInput<'_>,
    shape: StreamShape,
    layout_blocks: u32,
    coarse: u32,
    framing: PyrowaveFraming,
) -> Result<(Vec<u8>, PyrowaveFrame), Reject> {
    let bytes = input.bytes;
    let end = bytes.len();
    if end < HEADER_BYTES {
        return Err(Reject::NoSequenceHeader);
    }
    if lost_in(input.lost, 0, HEADER_BYTES) {
        return Err(Reject::FirstPacketLost);
    }
    let (w0, w1) = (word(bytes, 0), word(bytes, 4));
    if w0 >> 31 == 0 || w0 == PADDING_WORD {
        return Err(Reject::NoSequenceHeader);
    }
    // Sequence header: width-1:14 | height-1:14 | sequence:3 | extended:1, then
    // total_blocks:24 | code:2 | chroma_resolution:1 | colour flags.
    let width = (w0 & 0x3fff) + 1;
    let height = ((w0 >> 14) & 0x3fff) + 1;
    let wide_chroma = (w1 >> 26) & 1 == 1;
    if (w1 >> 24) & 3 != 0 {
        return Err(Reject::UnsupportedSequenceCode);
    }
    if width != shape.width
        || height != shape.height
        || wide_chroma != (shape.chroma == Chroma::Yuv444)
    {
        return Err(Reject::SequenceMismatch);
    }
    let seq = Sequence {
        number: (w0 >> 28) & 7,
        total_blocks: w1 & 0xff_ffff,
        layout_blocks,
        coarse,
    };

    let mut data = Vec::with_capacity(end);
    data.extend_from_slice(&bytes[..HEADER_BYTES]);
    let mut records = vec![PyrowaveRecord {
        offset: 0,
        len: HEADER_BYTES as u32,
        critical: true,
    }];
    let mut seen = vec![false; layout_blocks as usize];
    let mut skipped = 0u32;

    let mut pos = HEADER_BYTES;
    while pos < end {
        // The frame's last payload is cut to its declared length, but where that
        // is unknown (last packet lost) or the host padded with zeros, the rest
        // of the frame is zeros: that is the end, not a record.
        if end - pos < HEADER_BYTES || word_or_zero(bytes, pos) == 0 {
            if bytes[pos..].iter().all(|&b| b == 0) {
                if lost_in(input.lost, pos, end) {
                    skipped += 1;
                }
                break;
            }
            if end - pos < HEADER_BYTES {
                return Err(Reject::RecordPastEnd);
            }
        }
        if lost_in(input.lost, pos, pos + 4) {
            // The record's own header is lost: nothing says how long it was.
            skipped += 1;
            match resync(input, &seq, pos) {
                Some(next) => {
                    pos = next;
                    continue;
                }
                None => break,
            }
        }
        let w0 = word(bytes, pos);
        if w0 == PADDING_WORD {
            if lost_in(input.lost, pos + 4, pos + 8) {
                // A padding record whose length is lost: not a record to count.
                match resync(input, &seq, pos) {
                    Some(next) => {
                        pos = next;
                        continue;
                    }
                    None => break,
                }
            }
            let words = word(bytes, pos + 4) as usize;
            let len = words
                .checked_mul(4)
                .and_then(|n| n.checked_add(HEADER_BYTES))
                .filter(|len| pos + len <= end)
                .ok_or(Reject::RecordPastEnd)?;
            pos += len;
            continue;
        }
        if w0 >> 31 == 1 {
            return Err(Reject::SecondSequenceHeader);
        }
        let payload_words = ((w0 >> 16) & 0xfff) as usize;
        if payload_words < HEADER_BYTES / 4 {
            return Err(Reject::ShortRecord);
        }
        let len = payload_words * 4;
        if pos + len > end {
            return Err(Reject::RecordPastEnd);
        }
        if (w0 >> 28) & 7 != seq.number {
            return Err(Reject::WrongSequence);
        }
        if lost_in(input.lost, pos, pos + len) {
            skipped += 1;
            pos += len;
            continue;
        }
        let block = word(bytes, pos + 4) >> 8;
        if block >= seq.layout_blocks {
            return Err(Reject::BlockOutOfRange);
        }
        seen[block as usize] = true;
        records.push(PyrowaveRecord {
            offset: data.len() as u32,
            len: len as u32,
            critical: block < seq.coarse,
        });
        data.extend_from_slice(&bytes[pos..pos + len]);
        pos += len;
    }

    let frame = PyrowaveFrame {
        framing,
        records,
        width,
        height,
        chroma: shape.chroma,
        total_blocks: seq.total_blocks,
        blocks_received: seen.iter().filter(|&&s| s).count() as u32,
        coarse_complete: seen[..seq.coarse as usize].iter().all(|&s| s),
        records_skipped: skipped,
        packets: input.packets,
        packets_lost: input.packets_lost,
    };
    Ok((data, frame))
}

fn word_or_zero(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map_or(0, |w| word(w, 0))
}

/// Where to pick the records up after one whose header was lost, or `None`
/// when nothing later can be trusted. With the record-start flag known, it is
/// the next packet flagged as one. Without it (the documented fallback), the
/// next payload start whose first record is a finer one (not of the coarsest
/// level) that fits in one payload with 8 bytes to spare: a sanity check that
/// a continuation of an oversized record, or garbage, rarely passes.
fn resync(input: &RecordInput<'_>, seq: &Sequence, pos: usize) -> Option<usize> {
    if let Some(starts) = input.record_starts {
        let i = starts.partition_point(|&s| s <= pos);
        return starts.get(i).copied();
    }
    let first = input.payload_starts.partition_point(|&s| s <= pos);
    input.payload_starts[first..]
        .iter()
        .copied()
        .find(|&at| plausible_fine_record(input, seq, at))
}

fn plausible_fine_record(input: &RecordInput<'_>, seq: &Sequence, at: usize) -> bool {
    let bytes = input.bytes;
    if at + HEADER_BYTES > bytes.len() || lost_in(input.lost, at, at + HEADER_BYTES) {
        return false;
    }
    let (w0, w1) = (word(bytes, at), word(bytes, at + 4));
    let payload_words = ((w0 >> 16) & 0xfff) as usize;
    let len = payload_words * 4;
    let block = w1 >> 8;
    w0 >> 31 == 0
        && payload_words >= HEADER_BYTES / 4
        && len + HEADER_BYTES <= input.payload_size
        && at + len <= bytes.len()
        && (w0 >> 28) & 7 == seq.number
        && block >= seq.coarse
        && block < seq.layout_blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_counts_follow_the_upstream_layout() {
        // 128x128 is the smallest picture: its coarsest level is one block per band.
        let tiny = StreamShape {
            width: 128,
            height: 128,
            chroma: Chroma::Yuv420,
        };
        let (total, coarse) = block_counts(tiny).unwrap();
        assert_eq!(coarse, 12, "3 components x 4 bands x 1 block");
        assert!(total > coarse);
        // Sizes under 128 are raised to it.
        let small = StreamShape {
            width: 64,
            height: 33,
            ..tiny
        };
        assert_eq!(block_counts(small), block_counts(tiny));
        // 4:4:4 codes the top level's chroma too.
        let wide = StreamShape {
            chroma: Chroma::Yuv444,
            ..tiny
        };
        assert!(block_counts(wide).unwrap().0 > total);
        assert!(block_counts(StreamShape { width: 0, ..tiny }).is_none());
        assert!(
            block_counts(StreamShape {
                height: 16385,
                ..tiny
            })
            .is_none()
        );
    }
}

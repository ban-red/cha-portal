//! Synthetic PyroWave frames, built from the rules of
//! `docs/plans/vibepollo-pyrowave.md` section 3.2 (record framing and
//! length-prefixed framing) and the upstream bitstream's header layout. No
//! real host produced any of these bytes: every test using them says
//! "synthetic" in its name.

use cha_gamestream::client::media::pyrowave::{StreamShape, block_counts};
use cha_gamestream::handoff::Chroma;

pub const PADDING_WORD: u32 = 0xFFFF_FFFF;

/// The sequence header: width-1:14 | height-1:14 | sequence:3 | extended:1,
/// then total_blocks:24 | code:2 | chroma_resolution:1 | colour flags.
pub fn sequence_header(shape: StreamShape, sequence: u32, total_blocks: u32, code: u32) -> Vec<u8> {
    let w0 = (shape.width - 1) | ((shape.height - 1) << 14) | (sequence << 28) | (1 << 31);
    let w1 = (total_blocks & 0xff_ffff)
        | (code << 24)
        | (u32::from(shape.chroma == Chroma::Yuv444) << 26);
    [w0.to_le_bytes(), w1.to_le_bytes()].concat()
}

/// A block record of `words` 32-bit words, header included: ballot:16 |
/// payload_words:12 | sequence:3 | extended:1, then quant_code:8 | block_index:24.
/// The payload is a pattern with no zero byte, so nothing in it looks like padding.
pub fn block_record(index: u32, words: usize, sequence: u32) -> Vec<u8> {
    assert!((2..4096).contains(&words));
    let w0 = 0xBEEF | ((words as u32) << 16) | (sequence << 28);
    let w1 = 0x21 | (index << 8);
    let mut out = [w0.to_le_bytes(), w1.to_le_bytes()].concat();
    out.extend(
        (0..(words - 2) * 4).map(|i| (i as u8).wrapping_mul(7).wrapping_add(index as u8) | 1),
    );
    out
}

/// A padding record of exactly `bytes` bytes (8 or more, a multiple of 4).
pub fn padding(bytes: usize) -> Vec<u8> {
    assert!(bytes >= 8 && bytes.is_multiple_of(4));
    let mut out = PADDING_WORD.to_le_bytes().to_vec();
    out.extend((((bytes - 8) / 4) as u32).to_le_bytes());
    out.resize(bytes, 0);
    out
}

/// A laid-out record frame (the bytes after the 8-byte frame header).
#[derive(Clone, Debug)]
pub struct Laid {
    pub bytes: Vec<u8>,
    /// Where each video packet's payload begins in `bytes`.
    pub payload_starts: Vec<usize>,
    /// `(offset, len, block index)` of each block record, in stream order.
    pub records: Vec<(usize, usize, u32)>,
    /// Offsets of the packets that begin with a record (what the record-start flag marks).
    pub record_starts: Vec<usize>,
}

/// Bytes left in the payload that position `at` (a stream offset) is in; a
/// whole payload when `at` is on a boundary. Payload 0 loses 8 bytes to the frame header.
fn room(at: usize, payload: usize) -> usize {
    ((at + 8) / payload + 1) * payload - 8 - at
}

/// Lays records out as the spec describes (section 3.2) for payloads of
/// `payload` bytes whose first carries the 8-byte frame header: the sequence
/// header, then the coarsest group, then the rest, each group starting on a
/// payload boundary, records packed without crossing a payload except the
/// oversized ones (larger than the payload less 8), and padding where nothing
/// else fits. Packing is next-fit, not first-fit: the parser can't tell.
/// Sizes that would leave a gap of 4 bytes (too small for padding) panic.
pub fn lay_out(shape: StreamShape, sequence: u32, blocks: &[(u32, usize)], payload: usize) -> Laid {
    assert!(payload.is_multiple_of(4) && payload >= 24);
    let (_, coarse) = block_counts(shape).unwrap();
    let mut bytes = sequence_header(shape, sequence, blocks.len() as u32, 0);
    let mut records = Vec::new();
    let mut starts = std::collections::BTreeSet::from([0usize]);
    let fill = |bytes: &mut Vec<u8>, starts: &mut std::collections::BTreeSet<usize>| {
        let r = room(bytes.len(), payload);
        if r != payload {
            starts.insert(bytes.len());
            bytes.extend(padding(r));
        }
    };
    for group in 0..2 {
        let mut members: Vec<(u32, usize)> = blocks
            .iter()
            .copied()
            .filter(|&(index, _)| (index < coarse) == (group == 0))
            .collect();
        // Oversized records first (a stable sort keeps the rest in order).
        members.sort_by_key(|&(_, words)| usize::from(words * 4 <= payload - 8));
        if group == 1 {
            fill(&mut bytes, &mut starts);
        }
        for (index, words) in members {
            let record = block_record(index, words, sequence);
            let r = room(bytes.len(), payload);
            let oversized = record.len() > payload - 8;
            if !oversized && (record.len() > r || r - record.len() == 4) {
                fill(&mut bytes, &mut starts);
            }
            starts.insert(bytes.len());
            records.push((bytes.len(), record.len(), index));
            bytes.extend(record);
        }
    }
    let payload_starts: Vec<usize> = std::iter::once(0)
        .chain((1..).map(|k| k * payload - 8))
        .take_while(|&s| s < bytes.len())
        .collect();
    let record_starts = payload_starts
        .iter()
        .copied()
        .filter(|s| starts.contains(s))
        .collect();
    Laid {
        bytes,
        payload_starts,
        records,
        record_starts,
    }
}

/// A length-prefixed frame: the packet count, then each packet behind its
/// size. `packets` are lists of whole records (the first starts with the
/// sequence header).
pub fn length_prefixed(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut out = (packets.len() as u32).to_le_bytes().to_vec();
    for p in packets {
        out.extend((p.len() as u32).to_le_bytes());
        out.extend(p);
    }
    out
}

//! The PyroWave record parser (`client::media::pyrowave`) on synthetic frames
//! built from the rules of `docs/plans/vibepollo-pyrowave.md` section 3.2.
//! No real Vibepollo host produced any byte here, so every test says
//! "synthetic": they check the parser against our reading of the spec, not
//! against the host.

mod common;

use std::ops::Range;

use cha_gamestream::client::media::pyrowave::{
    RecordInput, Reject, StreamShape, block_counts, parse_frame,
};
use cha_gamestream::client::{PyrowaveFrame, PyrowaveFraming};
use cha_gamestream::handoff::Chroma;
use common::synthetic_pyrowave::{
    Laid, PADDING_WORD, block_record, lay_out, length_prefixed, padding, sequence_header,
};

/// Payload of a 1392-byte packet (16 bytes of NV header).
const PAYLOAD: usize = 1376;

const SHAPE: StreamShape = StreamShape {
    width: 256,
    height: 256,
    chroma: Chroma::Yuv420,
};

/// Every coarse block (small), then fine blocks of varying size, with one
/// oversized record (it spans payloads) at index `coarse + 5`.
fn blocks() -> Vec<(u32, usize)> {
    let (total, coarse) = block_counts(SHAPE).unwrap();
    (0..total.min(coarse + 40))
        .map(|i| {
            let words = if i < coarse {
                30 + i as usize
            } else if i == coarse + 5 {
                700 // 2800 bytes: more than a payload
            } else {
                40 + (i as usize * 37) % 200
            };
            (i, words)
        })
        .collect()
}

fn laid() -> Laid {
    lay_out(SHAPE, 3, &blocks(), PAYLOAD)
}

struct Frame<'a> {
    laid: &'a Laid,
    lost: Vec<Range<usize>>,
    flags: bool,
}

impl<'a> Frame<'a> {
    fn whole(laid: &'a Laid) -> Self {
        Self {
            laid,
            lost: Vec::new(),
            flags: false,
        }
    }

    fn lose(mut self, range: Range<usize>) -> Self {
        self.lost.push(range);
        self.lost.sort_by_key(|r| r.start);
        self
    }

    /// The video packet `k` never came.
    fn lose_packet(self, k: usize) -> Self {
        let starts = &self.laid.payload_starts;
        let end = starts.get(k + 1).copied().unwrap_or(self.laid.bytes.len());
        let range = starts[k]..end;
        self.lose(range)
    }

    fn with_flags(mut self) -> Self {
        self.flags = true;
        self
    }

    fn parse(&self, shape: StreamShape) -> Result<(Vec<u8>, PyrowaveFrame), Reject> {
        // What the receiver does: lost bytes are zeros.
        let mut bytes = self.laid.bytes.clone();
        for r in &self.lost {
            bytes[r.clone()].fill(0);
        }
        parse_frame(
            &RecordInput {
                bytes: &bytes,
                lost: &self.lost,
                payload_starts: &self.laid.payload_starts,
                record_starts: self.flags.then_some(&self.laid.record_starts[..]),
                payload_size: PAYLOAD,
                packets: self.laid.payload_starts.len() as u32,
                packets_lost: 0,
            },
            shape,
        )
    }
}

/// The bytes a decoder should get when records `kept` survive: the sequence
/// header and those records, back to back.
fn expect(laid: &Laid, kept: impl Fn(usize, u32) -> bool) -> Vec<u8> {
    let mut out = laid.bytes[..8].to_vec();
    for (i, &(offset, len, index)) in laid.records.iter().enumerate() {
        if kept(i, index) {
            out.extend_from_slice(&laid.bytes[offset..offset + len]);
        }
    }
    out
}

fn record_bytes<'a>(frame: &PyrowaveFrame, data: &'a [u8]) -> Vec<&'a [u8]> {
    (0..frame.records.len())
        .map(|i| frame.record(data, i).unwrap())
        .collect()
}

#[test]
fn synthetic_block_counts_for_a_1080p_picture_are_the_layouts() {
    // Worked out from the upstream layout: five levels, the coarsest with four
    // bands of 2x2 blocks per component at 1920x1088, and no top-level chroma at 4:2:0.
    let hd = |chroma| StreamShape {
        width: 1920,
        height: 1080,
        chroma,
    };
    assert_eq!(block_counts(hd(Chroma::Yuv420)), Some((3261, 48)));
    assert_eq!(block_counts(hd(Chroma::Yuv444)), Some((6321, 48)));
    // More sizes, checked against `cha_pyrowave_wgpu::BlockLayout` (MIT, the decoder's own count).
    for (width, height, chroma, want) in [
        (2560, 1440, Chroma::Yuv420, (5667, 72)),
        (2560, 1440, Chroma::Yuv444, (11187, 72)),
        (1280, 720, Chroma::Yuv420, (1473, 24)),
        (3840, 2160, Chroma::Yuv420, (12429, 144)),
        (256, 256, Chroma::Yuv420, (114, 12)),
        (100, 50, Chroma::Yuv444, (75, 12)),
    ] {
        let shape = StreamShape {
            width,
            height,
            chroma,
        };
        assert_eq!(block_counts(shape), Some(want), "{shape:?}");
    }
}

#[test]
fn synthetic_frames_parse_into_the_sequence_header_and_whole_records() {
    let laid = laid();
    // The layout has what the spec describes: padding, an oversized record spanning packets.
    assert!(laid.payload_starts.len() > 3);
    assert!(
        laid.records
            .iter()
            .any(|&(o, l, _)| (o + 8) / PAYLOAD != (o + l - 1 + 8) / PAYLOAD),
        "one record spans payloads"
    );
    assert!(
        laid.bytes
            .windows(4)
            .any(|w| w == PADDING_WORD.to_le_bytes()),
        "and padding fills the gaps"
    );

    let (data, frame) = Frame::whole(&laid).parse(SHAPE).unwrap();
    assert_eq!(data, expect(&laid, |_, _| true));
    assert_eq!(frame.framing, PyrowaveFraming::Record);
    assert_eq!((frame.width, frame.height), (256, 256));
    assert_eq!(frame.chroma, Chroma::Yuv420);
    assert_eq!(frame.total_blocks, blocks().len() as u32);
    assert_eq!(frame.blocks_received, blocks().len() as u32);
    assert!(frame.coarse_complete);
    assert_eq!(frame.records_skipped, 0);
    // One record per block, in stream order, after the sequence header.
    assert_eq!(frame.records.len(), 1 + blocks().len());
    let (_, coarse) = block_counts(SHAPE).unwrap();
    assert!(frame.records[0].critical && frame.records[0].len == 8);
    for (rec, &(_, _, index)) in frame.records[1..].iter().zip(&laid.records) {
        assert_eq!(rec.critical, index < coarse);
    }
    // Each is exactly an upstream packet of one block: payload_words * 4 bytes.
    for bytes in &record_bytes(&frame, &data)[1..] {
        let w0 = u32::from_le_bytes(bytes[..4].try_into().unwrap());
        assert_eq!(bytes.len(), ((w0 >> 16) & 0xfff) as usize * 4);
    }
}

#[test]
fn synthetic_trailing_zeros_end_the_frame_and_are_not_a_record() {
    let mut laid = laid();
    laid.bytes.extend([0u8; 300]);
    let (data, frame) = Frame::whole(&laid).parse(SHAPE).unwrap();
    assert_eq!(frame.records.len(), 1 + blocks().len());
    assert_eq!(data.len(), expect(&laid, |_, _| true).len());
    // But zeros followed by a record are a short record, refused.
    let mut broken = laid.clone();
    broken.bytes.extend(block_record(30, 50, 3));
    assert_eq!(
        Frame::whole(&broken).parse(SHAPE).err(),
        Some(Reject::ShortRecord)
    );
}

#[test]
fn synthetic_frames_are_refused_by_the_receivers_rules() {
    let shape_with = |width, height, chroma| StreamShape {
        width,
        height,
        chroma,
    };
    let base = laid();
    let (total, _) = block_counts(SHAPE).unwrap();
    let record_at = |laid: &Laid, n: usize| laid.records[n].0;
    let set_word = |laid: &mut Laid, at: usize, w: u32| {
        laid.bytes[at..at + 4].copy_from_slice(&w.to_le_bytes());
    };
    let edit = |laid: &mut Laid, at: usize, f: &dyn Fn(u32) -> u32| {
        let old = u32::from_le_bytes(laid.bytes[at..at + 4].try_into().unwrap());
        laid.bytes[at..at + 4].copy_from_slice(&f(old).to_le_bytes());
    };

    // payload_words below 2.
    let mut l = base.clone();
    let at = record_at(&l, 3);
    edit(&mut l, at, &|w| (w & !(0xfff << 16)) | (1 << 16));
    assert_eq!(
        Frame::whole(&l).parse(SHAPE).err(),
        Some(Reject::ShortRecord)
    );

    // A record running past the end of the frame.
    let mut l = base.clone();
    let at = record_at(&l, l.records.len() - 1);
    edit(&mut l, at, &|w| (w & !(0xfff << 16)) | (0xfff << 16));
    assert_eq!(
        Frame::whole(&l).parse(SHAPE).err(),
        Some(Reject::RecordPastEnd)
    );

    // A padding record whose count runs past the end.
    let mut l = base.clone();
    l.bytes.extend(padding(8));
    let at = l.bytes.len() - 4;
    set_word(&mut l, at, 100_000);
    assert_eq!(
        Frame::whole(&l).parse(SHAPE).err(),
        Some(Reject::RecordPastEnd)
    );

    // A sequence header whose size or chroma differs from the stream's.
    for shape in [
        shape_with(512, 256, Chroma::Yuv420),
        shape_with(256, 128, Chroma::Yuv420),
        shape_with(256, 256, Chroma::Yuv444),
    ] {
        assert_eq!(
            Frame::whole(&base).parse(shape).err(),
            Some(Reject::SequenceMismatch),
            "{shape:?}"
        );
    }

    // A conditional-replenishment sequence header (code 1), which the host never sends.
    let mut l = base.clone();
    l.bytes[..8].copy_from_slice(&sequence_header(SHAPE, 3, 10, 1));
    assert_eq!(
        Frame::whole(&l).parse(SHAPE).err(),
        Some(Reject::UnsupportedSequenceCode)
    );

    // A second sequence header.
    let mut l = base.clone();
    let at = record_at(&l, 2);
    let second = sequence_header(SHAPE, 3, 10, 0);
    l.bytes[at..at + 8].copy_from_slice(&second);
    assert_eq!(
        Frame::whole(&l).parse(SHAPE).err(),
        Some(Reject::SecondSequenceHeader)
    );

    // A block of another sequence.
    let mut l = base.clone();
    let at = record_at(&l, 4);
    edit(&mut l, at, &|w| (w & !(7 << 28)) | (5 << 28));
    assert_eq!(
        Frame::whole(&l).parse(SHAPE).err(),
        Some(Reject::WrongSequence)
    );

    // A block index outside the picture.
    let mut l = base.clone();
    let at = record_at(&l, 4) + 4;
    set_word(&mut l, at, 0x21 | (total << 8));
    assert_eq!(
        Frame::whole(&l).parse(SHAPE).err(),
        Some(Reject::BlockOutOfRange)
    );

    // No sequence header first: padding where it should be (still bit 31 set).
    let mut l = base.clone();
    l.bytes[..8].copy_from_slice(&padding(8));
    assert_eq!(
        Frame::whole(&l).parse(SHAPE).err(),
        Some(Reject::NoSequenceHeader)
    );

    // The first packet lost.
    assert_eq!(
        Frame::whole(&base).lose(0..40).parse(SHAPE).err(),
        Some(Reject::FirstPacketLost)
    );
    // Nothing at all.
    let empty = RecordInput {
        bytes: &[],
        lost: &[],
        payload_starts: &[],
        record_starts: None,
        payload_size: PAYLOAD,
        packets: 0,
        packets_lost: 0,
    };
    assert_eq!(
        parse_frame(&empty, SHAPE).err(),
        Some(Reject::FirstPacketLost)
    );
}

#[test]
fn synthetic_record_that_lost_bytes_is_skipped_and_the_rest_kept() {
    let laid = laid();
    // Lose 8 bytes in the middle of record 20's body (its header is intact).
    let (offset, len, lost_index) = laid.records[20];
    let hit = offset + len / 2..offset + len / 2 + 8;
    let (data, frame) = Frame::whole(&laid).lose(hit).parse(SHAPE).unwrap();
    assert_eq!(data, expect(&laid, |i, _| i != 20));
    assert_eq!(frame.records_skipped, 1);
    assert_eq!(frame.blocks_received, blocks().len() as u32 - 1);
    assert!(frame.coarse_complete, "{lost_index} is a fine block");

    // A coarse record lost: the frame goes on, but the coarse level isn't whole.
    let (offset, ..) = laid.records[2];
    let (data, frame) = Frame::whole(&laid)
        .lose(offset + 10..offset + 12)
        .parse(SHAPE)
        .unwrap();
    assert_eq!(data, expect(&laid, |i, _| i != 2));
    assert!(!frame.coarse_complete);

    // The second word of a record's header lost: the length is known, the block isn't.
    let (offset, ..) = laid.records[9];
    let (data, _) = Frame::whole(&laid)
        .lose(offset + 4..offset + 8)
        .parse(SHAPE)
        .unwrap();
    assert_eq!(data, expect(&laid, |i, _| i != 9));
}

#[test]
fn synthetic_oversized_record_with_a_lost_tail_is_skipped_by_its_length() {
    let laid = laid();
    let (n, &(offset, len, _)) = laid
        .records
        .iter()
        .enumerate()
        .find(|(_, r)| r.1 > PAYLOAD)
        .expect("one oversized record");
    // Its last packet is lost (the header is in the first): skipped by length,
    // and the records after it are read from exactly where it ends.
    let last_payload = (offset + len - 1 + 8) / PAYLOAD;
    let (data, frame) = Frame::whole(&laid)
        .lose_packet(last_payload)
        .parse(SHAPE)
        .unwrap();
    assert!(frame.records_skipped >= 1);
    let gone = |i: usize| {
        let (o, l, _) = laid.records[i];
        let lost = laid.payload_starts[last_payload]
            ..laid
                .payload_starts
                .get(last_payload + 1)
                .copied()
                .unwrap_or(laid.bytes.len());
        o < lost.end && o + l > lost.start
    };
    assert!(gone(n));
    assert_eq!(data, expect(&laid, |i, _| !gone(i)));
}

#[test]
fn synthetic_lost_packet_resyncs_at_the_next_flagged_packet() {
    let laid = laid();
    // A fine, packed packet in the middle (not the oversized spill, not the first).
    let k = laid
        .payload_starts
        .iter()
        .position(|&s| laid.records.iter().any(|&(o, _, i)| o == s && i > 40))
        .expect("a packet starting with a fine record");
    let k = k.max(2);
    let lost: Range<usize> = laid.payload_starts[k]..laid.payload_starts[k + 1];
    let (data, frame) = Frame::whole(&laid)
        .with_flags()
        .lose_packet(k)
        .parse(SHAPE)
        .unwrap();
    let in_lost = |i: usize| {
        let (o, l, _) = laid.records[i];
        o < lost.end && o + l > lost.start
    };
    assert!(laid.records.iter().enumerate().any(|(i, _)| in_lost(i)));
    assert_eq!(data, expect(&laid, |i, _| !in_lost(i)));
    assert!(frame.records_skipped >= 1);
}

#[test]
fn synthetic_lost_packet_resyncs_without_flags_at_a_plausible_fine_record() {
    // The documented fallback: no flags, so pick up at a payload start whose
    // first record is a finer one that fits a payload with 8 bytes to spare.
    let laid = laid();
    let (_, coarse) = block_counts(SHAPE).unwrap();
    let k = laid
        .payload_starts
        .iter()
        .enumerate()
        .skip(2)
        .find(|&(_, &s)| {
            laid.records
                .iter()
                .any(|&(o, l, i)| o == s && i > coarse + 6 && l + 8 <= PAYLOAD)
        })
        .map(|(k, _)| k)
        .expect("a packet that starts with a fine record");
    let lost = laid.payload_starts[k]..laid.payload_starts[k + 1];
    let with_flags = Frame::whole(&laid)
        .with_flags()
        .lose_packet(k)
        .parse(SHAPE)
        .unwrap();
    let without = Frame::whole(&laid).lose_packet(k).parse(SHAPE).unwrap();
    let in_lost = |i: usize| {
        let (o, l, _) = laid.records[i];
        o < lost.end && o + l > lost.start
    };
    assert_eq!(without.0, expect(&laid, |i, _| !in_lost(i)));
    assert_eq!(
        without.0, with_flags.0,
        "the fallback finds what the flags would"
    );
}

#[test]
fn synthetic_fallback_does_not_resync_inside_an_oversized_record() {
    // Losing the first packet of an oversized record loses its header. The
    // packets that follow start inside the record (payload data), which must not
    // be taken for records; the parser picks up where the record ends.
    let laid = laid();
    let (n, &(offset, len, _)) = laid
        .records
        .iter()
        .enumerate()
        .find(|(_, r)| r.1 > PAYLOAD)
        .unwrap();
    let first = (offset + 8) / PAYLOAD;
    let last = (offset + len - 1 + 8) / PAYLOAD;
    assert!(last > first, "it spans packets");
    // Lose from the record's own start to the end of its first packet.
    let lost = offset..laid.payload_starts[first + 1];
    let (data, frame) = Frame::whole(&laid).lose(lost).parse(SHAPE).unwrap();
    let genuine: Vec<&[u8]> = laid
        .records
        .iter()
        .map(|&(o, l, _)| &laid.bytes[o..o + l])
        .collect();
    let kept: Vec<&[u8]> = record_bytes(&frame, &data)[1..].to_vec();
    // Nothing that comes out is garbage: each is one of the records sent, whole.
    for record in &kept {
        assert!(genuine.contains(record));
    }
    assert!(!kept.contains(&genuine[n]), "the oversized record is gone");
    // The records before it survive, in order.
    assert_eq!(kept[..n], genuine[..n]);
    assert!(frame.records_skipped >= 1);
}

#[test]
fn synthetic_frame_with_nothing_to_resync_to_ends_with_what_it_has() {
    let laid = laid();
    // Everything from record 20 to the end is lost: the parser keeps the first 20.
    let (offset, ..) = laid.records[20];
    let (data, frame) = Frame::whole(&laid)
        .lose(offset..laid.bytes.len())
        .parse(SHAPE)
        .unwrap();
    assert_eq!(data, expect(&laid, |i, _| i < 20));
    assert!(frame.records_skipped >= 1);
    assert_eq!(frame.records.len(), 21);
}

#[test]
fn synthetic_length_prefixed_frames_are_told_by_bit_31_and_parsed() {
    let (_, coarse) = block_counts(SHAPE).unwrap();
    // Packets at 1024-byte boundaries, as upstream's packetize makes them: the
    // first starts with the sequence header.
    let header = sequence_header(SHAPE, 2, 3, 0);
    let a = block_record(0, 40, 2);
    let b = block_record(coarse + 3, 90, 2);
    let c = block_record(coarse + 9, 120, 2);
    let packets = vec![
        [header.clone(), a.clone()].concat(),
        [b.clone(), c.clone()].concat(),
    ];
    let wire = length_prefixed(&packets);
    assert_eq!(wire[3] & 0x80, 0, "a count never has bit 31");
    let parse = |bytes: &[u8], lost: &[Range<usize>]| {
        parse_frame(
            &RecordInput {
                bytes,
                lost,
                payload_starts: &[0],
                record_starts: None,
                payload_size: PAYLOAD,
                packets: 1,
                packets_lost: 0,
            },
            SHAPE,
        )
    };
    let (data, frame) = parse(&wire, &[]).unwrap();
    assert_eq!(frame.framing, PyrowaveFraming::LengthPrefixed);
    assert_eq!(data, [header.clone(), a, b, c].concat());
    assert_eq!(frame.records.len(), 4);
    assert_eq!(
        frame.records.iter().map(|r| r.critical).collect::<Vec<_>>(),
        [true, true, false, false]
    );
    assert_eq!(frame.blocks_received, 3);
    assert!(!frame.coarse_complete);

    // The last payload's zero padding is fine; anything else after the packets is not.
    let mut padded = wire.clone();
    padded.extend([0u8; 17]);
    assert!(parse(&padded, &[]).is_ok());
    let mut junk = wire.clone();
    junk.push(7);
    assert_eq!(parse(&junk, &[]).err(), Some(Reject::BadLengthPrefix));

    // Any loss drops the frame.
    assert_eq!(
        parse(&wire, std::slice::from_ref(&(100..104))).err(),
        Some(Reject::LossInLengthPrefixed)
    );
    // Sizes that don't add up, a zero count, an absurd count.
    let mut short = wire.clone();
    short.truncate(wire.len() - 5);
    assert_eq!(parse(&short, &[]).err(), Some(Reject::BadLengthPrefix));
    assert_eq!(
        parse(&[0, 0, 0, 0, 1, 2, 3, 4], &[]).err(),
        Some(Reject::BadLengthPrefix)
    );
    assert_eq!(
        parse(&[0xff, 0xff, 0xff, 0x7f, 1, 2, 3, 4], &[]).err(),
        Some(Reject::BadLengthPrefix)
    );
    // The same rules apply inside: a block before... a wrong sequence is refused.
    let wrong = length_prefixed(&[[header, block_record(0, 40, 6)].concat()]);
    assert_eq!(parse(&wrong, &[]).err(), Some(Reject::WrongSequence));
}

/// Whatever the bytes and the losses, the parser returns, and what it hands on
/// is a sequence header and whole records inside its own output.
#[test]
fn synthetic_hostile_frames_never_panic_and_never_hand_on_garbage() {
    let mut seed = 0x005E_ED0F_FEEDu64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let base = laid();
    for round in 0..20_000 {
        let mut bytes = base.bytes.clone();
        for _ in 0..next() % 6 {
            let at = (next() % bytes.len() as u64) as usize;
            // Often a header word, often anywhere.
            let at = if next() % 2 == 0 { at & !3 } else { at };
            bytes[at] = next() as u8;
        }
        if next() % 7 == 0 {
            bytes.truncate((next() % bytes.len() as u64) as usize);
        }
        let mut lost: Vec<Range<usize>> = Vec::new();
        let mut at = 0usize;
        for _ in 0..next() % 5 {
            at += (next() % 800) as usize;
            let len = (next() % 1500) as usize;
            if at >= bytes.len() {
                break;
            }
            let end = (at + len).min(bytes.len());
            lost.push(at..end);
            at = end + 1;
        }
        for r in &lost {
            bytes[r.clone()].fill(0);
        }
        let starts: Vec<usize> = base
            .payload_starts
            .iter()
            .copied()
            .filter(|&s| s < bytes.len())
            .collect();
        let input = RecordInput {
            bytes: &bytes,
            lost: &lost,
            payload_starts: &starts,
            record_starts: (round % 2 == 0).then_some(&base.record_starts[..]),
            payload_size: PAYLOAD,
            packets: starts.len() as u32,
            packets_lost: 0,
        };
        if let Ok((data, frame)) = parse_frame(&input, SHAPE) {
            let mut end = 0;
            for (i, r) in frame.records.iter().enumerate() {
                assert_eq!(r.offset as usize, end);
                end += r.len as usize;
                let bytes = frame.record(&data, i).expect("inside the output");
                if i > 0 {
                    let w0 = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                    assert_eq!(bytes.len(), ((w0 >> 16) & 0xfff) as usize * 4);
                }
            }
            assert_eq!(end, data.len());
        }
    }
}

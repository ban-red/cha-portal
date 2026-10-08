//! A PyroWave frame from the host, re-emitted as `cha-stream/1` intra
//! datagrams the way `cha-streamer` sends its own (`wt.rs`, `send_packets`):
//! `KEYFRAME | INTRA` on every datagram, one PyroWave packet per datagram or,
//! for a packet bigger than a datagram, several flagged `CONTINUES` (all but
//! the last) and `CONTINUED` (all but the first), so the page uses a packet
//! only whole. No FEC (`fec = 0`): PyroWave frames degrade, they don't stall.
//!
//! The host's frame arrives as the sequence header and the whole block records
//! that survived the network (`cha_gamestream::client::PyrowaveFrame`), so
//! what a lost record cost is already visible to the page as blocks it never
//! gets; it decodes them as zero coefficients (`partial = true`). Nothing is
//! transcoded: the page runs the existing `@cha/pyrowave-webgpu` decoder.
//!
//! Records are grouped into packets so a datagram isn't a single 100-byte
//! block: consecutive records go into one packet up to the datagram's room,
//! and the coarsest level's records (the sequence header's group) are never
//! mixed with finer ones, so a packet is [`Flags::CRITICAL`] only if all of it
//! is. The streamer doesn't set `CRITICAL` on PyroWave and the page doesn't
//! read it yet; it is set here because the mapping is free and the flag's
//! meaning ("candidates for duplication or FEC") fits.

use std::ops::Range;

use cha_gamestream::client::PyrowaveFrame;
use cha_proto::{DatagramHeader, Flags, HEADER_LEN, Kind};

/// One PyroWave packet: records of one group, back to back in the frame's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub range: Range<usize>,
    pub critical: bool,
}

/// Groups the frame's records into packets of at most `max_payload` bytes
/// where records allow (a record bigger than that is a packet of its own).
/// The sequence header leads the first packet.
pub fn packets(info: &PyrowaveFrame, max_payload: usize) -> Vec<Packet> {
    let mut out: Vec<Packet> = Vec::new();
    for record in &info.records {
        let start = record.offset as usize;
        let end = start + record.len as usize;
        match out.last_mut() {
            Some(open)
                if open.critical == record.critical
                    && open.range.end == start
                    && end - open.range.start <= max_payload =>
            {
                open.range.end = end;
            }
            _ => out.push(Packet {
                range: start..end,
                critical: record.critical,
            }),
        }
    }
    out
}

/// The datagrams that carry the frame: each a [`HEADER_LEN`]-byte header and
/// at most `max_datagram - HEADER_LEN` bytes of payload. `None` if the frame
/// would need more than 65535 of them, or `max_datagram` has no room for data.
pub fn datagrams(
    data: &[u8],
    info: &PyrowaveFrame,
    frame_id: u32,
    stream: u8,
    send_ts_us: u32,
    max_datagram: usize,
) -> Option<Vec<Vec<u8>>> {
    let max_payload = max_datagram.checked_sub(HEADER_LEN).filter(|&m| m > 0)?;
    let packets = packets(info, max_payload);
    let count: usize = packets
        .iter()
        .map(|p| p.range.len().div_ceil(max_payload).max(1))
        .sum();
    let frag_count = u16::try_from(count).ok().filter(|&c| c > 0)?;

    let mut out = Vec::with_capacity(count);
    for packet in &packets {
        let unit = data.get(packet.range.clone())?;
        let mut chunks = unit.chunks(max_payload).enumerate().peekable();
        while let Some((part, chunk)) = chunks.next() {
            let mut flags = Flags::KEYFRAME | Flags::INTRA;
            if packet.critical {
                flags |= Flags::CRITICAL;
            }
            if chunks.peek().is_some() {
                flags |= Flags::CONTINUES;
            }
            if part > 0 {
                flags |= Flags::CONTINUED;
            }
            let header = DatagramHeader {
                kind: Kind::Video,
                flags: Flags(flags),
                stream,
                fec: 0,
                frame_id,
                frag_index: out.len() as u16,
                frag_count,
                send_ts_us,
            };
            let mut head = [0u8; HEADER_LEN];
            header.encode(&mut head);
            let mut datagram = Vec::with_capacity(HEADER_LEN + chunk.len());
            datagram.extend_from_slice(&head);
            datagram.extend_from_slice(chunk);
            out.push(datagram);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use cha_gamestream::client::{PyrowaveFraming, PyrowaveRecord};
    use cha_gamestream::handoff::Chroma;

    use super::*;

    /// Synthetic: a frame as the media client would hand it over (the bytes
    /// are made up; only the shape matters here). Record sizes in bytes, the
    /// first the sequence header; `critical` for the first `coarse` of them.
    fn synthetic_frame(sizes: &[usize], coarse: usize) -> (Vec<u8>, PyrowaveFrame) {
        let mut data = Vec::new();
        let mut records = Vec::new();
        for (i, &len) in sizes.iter().enumerate() {
            records.push(PyrowaveRecord {
                offset: data.len() as u32,
                len: len as u32,
                critical: i < coarse,
            });
            data.extend((0..len).map(|b| (i as u8).wrapping_mul(31).wrapping_add(b as u8)));
        }
        let frame = PyrowaveFrame {
            framing: PyrowaveFraming::Record,
            records,
            width: 1280,
            height: 720,
            chroma: Chroma::Yuv420,
            total_blocks: sizes.len() as u32 - 1,
            blocks_received: sizes.len() as u32 - 1,
            coarse_complete: true,
            records_skipped: 0,
            packets: 0,
            packets_lost: 0,
        };
        (data, frame)
    }

    /// What the page does with INTRA datagrams (`docs/plans/c2-transport.md`
    /// section 3, "PyroWave"): a packet is whole if it starts at a fragment
    /// that isn't CONTINUED and runs through the last with CONTINUES set.
    /// Returns the whole packets, in order.
    fn page_packets(datagrams: &[Vec<u8>]) -> Vec<(Vec<u8>, bool)> {
        let mut out = Vec::new();
        let mut open: Option<(Vec<u8>, bool)> = None;
        for d in datagrams {
            let (h, payload) = DatagramHeader::decode(d).unwrap();
            if h.flags.has(Flags::CONTINUED) {
                open.as_mut()
                    .expect("a CONTINUED fragment follows its unit")
                    .0
                    .extend(payload);
            } else {
                assert!(
                    open.is_none(),
                    "the previous unit ended before this one began"
                );
                open = Some((payload.to_vec(), h.flags.has(Flags::CRITICAL)));
            }
            if !h.flags.has(Flags::CONTINUES) {
                out.push(open.take().unwrap());
            }
        }
        assert!(open.is_none());
        out
    }

    #[test]
    fn synthetic_frame_comes_out_as_intra_datagrams_with_the_streamers_flags() {
        // Sequence header (8), three coarse records, then fine ones: two small, one that
        // is bigger than a datagram (a busy block), two small again.
        let sizes = [8, 100, 120, 90, 200, 300, 2500, 150, 80];
        let (data, info) = synthetic_frame(&sizes, 4);
        let max_datagram = 1200;
        let grams = datagrams(&data, &info, 77, 3, 123_456, max_datagram).unwrap();

        // Every datagram is a video datagram of this frame, KEYFRAME|INTRA, no FEC, within the MTU.
        let count = grams.len() as u16;
        for (i, g) in grams.iter().enumerate() {
            assert!(g.len() <= max_datagram);
            let (h, payload) = DatagramHeader::decode(g).unwrap();
            assert_eq!(h.kind, Kind::Video);
            assert!(h.flags.has(Flags::KEYFRAME) && h.flags.has(Flags::INTRA));
            assert!(!h.flags.has(Flags::PARITY) && !h.flags.has(Flags::RECOVERY));
            assert_eq!(
                (h.fec, h.stream, h.frame_id, h.send_ts_us),
                (0, 3, 77, 123_456)
            );
            assert_eq!((h.frag_index, h.frag_count), (i as u16, count));
            assert!(!payload.is_empty());
        }

        // The page's rule gives back whole packets that concatenate to the frame, in order.
        let whole = page_packets(&grams);
        let joined: Vec<u8> = whole.iter().flat_map(|(p, _)| p.clone()).collect();
        assert_eq!(joined, data);

        // The coarse records (header and three) form one critical packet; the fine ones aren't.
        assert_eq!(whole[0].0.len(), 8 + 100 + 120 + 90);
        assert!(whole[0].1);
        assert!(whole[1..].iter().all(|(_, critical)| !critical));
        // 200 + 300 pack together; the 2500-byte record is a packet of its own, split over
        // three datagrams (1184 bytes of room each) flagged CONTINUES / CONTINUED.
        assert_eq!(whole[1].0.len(), 200 + 300);
        assert_eq!(whole[2].0.len(), 2500);
        let flags_of = |i: usize| DatagramHeader::decode(&grams[i]).unwrap().0.flags;
        let big = grams.len() - 1 - 3; // before the last packet (150 + 80), after the split
        assert!(flags_of(big).has(Flags::CONTINUES) && !flags_of(big).has(Flags::CONTINUED));
        assert!(flags_of(big + 1).has(Flags::CONTINUES) && flags_of(big + 1).has(Flags::CONTINUED));
        assert!(
            !flags_of(big + 2).has(Flags::CONTINUES) && flags_of(big + 2).has(Flags::CONTINUED)
        );
        // The critical flag is on every datagram of a critical packet and on no other.
        for g in &grams {
            let (h, _) = DatagramHeader::decode(g).unwrap();
            let critical = h.flags.has(Flags::CRITICAL);
            assert_eq!(
                critical,
                h.frag_index == 0,
                "only the first datagram is coarse"
            );
        }
    }

    /// From the host's bytes to the page's: a synthetic record frame (header, coarse
    /// blocks, padding, a lost record) through the media client's parser, then framed.
    #[test]
    fn synthetic_host_frame_with_a_lost_record_reaches_the_page_without_it() {
        use cha_gamestream::client::media::pyrowave::{
            RecordInput, StreamShape, block_counts, parse_frame,
        };
        let shape = StreamShape {
            width: 128,
            height: 128,
            chroma: Chroma::Yuv420,
        };
        let (_, coarse) = block_counts(shape).unwrap();
        let header = |index: u32, words: u32| {
            let mut r = (0xBEEF | (words << 16) | (2 << 28)).to_le_bytes().to_vec();
            r.extend((0x21 | (index << 8)).to_le_bytes());
            r.extend((0..(words as usize - 2) * 4).map(|i| (i as u8) | 1));
            r
        };
        let mut seq = ((shape.width - 1) | ((shape.height - 1) << 14) | (2 << 28) | (1 << 31))
            .to_le_bytes()
            .to_vec();
        seq.extend(30u32.to_le_bytes());
        let mut stream = seq;
        let mut records = Vec::new();
        for index in 0..coarse + 6 {
            records.push((stream.len(), header(index, 40)));
            stream.extend(&records.last().unwrap().1);
            if index == 3 {
                // Padding between records.
                stream.extend(0xFFFF_FFFFu32.to_le_bytes());
                stream.extend(2u32.to_le_bytes());
                stream.extend([0u8; 8]);
            }
        }
        // Record 9 loses some bytes in the network.
        let (at, _) = records[9];
        let hole = at + 40..at + 48;
        let mut bytes = stream.clone();
        bytes[hole.clone()].fill(0);
        let (data, info) = parse_frame(
            &RecordInput {
                bytes: &bytes,
                lost: std::slice::from_ref(&hole),
                payload_starts: &[0],
                record_starts: None,
                payload_size: 1376,
                packets: 1,
                packets_lost: 0,
            },
            shape,
        )
        .unwrap();
        assert_eq!(info.records_skipped, 1);

        let grams = datagrams(&data, &info, 5, 0, 0, 1200).unwrap();
        let whole = page_packets(&grams);
        let joined: Vec<u8> = whole.iter().flat_map(|(p, _)| p.clone()).collect();
        let mut want = stream[..8].to_vec();
        for (i, (_, record)) in records.iter().enumerate() {
            if i != 9 {
                want.extend(record);
            }
        }
        assert_eq!(
            joined, want,
            "the page gets every record but the lost one, no padding"
        );
        // The page's own parser would take every packet as it is: header words first.
        assert_eq!(u32::from_le_bytes(joined[..4].try_into().unwrap()) >> 31, 1);
        assert!(whole.iter().any(|(_, critical)| *critical));
        assert!(whole.iter().any(|(_, critical)| !*critical));
    }

    #[test]
    fn synthetic_packets_never_mix_coarse_and_fine_records() {
        let (_, info) = synthetic_frame(&[8, 50, 60, 70, 80], 3);
        let p = packets(&info, 10_000);
        assert_eq!(p.len(), 2);
        assert!(p[0].critical && !p[1].critical);
        assert_eq!(p[0].range, 0..118);
        assert_eq!(p[1].range, 118..268);
        // A tight limit gives one record per packet where two don't fit.
        let p = packets(&info, 100);
        assert_eq!(
            p.iter().map(|p| p.range.len()).collect::<Vec<_>>(),
            [58, 60, 70, 80]
        );
    }

    #[test]
    fn synthetic_frames_that_cannot_be_framed_say_so() {
        let (data, info) = synthetic_frame(&[8, 100], 1);
        assert!(datagrams(&data, &info, 1, 0, 0, HEADER_LEN).is_none());
        assert!(datagrams(&data, &info, 1, 0, 0, 0).is_none());
        // More than 65535 datagrams.
        let (data, info) = synthetic_frame(&[8, 70_000], 1);
        assert!(datagrams(&data, &info, 1, 0, 0, HEADER_LEN + 1).is_none());
        assert!(datagrams(&data, &info, 1, 0, 0, 1200).is_some());
    }
}

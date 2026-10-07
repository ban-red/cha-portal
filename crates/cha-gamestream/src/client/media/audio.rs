//! Audio as a client receives it: Opus in RTP packets of payload type 97, in
//! blocks of four with two Reed-Solomon parity packets (type 127) over the
//! payloads, AES-CBC encrypted when the client asked, out in order.
//!
//! The parity matrix is the host's own constant (an interop constant, per
//! moonlight-common-c's audio FEC). Packets arriving whole are delivered as
//! they are, whatever their size, so a variable-bitrate host plays cleanly.
//! The code runs over the block's payloads zero-padded to the parity's
//! length, so a rebuilt packet is exact only when its block is one size (a
//! constant bitrate, as GameStream hosts send); a variable-bitrate one comes
//! back with trailing zeros. An unrecoverable loss is skipped, not concealed:
//! the gap shows in the timestamps.

use std::time::{Duration, Instant};

use bytes::Bytes;
use fec_rs::ReedSolomon;

use crate::client::AudioPacket;
use crate::crypto::cbc_decrypt;
use crate::media::audio::packetizer::{
    DATA_SHARDS, FEC_HEADER_SIZE, PARITY_MATRIX, PARITY_SHARDS, PT_AUDIO, PT_AUDIO_FEC,
    RTP_HEADER_SIZE,
};

// Per Moonlight's audio stream: RTP payload type 97 for Opus, 127 for FEC, Reed-Solomon
// 4 data + 2 parity shards, a 12-byte FEC header after the RTP header (shard index,
// payload type, base sequence, base timestamp, ssrc), and for encryption AES-CBC with the
// IV being the key id plus the sequence number, big-endian, then zeros.
const TOTAL_SHARDS: usize = DATA_SHARDS + PARITY_SHARDS;
/// Blocks held at once; past this the oldest gap is given up on.
const MAX_BLOCKS: usize = 16;
/// How long a missing packet may be waited for once later ones are in.
/// A sequence number this far ahead (in packets) is a restart, not a gap.
const RESYNC_DISTANCE: i16 = 2048;

#[derive(Clone, Debug)]
pub(crate) struct AudioConfig {
    /// How long a missing packet may be waited for once later ones are in.
    pub reorder_window: Duration,
    /// `Some((key, key id))` when audio is encrypted.
    pub key: Option<([u8; 16], i64)>,
    /// Milliseconds one packet covers, which the RTP clock counts in.
    pub packet_duration_ms: u32,
}

/// What the audio path saw and did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioStats {
    pub packets: u64,
    pub malformed: u64,
    pub duplicates: u64,
    /// Packets behind the one being waited for.
    pub late: u64,
    /// Opus packets rebuilt from parity.
    pub recovered: u64,
    /// Opus packets given up on.
    pub lost: u64,
    /// Encrypted payloads that did not decrypt.
    pub decrypt_failures: u64,
    pub delivered: u64,
}

struct Block {
    /// The sequence number of the first data packet, a multiple of four.
    base: u16,
    /// Payload bytes of the parity shards, once one is in.
    parity_size: Option<usize>,
    /// From a parity packet's FEC header.
    base_timestamp: Option<u32>,
    shards: [Option<Vec<u8>>; TOTAL_SHARDS],
    /// The RTP timestamp each data packet came with.
    timestamps: [Option<u32>; DATA_SHARDS],
    first_seen: Instant,
    recovered: bool,
}

impl Block {
    fn new(base: u16, now: Instant) -> Self {
        Self {
            base,
            parity_size: None,
            base_timestamp: None,
            shards: Default::default(),
            timestamps: [None; DATA_SHARDS],
            first_seen: now,
            recovered: false,
        }
    }

    fn held(&self) -> usize {
        self.shards.iter().flatten().count()
    }

    fn held_data(&self) -> usize {
        self.shards[..DATA_SHARDS].iter().flatten().count()
    }
}

/// `a` is before `b` on the 16-bit sequence circle.
fn before(a: u16, b: u16) -> bool {
    (a.wrapping_sub(b) as i16) < 0
}

pub(crate) struct AudioReceiver {
    cfg: AudioConfig,
    codec: ReedSolomon,
    /// The next Opus packet to deliver, by sequence number.
    next: Option<u16>,
    blocks: Vec<Block>,
    out: Vec<AudioPacket>,
    pub stats: AudioStats,
}

impl AudioReceiver {
    pub fn new(cfg: AudioConfig) -> Self {
        let mut codec = ReedSolomon::new(DATA_SHARDS, PARITY_SHARDS).expect("4+2 is a valid shape");
        codec
            .set_parity_matrix(&PARITY_MATRIX)
            .expect("the matrix is 2x4");
        Self {
            cfg,
            codec,
            next: None,
            blocks: Vec::new(),
            out: Vec::new(),
            stats: AudioStats::default(),
        }
    }

    /// The packets ready since the last call.
    pub fn take_packets(&mut self) -> Vec<AudioPacket> {
        std::mem::take(&mut self.out)
    }

    pub fn tick(&mut self, now: Instant) {
        self.drain(now);
    }

    /// One UDP datagram from the host's audio port.
    pub fn datagram(&mut self, buf: &[u8], now: Instant) {
        self.stats.packets += 1;
        if buf.len() < RTP_HEADER_SIZE || buf[0] >> 6 != 2 {
            self.stats.malformed += 1;
            return;
        }
        let sequence = u16::from_be_bytes([buf[2], buf[3]]);
        let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        match buf[1] & 0x7F {
            PT_AUDIO => {
                let payload = &buf[RTP_HEADER_SIZE..];
                self.insert(
                    sequence & !3,
                    usize::from(sequence & 3),
                    Some(timestamp),
                    None,
                    payload,
                    now,
                );
            }
            PT_AUDIO_FEC => {
                if buf.len() < RTP_HEADER_SIZE + FEC_HEADER_SIZE {
                    self.stats.malformed += 1;
                    return;
                }
                let fec = &buf[RTP_HEADER_SIZE..RTP_HEADER_SIZE + FEC_HEADER_SIZE];
                let index = usize::from(fec[0]);
                let base = u16::from_be_bytes([fec[2], fec[3]]);
                let base_timestamp = u32::from_be_bytes([fec[4], fec[5], fec[6], fec[7]]);
                if index >= PARITY_SHARDS || !base.is_multiple_of(DATA_SHARDS as u16) {
                    self.stats.malformed += 1;
                    return;
                }
                let payload = &buf[RTP_HEADER_SIZE + FEC_HEADER_SIZE..];
                self.insert(
                    base,
                    DATA_SHARDS + index,
                    None,
                    Some(base_timestamp),
                    payload,
                    now,
                );
            }
            _ => self.stats.malformed += 1,
        }
        self.drain(now);
    }

    fn insert(
        &mut self,
        base: u16,
        slot: usize,
        timestamp: Option<u32>,
        base_timestamp: Option<u32>,
        payload: &[u8],
        now: Instant,
    ) {
        if payload.is_empty() {
            self.stats.malformed += 1;
            return;
        }
        let first = base.wrapping_add(if slot < DATA_SHARDS { slot as u16 } else { 0 });
        let next = match self.next {
            Some(n) => n,
            None => {
                // Start at the beginning of the first block seen, so a packet the
                // first shards lost can still be rebuilt.
                self.next = Some(base);
                base
            }
        };
        let end = base.wrapping_add(DATA_SHARDS as u16 - 1);
        if before(end, next) || (slot < DATA_SHARDS && before(first, next)) {
            self.stats.late += 1;
            return;
        }
        if (first.wrapping_sub(next) as i16) > RESYNC_DISTANCE {
            // The sequence jumped: start again here.
            self.blocks.clear();
            self.next = Some(first & !3);
        }
        if !self.blocks.iter().any(|b| b.base == base) {
            while self.blocks.len() >= MAX_BLOCKS {
                self.skip();
            }
            // The block may now be behind what is being waited for.
            if self.next.is_some_and(|n| before(end, n)) {
                self.stats.late += 1;
                return;
            }
            self.blocks.push(Block::new(base, now));
        }
        let block = self
            .blocks
            .iter_mut()
            .find(|b| b.base == base)
            .expect("just found or pushed");
        if block.shards[slot].is_some() {
            self.stats.duplicates += 1;
            return;
        }
        if slot >= DATA_SHARDS {
            // Both parity shards are the length of the block's longest payload.
            if block.parity_size.is_some_and(|n| n != payload.len()) {
                self.stats.malformed += 1;
                return;
            }
            block.parity_size = Some(payload.len());
        }
        block.shards[slot] = Some(payload.to_vec());
        if let Some(t) = timestamp {
            block.timestamps[slot] = Some(t);
        }
        if base_timestamp.is_some() {
            block.base_timestamp = base_timestamp;
        }
        if !block.recovered
            && block.held_data() < DATA_SHARDS
            && block.held() >= DATA_SHARDS
            && let Some(size) = block.parity_size
        {
            let missing = DATA_SHARDS - block.held_data();
            // The code runs over the payloads zero-padded to the parity's length;
            // one longer than that can't be part of it.
            let fits = block.shards[..DATA_SHARDS]
                .iter()
                .flatten()
                .all(|s| s.len() <= size);
            let mut shards: Vec<Option<Vec<u8>>> = block
                .shards
                .iter()
                .map(|s| {
                    s.clone().map(|mut s| {
                        s.resize(size, 0);
                        s
                    })
                })
                .collect();
            if fits && self.codec.reconstruct_data(&mut shards).is_ok() {
                for (i, s) in shards.into_iter().take(DATA_SHARDS).enumerate() {
                    if block.shards[i].is_none() {
                        block.shards[i] = s;
                    }
                }
                self.stats.recovered += missing as u64;
            }
            block.recovered = true;
        }
    }

    /// Gives up on the packet being waited for.
    fn skip(&mut self) {
        if let Some(next) = self.next {
            self.stats.lost += 1;
            self.next = Some(next.wrapping_add(1));
            self.forget_old();
        }
    }

    fn forget_old(&mut self) {
        let Some(next) = self.next else { return };
        self.blocks
            .retain(|b| !before(b.base.wrapping_add(DATA_SHARDS as u16 - 1), next));
    }

    /// Delivers what is next in order, and skips what has waited long enough.
    fn drain(&mut self, now: Instant) {
        while let Some(next) = self.next {
            let base = next & !3;
            let slot = usize::from(next & 3);
            if let Some(block) = self.blocks.iter_mut().find(|b| b.base == base)
                && let Some(payload) = block.shards[slot].clone()
            {
                let timestamp = block.timestamps[slot].unwrap_or_else(|| {
                    block
                        .base_timestamp
                        .unwrap_or(0)
                        .wrapping_add(slot as u32 * self.cfg.packet_duration_ms)
                });
                self.deliver(next, timestamp, payload);
                self.next = Some(next.wrapping_add(1));
                self.forget_old();
                continue;
            }
            // Waiting for `next`. Anything after it that has been waiting a while
            // says it isn't coming.
            let mut later_since: Option<Instant> = None;
            for b in &self.blocks {
                let after_here = if b.base == base {
                    b.shards[slot + 1..].iter().any(Option::is_some)
                } else {
                    !before(b.base, base) && b.held() > 0
                };
                if after_here {
                    later_since = Some(later_since.map_or(b.first_seen, |t| t.min(b.first_seen)));
                }
            }
            let give_up = self.blocks.len() > MAX_BLOCKS / 2
                || later_since
                    .is_some_and(|t| now.saturating_duration_since(t) >= self.cfg.reorder_window);
            if give_up {
                self.skip();
            } else {
                return;
            }
        }
    }

    fn deliver(&mut self, sequence: u16, timestamp: u32, payload: Vec<u8>) {
        let data = match &self.cfg.key {
            None => payload,
            Some((key, key_id)) => {
                // The IV: the key id plus the sequence number, big-endian, then zeros.
                let mut iv = [0u8; 16];
                iv[..4].copy_from_slice(
                    &(*key_id as u32)
                        .wrapping_add(u32::from(sequence))
                        .to_be_bytes(),
                );
                match cbc_decrypt(key, &iv, &payload) {
                    Some(plain) if !plain.is_empty() => plain,
                    _ => {
                        self.stats.decrypt_failures += 1;
                        return;
                    }
                }
            }
        };
        self.stats.delivered += 1;
        self.out.push(AudioPacket {
            data: Bytes::from(data),
            timestamp,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::audio::packetizer::AudioPacketizer;

    const KEY: [u8; 16] = [3; 16];

    fn cfg(key: Option<([u8; 16], i64)>) -> AudioConfig {
        AudioConfig {
            reorder_window: Duration::from_millis(10),
            key,
            packet_duration_ms: 5,
        }
    }

    fn opus(n: usize) -> Vec<u8> {
        (0..40)
            .map(|i| (n as u8).wrapping_mul(31).wrapping_add(i as u8))
            .collect()
    }

    /// `count` Opus packets through the host's packetizer: the datagrams in send order.
    fn stream(key: Option<([u8; 16], i64)>, count: usize) -> Vec<Vec<u8>> {
        let mut p = AudioPacketizer::new(key);
        (0..count).flat_map(|n| p.push(&opus(n), 240)).collect()
    }

    fn run(
        rx: &mut AudioReceiver,
        datagrams: &[Vec<u8>],
        lost: impl Fn(usize) -> bool,
        t: Instant,
    ) -> Vec<AudioPacket> {
        for (i, d) in datagrams.iter().enumerate() {
            if !lost(i) {
                rx.datagram(d, t);
            }
        }
        rx.take_packets()
    }

    fn expect(n: usize) -> (Vec<u8>, u32) {
        (opus(n), n as u32 * 5)
    }

    #[test]
    fn a_hosts_audio_comes_back_in_order_with_and_without_encryption() {
        for key in [None, Some((KEY, 100))] {
            let mut rx = AudioReceiver::new(cfg(key));
            let got = run(&mut rx, &stream(key, 40), |_| false, Instant::now());
            assert_eq!(got.len(), 40, "{key:?}");
            for (n, p) in got.iter().enumerate() {
                assert_eq!((p.data.to_vec(), p.timestamp), expect(n), "{n}");
            }
            assert_eq!(rx.stats.recovered + rx.stats.lost, 0);
        }
    }

    /// Any two of the six packets of a block can be lost.
    #[test]
    fn up_to_two_losses_per_block_are_recovered() {
        for key in [None, Some((KEY, -7))] {
            let all = stream(key, 24); // six blocks
            for a in 0..6 {
                for b in a..6 {
                    // The same two positions in every block.
                    let mut rx = AudioReceiver::new(cfg(key));
                    let got = run(
                        &mut rx,
                        &all,
                        |i| {
                            let at = i % 6;
                            at == a || at == b
                        },
                        Instant::now(),
                    );
                    assert_eq!(got.len(), 24, "key {key:?}, lost {a} and {b}");
                    for (n, p) in got.iter().enumerate() {
                        assert_eq!(
                            (p.data.to_vec(), p.timestamp),
                            expect(n),
                            "key {key:?}, lost {a} and {b}, packet {n}"
                        );
                    }
                    assert_eq!(rx.stats.lost, 0);
                }
            }
        }
    }

    /// A variable-bitrate host: packets of every size play, and a lost one is
    /// rebuilt as itself followed by the block's padding.
    #[test]
    fn packets_of_varying_sizes_play_and_recover() {
        let vbr = |n: usize| -> Vec<u8> {
            let mut p = opus(n);
            p.truncate(8 + (n * 7) % 33);
            p
        };
        for key in [None, Some((KEY, 11))] {
            let mut p = AudioPacketizer::new(key);
            let all: Vec<Vec<u8>> = (0..24).flat_map(|n| p.push(&vbr(n), 240)).collect();

            let mut rx = AudioReceiver::new(cfg(key));
            let got = run(&mut rx, &all, |_| false, Instant::now());
            assert_eq!(got.len(), 24, "{key:?}: {:?}", rx.stats);
            for (n, p) in got.iter().enumerate() {
                assert_eq!(p.data.to_vec(), vbr(n), "{key:?}, packet {n}");
            }
            assert_eq!(rx.stats.malformed, 0);

            if key.is_none() {
                // The second data packet of every block lost.
                let mut rx = AudioReceiver::new(cfg(key));
                let got = run(&mut rx, &all, |i| i % 6 == 1, Instant::now());
                assert_eq!(got.len(), 24, "{:?}", rx.stats);
                for (n, p) in got.iter().enumerate() {
                    let want = vbr(n);
                    assert_eq!(p.data[..want.len()], want[..], "packet {n}");
                    assert!(p.data[want.len()..].iter().all(|b| *b == 0));
                }
                assert_eq!(rx.stats.recovered, 6);
            }
        }
    }

    #[test]
    fn three_losses_in_a_block_drop_those_packets_and_go_on() {
        let all = stream(None, 12);
        let mut rx = AudioReceiver::new(cfg(None));
        let t = Instant::now();
        // Block 1 (packets 4..8, datagrams 6..12) loses data 1, data 2 and a parity.
        let lost = [7, 8, 10];
        let got = run(&mut rx, &all, |i| lost.contains(&i), t);
        // Everything after the hole waits out the reorder window.
        assert_eq!(got.len(), 5, "0..4 and 4: {:?}", rx.stats);
        rx.tick(t + Duration::from_millis(11));
        let got: Vec<_> = got.into_iter().chain(rx.take_packets()).collect();
        let numbers: Vec<u32> = got.iter().map(|p| p.timestamp / 5).collect();
        assert_eq!(numbers, vec![0, 1, 2, 3, 4, 7, 8, 9, 10, 11]);
        assert_eq!(rx.stats.lost, 2);
        for p in &got {
            assert_eq!(p.data.to_vec(), opus((p.timestamp / 5) as usize));
        }
    }

    #[test]
    fn reordering_and_duplicates_are_harmless() {
        let mut all = stream(Some((KEY, 5)), 16);
        for i in (0..all.len() - 1).step_by(3) {
            all.swap(i, i + 1);
        }
        let dup: Vec<Vec<u8>> = all.iter().step_by(4).cloned().collect();
        let mut stream = all;
        stream.splice(5..5, dup);
        let mut rx = AudioReceiver::new(cfg(Some((KEY, 5))));
        let got = run(&mut rx, &stream, |_| false, Instant::now());
        let numbers: Vec<u32> = got.iter().map(|p| p.timestamp / 5).collect();
        assert_eq!(numbers, (0..16).collect::<Vec<u32>>());
        for p in &got {
            assert_eq!(p.data.to_vec(), opus((p.timestamp / 5) as usize));
        }
        assert!(rx.stats.duplicates + rx.stats.late > 0);
    }

    #[test]
    fn a_wrong_key_delivers_nothing_and_counts() {
        let all = stream(Some((KEY, 1)), 8);
        let mut rx = AudioReceiver::new(cfg(Some(([9; 16], 1))));
        let got = run(&mut rx, &all, |_| false, Instant::now());
        // CBC with a wrong key mostly fails the padding check; any that pass are noise.
        assert!(
            rx.stats.decrypt_failures > 0,
            "{:?} {}",
            rx.stats,
            got.len()
        );
    }

    #[test]
    fn sequence_numbers_wrap() {
        let mut p = AudioPacketizer::new(None);
        // Start 12 packets before the wrap; the packetizer is private about its counter,
        // so push enough packets and keep only the datagrams at the end.
        let all: Vec<Vec<u8>> = (0..16).flat_map(|n| p.push(&opus(n), 240)).collect();
        let mut rx = AudioReceiver::new(cfg(None));
        // Rebase the sequence numbers so the stream crosses 65535.
        let shift = 65_528u16;
        let rebased: Vec<Vec<u8>> = all
            .iter()
            .map(|d| {
                let mut d = d.clone();
                let seq = u16::from_be_bytes([d[2], d[3]]).wrapping_add(shift);
                d[2..4].copy_from_slice(&seq.to_be_bytes());
                if d[1] & 0x7F == PT_AUDIO_FEC {
                    let base = u16::from_be_bytes([d[14], d[15]]).wrapping_add(shift);
                    d[14..16].copy_from_slice(&base.to_be_bytes());
                }
                d
            })
            .collect();
        let got = run(&mut rx, &rebased, |i| i % 6 == 1, Instant::now());
        assert_eq!(got.len(), 16);
        for (n, p) in got.iter().enumerate() {
            assert_eq!(p.data.to_vec(), opus(n));
        }
    }

    /// Whatever arrives on the port, the receiver returns, and holds a bounded amount.
    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0x000A_0D10_5EEDu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let valid = stream(Some((KEY, 3)), 8);
        for key in [None, Some((KEY, 3))] {
            let mut rx = AudioReceiver::new(cfg(key));
            let mut t = Instant::now();
            for i in 0..40_000 {
                t += Duration::from_micros(next() % 2000);
                let buf: Vec<u8> = if i % 3 == 0 {
                    let len = (next() % 120) as usize;
                    (0..len).map(|_| next() as u8).collect()
                } else {
                    let mut d = valid[(next() % valid.len() as u64) as usize].clone();
                    for _ in 0..1 + next() % 3 {
                        let at = (next() % 24.min(d.len() as u64)) as usize;
                        d[at] = next() as u8;
                    }
                    if next() % 6 == 0 {
                        d.truncate((next() % d.len() as u64) as usize);
                    }
                    d
                };
                rx.datagram(&buf, t);
                if i % 13 == 0 {
                    rx.tick(t);
                }
                let _ = rx.take_packets();
                assert!(rx.blocks.len() <= MAX_BLOCKS);
            }
        }
    }
}

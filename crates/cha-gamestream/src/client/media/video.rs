//! Video as a client receives it: NV video RTP packets, in Reed-Solomon FEC
//! blocks, optionally AES-GCM encrypted, reassembled into whole access units.
//!
//! This is the half `moonlight-common-rust` got wrong. The host's FEC covers
//! each **whole packet**, from the RTP header (see `media::video::packetizer`),
//! so a missing shard is a full-size buffer to the decoder, and a recovered
//! packet has its RTP and NV headers rebuilt from what the client knows.
//! Doing the maths over payloads only, with missing shards empty, fails with
//! `IncorrectShardSize` against every host that follows Moonlight's layout,
//! ours included. The layout is the host's (`media::video::packetizer`; the
//! field layout agrees with `Video.h` of moonlight-common-c):
//!
//! ```text
//! [encryption prefix: iv(12) frame number(4) tag(16)]   only when encrypted
//! [RTP header 12][reserved 4][NV video packet header 16][payload]
//! ```
//!
//! [`VideoReceiver`] is sans-IO: datagrams and the time go in, frames and
//! requests for the host come out. Its design is its own: it keeps a few
//! frames in flight so reordering across frames is harmless, and a frame it
//! can't complete is dropped, never fatal; it asks for a keyframe (or a
//! reference-frame invalidation) and resumes at the next frame that can start
//! a picture.
//!
//! **PyroWave mode** ([`VideoConfig::pyrowave`]) changes that: every frame
//! stands alone, so the receiver never asks the host for anything. Blocks are
//! recovered as usual, but a frame that is still short when a newer one has
//! arrived (or after the stall) is delivered anyway: its missing data packets
//! are zero-filled and noted, and the record parser
//! ([`super::pyrowave`]) skips the records that lost bytes. Only a frame
//! missing its first packet or a whole FEC block is dropped.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;

use super::pyrowave::{self, RecordInput, RecordStartFlag, StreamShape};
use crate::client::VideoFrame;
use crate::crypto::{GCM_TAG_LEN, gcm_decrypt};
use crate::handoff::VideoCodec;
use crate::media::video::fec::{Codecs, MAX_SHARDS, parity_for};
use crate::media::video::packetizer::{
    ENC_PREFIX_SIZE, FLAG_CONTAINS_PIC_DATA, FLAG_END_OF_FRAME, FLAG_START_OF_FRAME,
    NV_VIDEO_PACKET_SIZE, RTP_HEADER_SIZE,
};

/// A packet's RTP header is 12 bytes, or 16 with the extension bit the host
/// sets; the NV header follows.
// (per Moonlight's RTP packet layout in Video.h: the extension bit 0x10 in the first
// byte adds four bytes to the header)
const RTP_EXTENSION_FLAG: u8 = 0x10;
const RTP_EXTENSION_SIZE: usize = 4;

/// How long a frame may sit incomplete, with a newer frame already
/// arriving, before it is given up on: room for reordering, not for waiting.
/// How long a frame may sit incomplete with nothing newer arriving.
/// The least time between two requests for a keyframe after a loss, so a
/// burst of lost frames asks once.
/// How long to wait for the keyframe asked for before asking again.
/// Frames in flight at once; the oldest is given up on past this.
const MAX_PENDING_FRAMES: usize = 32;
/// A frame number further ahead than this is garbage, not a gap.
const MAX_AHEAD: i64 = 1 << 16;

/// The frame types of the frame header's fourth byte that matter here (2 = IDR,
/// 4 = intra refresh, 5 = P frame after invalidation, per Moonlight's depacketizer).
mod frame_type {
    pub const IDR: u8 = 2;
    pub const INTRA_REFRESH: u8 = 4;
    pub const P_AFTER_INVALIDATION: u8 = 5;
}

/// The receiver's clocks for giving up on what is missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoTiming {
    /// How long a frame may sit incomplete, once a newer frame has completed,
    /// before it is given up on: room for reordering, not for waiting.
    pub reorder_window: Duration,
    /// How long a frame may sit incomplete with nothing completing after it.
    pub stall: Duration,
    /// The least time between two requests for a keyframe after a loss, so a
    /// burst of lost frames asks once.
    pub request_gap: Duration,
    /// How long to wait for the keyframe asked for before asking again.
    pub request_retry: Duration,
}

impl Default for VideoTiming {
    fn default() -> Self {
        Self {
            reorder_window: Duration::from_millis(10),
            stall: Duration::from_millis(100),
            request_gap: Duration::from_millis(100),
            request_retry: Duration::from_secs(1),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VideoConfig {
    pub timing: VideoTiming,
    /// The packet size the stream was set up with: NV header and payload.
    pub packet_size: usize,
    /// Present when video is encrypted.
    pub key: Option<[u8; 16]>,
    pub codec: VideoCodec,
    /// Whether to ask the host to invalidate references after a loss instead
    /// of asking for a keyframe.
    pub invalidate_refs: bool,
    /// Set for a PyroWave stream.
    pub pyrowave: Option<PyrowaveConfig>,
}

/// What the receiver needs to read PyroWave frames.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyrowaveConfig {
    /// The negotiated picture: a sequence header that differs is refused.
    pub shape: StreamShape,
    /// Where packets say they start a record, if known (see
    /// [`pyrowave::RECORD_START_FLAG`]).
    pub record_start: Option<RecordStartFlag>,
}

/// What the receiver hands on.
#[derive(Debug)]
pub(crate) enum VideoEvent {
    /// The next frame, in order.
    Frame(VideoFrame),
    /// Ask the host for a keyframe.
    RequestIdr,
    /// Ask the host to stop referencing frames `first..=last` (wire numbers).
    Invalidate { first: u32, last: u32 },
}

/// What the video path saw and did, for the caller's overlay and the tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VideoStats {
    /// Datagrams received.
    pub packets: u64,
    /// Datagrams that were not a valid video packet.
    pub malformed: u64,
    /// Encrypted datagrams that failed authentication.
    pub decrypt_failures: u64,
    /// Packets for a shard already held, or for a block already finished.
    pub duplicates: u64,
    /// Packets for a frame already delivered or given up on.
    pub late: u64,
    /// FEC blocks that needed recovery and got it.
    pub blocks_recovered: u64,
    /// Frames completed with at least one block rebuilt from parity (a frame
    /// of several blocks counts once).
    pub frames_recovered: u64,
    /// Frames delivered.
    pub frames_delivered: u64,
    /// Frames given up on: not completed, or recovered into nonsense.
    pub frames_lost: u64,
    /// Frames that completed but could not start a picture (after a loss,
    /// before the next keyframe).
    pub frames_discarded: u64,
    /// Keyframes asked for after the first.
    pub idr_requests: u64,
    /// Reference-frame invalidations asked for.
    pub invalidations: u64,
    /// PyroWave frames delivered with data packets missing.
    pub pyrowave_partial_frames: u64,
    /// Data packets zero-filled in PyroWave frames.
    pub pyrowave_packets_zero_filled: u64,
    /// Block records skipped for lost bytes.
    pub pyrowave_records_skipped: u64,
    /// PyroWave frames refused by the record rules (counted in `frames_lost` too).
    pub pyrowave_frames_rejected: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockState {
    Open,
    Done,
    Failed,
}

/// One FEC block: `data` shards then `parity` shards, each a whole packet.
struct Block {
    data: usize,
    percent: usize,
    /// The RTP sequence number of the block's first shard.
    base_seq: u16,
    shards: Vec<Option<Vec<u8>>>,
    got_data: usize,
    got_parity: usize,
    state: BlockState,
    /// PyroWave: recovery was tried and gave nonsense; the shards it filled
    /// were taken back, and it is not tried again.
    recovery_failed: bool,
    /// Parity rebuilt data shards of this block.
    recovered: bool,
}

impl Block {
    fn new(data: usize, percent: usize, base_seq: u16) -> Self {
        let total = data + parity_for(data, percent);
        Self {
            data,
            percent,
            base_seq,
            shards: vec![None; total],
            got_data: 0,
            got_parity: 0,
            state: BlockState::Open,
            recovery_failed: false,
            recovered: false,
        }
    }
}

/// One frame's blocks so far.
struct Pending {
    last_block: u8,
    blocks: Vec<Option<Block>>,
    first_seen: Instant,
    last_activity: Instant,
}

impl Pending {
    fn complete(&self) -> bool {
        self.blocks
            .iter()
            .all(|b| b.as_ref().is_some_and(|b| b.state == BlockState::Done))
    }

    fn failed(&self) -> bool {
        self.blocks
            .iter()
            .flatten()
            .any(|b| b.state == BlockState::Failed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    /// Frames flow.
    Nothing,
    /// A keyframe is needed.
    Key,
    /// A frame that doesn't reference what was lost is needed: a keyframe, an
    /// intra refresh or a frame predicted past the invalidation.
    Refs,
}

/// The receiving half of one video stream.
pub(crate) struct VideoReceiver {
    cfg: VideoConfig,
    /// Bytes of a whole shard, as sent: RTP header, reserved, NV header, payload.
    shard_len: usize,
    codecs: Codecs,
    /// Frames in flight, by extended frame number (the wire's `u32` unwrapped
    /// around `next`).
    frames: BTreeMap<i64, Pending>,
    /// The frame to deliver next.
    next: Option<i64>,
    last_good: Option<i64>,
    need: Need,
    last_request: Instant,
    events: Vec<VideoEvent>,
    pub stats: VideoStats,
}

impl VideoReceiver {
    /// `now` stands for the request the control stream's start already made.
    pub fn new(cfg: VideoConfig, now: Instant) -> Self {
        let shard_len = RTP_HEADER_SIZE + 4 + cfg.packet_size;
        // PyroWave frames stand alone: nothing is ever asked for.
        let need = if cfg.pyrowave.is_some() {
            Need::Nothing
        } else {
            Need::Key
        };
        Self {
            cfg,
            shard_len,
            codecs: Codecs::default(),
            frames: BTreeMap::new(),
            next: None,
            last_good: None,
            need,
            last_request: now,
            events: Vec::new(),
            stats: VideoStats::default(),
        }
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<VideoEvent> {
        std::mem::take(&mut self.events)
    }

    /// One UDP datagram from the host's video port.
    pub fn datagram(&mut self, datagram: &[u8], now: Instant) {
        self.stats.packets += 1;
        let plain = match &self.cfg.key {
            None => datagram.to_vec(),
            Some(key) => {
                if datagram.len() <= ENC_PREFIX_SIZE {
                    self.stats.malformed += 1;
                    return;
                }
                let (prefix, body) = datagram.split_at(ENC_PREFIX_SIZE);
                let iv: [u8; 12] = prefix[..12].try_into().expect("12 bytes");
                let tag: [u8; GCM_TAG_LEN] = prefix[16..32].try_into().expect("16 bytes");
                // Not trusted yet: a frame already behind us is dropped before
                // it costs a decryption, and nothing else is learned from it.
                let number = u32::from_le_bytes(prefix[12..16].try_into().expect("4 bytes"));
                if let Some(next) = self.next
                    && number != 0
                    && (number.wrapping_sub(next as u32) as i32) < 0
                {
                    self.stats.late += 1;
                    return;
                }
                let mut plain = body.to_vec();
                if gcm_decrypt(key, &iv, &mut plain, &tag).is_err() {
                    self.stats.decrypt_failures += 1;
                    return;
                }
                plain
            }
        };
        self.packet(plain, now);
        self.advance(now);
    }

    /// Handling datagrams took `spent`: that is not the network being quiet, so
    /// the frames in flight are not held to it (recovering a big block is slow
    /// in an unoptimised build).
    pub fn processing_took(&mut self, spent: Duration) {
        for f in self.frames.values_mut() {
            f.last_activity += spent;
            f.first_seen += spent;
        }
    }

    /// Time passing: gives up on frames that won't complete and asks again
    /// for a keyframe that hasn't come.
    pub fn tick(&mut self, now: Instant) {
        self.advance(now);
        if self.need != Need::Nothing
            && now.duration_since(self.last_request) >= self.cfg.timing.request_retry
        {
            self.request_idr(now);
        }
    }

    /// Takes a decrypted packet into its block.
    fn packet(&mut self, mut plain: Vec<u8>, now: Instant) {
        let header = if plain.first().is_some_and(|b| b & RTP_EXTENSION_FLAG != 0) {
            RTP_HEADER_SIZE + RTP_EXTENSION_SIZE
        } else {
            RTP_HEADER_SIZE
        };
        if plain.len() > self.shard_len || plain.len() < header + NV_VIDEO_PACKET_SIZE + 1 {
            self.stats.malformed += 1;
            return;
        }
        let sequence = u16::from_be_bytes([plain[2], plain[3]]);
        let nv = &plain[header..header + NV_VIDEO_PACKET_SIZE];
        let frame = u32::from_le_bytes(nv[4..8].try_into().expect("4 bytes"));
        // The FEC fields: per Moonlight's NV_VIDEO_PACKET, fecInfo holds the shard
        // index (bits 12..22), the percentage (4..12) and the data count (22..32);
        // multiFecBlocks holds the block number (bits 4..6) and the last block (6..8).
        let multi = nv[11];
        let info = u32::from_le_bytes(nv[12..16].try_into().expect("4 bytes"));
        let data = (info >> 22) as usize & 0x3FF;
        let index = (info >> 12) as usize & 0x3FF;
        let percent = (info >> 4) as usize & 0xFF;
        let block_no = usize::from((multi >> 4) & 3);
        let last_block = usize::from((multi >> 6) & 3);
        let total = data + parity_for(data, percent);
        if data == 0 || total > MAX_SHARDS || index >= total || block_no > last_block {
            self.stats.malformed += 1;
            return;
        }

        let ext = match self.next {
            None => {
                self.next = Some(i64::from(frame));
                i64::from(frame)
            }
            Some(next) => {
                let delta = i64::from(frame.wrapping_sub(next as u32) as i32);
                if delta < 0 {
                    self.stats.late += 1;
                    return;
                }
                if delta > MAX_AHEAD {
                    self.stats.malformed += 1;
                    return;
                }
                next + delta
            }
        };

        if !self.frames.contains_key(&ext) {
            // Room for one more frame: the oldest in flight is given up on.
            while self.frames.len() >= MAX_PENDING_FRAMES {
                self.lose_next(now);
            }
            if self.next.is_some_and(|next| ext < next) {
                self.stats.late += 1;
                return;
            }
        }
        let pending = self.frames.entry(ext).or_insert_with(|| Pending {
            last_block: last_block as u8,
            blocks: (0..=last_block).map(|_| None).collect(),
            first_seen: now,
            last_activity: now,
        });
        if usize::from(pending.last_block) != last_block {
            self.stats.malformed += 1;
            return;
        }
        pending.last_activity = now;
        let base_seq = sequence.wrapping_sub(index as u16);
        let block =
            pending.blocks[block_no].get_or_insert_with(|| Block::new(data, percent, base_seq));
        if block.data != data || block.percent != percent || block.base_seq != base_seq {
            self.stats.malformed += 1;
            return;
        }
        if block.state != BlockState::Open || block.shards[index].is_some() {
            self.stats.duplicates += 1;
            return;
        }
        // A short packet is padded to a whole shard: that is what the code ran over.
        plain.resize(self.shard_len, 0);
        block.shards[index] = Some(plain);
        if index < data {
            block.got_data += 1;
        } else {
            block.got_parity += 1;
        }
        if block.got_data == block.data {
            block.state = BlockState::Done;
        } else if block.got_data + block.got_parity >= block.data
            && !block.recovery_failed
            && recover(
                block,
                frame,
                block_no as u8,
                last_block as u8,
                &mut self.codecs,
                self.cfg.pyrowave.is_some(),
            )
        {
            block.recovered = true;
            self.stats.blocks_recovered += 1;
        }
    }

    /// Delivers what is complete in order, and gives up on what isn't going to be.
    fn advance(&mut self, now: Instant) {
        loop {
            let Some(next) = self.next else { return };
            match self.frames.get(&next) {
                Some(f) if f.complete() => {
                    let f = self.frames.remove(&next).expect("just found");
                    if f.blocks.iter().flatten().any(|b| b.recovered) {
                        self.stats.frames_recovered += 1;
                    }
                    self.deliver(next, &f, now);
                    self.next = Some(next + 1);
                }
                Some(f) if f.failed() => self.lose_next(now),
                Some(f) => {
                    let pyrowave = self.cfg.pyrowave.is_some();
                    // Only a newer frame that has completed says this one isn't coming
                    // (a client that is merely slow has none): else wait out the stall.
                    // PyroWave settles for any packet of a newer frame: the host sends
                    // one frame at a time, so what is missing now is not coming, and
                    // what is there is worth more than waiting.
                    let newer = if pyrowave {
                        self.frames.range(next + 1..).next().is_some()
                    } else {
                        self.frames.range(next + 1..).any(|(_, n)| n.complete())
                    };
                    let idle = now.saturating_duration_since(f.last_activity);
                    let t = self.cfg.timing;
                    if (newer && idle >= t.reorder_window) || idle >= t.stall {
                        if pyrowave {
                            // Deliver what there is, zero-filled where it isn't.
                            let f = self.frames.remove(&next).expect("just found");
                            self.deliver(next, &f, now);
                            self.next = Some(next + 1);
                        } else {
                            self.lose_next(now);
                        }
                    } else {
                        return;
                    }
                }
                None => match self.frames.range(next..).next() {
                    // Nothing of this frame ever came, and a later one has been
                    // arriving for a while: the network dropped it whole.
                    Some((&later, f))
                        if (f.complete()
                            && now.saturating_duration_since(f.first_seen)
                                >= self.cfg.timing.reorder_window)
                            || now.saturating_duration_since(f.first_seen)
                                >= self.cfg.timing.stall =>
                    {
                        self.lose(next, later - 1, now);
                        self.next = Some(later);
                    }
                    _ => return,
                },
            }
        }
    }

    /// Gives up on the frame `next` and moves on.
    fn lose_next(&mut self, now: Instant) {
        let Some(next) = self.next else { return };
        self.frames.remove(&next);
        self.lose(next, next, now);
        self.next = Some(next + 1);
    }

    /// Frames `first..=last` are gone. Asks the host to repair the picture,
    /// once for a burst.
    fn lose(&mut self, first: i64, last: i64, now: Instant) {
        self.stats.frames_lost += (last - first + 1) as u64;
        if self.cfg.pyrowave.is_some() {
            // Every frame stands alone: no keyframe, no invalidation.
            return;
        }
        if self.need == Need::Nothing {
            self.last_request = now;
            if self.cfg.invalidate_refs {
                self.need = Need::Refs;
                self.stats.invalidations += 1;
                let from = self.last_good.map_or(first, |g| g + 1).min(first);
                self.events.push(VideoEvent::Invalidate {
                    first: from as u32,
                    last: last as u32,
                });
            } else {
                self.need = Need::Key;
                self.events.push(VideoEvent::RequestIdr);
                self.stats.idr_requests += 1;
            }
        } else if now.duration_since(self.last_request) >= self.cfg.timing.request_gap {
            self.request_idr(now);
        }
    }

    fn request_idr(&mut self, now: Instant) {
        if self.cfg.pyrowave.is_some() {
            return;
        }
        self.need = Need::Key;
        self.last_request = now;
        self.stats.idr_requests += 1;
        self.events.push(VideoEvent::RequestIdr);
    }

    /// The consumer had no room for delivered frame `number`: the picture it
    /// has is broken from there.
    pub fn dropped_downstream(&mut self, number: u32, now: Instant) {
        self.lose(i64::from(number), i64::from(number), now);
    }

    /// A completed frame: hand it on if it can start or continue a picture.
    fn deliver(&mut self, number: i64, pending: &Pending, now: Instant) {
        if let Some(config) = self.cfg.pyrowave {
            self.deliver_pyrowave(number, pending, now, config);
            return;
        }
        let Some((data, kind)) = assemble(pending, self.cfg.codec) else {
            // Recovered or sent as nonsense: as good as lost.
            self.lose(number, number, now);
            return;
        };
        let resumes = match self.need {
            Need::Nothing => true,
            Need::Key => kind == frame_type::IDR,
            Need::Refs => matches!(
                kind,
                frame_type::IDR | frame_type::INTRA_REFRESH | frame_type::P_AFTER_INVALIDATION
            ),
        };
        if !resumes {
            self.stats.frames_discarded += 1;
            // Frames are arriving again but not the one asked for: ask again.
            if now.duration_since(self.last_request) >= self.cfg.timing.request_gap {
                self.request_idr(now);
            }
            return;
        }
        self.need = Need::Nothing;
        self.last_good = Some(number);
        self.stats.frames_delivered += 1;
        self.events.push(VideoEvent::Frame(VideoFrame {
            data,
            key: kind == frame_type::IDR,
            number: number as u32,
            received: now,
            pyrowave: None,
        }));
    }

    /// A PyroWave frame, whole or not: zero-filled where packets are missing,
    /// parsed into its records, delivered unless the rules refuse it.
    fn deliver_pyrowave(
        &mut self,
        number: i64,
        pending: &Pending,
        now: Instant,
        config: PyrowaveConfig,
    ) {
        let Some(frame) = assemble_pyrowave(pending, config.record_start) else {
            // The first packet or a whole FEC block never came.
            self.lose(number, number, now);
            return;
        };
        let input = RecordInput {
            bytes: &frame.bytes,
            lost: &frame.lost,
            payload_starts: &frame.payload_starts,
            record_starts: frame.record_starts.as_deref(),
            payload_size: frame.payload_size,
            packets: frame.packets,
            packets_lost: frame.packets_lost,
        };
        match pyrowave::parse_frame(&input, config.shape) {
            Ok((data, info)) => {
                if info.packets_lost > 0 {
                    self.stats.pyrowave_partial_frames += 1;
                    self.stats.pyrowave_packets_zero_filled += u64::from(info.packets_lost);
                }
                self.stats.pyrowave_records_skipped += u64::from(info.records_skipped);
                self.last_good = Some(number);
                self.stats.frames_delivered += 1;
                self.events.push(VideoEvent::Frame(VideoFrame {
                    data: Bytes::from(data),
                    key: true,
                    number: number as u32,
                    received: now,
                    pyrowave: Some(info),
                }));
            }
            Err(reason) => {
                tracing::debug!(frame = number, %reason, "a PyroWave frame was refused");
                self.stats.pyrowave_frames_rejected += 1;
                self.lose(number, number, now);
            }
        }
    }
}

/// Recovers the data shards of `block` from the rest, over whole packets, and
/// rebuilds what the host's headers said. False when it can't.
fn recover(
    block: &mut Block,
    frame: u32,
    block_no: u8,
    last_block: u8,
    codecs: &mut Codecs,
    pyrowave: bool,
) -> bool {
    let parity = block.shards.len() - block.data;
    let missing: Vec<usize> = (0..block.data)
        .filter(|&i| block.shards[i].is_none())
        .collect();
    let reference: [u8; 12] = block
        .shards
        .iter()
        .flatten()
        .next()
        .expect("at least `data` shards are held")[..12]
        .try_into()
        .expect("12 bytes");
    let ok = codecs
        .get(block.data, parity)
        .and_then(|codec| codec.reconstruct_data(&mut block.shards))
        .is_ok();
    if !ok {
        return recovery_failed(block, &missing, pyrowave);
    }
    for &i in &missing {
        let Some(shard) = block.shards[i].as_mut() else {
            return recovery_failed(block, &missing, pyrowave);
        };
        // What the code returns is the packet the host protected. The fields the
        // client knows are written from that, and the start and end flags are
        // checked against the shard's position (the host sets them before FEC),
        // so a nonsense recovery is refused rather than decoded.
        shard[0] = reference[0];
        shard[1] = reference[1];
        shard[2..4].copy_from_slice(&block.base_seq.wrapping_add(i as u16).to_be_bytes());
        shard[4..12].copy_from_slice(&reference[4..12]);
        let header = if shard[0] & RTP_EXTENSION_FLAG != 0 {
            RTP_HEADER_SIZE + RTP_EXTENSION_SIZE
        } else {
            RTP_HEADER_SIZE
        };
        shard[header + 4..header + 8].copy_from_slice(&frame.to_le_bytes());
        shard[header + 11] = (last_block << 6) | (block_no << 4);
        // Our own check, derived from the host's packetizer: it writes every data
        // shard's flags before the FEC runs, from its position alone (picture
        // data on all, start on the first, end on the last), and its stream
        // packet index as the RTP sequence number shifted up a byte. A
        // recovery that doesn't reproduce exactly that is not a recovery.
        let expected_flags = FLAG_CONTAINS_PIC_DATA
            | if i == 0 { FLAG_START_OF_FRAME } else { 0 }
            | if i == block.data - 1 {
                FLAG_END_OF_FRAME
            } else {
                0
            };
        let stream_index = u32::from_le_bytes(shard[header..header + 4].try_into().expect("4"));
        let sequence = block.base_seq.wrapping_add(i as u16);
        // PyroWave's packets carry a record-start flag among the flags, whose
        // place isn't known yet (see `pyrowave::RECORD_START_FLAG`): only the
        // stream index is checked for them.
        let sane = (pyrowave || shard[header + 8] == expected_flags)
            && stream_index & 0xFF == 0
            && (stream_index >> 8) as u16 == sequence;
        if !sane {
            return recovery_failed(block, &missing, pyrowave);
        }
    }
    block.state = BlockState::Done;
    true
}

/// A recovery that didn't work. Ordinary frames are given up on; a PyroWave
/// block takes back the shards recovery filled in (they are lost packets, to
/// be zero-filled) and stays open.
fn recovery_failed(block: &mut Block, filled: &[usize], pyrowave: bool) -> bool {
    if pyrowave {
        for &i in filled {
            block.shards[i] = None;
        }
        block.recovery_failed = true;
    } else {
        block.state = BlockState::Failed;
    }
    false
}

/// The payload of a shard: after the RTP and NV headers.
fn payload(shard: &[u8]) -> &[u8] {
    let header = if shard[0] & RTP_EXTENSION_FLAG != 0 {
        RTP_HEADER_SIZE + RTP_EXTENSION_SIZE
    } else {
        RTP_HEADER_SIZE
    };
    &shard[header + NV_VIDEO_PACKET_SIZE..]
}

/// A PyroWave frame laid out for the record parser.
struct PyrowaveBytes {
    bytes: Vec<u8>,
    lost: Vec<std::ops::Range<usize>>,
    payload_starts: Vec<usize>,
    record_starts: Option<Vec<usize>>,
    payload_size: usize,
    packets: u32,
    packets_lost: u32,
}

/// The frame's payload bytes in order, the frame header off, the last packet
/// cut to its declared length and lost data packets as zeros. `None` when the
/// frame can't be read at all: a block that never came (or holds no data
/// packet we have or could rebuild), or no first packet.
fn assemble_pyrowave(frame: &Pending, flag: Option<RecordStartFlag>) -> Option<PyrowaveBytes> {
    // Every block must have shown at least one data packet.
    let mut blocks = Vec::with_capacity(frame.blocks.len());
    for block in &frame.blocks {
        let block = block.as_ref()?;
        if block.state != BlockState::Done && block.got_data == 0 {
            return None;
        }
        blocks.push(block);
    }
    let first = blocks.first()?.shards.first()?.as_deref()?;
    let first_payload = payload(first);
    let payload_size = first_payload.len();
    // The frame header: 8 bytes, or 44 (`0x81`), as for any frame.
    let header_len = match first_payload.first()? {
        0x01 if payload_size >= 8 => 8,
        0x81 if payload_size >= 44 => 44,
        _ => return None,
    };
    let declared = usize::from(u16::from_le_bytes([first_payload[4], first_payload[5]]));

    let packets: Vec<Option<&[u8]>> = blocks
        .iter()
        .flat_map(|b| b.shards[..b.data].iter().map(|s| s.as_deref()))
        .collect();
    let count = packets.len();
    let mut bytes = Vec::with_capacity(count * payload_size);
    let mut lost: Vec<std::ops::Range<usize>> = Vec::new();
    let mut payload_starts = Vec::with_capacity(count);
    let mut record_starts = flag.map(|_| Vec::new());
    let mut packets_lost = 0u32;
    for (i, shard) in packets.iter().enumerate() {
        let start = bytes.len();
        payload_starts.push(start);
        // The first packet's frame header is not part of the stream.
        let skip = if i == 0 { header_len } else { 0 };
        match shard {
            Some(shard) => {
                let p = payload(shard);
                let p = &p[..p.len().min(payload_size)];
                bytes.extend_from_slice(&p[skip.min(p.len())..]);
                if let (Some(flag), Some(starts)) = (flag, record_starts.as_mut()) {
                    let header = if shard[0] & RTP_EXTENSION_FLAG != 0 {
                        RTP_HEADER_SIZE + RTP_EXTENSION_SIZE
                    } else {
                        RTP_HEADER_SIZE
                    };
                    if shard
                        .get(header + flag.nv_header_byte)
                        .is_some_and(|b| b & flag.mask != 0)
                    {
                        starts.push(start);
                    }
                }
            }
            None => {
                packets_lost += 1;
                bytes.resize(start + payload_size - skip, 0);
                match lost.last_mut() {
                    Some(last) if last.end == start => last.end = bytes.len(),
                    _ => lost.push(start..bytes.len()),
                }
            }
        }
    }
    // The last packet's declared length, when it is a plausible one.
    let valid = declared >= 1 && declared <= payload_size && (count > 1 || declared > header_len);
    if valid {
        let total = (count - 1) * payload_size + declared - header_len;
        if total < bytes.len() {
            bytes.truncate(total);
            for r in &mut lost {
                r.end = r.end.min(total);
            }
            lost.retain(|r| r.start < r.end);
        }
    }
    Some(PyrowaveBytes {
        bytes,
        lost,
        payload_starts,
        record_starts,
        payload_size,
        packets: count as u32,
        packets_lost,
    })
}

/// A complete frame's access unit and its type: the payloads in order, the
/// last cut to its length, the frame header (the first eight or 44 bytes) off.
/// `None` for a frame that is not one.
fn assemble(frame: &Pending, codec: VideoCodec) -> Option<(Bytes, u8)> {
    let payloads: Vec<&[u8]> = frame
        .blocks
        .iter()
        .flatten()
        .flat_map(|b| b.shards[..b.data].iter().flatten().map(|s| payload(s)))
        .collect();
    let first = payloads.first()?;
    // Frame header per Moonlight's video depacketizer: first byte 0x01 means 8 bytes,
    // 0x81 means 44; byte 3 is the frame type, bytes 4..6 the last packet's payload length.
    let header_len = match first.first()? {
        0x01 if first.len() >= 8 => 8,
        0x81 if first.len() >= 44 => 44,
        _ => return None,
    };
    let kind = first[3];
    let last_index = payloads.len() - 1;
    let last_len = payloads[last_index].len();
    // The length of the last packet's payload, as the host wrote it. Without
    // it, H.264 and HEVC carry on (decoders skip trailing zeros); AV1's OBUs
    // can't take padding.
    let declared = usize::from(u16::from_le_bytes([first[4], first[5]]));
    let valid = declared >= 1 && declared <= last_len && (last_index > 0 || declared > header_len);
    if !valid && codec == VideoCodec::Av1 {
        return None;
    }
    let mut out = Vec::with_capacity(payloads.iter().map(|p| p.len()).sum());
    for (i, mut p) in payloads.into_iter().enumerate() {
        if i == last_index && valid {
            p = &p[..declared];
        }
        if i == 0 {
            p = &p[header_len..];
        }
        out.extend_from_slice(p);
    }
    (!out.is_empty()).then(|| (Bytes::from(out), kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::video::packetizer::{Packetizer, PacketizerConfig, ShardBatch};

    const KEY: [u8; 16] = *b"0123456789abcdef";
    const PACKET_SIZE: usize = 1392;

    fn cfg(key: Option<[u8; 16]>, rfi: bool) -> VideoConfig {
        VideoConfig {
            timing: VideoTiming::default(),
            packet_size: PACKET_SIZE,
            key,
            codec: VideoCodec::H264,
            invalidate_refs: rfi,
            pyrowave: None,
        }
    }

    fn packetizer(fec: u8, min: u32, key: Option<[u8; 16]>) -> Packetizer {
        Packetizer::new(PacketizerConfig {
            packet_size: PACKET_SIZE,
            fec_percent: fec,
            min_fec_packets: min,
            key,
        })
    }

    fn frame_bytes(len: usize, seed: u8) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed) | 1)
            .collect()
    }

    fn shards(batch: &ShardBatch) -> Vec<Vec<u8>> {
        batch.shards().map(<[u8]>::to_vec).collect()
    }

    /// The shape of each block of a batch, from the headers (unencrypted batches only).
    fn blocks(batch: &ShardBatch) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        let mut last = None;
        for s in batch.shards() {
            let nv = &s[16..32];
            let info = u32::from_le_bytes(nv[12..16].try_into().unwrap());
            let (data, percent) = ((info >> 22) as usize & 0x3FF, (info >> 4) as usize & 0xFF);
            let block = (nv[11] >> 4) & 3;
            if last != Some(block) {
                out.push((data, parity_for(data, percent)));
                last = Some(block);
            }
        }
        out
    }

    fn feed(
        rx: &mut VideoReceiver,
        shards: &[Vec<u8>],
        lost: &[usize],
        now: Instant,
    ) -> Vec<VideoEvent> {
        for (i, s) in shards.iter().enumerate() {
            if !lost.contains(&i) {
                rx.datagram(s, now);
            }
        }
        rx.take_events()
    }

    fn frames(events: Vec<VideoEvent>) -> Vec<VideoFrame> {
        events
            .into_iter()
            .filter_map(|e| match e {
                VideoEvent::Frame(f) => Some(f),
                _ => None,
            })
            .collect()
    }

    /// Absolute shard positions of each block: `(first, data, parity)`.
    fn layout(batch: &ShardBatch) -> Vec<(usize, usize, usize)> {
        let mut at = 0;
        blocks(batch)
            .into_iter()
            .map(|(d, p)| {
                let r = (at, d, p);
                at += d + p;
                r
            })
            .collect()
    }

    /// Golden: a frame the host packetized, parsed by the client, byte for byte,
    /// encrypted and not, for every size across the block boundaries.
    #[test]
    fn a_hosts_frames_come_back_exact() {
        for key in [None, Some(KEY)] {
            let mut p = packetizer(20, 0, key);
            let mut rx = VideoReceiver::new(cfg(key, false), Instant::now());
            let now = Instant::now();
            for (n, len) in [1, 200, 1000, 1008, 1009, 100_000, 400_000, 1_000_000]
                .into_iter()
                .enumerate()
            {
                let au = frame_bytes(len, n as u8);
                let batch = p
                    .packetize(&au, n == 0, n as u32 + 1, 0, 0)
                    .expect("packetizes");
                let got = frames(feed(&mut rx, &shards(&batch), &[], now));
                assert_eq!(got.len(), 1, "{len} bytes, key {key:?}");
                assert_eq!(got[0].data, au, "{len} bytes, key {key:?}");
                assert_eq!(got[0].number, n as u32 + 1);
                assert_eq!(got[0].key, n == 0);
            }
            assert_eq!(rx.stats.frames_lost, 0);
            assert_eq!(rx.stats.blocks_recovered, 0);
        }
    }

    /// The bug this module exists for: whole-packet recovery, for every shard
    /// position of a block, up to the parity it has, in single and multi-block frames.
    #[test]
    fn losses_up_to_the_parity_are_recovered_byte_exact() {
        for key in [None, Some(KEY)] {
            for len in [200usize, 100_000, 400_000, 1_000_000] {
                let au = frame_bytes(len, 9);
                let mut p = packetizer(20, 2, None);
                let plain = p.packetize(&au, true, 1, 0, 0).unwrap();
                // The block layout, from the plain batch; the encrypted one has the same shape.
                let mut layout = layout(&plain);
                let multi = layout.len() > 1;
                if len >= 400_000 {
                    // Debug builds are slow at 1 MB: the first and last blocks stand for the rest.
                    let last = layout.len() - 1;
                    layout.swap(1, last);
                    layout.truncate(2);
                }
                let mut p = packetizer(20, 2, key);
                let batch = p.packetize(&au, true, 1, 0, 0).unwrap();
                let all = shards(&batch);
                assert_eq!(all.len(), plain.shard_count());

                // (name, positions to lose)
                let mut cases: Vec<(String, Vec<usize>)> = Vec::new();
                for (b, &(first, data, parity)) in layout.iter().enumerate() {
                    cases.push((format!("block {b}: the first data shard"), vec![first]));
                    cases.push((
                        format!("block {b}: the last data shard"),
                        vec![first + data - 1],
                    ));
                    cases.push((
                        format!("block {b}: a middle data shard"),
                        vec![first + data / 2],
                    ));
                    cases.push((
                        format!("block {b}: the first parity shard"),
                        vec![first + data],
                    ));
                    cases.push((
                        format!("block {b}: the last parity shard"),
                        vec![first + data + parity - 1],
                    ));
                    // As many as the parity: data shards from the front, from the back,
                    // and a mix with parity lost too.
                    cases.push((
                        format!("block {b}: {parity} data shards from the front"),
                        (first..first + parity.min(data)).collect(),
                    ));
                    cases.push((
                        format!("block {b}: {parity} data shards ending at the last"),
                        (first + data - parity.min(data)..first + data).collect(),
                    ));
                    if parity >= 2 {
                        cases.push((
                            format!("block {b}: last data shard and a parity shard"),
                            vec![first + data - 1, first + data],
                        ));
                    }
                    cases.push((
                        format!("block {b}: all the parity"),
                        (first + data..first + data + parity).collect(),
                    ));
                }
                if multi {
                    // One loss in every block of the frame at once.
                    cases.push((
                        "every block loses its last data shard".into(),
                        layout.iter().map(|&(f, d, _)| f + d - 1).collect(),
                    ));
                    cases.push((
                        "every block loses its full parity budget in data".into(),
                        layout.iter().flat_map(|&(f, _, par)| f..f + par).collect(),
                    ));
                }
                for (name, lost) in cases {
                    let mut rx = VideoReceiver::new(cfg(key, false), Instant::now());
                    let got = frames(feed(&mut rx, &all, &lost, Instant::now()));
                    assert_eq!(got.len(), 1, "{len} B, key {key:?}, {name}: {lost:?}");
                    assert_eq!(got[0].data, au, "{len} B, key {key:?}, {name}");
                    let data_lost = lost
                        .iter()
                        .filter(|&&i| layout.iter().any(|&(f, d, _)| (f..f + d).contains(&i)))
                        .count();
                    assert_eq!(
                        rx.stats.blocks_recovered > 0,
                        data_lost > 0,
                        "{len} B, {name}"
                    );
                    // A frame counts once, however many of its blocks were rebuilt.
                    assert_eq!(
                        rx.stats.frames_recovered,
                        u64::from(data_lost > 0),
                        "{len} B, {name}"
                    );
                    assert_eq!(rx.stats.frames_lost, 0, "{len} B, {name}");
                }
            }
        }
    }

    /// One loss past a block's parity and the frame is gone; the receiver asks
    /// for a keyframe once, drops the frames until one arrives, and resumes there.
    #[test]
    fn a_frame_beyond_the_fec_is_dropped_and_the_next_keyframe_resumes() {
        let mut p = packetizer(20, 2, None);
        let start = Instant::now();
        let mut rx = VideoReceiver::new(cfg(None, false), start);
        let mut t = start;
        let mut log: Vec<VideoEvent> = Vec::new();
        let mut send = |rx: &mut VideoReceiver,
                        log: &mut Vec<VideoEvent>,
                        n: u32,
                        key: bool,
                        lost: &[usize],
                        t: Instant| {
            let au = frame_bytes(30_000, n as u8);
            let batch = p.packetize(&au, key, n, 0, 0).unwrap();
            log.extend(feed(rx, &shards(&batch), lost, t));
            (au, layout(&batch)[0])
        };
        let delivered = |log: &[VideoEvent]| -> Vec<(u32, bool, Bytes)> {
            log.iter()
                .filter_map(|e| match e {
                    VideoEvent::Frame(f) => Some((f.number, f.key, f.data.clone())),
                    _ => None,
                })
                .collect()
        };
        let idr_requests = |log: &[VideoEvent]| {
            log.iter()
                .filter(|e| matches!(e, VideoEvent::RequestIdr))
                .count()
        };

        let (au1, (_, data, parity)) = send(&mut rx, &mut log, 1, true, &[], t);
        assert_eq!(delivered(&log), vec![(1, true, au1.into())]);
        assert_eq!(idr_requests(&log), 0);

        // Frame 2 loses parity + 1 data shards of its only block.
        t += Duration::from_millis(16);
        let lost: Vec<usize> = (0..=parity).collect();
        assert!(data > parity + 1);
        send(&mut rx, &mut log, 2, false, &lost, t);
        assert_eq!(delivered(&log).len(), 1, "2 is not delivered");
        // Frame 3 arrives (a P frame); frame 2 is given up on and 3 can't start a picture.
        t += Duration::from_millis(16);
        send(&mut rx, &mut log, 3, false, &[], t);
        rx.tick(t + Duration::from_millis(1));
        log.extend(rx.take_events());
        assert_eq!(delivered(&log).len(), 1, "3 can't start a picture");
        assert_eq!(idr_requests(&log), 1, "asked for a keyframe once");
        assert_eq!(rx.stats.frames_lost, 1);
        assert_eq!(rx.stats.frames_discarded, 1);

        // P frames keep being dropped; a keyframe is delivered.
        t += Duration::from_millis(16);
        send(&mut rx, &mut log, 4, false, &[], t);
        assert_eq!(delivered(&log).len(), 1);
        t += Duration::from_millis(16);
        let (au5, _) = send(&mut rx, &mut log, 5, true, &[], t);
        assert_eq!(delivered(&log).last(), Some(&(5, true, au5.into())));
        // And the P frame after it flows.
        t += Duration::from_millis(16);
        let (au6, _) = send(&mut rx, &mut log, 6, false, &[], t);
        assert_eq!(delivered(&log).last(), Some(&(6, false, au6.into())));
        assert_eq!(rx.stats.frames_delivered, 3);
        assert_eq!(idr_requests(&log), 1, "and never asked again");
    }

    /// With invalidation on, the first answer to a loss is an invalidation of the lost
    /// range, and a P frame after it (type 5) resumes the picture.
    #[test]
    fn invalidation_resumes_at_a_frame_predicted_past_the_loss() {
        let mut p = packetizer(0, 0, None); // no parity at all: any loss is fatal for the frame
        let start = Instant::now();
        let mut rx = VideoReceiver::new(cfg(None, true), start);
        let mut t = start;
        let mut log: Vec<VideoEvent> = Vec::new();
        let batch = p.packetize(&frame_bytes(5_000, 1), true, 1, 0, 0).unwrap();
        log.extend(feed(&mut rx, &shards(&batch), &[], t));
        assert_eq!(frames(std::mem::take(&mut log)).len(), 1);

        t += Duration::from_millis(16);
        let batch2 = p.packetize(&frame_bytes(5_000, 2), false, 2, 0, 0).unwrap();
        log.extend(feed(&mut rx, &shards(&batch2), &[1], t));
        t += Duration::from_millis(16);
        let batch3 = p.packetize(&frame_bytes(5_000, 3), false, 3, 0, 0).unwrap();
        log.extend(feed(&mut rx, &shards(&batch3), &[], t));
        rx.tick(t + Duration::from_millis(11));
        log.extend(rx.take_events());
        let asked: Vec<_> = log
            .iter()
            .filter_map(|e| match e {
                VideoEvent::Invalidate { first, last } => Some((*first, *last)),
                _ => None,
            })
            .collect();
        assert_eq!(asked, vec![(2, 2)], "{log:?}");
        assert!(
            !log.iter()
                .any(|e| matches!(e, VideoEvent::Frame(_) | VideoEvent::RequestIdr))
        );
        assert_eq!(rx.stats.invalidations, 1);

        // The host answers with frame 4 of type 5: a P frame with the lost refs dropped.
        t += Duration::from_millis(16);
        let batch4 = p.packetize(&frame_bytes(5_000, 4), false, 4, 0, 0).unwrap();
        let mut all = shards(&batch4);
        // The frame header (0x01, latency, type) follows RTP 12, reserved 4 and NV 16.
        assert_eq!(all[0][32], 0x01);
        all[0][35] = 5;
        let got = frames(feed(&mut rx, &all, &[], t));
        assert_eq!(got.len(), 1, "type 5 resumes");
        assert!(!got[0].key);
        assert_eq!(got[0].data, frame_bytes(5_000, 4));
    }

    /// A recovery from corrupted shards doesn't reproduce the headers the host
    /// wrote, and the frame is refused instead of decoded.
    #[test]
    fn a_corrupted_recovery_is_rejected() {
        let mut p = packetizer(20, 2, None);
        let mut rx = VideoReceiver::new(cfg(None, false), Instant::now());
        let batch = p.packetize(&frame_bytes(10_000, 1), true, 1, 0, 0).unwrap();
        let mut all = shards(&batch);
        all[1][16 + 8] ^= 0x01; // a received shard's flags, which the code mixes into the rebuilt one
        let now = Instant::now();
        let got = frames(feed(&mut rx, &all, &[0], now));
        assert!(got.is_empty());
        assert_eq!(rx.stats.frames_lost, 1);
        // The same loss without the corruption is recovered.
        let mut rx = VideoReceiver::new(cfg(None, false), now);
        assert_eq!(frames(feed(&mut rx, &shards(&batch), &[0], now)).len(), 1);
    }

    /// Packets arriving out of order, twice, or across frames change nothing.
    #[test]
    fn reordering_and_duplicates_are_harmless() {
        let mut p = packetizer(20, 2, None);
        let mut rx = VideoReceiver::new(cfg(None, false), Instant::now());
        let now = Instant::now();
        let a = p.packetize(&frame_bytes(60_000, 1), true, 1, 0, 0).unwrap();
        let b = p
            .packetize(&frame_bytes(60_000, 2), false, 2, 0, 0)
            .unwrap();
        let mut order: Vec<Vec<u8>> = shards(&a);
        // Swap neighbours throughout, duplicate every seventh, and let frame 2 start
        // before frame 1's last packets arrive.
        for i in (0..order.len() - 1).step_by(2) {
            order.swap(i, i + 1);
        }
        // The first three data shards arrive last, so the frame is open while the rest repeat.
        let tail: Vec<Vec<u8>> = order.drain(..3).collect();
        let dups: Vec<Vec<u8>> = order.iter().step_by(7).cloned().collect();
        let mut stream = order;
        // Each repeated shard arrives early too, so its original is the duplicate.
        stream.splice(10..10, dups);
        stream.extend(shards(&b).into_iter().take(2));
        stream.extend(tail);
        stream.extend(shards(&b).into_iter().skip(2));
        for s in &stream {
            rx.datagram(s, now);
        }
        let got = frames(rx.take_events());
        assert_eq!(got.len(), 2, "{:?}", rx.stats);
        assert_eq!(got[0].data, frame_bytes(60_000, 1));
        assert_eq!(got[1].data, frame_bytes(60_000, 2));
        assert!(rx.stats.duplicates > 0);
        assert_eq!(rx.stats.frames_lost, 0);
    }

    /// A frame that never came at all is noticed from the next one's arrival.
    #[test]
    fn a_whole_lost_frame_is_noticed() {
        let mut p = packetizer(20, 2, None);
        let start = Instant::now();
        let mut rx = VideoReceiver::new(cfg(None, false), start);
        let mut t = start;
        let a = p.packetize(&frame_bytes(3_000, 1), true, 1, 0, 0).unwrap();
        feed(&mut rx, &shards(&a), &[], t);
        let _lost = p.packetize(&frame_bytes(3_000, 2), false, 2, 0, 0).unwrap();
        t += Duration::from_millis(30);
        let c = p.packetize(&frame_bytes(3_000, 3), false, 3, 0, 0).unwrap();
        feed(&mut rx, &shards(&c), &[], t);
        rx.tick(t + Duration::from_millis(11));
        let events = rx.take_events();
        assert!(events.iter().any(|e| matches!(e, VideoEvent::RequestIdr)));
        assert_eq!(rx.stats.frames_lost, 1);
    }

    /// A frame cut off with nothing after it is given up on after a stall.
    #[test]
    fn a_stalled_frame_is_given_up_on() {
        let mut p = packetizer(0, 0, None);
        let start = Instant::now();
        let mut rx = VideoReceiver::new(cfg(None, false), start);
        let a = p.packetize(&frame_bytes(8_000, 1), true, 1, 0, 0).unwrap();
        let n = shards(&a).len();
        feed(&mut rx, &shards(&a), &[n - 1], start);
        rx.tick(start + Duration::from_millis(50));
        assert!(rx.take_events().is_empty(), "still waiting");
        rx.tick(start + Duration::from_millis(101));
        assert!(
            rx.take_events()
                .iter()
                .any(|e| matches!(e, VideoEvent::RequestIdr))
        );
        assert_eq!(rx.stats.frames_lost, 1);
    }

    /// The keyframe asked for is asked for again if it doesn't come.
    #[test]
    fn a_missing_keyframe_is_asked_for_again() {
        let start = Instant::now();
        let mut rx = VideoReceiver::new(cfg(None, false), start);
        rx.tick(start + Duration::from_millis(900));
        assert!(rx.take_events().is_empty());
        rx.tick(start + Duration::from_millis(1001));
        assert_eq!(rx.take_events().len(), 1);
        rx.tick(start + Duration::from_millis(1500));
        assert!(rx.take_events().is_empty());
        rx.tick(start + Duration::from_millis(2100));
        assert_eq!(rx.take_events().len(), 1);
    }

    #[test]
    fn tampered_encrypted_packets_are_refused() {
        let mut p = packetizer(20, 2, Some(KEY));
        let mut rx = VideoReceiver::new(cfg(Some(KEY), false), Instant::now());
        let batch = p.packetize(&frame_bytes(3_000, 1), true, 1, 0, 0).unwrap();
        let now = Instant::now();
        let mut all = shards(&batch);
        // Flipped ciphertext, flipped tag, a wrong key's packet.
        all[0][40] ^= 1;
        all[1][20] ^= 1;
        rx.datagram(&all[0], now);
        rx.datagram(&all[1], now);
        assert_eq!(rx.stats.decrypt_failures, 2);
        let mut other = packetizer(20, 2, Some([9; 16]));
        let foreign = other
            .packetize(&frame_bytes(3_000, 1), true, 1, 0, 0)
            .unwrap();
        rx.datagram(&shards(&foreign)[2], now);
        assert_eq!(rx.stats.decrypt_failures, 3);
        // The rest of the frame still arrives: the two refused shards are recovered from parity.
        let got = frames(feed(&mut rx, &all[2..], &[], now));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, frame_bytes(3_000, 1));
    }

    #[test]
    fn av1_frames_are_cut_to_their_length() {
        let mut p = packetizer(20, 2, None);
        let mut rx = VideoReceiver::new(
            VideoConfig {
                codec: VideoCodec::Av1,
                ..cfg(None, false)
            },
            Instant::now(),
        );
        let au = frame_bytes(5_000, 3); // no zero tail to hide a wrong length in
        let batch = p.packetize(&au, true, 1, 0, 0).unwrap();
        let got = frames(feed(&mut rx, &shards(&batch), &[0, 2], Instant::now()));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, au);
    }

    /// Whatever arrives on the port, the receiver returns; it never panics and
    /// holds a bounded amount.
    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0x5EED_0FC0_FFEEu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut p = packetizer(20, 2, None);
        let valid: Vec<Vec<u8>> =
            shards(&p.packetize(&frame_bytes(20_000, 1), true, 1, 0, 0).unwrap());
        for key in [None, Some(KEY)] {
            let mut rx = VideoReceiver::new(cfg(key, false), Instant::now());
            let mut t = Instant::now();
            for i in 0..40_000 {
                t += Duration::from_micros(next() % 3000);
                let mut buf: Vec<u8> = if i % 3 == 0 {
                    let len = (next() % 1100) as usize;
                    (0..len).map(|_| next() as u8).collect()
                } else {
                    // A valid shard with a few bytes flipped, often in the headers.
                    let mut s = valid[(next() % valid.len() as u64) as usize].clone();
                    for _ in 0..1 + next() % 4 {
                        let at = (next() % 48.min(s.len() as u64)) as usize;
                        s[at] = next() as u8;
                    }
                    if next() % 5 == 0 {
                        s.truncate((next() % s.len() as u64) as usize);
                    }
                    s
                };
                if next() % 11 == 0 {
                    buf.extend((0..next() % 600).map(|_| next() as u8));
                }
                rx.datagram(&buf, t);
                if i % 17 == 0 {
                    rx.tick(t);
                }
                let _ = rx.take_events();
                assert!(rx.frames.len() <= MAX_PENDING_FRAMES, "{}", rx.frames.len());
            }
        }
    }
}

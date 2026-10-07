// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: no unsafe (shards are plain chunks), key and counters live in the packetizer, the block plan comes
// from fec.rs, and a frame the FEC can't carry is an error instead of a malformed packet.

//! Video RTP: an access unit becomes NV video packets (RTP header, NV header,
//! payload), grouped in FEC blocks with Reed-Solomon parity, each optionally
//! AES-GCM encrypted.
//!
//! One shard on the wire, offsets from its start:
//!
//! ```text
//! [encryption prefix: iv(12) frame number(4) tag(16)]   only when encrypted
//! [RTP header 12][reserved 4][NV video packet header 16][payload]
//! ```
//!
//! The first payload carries the 8-byte frame header (`01`, latency, frame
//! type, length of the last payload) before the access unit. FEC covers the
//! RTP header, reserved bytes, NV header and payload of each shard; parity
//! shards get their own headers afterwards.

use crate::crypto::{GCM_TAG_LEN, gcm_encrypt};

use super::fec::{self, Codecs};

pub(crate) const NV_VIDEO_PACKET_SIZE: usize = 16;
pub(crate) const RTP_HEADER_SIZE: usize = 12;
pub(crate) const RESERVED_SIZE: usize = 4;
pub(crate) const NV_PACKET_OFFSET: usize = RTP_HEADER_SIZE + RESERVED_SIZE;
pub(crate) const PAYLOAD_OFFSET: usize = NV_PACKET_OFFSET + NV_VIDEO_PACKET_SIZE;
/// iv(12) + frame number(4) + tag(16).
pub(crate) const ENC_PREFIX_SIZE: usize = 12 + 4 + GCM_TAG_LEN;
pub(crate) const FRAME_HEADER_SIZE: usize = 8;

pub(crate) const FLAG_CONTAINS_PIC_DATA: u8 = 0x1;
pub(crate) const FLAG_END_OF_FRAME: u8 = 0x2;
pub(crate) const FLAG_START_OF_FRAME: u8 = 0x4;

/// Equal-sized shards in one contiguous buffer, ready to send.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ShardBatch {
    data: Vec<u8>,
    shard_size: usize,
}

impl ShardBatch {
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
    pub fn shard_size(&self) -> usize {
        self.shard_size
    }
    #[cfg(test)]
    pub fn shard_count(&self) -> usize {
        if self.shard_size == 0 {
            0
        } else {
            self.data.len() / self.shard_size
        }
    }
    #[cfg(test)]
    pub fn shards(&self) -> impl Iterator<Item = &[u8]> {
        self.data.chunks_exact(self.shard_size.max(1))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum PacketizeError {
    #[error(transparent)]
    TooLarge(#[from] fec::TooLarge),
    #[error("FEC: {0}")]
    Fec(#[from] fec_rs::Error),
    #[error("encrypting a shard failed")]
    Encrypt,
    #[error("an empty frame")]
    Empty,
}

pub(crate) struct PacketizerConfig {
    /// The client's packet size (NV header plus payload).
    pub packet_size: usize,
    pub fec_percent: u8,
    pub min_fec_packets: u32,
    /// Present when video is encrypted.
    pub key: Option<[u8; 16]>,
}

pub(crate) struct Packetizer {
    cfg: PacketizerConfig,
    codecs: Codecs,
    /// The next packet's stream sequence number, across frames.
    sequence: u32,
    /// Counter for GCM IVs; one per encrypted shard.
    iv_counter: u64,
}

fn write_rtp_header(shard: &mut [u8], sequence: u16, timestamp: u32) {
    shard[0] = 0x90; // version 2, extension bit set: what Moonlight expects
    shard[1] = 0;
    shard[2..4].copy_from_slice(&sequence.to_be_bytes());
    shard[4..8].copy_from_slice(&timestamp.to_be_bytes());
    shard[8..12].copy_from_slice(&0u32.to_be_bytes());
}

fn write_nv_header(
    shard: &mut [u8],
    stream_index: u32,
    frame: u32,
    flags: u8,
    multi_fec_blocks: u8,
    fec_info: u32,
) {
    let nv = &mut shard[NV_PACKET_OFFSET..PAYLOAD_OFFSET];
    nv[0..4].copy_from_slice(&stream_index.to_le_bytes());
    nv[4..8].copy_from_slice(&frame.to_le_bytes());
    nv[8] = flags;
    nv[9] = 0;
    nv[10] = 0x10;
    nv[11] = multi_fec_blocks;
    nv[12..16].copy_from_slice(&fec_info.to_le_bytes());
}

fn fec_info(index: usize, data: usize, percent: usize) -> u32 {
    ((index << 12) | (data << 22) | (percent << 4)) as u32
}

impl Packetizer {
    pub fn new(cfg: PacketizerConfig) -> Self {
        Self {
            cfg,
            codecs: Codecs::default(),
            sequence: 0,
            iv_counter: 0,
        }
    }

    /// Packetizes one access unit. `frame_number` counts from 1 per stream;
    /// `latency` is in tenths of a millisecond.
    pub fn packetize(
        &mut self,
        frame: &[u8],
        key_frame: bool,
        frame_number: u32,
        rtp_timestamp: u32,
        latency: u16,
    ) -> Result<ShardBatch, PacketizeError> {
        if frame.is_empty() {
            return Err(PacketizeError::Empty);
        }
        let payload_size = self
            .cfg
            .packet_size
            .saturating_sub(NV_VIDEO_PACKET_SIZE)
            .max(1);
        let total = FRAME_HEADER_SIZE + frame.len();
        let data_shards = total.div_ceil(payload_size);
        let last_payload = match total % payload_size {
            0 => payload_size,
            n => n,
        };

        let mut header = [0u8; FRAME_HEADER_SIZE];
        header[0] = 0x01;
        header[1..3].copy_from_slice(&latency.to_le_bytes());
        header[3] = if key_frame { 2 } else { 1 };
        header[4..8].copy_from_slice(&(last_payload as u32).to_le_bytes());

        let plan = fec::plan(
            data_shards,
            usize::from(self.cfg.fec_percent),
            self.cfg.min_fec_packets as usize,
        )?;
        let last_block = (plan.len() - 1) as u8;
        let prefix = if self.cfg.key.is_some() {
            ENC_PREFIX_SIZE
        } else {
            0
        };
        let shard_len = PAYLOAD_OFFSET + payload_size;
        let stride = prefix + shard_len;

        let mut batch = ShardBatch {
            data: Vec::with_capacity(stride * (data_shards + data_shards / 4 + 2)),
            shard_size: stride,
        };
        let mut next_data = 0usize;

        for (block_index, block) in plan.iter().enumerate() {
            let shards_in_block = block.data + block.parity;
            let mut buf = vec![0u8; shards_in_block * stride];
            let multi_fec = ((block_index as u8) << 4) | (last_block << 6);

            for i in 0..block.data {
                let shard = &mut buf[i * stride + prefix..(i + 1) * stride];
                let sequence = self.sequence;
                self.sequence = self.sequence.wrapping_add(1);

                write_rtp_header(shard, sequence as u16, rtp_timestamp);
                let mut flags = FLAG_CONTAINS_PIC_DATA;
                if i == 0 {
                    flags |= FLAG_START_OF_FRAME;
                }
                if i == block.data - 1 {
                    flags |= FLAG_END_OF_FRAME;
                }
                write_nv_header(
                    shard,
                    sequence << 8,
                    frame_number,
                    flags,
                    multi_fec,
                    fec_info(i, block.data, block.percent),
                );

                // Payload: bytes `from..from + len` of header ++ frame.
                let from = next_data * payload_size;
                let len = payload_size.min(total - from);
                let dst = &mut shard[PAYLOAD_OFFSET..PAYLOAD_OFFSET + len];
                let in_header = FRAME_HEADER_SIZE.saturating_sub(from).min(len);
                dst[..in_header].copy_from_slice(
                    &header[from.min(FRAME_HEADER_SIZE)..from.min(FRAME_HEADER_SIZE) + in_header],
                );
                let frame_from = from.saturating_sub(FRAME_HEADER_SIZE);
                dst[in_header..]
                    .copy_from_slice(&frame[frame_from..frame_from + (len - in_header)]);
                next_data += 1;
            }

            if block.parity > 0 {
                let codec = self.codecs.get(block.data, block.parity)?;
                let mut views: Vec<&mut [u8]> = buf
                    .chunks_exact_mut(stride)
                    .map(|s| &mut s[prefix..])
                    .collect();
                codec.encode(&mut views)?;
                for p in 0..block.parity {
                    let sequence = self.sequence;
                    self.sequence = self.sequence.wrapping_add(1);
                    // FEC overwrote the whole shard; put back what Moonlight reads.
                    let shard =
                        &mut buf[(block.data + p) * stride + prefix..(block.data + p + 1) * stride];
                    shard[0] = 0x90;
                    shard[1] = 0;
                    shard[2..4].copy_from_slice(&(sequence as u16).to_be_bytes());
                    let nv = &mut shard[NV_PACKET_OFFSET..PAYLOAD_OFFSET];
                    nv[4..8].copy_from_slice(&frame_number.to_le_bytes());
                    nv[11] = multi_fec;
                    nv[12..16].copy_from_slice(
                        &fec_info(block.data + p, block.data, block.percent).to_le_bytes(),
                    );
                }
            }

            if let Some(key) = &self.cfg.key {
                for s in 0..shards_in_block {
                    let mut iv = [0u8; 12];
                    iv[..8].copy_from_slice(&self.iv_counter.to_le_bytes());
                    iv[11] = b'V';
                    self.iv_counter += 1;
                    let (pre, body) = buf[s * stride..(s + 1) * stride].split_at_mut(prefix);
                    let tag = gcm_encrypt(key, &iv, body).map_err(|_| PacketizeError::Encrypt)?;
                    pre[..12].copy_from_slice(&iv);
                    pre[12..16].copy_from_slice(&frame_number.to_le_bytes());
                    pre[16..32].copy_from_slice(&tag);
                }
            }
            batch.data.extend_from_slice(&buf);
        }
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::gcm_decrypt;

    fn cfg(fec: u8, min: u32, key: Option<[u8; 16]>) -> PacketizerConfig {
        PacketizerConfig {
            packet_size: 1024,
            fec_percent: fec,
            min_fec_packets: min,
            key,
        }
    }

    fn frame(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Reassembles the access unit from a batch the way a client would,
    /// optionally without some shards (by position), using FEC to recover.
    fn reassemble(batch: &ShardBatch, packet_size: usize, lost: &[usize]) -> Vec<u8> {
        let mut out = Vec::new();
        let payload_size = packet_size - NV_VIDEO_PACKET_SIZE;
        let shards: Vec<&[u8]> = batch.shards().collect();
        let mut i = 0;
        while i < shards.len() {
            let nv = |s: &[u8]| -> (u32, usize, usize) {
                let n = &s[NV_PACKET_OFFSET..PAYLOAD_OFFSET];
                let info = u32::from_le_bytes(n[12..16].try_into().unwrap());
                (
                    info,
                    ((info >> 22) & 0x3FF) as usize,
                    ((info >> 4) & 0xFF) as usize,
                )
            };
            let (_, data, pct) = nv(shards[i]);
            let parity = fec::parity_for(data, pct);
            let mut block: Vec<Option<Vec<u8>>> = (0..data + parity)
                .map(|k| {
                    if lost.contains(&(i + k)) {
                        None
                    } else {
                        Some(shards[i + k].to_vec())
                    }
                })
                .collect();
            if block.iter().any(Option::is_none) {
                fec_rs::ReedSolomon::new(data, parity)
                    .unwrap()
                    .reconstruct_data(&mut block)
                    .unwrap();
            }
            for s in block.iter().take(data) {
                out.extend_from_slice(
                    &s.as_ref().unwrap()[PAYLOAD_OFFSET..PAYLOAD_OFFSET + payload_size],
                );
            }
            i += data + parity;
        }
        // Strip the 8-byte frame header and the padding of the last shard.
        let last = u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize;
        let total = out.len() - payload_size + last;
        out[FRAME_HEADER_SIZE..total].to_vec()
    }

    #[test]
    fn golden_small_keyframe() {
        let mut p = Packetizer::new(cfg(20, 0, None));
        let au = frame(2000);
        let batch = p.packetize(&au, true, 1, 0x1234, 7).unwrap();
        // 2008 bytes at 1008 per payload: 2 data shards, 1 parity.
        assert_eq!(batch.shard_size(), 1024 + 16);
        assert_eq!(batch.shard_count(), 3);
        let s0 = batch.shards().next().unwrap();
        // RTP: V2+ext, type 0, sequence 0, timestamp, ssrc 0.
        assert_eq!(&s0[..12], &[0x90, 0, 0, 0, 0, 0, 0x12, 0x34, 0, 0, 0, 0]);
        // NV header: stream index 0, frame 1, flags start|pic, multi-fec 0x10, blocks 0, fec info.
        assert_eq!(&s0[16..20], &[0, 0, 0, 0]);
        assert_eq!(&s0[20..24], &1u32.to_le_bytes());
        assert_eq!(s0[24], FLAG_CONTAINS_PIC_DATA | FLAG_START_OF_FRAME);
        assert_eq!(s0[26], 0x10);
        assert_eq!(s0[27], 0);
        let info0 = u32::from_le_bytes(s0[28..32].try_into().unwrap());
        assert_eq!(info0, (2 << 22) | (20 << 4));
        // Payload begins with the frame header: 01, latency 7, IDR, last payload 2008-1008=1000.
        assert_eq!(&s0[32..40], &[1, 7, 0, 2, 0xE8, 0x03, 0, 0]);
        assert_eq!(&s0[40..44], &au[..4]);
        let s1 = batch.shards().nth(1).unwrap();
        assert_eq!(s1[24], FLAG_CONTAINS_PIC_DATA | FLAG_END_OF_FRAME);
        assert_eq!(u32::from_le_bytes(s1[16..20].try_into().unwrap()), 1 << 8);
        let s2 = batch.shards().nth(2).unwrap();
        // Parity: next sequence, same frame, fec index = data count.
        assert_eq!(&s2[2..4], &2u16.to_be_bytes());
        assert_eq!(u32::from_le_bytes(s2[20..24].try_into().unwrap()), 1);
        let info2 = u32::from_le_bytes(s2[28..32].try_into().unwrap());
        assert_eq!(info2 >> 12 & 0x3FF, 2);
        assert_eq!(info2 >> 22, 2);
        assert_eq!(reassemble(&batch, 1024, &[]), au);
    }

    #[test]
    fn exactly_full_last_payload() {
        let mut p = Packetizer::new(cfg(0, 0, None));
        let au = frame(1008 * 2 - FRAME_HEADER_SIZE);
        let batch = p.packetize(&au, false, 3, 0, 0).unwrap();
        assert_eq!(batch.shard_count(), 2);
        assert_eq!(batch.shards().next().unwrap()[35], 1, "P frame type");
        assert_eq!(reassemble(&batch, 1024, &[]), au);
    }

    #[test]
    fn lost_packets_are_recovered_from_parity() {
        let mut p = Packetizer::new(cfg(30, 0, None));
        let au = frame(20_000);
        let batch = p.packetize(&au, true, 9, 0, 0).unwrap();
        assert_eq!(reassemble(&batch, 1024, &[0, 3]), au);
    }

    #[test]
    fn big_frames_use_several_blocks_with_their_indexes() {
        let mut p = Packetizer::new(cfg(20, 0, None));
        let au = frame(400_000);
        let batch = p.packetize(&au, true, 1, 0, 0).unwrap();
        let blocks: Vec<(u8, u8)> = batch
            .shards()
            .map(|s| (s[27] >> 4 & 3, s[27] >> 6))
            .collect();
        let last = blocks.iter().map(|b| b.0).max().unwrap();
        assert!(last >= 1);
        assert!(blocks.iter().all(|b| b.1 == last));
        assert_eq!(reassemble(&batch, 1024, &[]), au);
        // Sequence numbers are consecutive across data and parity.
        let seqs: Vec<u16> = batch
            .shards()
            .map(|s| u16::from_be_bytes([s[2], s[3]]))
            .collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0].wrapping_add(1)));
        // The next frame carries on from there.
        let next = p.packetize(&frame(10), false, 2, 0, 0).unwrap();
        assert_eq!(
            u16::from_be_bytes([next.as_bytes()[2], next.as_bytes()[3]]),
            seqs.len() as u16
        );
    }

    #[test]
    fn encrypted_shards_decrypt_to_what_would_have_been_sent() {
        let key = [0x42u8; 16];
        let au = frame(3000);
        let plain = Packetizer::new(cfg(20, 1, None))
            .packetize(&au, true, 5, 0, 0)
            .unwrap();
        let enc = Packetizer::new(cfg(20, 1, Some(key)))
            .packetize(&au, true, 5, 0, 0)
            .unwrap();
        assert_eq!(enc.shard_size(), plain.shard_size() + ENC_PREFIX_SIZE);
        for (n, (e, p)) in enc.shards().zip(plain.shards()).enumerate() {
            let iv: [u8; 12] = e[..12].try_into().unwrap();
            // The IV counts shards from 0 and ends in 'V'.
            assert_eq!(&iv[..8], &(n as u64).to_le_bytes());
            assert_eq!(iv[11], b'V');
            assert_eq!(&e[12..16], &5u32.to_le_bytes());
            let tag: [u8; 16] = e[16..32].try_into().unwrap();
            let mut body = e[ENC_PREFIX_SIZE..].to_vec();
            gcm_decrypt(&key, &iv, &mut body, &tag).unwrap();
            assert_eq!(body, p);
        }
    }

    #[test]
    fn empty_frames_are_refused() {
        assert!(matches!(
            Packetizer::new(cfg(20, 0, None)).packetize(&[], true, 1, 0, 0),
            Err(PacketizeError::Empty)
        ));
    }

    #[test]
    fn frames_beyond_four_blocks_fail_cleanly() {
        let mut p = Packetizer::new(cfg(20, 0, None));
        let r = p.packetize(&frame(1008 * 1100), true, 1, 0, 0);
        assert!(matches!(r, Err(PacketizeError::TooLarge(_))));
    }
}

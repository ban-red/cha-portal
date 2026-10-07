// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: the packetizer is separate from the Opus encoder and the sockets (the backend brings Opus),
// no unsafe header casts, FEC over zero-padded payloads of a block, no per-packet allocation of the encoder.

//! Audio RTP: Opus packets become RTP packets of payload type 97, in blocks
//! of four data packets followed by two Reed-Solomon parity packets (type
//! 127, with an FEC header naming the block). Payloads are AES-CBC encrypted
//! when the client asks.
//!
//! Moonlight's audio parity matrix is not what a stock Reed-Solomon code
//! generates; the matrix below is the one from moonlight-common-c.

use fec_rs::ReedSolomon;

use crate::crypto::cbc_encrypt;

const DATA_SHARDS: usize = 4;
const PARITY_SHARDS: usize = 2;
/// moonlight-common-c's RtpAudioQueue.c, the rows for the two parity shards.
const PARITY_MATRIX: [u8; 8] = [0x77, 0x40, 0x38, 0x0e, 0xc7, 0xa7, 0x0d, 0x6c];

pub(crate) const RTP_HEADER_SIZE: usize = 12;
pub(crate) const FEC_HEADER_SIZE: usize = 12;
const PT_AUDIO: u8 = 97;
const PT_AUDIO_FEC: u8 = 127;

pub(crate) struct AudioPacketizer {
    codec: ReedSolomon,
    /// `Some((key, key id))` when audio is encrypted.
    key: Option<([u8; 16], i64)>,
    sequence: u16,
    /// Total samples per channel sent, for the RTP clock.
    samples: u64,
    block: [Vec<u8>; DATA_SHARDS],
    base_sequence: u16,
    base_timestamp: u32,
}

impl AudioPacketizer {
    pub fn new(key: Option<([u8; 16], i64)>) -> Self {
        let mut codec = ReedSolomon::new(DATA_SHARDS, PARITY_SHARDS).expect("4+2 is a valid shape");
        codec
            .set_parity_matrix(&PARITY_MATRIX)
            .expect("the matrix is 2x4");
        Self {
            codec,
            key,
            sequence: 0,
            samples: 0,
            block: Default::default(),
            base_sequence: 0,
            base_timestamp: 0,
        }
    }

    /// Packetizes one Opus packet of `samples` samples per channel. Returns
    /// the RTP packet, followed by the two parity packets when it completes
    /// a block of four.
    pub fn push(&mut self, opus: &[u8], samples: u64) -> Vec<Vec<u8>> {
        // The RTP clock is milliseconds, as Moonlight reads it.
        let timestamp = (self.samples / 48) as u32;
        self.samples += samples;
        let slot = usize::from(self.sequence) % DATA_SHARDS;
        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);

        let payload = match &self.key {
            Some((key, key_id)) => {
                // IV: the key id plus the sequence number, big-endian, then zeros.
                let mut iv = [0u8; 16];
                iv[..4].copy_from_slice(
                    &(*key_id as u32)
                        .wrapping_add(u32::from(sequence))
                        .to_be_bytes(),
                );
                cbc_encrypt(key, &iv, opus)
            }
            None => opus.to_vec(),
        };

        if slot == 0 {
            self.base_sequence = sequence;
            self.base_timestamp = timestamp;
        }
        let mut packet = Vec::with_capacity(RTP_HEADER_SIZE + payload.len());
        packet.extend_from_slice(&rtp_header(PT_AUDIO, sequence, timestamp));
        packet.extend_from_slice(&payload);
        self.block[slot] = payload;
        let mut out = vec![packet];

        if slot == DATA_SHARDS - 1 {
            let len = self.block.iter().map(Vec::len).max().unwrap_or(0);
            let data: Vec<Vec<u8>> = self
                .block
                .iter()
                .map(|p| {
                    let mut p = p.clone();
                    p.resize(len, 0);
                    p
                })
                .collect();
            let mut parity = vec![vec![0u8; len]; PARITY_SHARDS];
            if len > 0 && self.codec.encode_sep(&data, &mut parity).is_ok() {
                for (i, shard) in parity.iter().enumerate() {
                    let mut p = Vec::with_capacity(RTP_HEADER_SIZE + FEC_HEADER_SIZE + len);
                    // The next sequence number, unincremented: FEC packets have their own numbering.
                    p.extend_from_slice(&rtp_header(
                        PT_AUDIO_FEC,
                        self.sequence.wrapping_add(i as u16),
                        0,
                    ));
                    p.push(i as u8);
                    p.push(PT_AUDIO);
                    p.extend_from_slice(&self.base_sequence.to_be_bytes());
                    p.extend_from_slice(&self.base_timestamp.to_be_bytes());
                    p.extend_from_slice(&0u32.to_be_bytes());
                    p.extend_from_slice(shard);
                    out.push(p);
                }
            }
        }
        out
    }
}

fn rtp_header(payload_type: u8, sequence: u16, timestamp: u32) -> [u8; RTP_HEADER_SIZE] {
    let mut h = [0u8; RTP_HEADER_SIZE];
    h[0] = 0x80;
    h[1] = payload_type;
    h[2..4].copy_from_slice(&sequence.to_be_bytes());
    h[4..8].copy_from_slice(&timestamp.to_be_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opus(n: u8) -> Vec<u8> {
        (0..40)
            .map(|i| n.wrapping_mul(31).wrapping_add(i))
            .collect()
    }

    #[test]
    fn golden_plain_block() {
        let mut p = AudioPacketizer::new(None);
        let mut all = Vec::new();
        for n in 0..4 {
            all.extend(p.push(&opus(n), 240));
        }
        // 4 data packets then 2 parity.
        assert_eq!(all.len(), 6);
        assert_eq!(&all[0][..12], &[0x80, 97, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&all[0][12..], &opus(0)[..]);
        // 5 ms per packet: the clock reads 5 ms ticks.
        assert_eq!(&all[1][2..8], &[0, 1, 0, 0, 0, 5]);
        assert_eq!(&all[3][2..8], &[0, 3, 0, 0, 0, 15]);
        // Parity: type 127, sequence 4 and 5, timestamp 0, then the FEC header.
        assert_eq!(&all[4][..12], &[0x80, 127, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&all[5][2..4], &[0, 5]);
        assert_eq!(&all[4][12..24], &[0, 97, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&all[5][12..14], &[1, 97]);
        assert_eq!(all[4].len(), 12 + 12 + 40);
    }

    #[test]
    fn parity_follows_moonlights_matrix() {
        let mut p = AudioPacketizer::new(None);
        let blocks: Vec<Vec<u8>> = (0..4).map(opus).collect();
        let mut out = Vec::new();
        for b in &blocks {
            out.extend(p.push(b, 240));
        }
        // parity[r] = sum over data shards of matrix[r][c] * data[c] in GF(256).
        let gf = |a: u8, b: u8| fec_rs::galois::mul(a, b);
        for r in 0..2 {
            let expect: Vec<u8> = (0..40)
                .map(|i| {
                    (0..4).fold(0u8, |acc, c| {
                        acc ^ gf(PARITY_MATRIX[r * 4 + c], blocks[c][i])
                    })
                })
                .collect();
            assert_eq!(&out[4 + r][24..], &expect[..], "parity row {r}");
        }
    }

    #[test]
    fn a_lost_packet_is_recovered_with_the_same_code() {
        let mut p = AudioPacketizer::new(None);
        let blocks: Vec<Vec<u8>> = (0..4).map(opus).collect();
        let mut out = Vec::new();
        for b in &blocks {
            out.extend(p.push(b, 240));
        }
        let mut shards: Vec<Option<Vec<u8>>> = out
            .iter()
            .map(|pk| Some(pk[if pk[1] == 97 { 12 } else { 24 }..].to_vec()))
            .collect();
        shards[2] = None;
        shards[0] = None;
        let mut codec = ReedSolomon::new(4, 2).unwrap();
        codec.set_parity_matrix(&PARITY_MATRIX).unwrap();
        codec.reconstruct_data(&mut shards).unwrap();
        assert_eq!(shards[0].as_ref().unwrap(), &blocks[0]);
        assert_eq!(shards[2].as_ref().unwrap(), &blocks[2]);
    }

    #[test]
    fn encrypted_payloads_use_the_key_id_plus_sequence_as_iv() {
        let key = [3u8; 16];
        let mut p = AudioPacketizer::new(Some((key, 100)));
        let pk = p.push(&opus(1), 240);
        assert_eq!(pk[0].len(), 12 + 48, "40 bytes pad to 48");
        let mut iv = [0u8; 16];
        iv[..4].copy_from_slice(&100u32.to_be_bytes());
        assert_eq!(&pk[0][12..], &cbc_encrypt(&key, &iv, &opus(1))[..]);
        // The next packet's IV counts up.
        let pk = p.push(&opus(2), 240);
        iv[..4].copy_from_slice(&101u32.to_be_bytes());
        assert_eq!(&pk[0][12..], &cbc_encrypt(&key, &iv, &opus(2))[..]);
    }

    #[test]
    fn sequence_numbers_wrap_and_blocks_stay_aligned() {
        let mut p = AudioPacketizer::new(None);
        p.sequence = 65532;
        let mut parity_seen = 0;
        for n in 0..4 {
            let out = p.push(&opus(n), 240);
            parity_seen += out.len() - 1;
        }
        assert_eq!(parity_seen, 2);
        assert_eq!(p.sequence, 0);
    }
}

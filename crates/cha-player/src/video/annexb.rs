//! Annex-B access units, split for a hardware decoder: the parameter sets for
//! its format description, and the picture NAL units as AVCC (4-byte length
//! prefixed) samples.

use cha_client::Codec;

/// The parameter sets an access unit carried, NAL headers included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParamSets {
    pub vps: Vec<Vec<u8>>,
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
}

impl ParamSets {
    pub fn is_empty(&self) -> bool {
        self.sps.is_empty() || self.pps.is_empty()
    }

    /// In the order a format description wants them (VPS, SPS, PPS).
    pub fn ordered(&self) -> Vec<&[u8]> {
        self.vps
            .iter()
            .chain(&self.sps)
            .chain(&self.pps)
            .map(Vec::as_slice)
            .collect()
    }
}

/// One access unit, ready for a decoder.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Sample {
    /// Present when the unit carried parameter sets (every key frame, from a
    /// Moonlight host).
    pub params: Option<ParamSets>,
    /// The picture NAL units, each prefixed by its 4-byte big-endian length.
    pub avcc: Vec<u8>,
    /// An IDR / IRAP picture was seen.
    pub key: bool,
}

/// The NAL units of an Annex-B buffer, without start codes.
pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|s| {
            // The next start code is `00 00 01`, maybe with a leading zero.
            let mut e = s - 3;
            if e > 0 && data[e - 1] == 0 {
                e -= 1;
            }
            e
        })
        .chain(std::iter::once(data.len()))
        .collect();
    starts
        .into_iter()
        .zip(ends)
        .filter(|(s, e)| e > s)
        .map(move |(s, e)| trim_trailing_zeros(&data[s..e]))
}

/// A NAL unit never ends in zero bytes (they are trailing padding).
fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let mut end = nal.len();
    while end > 1 && nal[end - 1] == 0 {
        end -= 1;
    }
    &nal[..end]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Vps,
    Sps,
    Pps,
    /// Picture data, or an SEI: goes into the sample.
    Slice {
        key: bool,
    },
    /// Access unit delimiters, filler: dropped.
    Skip,
}

fn classify(codec: Codec, nal: &[u8]) -> Kind {
    let Some(&first) = nal.first() else {
        return Kind::Skip;
    };
    match codec {
        Codec::H264 => match first & 0x1f {
            7 => Kind::Sps,
            8 => Kind::Pps,
            9..=12 => Kind::Skip,
            5 => Kind::Slice { key: true },
            _ => Kind::Slice { key: false },
        },
        Codec::Hevc => match (first >> 1) & 0x3f {
            32 => Kind::Vps,
            33 => Kind::Sps,
            34 => Kind::Pps,
            35..=38 => Kind::Skip,
            16..=21 => Kind::Slice { key: true },
            _ => Kind::Slice { key: false },
        },
        Codec::Av1 => Kind::Skip,
    }
}

/// Split one Annex-B access unit.
pub fn to_sample(codec: Codec, data: &[u8]) -> Sample {
    let mut sample = Sample::default();
    let mut params = ParamSets::default();
    let mut any_params = false;
    for nal in nal_units(data) {
        match classify(codec, nal) {
            Kind::Vps => {
                params.vps.push(nal.to_vec());
                any_params = true;
            }
            Kind::Sps => {
                params.sps.push(nal.to_vec());
                any_params = true;
            }
            Kind::Pps => {
                params.pps.push(nal.to_vec());
                any_params = true;
            }
            Kind::Slice { key } => {
                sample.key |= key;
                sample
                    .avcc
                    .extend_from_slice(&(nal.len() as u32).to_be_bytes());
                sample.avcc.extend_from_slice(nal);
            }
            Kind::Skip => {}
        }
    }
    if any_params {
        sample.params = Some(params);
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    const SC4: [u8; 4] = [0, 0, 0, 1];
    const SC3: [u8; 3] = [0, 0, 1];

    fn h264_unit() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend(SC4); // AUD
        d.extend([0x09, 0xf0]);
        d.extend(SC4); // SPS
        d.extend([0x67, 0x64, 0x00, 0x28, 0xac]);
        d.extend(SC4); // PPS
        d.extend([0x68, 0xee, 0x3c, 0x80]);
        d.extend(SC3); // IDR slice
        d.extend([0x65, 0x88, 0x84, 0x00, 0x33]);
        d
    }

    #[test]
    fn splits_start_codes_of_both_lengths() {
        let data = h264_unit();
        let nals: Vec<&[u8]> = nal_units(&data).collect();
        assert_eq!(nals.len(), 4);
        assert_eq!(nals[0], [0x09, 0xf0]);
        assert_eq!(nals[1], [0x67, 0x64, 0x00, 0x28, 0xac]);
        assert_eq!(nals[3], [0x65, 0x88, 0x84, 0x00, 0x33]);
    }

    #[test]
    fn h264_key_frame_gives_params_and_avcc() {
        let sample = to_sample(Codec::H264, &h264_unit());
        assert!(sample.key);
        let params = sample.params.expect("parameter sets");
        assert_eq!(params.sps, vec![vec![0x67, 0x64, 0x00, 0x28, 0xac]]);
        assert_eq!(params.pps, vec![vec![0x68, 0xee, 0x3c, 0x80]]);
        assert!(params.vps.is_empty());
        assert_eq!(
            sample.avcc,
            [0, 0, 0, 5, 0x65, 0x88, 0x84, 0x00, 0x33],
            "AUD and parameter sets are not in the sample"
        );
    }

    #[test]
    fn h264_delta_frame_has_no_params() {
        let mut d = Vec::new();
        d.extend(SC4);
        d.extend([0x41, 0x9a, 0x24, 0x6c]);
        let sample = to_sample(Codec::H264, &d);
        assert!(!sample.key);
        assert!(sample.params.is_none());
        assert_eq!(sample.avcc, [0, 0, 0, 4, 0x41, 0x9a, 0x24, 0x6c]);
    }

    #[test]
    fn hevc_parameter_sets_in_order() {
        let mut d = Vec::new();
        d.extend(SC4);
        d.extend([0x40, 0x01, 0x0c]); // VPS (32)
        d.extend(SC4);
        d.extend([0x42, 0x01, 0x01, 0x01]); // SPS (33)
        d.extend(SC4);
        d.extend([0x44, 0x01, 0xc1]); // PPS (34)
        d.extend(SC4);
        d.extend([0x26, 0x01, 0xaf, 0x08]); // IDR_W_RADL (19)
        let sample = to_sample(Codec::Hevc, &d);
        assert!(sample.key);
        let params = sample.params.unwrap();
        assert_eq!(params.ordered().len(), 3);
        assert_eq!(params.ordered()[0][0], 0x40);
        assert_eq!(params.ordered()[2][0], 0x44);
        assert_eq!(sample.avcc, [0, 0, 0, 4, 0x26, 0x01, 0xaf, 0x08]);
    }

    #[test]
    fn trailing_zero_padding_is_not_part_of_the_nal() {
        let mut d = Vec::new();
        d.extend(SC4);
        d.extend([0x41, 0x9a, 0x00, 0x00]);
        d.extend(SC4);
        d.extend([0x41, 0x9b]);
        let nals: Vec<&[u8]> = nal_units(&d).collect();
        assert_eq!(nals, vec![&[0x41, 0x9a][..], &[0x41, 0x9b][..]]);
    }
}

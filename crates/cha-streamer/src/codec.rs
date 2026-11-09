//! The codecs a session can ask for: NVENC's (the hardware tier, every
//! transport) and PyroWave (the LAN tier, WebTransport only, plan §3.2).

use cha_nvenc::Codec;
use cha_pyrowave::Chroma;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    Hw(Codec),
    /// The same at 10 bits a sample (HEVC Main 10, AV1 10-bit), SDR for now
    /// (docs/plans/hdr-and-vrr.md, step 6). Offered only when `--codecs`
    /// names it, never over WebRTC.
    Hw10(Codec),
    PyroWave(Chroma),
}

impl VideoCodec {
    pub fn name(&self) -> &'static str {
        match self {
            VideoCodec::Hw(c) => c.name(),
            VideoCodec::Hw10(Codec::Hevc) => "hevc-10",
            VideoCodec::Hw10(Codec::Av1) => "av1-10",
            VideoCodec::Hw10(Codec::H264) => "h264-10",
            VideoCodec::PyroWave(Chroma::Yuv420) => "pyrowave420",
            VideoCodec::PyroWave(Chroma::Yuv444) => "pyrowave444",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "pyrowave420" => Some(VideoCodec::PyroWave(Chroma::Yuv420)),
            "pyrowave444" => Some(VideoCodec::PyroWave(Chroma::Yuv444)),
            "hevc-10" => Some(VideoCodec::Hw10(Codec::Hevc)),
            "av1-10" => Some(VideoCodec::Hw10(Codec::Av1)),
            other => Codec::from_name(other).map(VideoCodec::Hw),
        }
    }

    /// The hardware codec, at either depth (`None` for PyroWave).
    pub fn hw(&self) -> Option<Codec> {
        match self {
            VideoCodec::Hw(c) | VideoCodec::Hw10(c) => Some(*c),
            VideoCodec::PyroWave(_) => None,
        }
    }

    /// 10 bits a sample.
    pub fn ten_bit(&self) -> bool {
        matches!(self, VideoCodec::Hw10(_))
    }

    /// The WebCodecs codec string the page configures its decoder with.
    pub fn codec_string(&self) -> &'static str {
        match self {
            VideoCodec::Hw(Codec::H264) => "avc1.640033",
            VideoCodec::Hw(Codec::Hevc) => "hev1.1.6.L153.B0",
            VideoCodec::Hw(Codec::Av1) => "av01.0.13M.08",
            // Main 10 at level 5.1; AV1 Main profile, level 5.1, 10 bits.
            VideoCodec::Hw10(Codec::Hevc) => "hev1.2.4.L153.B0",
            VideoCodec::Hw10(Codec::Av1) => "av01.0.13M.10",
            VideoCodec::Hw10(Codec::H264) => "avc1.6e0033",
            VideoCodec::PyroWave(_) => "pyrowave",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for name in ["h264", "hevc", "av1", "pyrowave420", "pyrowave444"] {
            assert_eq!(VideoCodec::from_name(name).unwrap().name(), name);
        }
        assert!(VideoCodec::from_name("vp9").is_none());
        for name in ["hevc-10", "av1-10"] {
            let codec = VideoCodec::from_name(name).unwrap();
            assert_eq!(codec.name(), name);
            assert!(codec.ten_bit());
        }
        // No 10-bit H.264: browsers don't decode it.
        assert!(VideoCodec::from_name("h264-10").is_none());
        assert_eq!(
            VideoCodec::from_name("hevc-10").unwrap().codec_string(),
            "hev1.2.4.L153.B0"
        );
        assert_eq!(VideoCodec::Hw(Codec::Av1).codec_string(), "av01.0.13M.08");
        assert!(VideoCodec::from_name("pyrowave420").unwrap().hw().is_none());
    }
}

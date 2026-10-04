//! The codecs a session can ask for: NVENC's (the hardware tier, every
//! transport) and PyroWave (the LAN tier, WebTransport only, plan §3.2).

use cha_nvenc::Codec;
use cha_pyrowave::Chroma;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    Hw(Codec),
    PyroWave(Chroma),
}

impl VideoCodec {
    pub fn name(&self) -> &'static str {
        match self {
            VideoCodec::Hw(c) => c.name(),
            VideoCodec::PyroWave(Chroma::Yuv420) => "pyrowave420",
            VideoCodec::PyroWave(Chroma::Yuv444) => "pyrowave444",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "pyrowave420" => Some(VideoCodec::PyroWave(Chroma::Yuv420)),
            "pyrowave444" => Some(VideoCodec::PyroWave(Chroma::Yuv444)),
            other => Codec::from_name(other).map(VideoCodec::Hw),
        }
    }

    /// The NVENC codec, for transports that only carry those (WebRTC).
    pub fn hw(&self) -> Option<Codec> {
        match self {
            VideoCodec::Hw(c) => Some(*c),
            VideoCodec::PyroWave(_) => None,
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
        assert!(VideoCodec::from_name("pyrowave420").unwrap().hw().is_none());
    }
}

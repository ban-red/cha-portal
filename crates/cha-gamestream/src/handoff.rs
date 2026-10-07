//! What the two halves of a GameStream host share: the description of one
//! stream, and the serializable record the front hands to the media side.
//!
//! The front (nvhttp, pairing, RTSP) learns everything about a stream from
//! the client's `/launch` and RTSP `ANNOUNCE`. It packs that into a
//! [`SessionHandoff`] and gives it to [`Directory::start_media`], which may
//! run the media half (control, video, audio) in the same process or another
//! one. Nothing here depends on either half.
//!
//! [`Directory::start_media`]: crate::directory::Directory::start_media

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// A paired client, by the SHA-256 of its certificate (lower-case hex). This
/// is the identity every authorization decision uses; the `uniqueid` a client
/// states is only a label.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClientId(pub String);

impl ClientId {
    pub fn fingerprint(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Chroma {
    #[default]
    Yuv420,
    Yuv444,
}

/// What a backend can encode.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub codecs: Vec<VideoCodec>,
    pub hdr: bool,
    pub yuv444: bool,
}

impl Capabilities {
    /// What stops a stream of `params` from being served, if anything.
    pub fn unsupported(&self, params: &StreamParams) -> Option<String> {
        if !self.codecs.contains(&params.codec) {
            return Some(format!("codec {:?}", params.codec));
        }
        if params.hdr && !self.hdr {
            return Some("HDR".into());
        }
        if params.chroma == Chroma::Yuv444 && !self.yuv444 {
            return Some("4:4:4".into());
        }
        None
    }
}

/// The ports of one session's media half, as the client is told them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPorts {
    pub video: u16,
    pub control: u16,
    pub audio: u16,
}

/// How the audio is laid out: an Opus multistream configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioParams {
    pub channels: u8,
    /// Windows speaker mask the client gave (0x3 for stereo).
    pub channel_mask: u32,
    pub high_quality: bool,
    pub streams: u8,
    pub coupled_streams: u8,
    /// Channel mapping, `channels` entries.
    pub mapping: Vec<u8>,
    /// What the client asked for per packet, in milliseconds (usually 5).
    pub packet_duration_ms: u32,
    /// Bitrate the layout is meant for, bits per second.
    pub bitrate: u32,
}

/// Everything a media backend needs to know about one stream; nothing about
/// the wire. This is what [`MediaBackend::start`] gets.
///
/// [`MediaBackend::start`]: crate::backend::MediaBackend::start
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamParams {
    pub client: ClientId,
    /// Address of the client whose session this is. Video and audio
    /// `PING`s from any other address are ignored.
    pub client_ip: IpAddr,
    pub app_id: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub codec: VideoCodec,
    pub hdr: bool,
    pub chroma: Chroma,
    pub full_range: bool,
    pub max_ref_frames: u32,
    /// Bytes of the client's video packets: header and payload, before FEC
    /// and encryption add theirs.
    pub packet_size: usize,
    /// Share of parity packets among data packets, percent.
    pub fec_percent: u8,
    pub min_fec_packets: u32,
    pub audio: AudioParams,
}

/// The key the client gave in `/launch` (`rikey`) and its id (`rikeyid`).
/// It encrypts control messages, and video and audio when those are on.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionKeys {
    pub key: [u8; 16],
    pub key_id: i64,
}

impl std::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionKeys")
            .field("key", &"<redacted>")
            .field("key_id", &self.key_id)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Encryption {
    /// Control messages are always encrypted (the host refuses clients that
    /// don't); kept in the record so the media half need not assume.
    pub control: bool,
    pub video: bool,
    pub audio: bool,
}

/// Everything the media half needs to serve one session. Serializable so it
/// can cross a process boundary; it holds the session's key, so it travels
/// only over a channel the two halves trust.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionHandoff {
    /// The front's number for the session, unique for the host's life.
    pub session_id: u64,
    pub keys: SessionKeys,
    pub encryption: Encryption,
    /// The ENet connect data the control peer must present, which the front
    /// told the client in RTSP `SETUP` (`X-SS-Connect-Data`).
    pub control_connect_data: u32,
    /// The payload the client's Sunshine-style `PING`s must carry
    /// (`X-SS-Ping-Payload`); legacy `PING`s are accepted from the client's
    /// address too.
    pub ping_payload: [u8; 16],
    pub params: StreamParams,
}

/// One Opus multistream layout the host offers in `DESCRIBE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpusLayout {
    pub channels: u8,
    pub streams: u8,
    pub coupled_streams: u8,
    pub mapping: [u8; 8],
    pub high_quality: bool,
    pub bitrate: u32,
}

// Ported from Moonshine (session/stream/audio/mod.rs), which matches Sunshine's table.
/// The six layouts, in the order `DESCRIBE` lists them: stereo, 5.1, 7.1, each
/// normal then high quality.
pub const OPUS_LAYOUTS: [OpusLayout; 6] = [
    OpusLayout {
        channels: 2,
        streams: 1,
        coupled_streams: 1,
        mapping: [0, 1, 0, 0, 0, 0, 0, 0],
        high_quality: false,
        bitrate: 96_000,
    },
    OpusLayout {
        channels: 2,
        streams: 1,
        coupled_streams: 1,
        mapping: [0, 1, 0, 0, 0, 0, 0, 0],
        high_quality: true,
        bitrate: 512_000,
    },
    OpusLayout {
        channels: 6,
        streams: 4,
        coupled_streams: 2,
        mapping: [0, 1, 4, 5, 2, 3, 0, 0],
        high_quality: false,
        bitrate: 256_000,
    },
    OpusLayout {
        channels: 6,
        streams: 6,
        coupled_streams: 0,
        mapping: [0, 1, 2, 3, 4, 5, 0, 0],
        high_quality: true,
        bitrate: 1_536_000,
    },
    OpusLayout {
        channels: 8,
        streams: 5,
        coupled_streams: 3,
        mapping: [0, 1, 4, 5, 6, 7, 2, 3],
        high_quality: false,
        bitrate: 450_000,
    },
    OpusLayout {
        channels: 8,
        streams: 8,
        coupled_streams: 0,
        mapping: [0, 1, 2, 3, 4, 5, 6, 7],
        high_quality: true,
        bitrate: 2_048_000,
    },
];

impl AudioParams {
    /// The layout for what the client asked: 2, 6 or 8 channels (anything
    /// else is stereo), normal or high quality.
    pub fn select(
        channels: u8,
        channel_mask: u32,
        high_quality: bool,
        packet_duration_ms: u32,
    ) -> Self {
        let channels = match channels {
            6 | 8 => channels,
            _ => 2,
        };
        let layout = OPUS_LAYOUTS
            .iter()
            .find(|l| l.channels == channels && l.high_quality == high_quality)
            .unwrap_or(&OPUS_LAYOUTS[0]);
        Self {
            channels,
            channel_mask: if channels == 2 && channel_mask == 0 {
                0x3
            } else {
                channel_mask
            },
            high_quality,
            streams: layout.streams,
            coupled_streams: layout.coupled_streams,
            mapping: layout.mapping[..channels as usize].to_vec(),
            packet_duration_ms,
            bitrate: layout.bitrate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handoff() -> SessionHandoff {
        SessionHandoff {
            session_id: 3,
            keys: SessionKeys {
                key: [7; 16],
                key_id: -12,
            },
            encryption: Encryption {
                control: true,
                video: true,
                audio: false,
            },
            control_connect_data: 0xDEAD_BEEF,
            ping_payload: *b"0123456789ABCDEF",
            params: StreamParams {
                client: ClientId("ab".repeat(32)),
                client_ip: "fe80::1".parse().unwrap(),
                app_id: 9,
                width: 2560,
                height: 1440,
                fps: 60,
                bitrate_bps: 40_000_000,
                codec: VideoCodec::Av1,
                hdr: true,
                chroma: Chroma::Yuv444,
                full_range: true,
                max_ref_frames: 4,
                packet_size: 1392,
                fec_percent: 20,
                min_fec_packets: 2,
                audio: AudioParams::select(6, 0x3F, false, 5),
            },
        }
    }

    /// The handoff crosses a process boundary as JSON (or anything serde speaks) and arrives whole.
    #[test]
    fn a_handoff_survives_serialization() {
        let h = handoff();
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(serde_json::from_str::<SessionHandoff>(&json).unwrap(), h);
    }

    #[test]
    fn keys_stay_out_of_debug_output() {
        let text = format!("{:?}", handoff());
        assert!(
            text.contains("<redacted>") && !text.contains("[7, 7"),
            "{text}"
        );
    }

    #[test]
    fn audio_layouts_follow_the_request() {
        let stereo = AudioParams::select(2, 0, false, 10);
        assert_eq!(
            (
                stereo.channels,
                stereo.streams,
                stereo.coupled_streams,
                stereo.channel_mask,
                stereo.bitrate
            ),
            (2, 1, 1, 3, 96_000)
        );
        let high71 = AudioParams::select(8, 0x63F, true, 5);
        assert_eq!(
            (
                high71.streams,
                high71.coupled_streams,
                high71.mapping.len(),
                high71.bitrate
            ),
            (8, 0, 8, 2_048_000)
        );
        let normal51 = AudioParams::select(6, 0x3F, false, 5);
        assert_eq!(
            (normal51.streams, normal51.coupled_streams, normal51.mapping),
            (4, 2, vec![0, 1, 4, 5, 2, 3])
        );
        assert_eq!(
            AudioParams::select(3, 0, true, 5).channels,
            2,
            "anything else is stereo"
        );
    }

    #[test]
    fn capabilities_say_what_is_unsupported() {
        let caps = Capabilities {
            codecs: vec![VideoCodec::H264],
            hdr: false,
            yuv444: false,
        };
        let mut p = handoff().params;
        assert_eq!(caps.unsupported(&p).as_deref(), Some("codec Av1"));
        p.codec = VideoCodec::H264;
        assert_eq!(caps.unsupported(&p).as_deref(), Some("HDR"));
        p.hdr = false;
        assert_eq!(caps.unsupported(&p).as_deref(), Some("4:4:4"));
        p.chroma = Chroma::Yuv420;
        assert_eq!(caps.unsupported(&p), None);
    }
}

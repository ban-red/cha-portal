// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: no sdp-types dependency (the SDP Moonlight sends is a list of `a=key:value` lines), DESCRIBE follows
// the host's capabilities, ANNOUNCE is validated into StreamParams with typed errors.

//! The SDP of RTSP: the host's `DESCRIBE` answer, and the client's `ANNOUNCE`
//! parsed into what it chose.

use std::collections::HashMap;
use std::fmt::Write;

use crate::config::HostConfig;
use crate::handoff::{AudioParams, Chroma, Encryption, OPUS_LAYOUTS, StreamParams, VideoCodec};

/// `x-ss-general.encryptionSupported` and `encryptionEnabled` bits.
pub(crate) mod enc {
    pub const CONTROL_V2: u32 = 0x01;
    pub const VIDEO: u32 = 0x02;
    pub const AUDIO: u32 = 0x04;
}

/// What the host answers `DESCRIBE` with: its feature flags, encryption
/// and the video and audio formats it offers.
pub(crate) fn describe(config: &HostConfig) -> String {
    let mut s = String::new();
    // Pen and touch events (1) and controller touch events (2) are understood.
    let _ = writeln!(s, "a=x-ss-general.featureFlags:3");
    let supported = enc::CONTROL_V2
        | enc::AUDIO
        | if config.video_encryption {
            enc::VIDEO
        } else {
            0
        };
    let _ = writeln!(s, "a=x-ss-general.encryptionSupported:{supported}");
    // Control messages must be encrypted: it is what authenticates the control peer.
    let _ = writeln!(s, "a=x-ss-general.encryptionRequested:{}", enc::CONTROL_V2);
    if config.capabilities.codecs.contains(&VideoCodec::Hevc) {
        // How a client learns HEVC is on offer: the base64 of an HEVC VPS prefix.
        let _ = writeln!(s, "sprop-parameter-sets=AAAAAU");
    }
    let _ = writeln!(s, "a=x-nv-video[0].refPicInvalidation:1");
    if config.capabilities.codecs.contains(&VideoCodec::Av1) {
        let _ = writeln!(s, "a=rtpmap:98 AV1/90000");
    }
    let _ = writeln!(s, "a=fmtp:96 packetization-mode=1");
    // One line per Opus layout. Values are single digits run together, as
    // moonlight-common-c reads them.
    for (i, layout) in OPUS_LAYOUTS.iter().enumerate() {
        let channels = layout.channels as usize;
        let mut mapping = layout.mapping;
        // GFE advertises normal-quality surround with the LFE rotated to the
        // end; Moonlight undoes it, so the rotation is applied here.
        if (i == 2 || i == 4) && channels >= 6 {
            mapping[3..channels].rotate_left(1);
        }
        let mut params = format!(
            "{}{}{}",
            layout.channels, layout.streams, layout.coupled_streams
        );
        for m in &mapping[..channels] {
            let _ = write!(params, "{m}");
        }
        let _ = writeln!(s, "a=fmtp:97 surround-params={params}");
    }
    s
}

/// The `a=key:value` attributes of an SDP body.
pub(crate) struct Attributes(HashMap<String, String>);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum AnnounceError {
    #[error("the SDP has no {0}")]
    Missing(&'static str),
    #[error("{0} is not usable")]
    Invalid(&'static str),
    #[error("{0} isn't offered by this host")]
    Unsupported(&'static str),
    #[error("control encryption is required")]
    ControlNotEncrypted,
}

const MAX_ATTRIBUTES: usize = 512;

impl Attributes {
    /// The first value of each attribute; lines that aren't `a=key:value` are
    /// skipped, and so are lines past a sane count.
    pub fn parse(body: &str) -> Self {
        let mut map = HashMap::new();
        for line in body.lines().take(MAX_ATTRIBUTES * 2) {
            let Some(attr) = line.trim().strip_prefix("a=") else {
                continue;
            };
            if let Some((k, v)) = attr.split_once(':')
                && map.len() < MAX_ATTRIBUTES
            {
                map.entry(k.trim().to_owned())
                    .or_insert_with(|| v.trim().to_owned());
            }
        }
        Self(map)
    }

    fn get<T: std::str::FromStr>(&self, key: &'static str) -> Result<T, AnnounceError> {
        self.0
            .get(key)
            .ok_or(AnnounceError::Missing(key))?
            .parse()
            .map_err(|_| AnnounceError::Invalid(key))
    }

    fn opt<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.0.get(key)?.parse().ok()
    }
}

/// Who the stream is for: filled in from the session, not the SDP.
pub(crate) struct Peer {
    pub client: crate::handoff::ClientId,
    pub client_ip: std::net::IpAddr,
    pub app_id: u32,
    /// What `/launch` said about audio, when the SDP doesn't say.
    pub launch_surround_audio_info: u32,
}

/// Turns the client's choices into stream parameters, or says why not.
pub(crate) fn parse_announce(
    body: &str,
    config: &HostConfig,
    peer: Peer,
) -> Result<(StreamParams, Encryption), AnnounceError> {
    let a = Attributes::parse(body);
    let width: u32 = a.get("x-nv-video[0].clientViewportWd")?;
    let height: u32 = a.get("x-nv-video[0].clientViewportHt")?;
    let fps: u32 = a.get("x-nv-video[0].maxFPS")?;
    if !(64..=16_384).contains(&width) || !(64..=16_384).contains(&height) {
        return Err(AnnounceError::Invalid("the picture size"));
    }
    if !(1..=1000).contains(&fps) {
        return Err(AnnounceError::Invalid("x-nv-video[0].maxFPS"));
    }

    let requested_packet: usize = a.get("x-nv-video[0].packetSize")?;
    if !(256..=16_384).contains(&requested_packet) {
        return Err(AnnounceError::Invalid("x-nv-video[0].packetSize"));
    }
    // A client asks only for what it believes it can receive: a request under
    // the cap stands, one over it is lowered.
    let packet_size = match config.max_packet_size {
        0 => requested_packet,
        cap if cap < 200 => requested_packet,
        cap => requested_packet.min(cap),
    };

    let kbps: u64 = a.get("x-ml-video.configuredBitrateKbps")?;
    if kbps == 0 || kbps > 4_000_000 {
        return Err(AnnounceError::Invalid("x-ml-video.configuredBitrateKbps"));
    }

    let codec = match a.get::<u32>("x-nv-vqos[0].bitStreamFormat")? {
        0 => VideoCodec::H264,
        1 => VideoCodec::Hevc,
        2 => VideoCodec::Av1,
        _ => return Err(AnnounceError::Invalid("x-nv-vqos[0].bitStreamFormat")),
    };
    if !config.capabilities.codecs.contains(&codec) {
        return Err(AnnounceError::Unsupported("that video codec"));
    }
    let hdr = a.opt::<u32>("x-nv-video[0].dynamicRangeMode").unwrap_or(0) == 1;
    if hdr && !config.capabilities.hdr {
        return Err(AnnounceError::Unsupported("HDR"));
    }
    let chroma = if a
        .opt::<u32>("x-ss-video[0].chromaSamplingType")
        .unwrap_or(0)
        == 1
    {
        Chroma::Yuv444
    } else {
        Chroma::Yuv420
    };
    if chroma == Chroma::Yuv444 && !config.capabilities.yuv444 {
        return Err(AnnounceError::Unsupported("4:4:4"));
    }
    // Bit 0 of the colour-space mode: full-range luma. The rest picks an SDR
    // colour space, of which only Rec. 709 is encoded.
    let full_range = a.opt::<u32>("x-nv-video[0].encoderCscMode").unwrap_or(0) & 1 != 0;

    let encryption_enabled = a.opt::<u32>("x-ss-general.encryptionEnabled").unwrap_or(0);
    if encryption_enabled & enc::CONTROL_V2 == 0 {
        return Err(AnnounceError::ControlNotEncrypted);
    }
    if encryption_enabled & enc::VIDEO != 0 && !config.video_encryption {
        return Err(AnnounceError::Unsupported("video encryption"));
    }

    let packet_duration: u32 = a.get("x-nv-aqos.packetDuration")?;
    let surround = a.opt::<u32>("x-nv-audio.surround.enable").unwrap_or(0) != 0;
    let (channels, mask) = if surround {
        (
            a.opt::<u8>("x-nv-audio.surround.numChannels").unwrap_or(2),
            a.opt::<u32>("x-nv-audio.surround.channelMask")
                .unwrap_or(0x3),
        )
    } else {
        // What /launch said: the channel mask above 16 bits, the count below.
        (
            (peer.launch_surround_audio_info & 0xFFFF) as u8,
            peer.launch_surround_audio_info >> 16,
        )
    };
    let high_quality = a
        .opt::<u32>("x-nv-audio.surround.AudioQuality")
        .unwrap_or(1)
        != 0;
    let audio = AudioParams::select(channels, mask, high_quality, packet_duration);

    let params = StreamParams {
        client: peer.client,
        client_ip: peer.client_ip,
        app_id: peer.app_id,
        width,
        height,
        fps,
        bitrate_bps: (kbps * 1000).min(u64::from(u32::MAX)) as u32,
        codec,
        hdr,
        chroma,
        full_range,
        max_ref_frames: a.opt("x-nv-video[0].maxNumReferenceFrames").unwrap_or(1),
        packet_size,
        fec_percent: config.fec_percent,
        min_fec_packets: a.opt("x-nv-vqos[0].fec.minRequiredFecPackets").unwrap_or(0),
        audio,
    };
    let encryption = Encryption {
        control: true,
        video: encryption_enabled & enc::VIDEO != 0,
        audio: encryption_enabled & enc::AUDIO != 0,
    };
    Ok((params, encryption))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::handoff::{Capabilities, ClientId};

    pub(crate) fn config() -> HostConfig {
        let mut c = HostConfig::new("Test", "ABCDEF0123456789ABCDEF0123456789");
        c.capabilities = Capabilities {
            codecs: vec![VideoCodec::H264, VideoCodec::Hevc],
            hdr: true,
            yuv444: false,
        };
        // Tests opt in to video encryption where they need it.
        c.video_encryption = false;
        c
    }

    fn peer() -> Peer {
        Peer {
            client: ClientId("ab".into()),
            client_ip: "10.0.0.2".parse().unwrap(),
            app_id: 4,
            launch_surround_audio_info: 0x30002,
        }
    }

    pub(crate) fn sdp(extra: &[(&str, &str)]) -> String {
        let mut base: Vec<(&str, &str)> = vec![
            ("x-nv-video[0].clientViewportWd", "2560"),
            ("x-nv-video[0].clientViewportHt", "1440"),
            ("x-nv-video[0].maxFPS", "60"),
            ("x-nv-video[0].packetSize", "1392"),
            ("x-ml-video.configuredBitrateKbps", "40000"),
            ("x-nv-vqos[0].fec.minRequiredFecPackets", "2"),
            ("x-nv-vqos[0].bitStreamFormat", "1"),
            ("x-nv-aqos.packetDuration", "5"),
            ("x-ss-general.encryptionEnabled", "5"),
        ];
        for (k, v) in extra {
            base.retain(|(bk, _)| bk != k);
            base.push((k, v));
        }
        let mut s = String::from("v=0\r\ns=NVIDIA Streaming Client\r\n");
        for (k, v) in base {
            s += &format!("a={k}:{v} \r\n");
        }
        s
    }

    #[test]
    fn describe_lists_what_the_host_offers() {
        let text = describe(&config());
        assert!(text.contains("a=x-ss-general.encryptionSupported:5\n"));
        assert!(text.contains("a=x-ss-general.encryptionRequested:1\n"));
        let host = HostConfig::new("Test", "ABCDEF0123456789ABCDEF0123456789");
        assert!(
            describe(&host).contains("a=x-ss-general.encryptionSupported:7\n"),
            "a host offers video encryption by default"
        );
        assert!(
            text.contains("sprop-parameter-sets=AAAAAU\n"),
            "HEVC on offer"
        );
        assert!(!text.contains("AV1/90000"));
        assert!(text.contains("a=x-nv-video[0].refPicInvalidation:1\n"));
        let mut c = config();
        c.video_encryption = true;
        c.capabilities.codecs = vec![VideoCodec::H264, VideoCodec::Av1];
        let text = describe(&c);
        assert!(text.contains("encryptionSupported:7\n"));
        assert!(text.contains("a=rtpmap:98 AV1/90000\n"));
        assert!(!text.contains("sprop-parameter-sets"));
    }

    #[test]
    fn surround_lines_match_moonlights_reading() {
        let text = describe(&config());
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| l.starts_with("a=fmtp:97"))
            .collect();
        assert_eq!(lines.len(), 6);
        assert_eq!(lines[0], "a=fmtp:97 surround-params=21101");
        // 5.1 normal: 6 channels, 4 streams, 2 coupled; mapping 0 1 4 5 2 3 with
        // entries from index 3 rotated left: 0 1 4 2 3 5.
        assert_eq!(lines[2], "a=fmtp:97 surround-params=642014235");
        assert_eq!(lines[3], "a=fmtp:97 surround-params=660012345");
    }

    #[test]
    fn announce_parses_the_clients_choices() {
        let (p, e) = parse_announce(&sdp(&[]), &config(), peer()).unwrap();
        assert_eq!(
            (p.width, p.height, p.fps, p.packet_size, p.bitrate_bps),
            (2560, 1440, 60, 1392, 40_000_000)
        );
        assert_eq!(p.codec, VideoCodec::Hevc);
        assert_eq!((p.min_fec_packets, p.fec_percent, p.app_id), (2, 20, 4));
        assert_eq!(p.audio.channels, 2);
        assert_eq!(p.audio.packet_duration_ms, 5);
        assert_eq!(
            e,
            Encryption {
                control: true,
                video: false,
                audio: true
            }
        );
        assert!(!p.hdr && !p.full_range);
    }

    #[test]
    fn announce_honours_hdr_surround_and_range() {
        let s = sdp(&[
            ("x-nv-video[0].dynamicRangeMode", "1"),
            ("x-nv-video[0].encoderCscMode", "1"),
            ("x-nv-audio.surround.enable", "1"),
            ("x-nv-audio.surround.numChannels", "6"),
            ("x-nv-audio.surround.channelMask", "63"),
            ("x-nv-audio.surround.AudioQuality", "0"),
        ]);
        let (p, _) = parse_announce(&s, &config(), peer()).unwrap();
        assert!(p.hdr && p.full_range);
        assert_eq!(
            (
                p.audio.channels,
                p.audio.streams,
                p.audio.coupled_streams,
                p.audio.channel_mask
            ),
            (6, 4, 2, 63)
        );
        assert!(!p.audio.high_quality);
    }

    #[test]
    fn audio_falls_back_to_what_launch_said() {
        let mut who = peer();
        who.launch_surround_audio_info = (0x3F << 16) | 6;
        let (p, _) =
            parse_announce(&sdp(&[("x-nv-aqos.packetDuration", "10")]), &config(), who).unwrap();
        assert_eq!(
            (
                p.audio.channels,
                p.audio.channel_mask,
                p.audio.packet_duration_ms
            ),
            (6, 0x3F, 10)
        );
    }

    #[test]
    fn the_packet_size_cap_only_lowers() {
        let mut c = config();
        c.max_packet_size = 1200;
        assert_eq!(
            parse_announce(&sdp(&[]), &c, peer()).unwrap().0.packet_size,
            1200
        );
        assert_eq!(
            parse_announce(&sdp(&[("x-nv-video[0].packetSize", "1024")]), &c, peer())
                .unwrap()
                .0
                .packet_size,
            1024
        );
        c.max_packet_size = 50;
        assert_eq!(
            parse_announce(&sdp(&[]), &c, peer()).unwrap().0.packet_size,
            1392
        );
    }

    #[test]
    fn announce_refuses_what_it_cannot_serve() {
        let c = config();
        let bad = |extra: &[(&str, &str)]| parse_announce(&sdp(extra), &c, peer()).unwrap_err();
        assert_eq!(
            bad(&[("x-nv-vqos[0].bitStreamFormat", "2")]),
            AnnounceError::Unsupported("that video codec")
        );
        assert_eq!(
            bad(&[("x-nv-vqos[0].bitStreamFormat", "9")]),
            AnnounceError::Invalid("x-nv-vqos[0].bitStreamFormat")
        );
        assert_eq!(
            bad(&[("x-ss-video[0].chromaSamplingType", "1")]),
            AnnounceError::Unsupported("4:4:4")
        );
        assert_eq!(
            bad(&[("x-ss-general.encryptionEnabled", "4")]),
            AnnounceError::ControlNotEncrypted
        );
        assert_eq!(
            bad(&[("x-ss-general.encryptionEnabled", "7")]),
            AnnounceError::Unsupported("video encryption")
        );
        assert_eq!(
            bad(&[("x-nv-video[0].maxFPS", "0")]),
            AnnounceError::Invalid("x-nv-video[0].maxFPS")
        );
        assert_eq!(
            bad(&[("x-nv-video[0].clientViewportWd", "5")]),
            AnnounceError::Invalid("the picture size")
        );
        assert_eq!(
            bad(&[("x-nv-video[0].packetSize", "10")]),
            AnnounceError::Invalid("x-nv-video[0].packetSize")
        );
        assert_eq!(
            bad(&[("x-ml-video.configuredBitrateKbps", "x")]),
            AnnounceError::Invalid("x-ml-video.configuredBitrateKbps")
        );
        assert_eq!(
            parse_announce("v=0\r\n", &c, peer()).unwrap_err(),
            AnnounceError::Missing("x-nv-video[0].clientViewportWd")
        );
        let mut no_hdr = config();
        no_hdr.capabilities.hdr = false;
        assert_eq!(
            parse_announce(
                &sdp(&[("x-nv-video[0].dynamicRangeMode", "1")]),
                &no_hdr,
                peer()
            )
            .unwrap_err(),
            AnnounceError::Unsupported("HDR")
        );
    }

    /// Whatever text arrives as an ANNOUNCE body, it is parsed or refused; nothing panics.
    #[test]
    fn hostile_bodies_never_panic() {
        let mut seed = 0x0BAD_C0DE_FEED_FACEu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let atoms = [
            "a=",
            ":",
            "\r\n",
            "x-nv-video[0].maxFPS",
            "99999999999999999999",
            "-1",
            "x-nv-video[0].packetSize",
            "0",
            "é",
            "x-ss-general.encryptionEnabled",
        ];
        for _ in 0..20_000 {
            let mut body = sdp(&[]);
            for _ in 0..(next() % 10) {
                body += atoms[(next() % atoms.len() as u64) as usize];
            }
            let _ = parse_announce(&body, &config(), peer());
            let cut: String = body
                .chars()
                .skip(next() as usize % body.chars().count())
                .collect();
            let _ = parse_announce(&cut, &config(), peer());
        }
    }

    #[test]
    fn attributes_ignore_noise_and_keep_the_first() {
        let a = Attributes::parse(
            "v=0\r\na=k:1\r\na=k:2\r\nnot an attribute\r\na=flag\r\na=sp ace : v \r\n",
        );
        assert_eq!(a.get::<u32>("k"), Ok(1));
        assert_eq!(a.opt::<String>("sp ace").as_deref(), Some("v"));
        assert_eq!(a.opt::<u32>("flag"), None);
    }
}

//! The SDP of RTSP, from the client's side: reading the host's `DESCRIBE`
//! answer and writing our `ANNOUNCE`. The attribute names and the values
//! hosts expect are interop constants; the code is written from what our
//! host's `front::rtsp::sdp` writes and accepts. Where tuning values are
//! kept as other clients use them, the comments say so. Only hosts at app
//! version 7.1.431 and later are spoken to.

use std::fmt::Write;

use super::ClientError;
use super::launch::{ColorSpace, StreamRequest};
use crate::front::rtsp::sdp::enc;
use crate::handoff::{AudioParams, Chroma, OPUS_LAYOUTS, OpusLayout, VideoCodec};

/// How a host's `DESCRIBE` says it offers a codec: HEVC by a base64 VPS
/// prefix, AV1 by its rtpmap (the markers our own host writes).
const HEVC_MARKER: &str = "sprop-parameter-sets=AAAAAU";
const AV1_MARKER: &str = "AV1/90000";

/// Above this video bitrate (kbps) surround audio asks for the high-quality
/// layout (the threshold Moonlight clients use).
const HIGH_AUDIO_BITRATE_KBPS: u32 = 15_000;

/// An Opus multistream layout, as `DESCRIBE` offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OpusConfig {
    pub streams: u8,
    pub coupled: u8,
    pub mapping: Vec<u8>,
}

/// What the host's `DESCRIBE` answer says.
#[derive(Debug, Default)]
pub(super) struct Describe {
    pub av1: bool,
    pub hevc: bool,
    pub ref_invalidation: bool,
    pub enc_supported: u32,
    pub enc_requested: u32,
    /// `(normal, high quality)` for the channel count we asked for.
    pub normal: Option<OpusConfig>,
    pub high: Option<OpusConfig>,
}

/// A number as hosts write them in headers: decimal, `0x` hex or leading-zero
/// octal, up to the first other character.
pub(super) fn c_uint(s: &str) -> Option<u32> {
    let s = s.trim();
    let (digits, radix) = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => (hex, 16),
        None if s.len() > 1 && s.starts_with('0') => (&s[1..], 8),
        None => (s, 10),
    };
    let end = digits
        .find(|c: char| !c.is_digit(radix))
        .unwrap_or(digits.len());
    u32::from_str_radix(&digits[..end], radix).ok()
}

fn attribute<'a>(payload: &'a str, name: &str) -> Option<&'a str> {
    payload.lines().find_map(|line| {
        let (key, value) = line.trim().strip_prefix("a=")?.split_once(':')?;
        (key.trim() == name).then_some(value.trim())
    })
}

/// What hosts without any `surround-params` line offer for 5.1: 4 streams,
/// 2 coupled, in the wire order [`wire_order`] gives for the table's 5.1.
/// An interop constant, the layout such hosts use.
const LEGACY_5_1: (u8, u8, [u8; 6]) = (4, 2, [0, 4, 1, 5, 2, 3]);

/// Position of the LFE channel in the order of [`OPUS_LAYOUTS`] mappings
/// (front left, front right, centre, LFE, then the rest).
const TABLE_LFE_SLOT: usize = 3;

/// One `surround-params` line, read: `channels streams coupled mapping...`,
/// every field one digit.
struct Candidate {
    channels: u8,
    config: OpusConfig,
}

fn candidate(rest: &str) -> Option<Candidate> {
    let digits: Vec<u8> = rest
        .trim()
        .bytes()
        .map(|b| b.is_ascii_digit().then(|| b - b'0'))
        .collect::<Option<_>>()?;
    let (&channels, tail) = digits.split_first()?;
    let (&streams, tail) = tail.split_first()?;
    let (&coupled, mapping) = tail.split_first()?;
    (mapping.len() == usize::from(channels)).then(|| Candidate {
        channels,
        config: OpusConfig {
            streams,
            coupled,
            mapping: mapping.to_vec(),
        },
    })
}

/// A table layout as `DESCRIBE` writes it: normal-quality surround lists the
/// LFE channel last (the table has it after the centre); every other layout
/// is written as the table has it.
fn wire_order(layout: &OpusLayout) -> Vec<u8> {
    let table = &layout.mapping[..usize::from(layout.channels)];
    if layout.high_quality || table.len() <= TABLE_LFE_SLOT {
        return table.to_vec();
    }
    let lfe = table[TABLE_LFE_SLOT];
    table
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != TABLE_LFE_SLOT)
        .map(|(_, &m)| m)
        .chain([lfe])
        .collect()
}

fn table_config(layout: &OpusLayout) -> OpusConfig {
    OpusConfig {
        streams: layout.streams,
        coupled: layout.coupled_streams,
        mapping: layout.mapping[..usize::from(layout.channels)].to_vec(),
    }
}

/// The table layout for `channels` and quality that `candidates` offer, if
/// any: same stream and coupled counts and the mapping in wire order. The
/// answer is in the table's channel order.
fn pick(candidates: &[Candidate], channels: u8, high_quality: bool) -> Option<OpusConfig> {
    let layout = OPUS_LAYOUTS
        .iter()
        .find(|l| l.channels == channels && l.high_quality == high_quality)?;
    let wire = wire_order(layout);
    candidates
        .iter()
        .any(|c| {
            c.channels == channels
                && c.config.streams == layout.streams
                && c.config.coupled == layout.coupled_streams
                && c.config.mapping == wire
        })
        .then(|| table_config(layout))
}

impl Describe {
    pub fn parse(payload: &str, channels: u8) -> Result<Self, ClientError> {
        let uint = |name| attribute(payload, name).and_then(c_uint).unwrap_or(0);
        let mut d = Describe {
            av1: payload.contains(AV1_MARKER),
            hevc: payload.contains(HEVC_MARKER),
            ref_invalidation: payload.contains("x-nv-video[0].refPicInvalidation"),
            enc_supported: uint("x-ss-general.encryptionSupported"),
            enc_requested: uint("x-ss-general.encryptionRequested"),
            ..Self::default()
        };
        if channels <= 2 {
            // Stereo has no `surround-params`.
            d.normal = OPUS_LAYOUTS
                .iter()
                .find(|l| l.channels == 2 && !l.high_quality)
                .map(table_config);
            return Ok(d);
        }

        let lines: Vec<&str> = payload
            .lines()
            .filter_map(|l| l.trim().strip_prefix("a=fmtp:97 surround-params="))
            .collect();
        if lines.is_empty() {
            let (streams, coupled, mapping) = LEGACY_5_1;
            if channels == 6 {
                d.normal = Some(OpusConfig {
                    streams,
                    coupled,
                    mapping: mapping.to_vec(),
                });
                return Ok(d);
            }
            return Err(ClientError::Unsupported(format!(
                "the host offers no {channels}-channel audio"
            )));
        }
        // Lines we can't read are lines of layouts we wouldn't pick anyway.
        let candidates: Vec<Candidate> = lines.iter().filter_map(|l| candidate(l)).collect();
        d.normal = pick(&candidates, channels, false);
        d.high = pick(&candidates, channels, true);
        if d.normal.is_none() && d.high.is_none() {
            return Err(ClientError::Unsupported(format!(
                "the host offers no {channels}-channel layout we know"
            )));
        }
        Ok(d)
    }

    /// Whether the host offers `codec`.
    pub fn offers(&self, codec: VideoCodec) -> bool {
        match codec {
            VideoCodec::Av1 => self.av1,
            VideoCodec::Hevc => self.hevc,
            VideoCodec::H264 => true,
        }
    }
}

/// Channel mask Windows uses for `channels` speakers.
pub(super) fn channel_mask(channels: u8) -> u32 {
    match channels {
        6 => 0x3F,
        8 => 0x63F,
        _ => 0x3,
    }
}

/// The audio layout of a stream: the host's configuration for our channel
/// count and quality, and the bitrate the layout table gives it.
pub(super) fn audio_params(
    d: &Describe,
    channels: u8,
    high_quality: bool,
    packet_ms: u32,
) -> Result<AudioParams, ClientError> {
    let channels = match channels {
        6 | 8 => channels,
        _ => 2,
    };
    let config = if high_quality {
        d.high.clone().or_else(|| {
            // Stereo's high-quality layout is the normal one at a higher bitrate.
            (channels == 2).then(|| d.normal.clone()).flatten()
        })
    } else {
        d.normal.clone()
    }
    .ok_or_else(|| ClientError::Unsupported("the host doesn't offer that audio quality".into()))?;
    let bitrate = OPUS_LAYOUTS
        .iter()
        .find(|l| l.channels == channels && l.high_quality == high_quality)
        .map_or(0, |l| l.bitrate);
    Ok(AudioParams {
        channels,
        channel_mask: channel_mask(channels),
        high_quality,
        streams: config.streams,
        coupled_streams: config.coupled,
        mapping: config.mapping,
        packet_duration_ms: packet_ms,
        bitrate,
    })
}

/// Whether to ask for the high-quality audio layout: by default only for
/// surround above 15 Mbps when the host offers one (the threshold Moonlight
/// clients use); a request can force either way.
pub(super) fn wants_high_quality(request: &StreamRequest, d: &Describe) -> bool {
    match request.high_quality_audio {
        Some(wanted) => wanted && (request.audio_channels <= 2 || d.high.is_some()),
        None => {
            request.bitrate_kbps >= HIGH_AUDIO_BITRATE_KBPS
                && request.audio_channels > 2
                && d.high.is_some()
        }
    }
}

/// What we ask the host to encrypt, from what it supports and requires.
/// Policy as Moonlight clients behave: control encryption is always on when
/// the host supports it, and a stream kind is encrypted when the host
/// requests it, or when we want it and the host supports it.
pub(super) fn encryption_enabled(
    d: &Describe,
    request: &StreamRequest,
) -> Result<u32, ClientError> {
    use super::launch::Encrypt;
    let mut enabled = 0;
    if d.enc_supported & enc::CONTROL_V2 != 0 {
        enabled |= enc::CONTROL_V2;
    }
    for (bit, name, wish) in [
        (enc::VIDEO, "video", request.video_encryption),
        (enc::AUDIO, "audio", request.audio_encryption),
    ] {
        let supported = d.enc_supported & bit != 0;
        let requested = d.enc_requested & bit != 0;
        match wish {
            // The host's requirement stands even against our opt-out.
            Encrypt::Off if requested => enabled |= bit,
            Encrypt::Off => {}
            Encrypt::IfSupported if supported => enabled |= bit,
            Encrypt::IfSupported => {}
            Encrypt::Required if supported => enabled |= bit,
            Encrypt::Required => {
                return Err(ClientError::Unsupported(format!(
                    "the host can't encrypt {name}"
                )));
            }
        }
    }
    Ok(enabled)
}

/// What the `ANNOUNCE` says, resolved.
pub(super) struct Announce<'a> {
    pub request: &'a StreamRequest,
    pub sunshine: bool,
    pub codec: VideoCodec,
    pub hdr: bool,
    pub chroma: Chroma,
    pub encryption: u32,
    pub audio: &'a AudioParams,
    /// Packet size after encryption overhead.
    pub packet_size: usize,
    pub video_port: u16,
    pub rtsp_port: u16,
    pub address: &'a str,
    pub ip_version: &'a str,
    pub rtsp_client_version: u32,
    pub ref_invalidation: bool,
}

impl Announce<'_> {
    fn attributes(&self) -> Vec<(String, String)> {
        let r = self.request;
        let mut a: Vec<(String, String)> = Vec::new();
        let mut add = |k: &str, v: String| a.push((k.to_owned(), v));

        // Values as Moonlight clients send them (moonlight-common-c's SDP
        // generator), kept for compatibility: the attribute names and the
        // tuning values and heuristics below (bitrate scaling, the
        // low-resolution DRC table, QoS types, feature flags, reference
        // frames, rate control mode, timeout, channel masks, and the SDP
        // header and tail lines).
        //
        // Sunshine-family extensions: client feature flags (bits 0 and 1),
        // what to encrypt, chroma sampling, the configured bitrate.
        if self.sunshine {
            add("x-ml-general.featureFlags", "3".into());
            add(
                "x-ss-general.encryptionEnabled",
                self.encryption.to_string(),
            );
            add(
                "x-ss-video[0].chromaSamplingType",
                u8::from(self.chroma == Chroma::Yuv444).to_string(),
            );
            add(
                "x-ml-video.configuredBitrateKbps",
                r.bitrate_kbps.to_string(),
            );
        }

        // The picture.
        add("x-nv-video[0].clientViewportWd", r.width.to_string());
        add("x-nv-video[0].clientViewportHt", r.height.to_string());
        add("x-nv-video[0].maxFPS", r.fps.to_string());
        add(
            "x-nv-video[0].clientRefreshRateX100",
            (r.fps * 100).to_string(),
        );
        add("x-nv-video[0].packetSize", self.packet_size.to_string());
        add("x-nv-video[0].videoEncoderSlicesPerFrame", "1".into());
        match self.codec {
            VideoCodec::Av1 => add("x-nv-vqos[0].bitStreamFormat", "2".into()),
            VideoCodec::Hevc => {
                add("x-nv-clientSupportHevc", "1".into());
                add("x-nv-vqos[0].bitStreamFormat", "1".into());
            }
            VideoCodec::H264 => {
                add("x-nv-clientSupportHevc", "0".into());
                add("x-nv-vqos[0].bitStreamFormat", "0".into());
            }
        }
        add(
            "x-nv-video[0].dynamicRangeMode",
            u8::from(self.hdr).to_string(),
        );
        let color_space = if self.hdr {
            ColorSpace::Rec2020
        } else {
            r.color_space
        };
        add(
            "x-nv-video[0].encoderCscMode",
            (((color_space as u32) << 1) | u32::from(r.full_range)).to_string(),
        );
        // 0 leaves the reference frame count to the host; we send it only
        // when the host says it can invalidate lost frames.
        add(
            "x-nv-video[0].maxNumReferenceFrames",
            if self.ref_invalidation { "0" } else { "1" }.into(),
        );

        // Bitrate: 80% of the configured rate, 500 kbps less on remote
        // links, capped at 100 Mbps; initial, peak, minimum and maximum
        // all carry the same figure.
        let mut kbps = (u64::from(r.bitrate_kbps) * 80 / 100) as u32;
        if r.remote && kbps > 500 {
            kbps -= 500;
        }
        let kbps = kbps.min(100_000).to_string();
        for key in [
            "x-nv-video[0].initialBitrateKbps",
            "x-nv-video[0].initialPeakBitrateKbps",
            "x-nv-vqos[0].bw.minimumBitrateKbps",
            "x-nv-vqos[0].bw.maximumBitrateKbps",
        ] {
            add(key, kbps.clone());
        }
        add("x-nv-video[0].rateControlMode", "4".into());
        add("x-nv-video[0].timeoutLengthMs", "7000".into());
        add("x-nv-video[0].framesWithInvalidRefThreshold", "0".into());
        add("x-nv-vqos[0].fec.enable", "1".into());
        add(
            "x-nv-vqos[0].fec.minRequiredFecPackets",
            r.min_fec_packets.to_string(),
        );
        add("x-nv-vqos[0].bllFec.enable", "0".into());
        add("x-nv-vqos[0].videoQualityScoreUpdateTime", "5000".into());
        let (video_qos, audio_qos) = if r.remote { ("0", "0") } else { ("5", "4") };
        add("x-nv-vqos[0].qosTrafficType", video_qos.into());
        add("x-nv-aqos.qosTrafficType", audio_qos.into());

        // Control channel flags: 0x87, plus 0x20 when audio is encrypted;
        // "13" selects the reliable-UDP (ENet) control transport.
        let mut flags = 0x87u32;
        if self.encryption & enc::AUDIO != 0 {
            flags |= 0x20;
        }
        add("x-nv-general.featureFlags", flags.to_string());
        add("x-nv-general.useReliableUdp", "13".into());
        add("x-nv-general.enableRecoveryMode", "0".into());
        add(
            "x-nv-general.serverAddress",
            format!("rtsp://{}:{}", self.address, self.rtsp_port),
        );
        // Dynamic resolution stays off, except that pictures below 720x540
        // ask for DRC table type 2.
        if r.width < 720 || r.height < 540 {
            add("x-nv-vqos[0].drc.enable", "1".into());
            add("x-nv-vqos[0].drc.tableType", "2".into());
        } else {
            add("x-nv-vqos[0].drc.enable", "0".into());
        }

        // The audio.
        add(
            "x-nv-audio.surround.numChannels",
            self.audio.channels.to_string(),
        );
        add(
            "x-nv-audio.surround.channelMask",
            self.audio.channel_mask.to_string(),
        );
        add(
            "x-nv-audio.surround.enable",
            u8::from(self.audio.channels > 2).to_string(),
        );
        add(
            "x-nv-audio.surround.AudioQuality",
            u8::from(self.audio.high_quality).to_string(),
        );
        add(
            "x-nv-aqos.packetDuration",
            self.audio.packet_duration_ms.to_string(),
        );
        a
    }

    pub fn sdp(&self) -> String {
        let mut s = String::new();
        let _ = write!(
            s,
            "v=0\r\no=android 0 {} IN {} {}\r\ns=NVIDIA Streaming Client\r\n",
            self.rtsp_client_version, self.ip_version, self.address
        );
        for (k, v) in self.attributes() {
            let _ = write!(s, "a={k}:{v} \r\n");
        }
        let _ = write!(s, "t=0 0\r\nm=video {}  \r\n", self.video_port);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::front::launch::Encrypt;
    use crate::config::HostConfig;
    use crate::front::rtsp::sdp::{Peer, describe, parse_announce};
    use crate::handoff::{Capabilities, ClientId};

    fn host_config() -> HostConfig {
        let mut c = HostConfig::new("T", "ABCDEF0123456789ABCDEF0123456789");
        c.capabilities = Capabilities {
            codecs: vec![VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1],
            hdr: true,
            yuv444: true,
        };
        c.video_encryption = true;
        c
    }

    fn request() -> StreamRequest {
        StreamRequest::new(3, 2560, 1440, 60)
    }

    #[test]
    fn c_style_numbers() {
        assert_eq!(c_uint("7"), Some(7));
        assert_eq!(c_uint(" 0x1F "), Some(31));
        assert_eq!(c_uint("12 extra"), Some(12));
        assert_eq!(c_uint("017"), Some(15));
        assert_eq!(c_uint("0"), Some(0));
        assert_eq!(c_uint("x"), None);
        assert_eq!(c_uint(""), None);
        assert_eq!(c_uint("99999999999"), None);
    }

    #[test]
    fn describe_is_read_as_our_host_writes_it() {
        let host = describe(&host_config());
        let d = Describe::parse(&host, 2).unwrap();
        assert!(d.av1 && d.hevc && d.ref_invalidation);
        assert_eq!(d.enc_supported, 7);
        assert_eq!(d.enc_requested, 1);
        assert_eq!(d.normal.unwrap().mapping, [0, 1]);

        // 5.1: the host's normal layout (the table's) comes back after the LFE rotation is undone.
        let d = Describe::parse(&host, 6).unwrap();
        let normal = d.normal.unwrap();
        assert_eq!((normal.streams, normal.coupled), (4, 2));
        assert_eq!(normal.mapping, [0, 1, 4, 5, 2, 3]);
        let high = d.high.unwrap();
        assert_eq!((high.streams, high.coupled), (6, 0));
        assert_eq!(high.mapping, [0, 1, 2, 3, 4, 5]);
        let d = Describe::parse(&host, 8).unwrap();
        assert_eq!(d.normal.unwrap().mapping, [0, 1, 4, 5, 6, 7, 2, 3]);
    }

    #[test]
    fn the_audio_params_are_the_hosts_own_table() {
        let host = describe(&host_config());
        for (channels, high) in [
            (2, false),
            (2, true),
            (6, false),
            (6, true),
            (8, false),
            (8, true),
        ] {
            let d = Describe::parse(&host, channels).unwrap();
            let got = audio_params(&d, channels, high, 5).unwrap();
            let want = AudioParams::select(channels, channel_mask(channels), high, 5);
            assert_eq!(got, want, "{channels} channels, high quality {high}");
        }
    }

    /// Every layout the host advertises comes back exactly as the table has it.
    #[test]
    fn the_parser_recovers_each_advertised_layout() {
        let host = describe(&host_config());
        for channels in [2u8, 6, 8] {
            let d = Describe::parse(&host, channels).unwrap();
            for high in [false, true] {
                let Some(layout) = OPUS_LAYOUTS
                    .iter()
                    .find(|l| l.channels == channels && l.high_quality == high)
                else {
                    continue;
                };
                let got = if high {
                    d.high.clone()
                } else {
                    d.normal.clone()
                };
                if channels == 2 && high {
                    // Stereo has no surround line; its high quality reuses the normal one.
                    assert!(got.is_none());
                    continue;
                }
                assert_eq!(got, Some(table_config(layout)), "{channels}ch high {high}");
            }
        }
    }

    #[test]
    fn hosts_without_surround_params_get_the_legacy_5_1() {
        let d = Describe::parse("a=x-nv-video[0].refPicInvalidation:1\n", 6).unwrap();
        let (streams, coupled, mapping) = LEGACY_5_1;
        let normal = d.normal.unwrap();
        assert_eq!((normal.streams, normal.coupled), (streams, coupled));
        assert_eq!(normal.mapping, mapping);
        assert!(d.high.is_none());
        assert!(Describe::parse("", 8).is_err());
    }

    #[test]
    fn bad_surround_parameters_are_errors_not_panics() {
        assert!(Describe::parse("a=fmtp:97 surround-params=6abc", 6).is_err());
        assert!(Describe::parse("a=fmtp:97 surround-params=64201", 6).is_err());
        assert!(Describe::parse("nothing here", 8).is_err());
        // Old hosts' 5.1 has a built-in answer.
        assert_eq!(Describe::parse("", 6).unwrap().normal.unwrap().streams, 4);
    }

    fn announce<'a>(
        r: &'a StreamRequest,
        audio: &'a AudioParams,
        codec: VideoCodec,
        encryption: u32,
    ) -> Announce<'a> {
        Announce {
            request: r,
            sunshine: true,
            codec,
            hdr: r.hdr,
            chroma: r.chroma,
            encryption,
            audio,
            packet_size: if encryption & enc::VIDEO != 0 {
                r.packet_size - 32
            } else {
                r.packet_size
            },
            video_port: 47998,
            rtsp_port: 48010,
            address: "10.0.0.2",
            ip_version: "IPv4",
            rtsp_client_version: 14,
            ref_invalidation: false,
        }
    }

    fn peer() -> Peer {
        Peer {
            client: ClientId("ab".into()),
            client_ip: "10.0.0.2".parse().unwrap(),
            app_id: 3,
            launch_surround_audio_info: 0x30002,
        }
    }

    /// Our `ANNOUNCE` is what the host's parser makes `StreamParams` of, for each codec and encryption choice.
    #[test]
    fn the_host_reads_our_announce_as_we_meant_it() {
        let cfg = host_config();
        for codec in [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1] {
            for encryption in [enc::CONTROL_V2, enc::CONTROL_V2 | enc::VIDEO | enc::AUDIO] {
                let mut r = request();
                r.bitrate_kbps = 40_000;
                r.packet_size = 1392;
                let audio = AudioParams::select(2, 3, false, 5);
                let a = announce(&r, &audio, codec, encryption);
                let (params, got) = parse_announce(&a.sdp(), &cfg, peer()).unwrap();
                assert_eq!(params.codec, codec);
                assert_eq!((params.width, params.height, params.fps), (2560, 1440, 60));
                assert_eq!(params.bitrate_bps, 40_000_000);
                assert_eq!(params.packet_size, a.packet_size);
                assert_eq!(params.min_fec_packets, 2);
                assert_eq!(params.audio, audio);
                assert_eq!(params.max_ref_frames, 1);
                assert!(got.control);
                assert_eq!(got.video, encryption & enc::VIDEO != 0);
                assert_eq!(got.audio, encryption & enc::AUDIO != 0);
            }
        }
    }

    #[test]
    fn hdr_444_and_surround_reach_the_host() {
        let cfg = host_config();
        let mut r = request();
        r.hdr = true;
        r.chroma = Chroma::Yuv444;
        r.full_range = true;
        r.audio_channels = 6;
        let d = Describe::parse(&describe(&cfg), 6).unwrap();
        let audio = audio_params(&d, 6, false, 5).unwrap();
        let a = announce(&r, &audio, VideoCodec::Hevc, enc::CONTROL_V2);
        let (params, _) = parse_announce(&a.sdp(), &cfg, peer()).unwrap();
        assert!(params.hdr && params.full_range);
        assert_eq!(params.chroma, Chroma::Yuv444);
        assert_eq!(params.audio.channels, 6);
        assert_eq!(params.audio.streams, 4);
    }

    #[test]
    fn the_sdp_shape_is_moonlights() {
        let r = request();
        let audio = AudioParams::select(2, 3, false, 5);
        let text = announce(&r, &audio, VideoCodec::Hevc, enc::CONTROL_V2).sdp();
        assert!(text.starts_with(
            "v=0\r\no=android 0 14 IN IPv4 10.0.0.2\r\ns=NVIDIA Streaming Client\r\n"
        ));
        assert!(text.contains("a=x-nv-video[0].clientViewportWd:2560 \r\n"));
        assert!(text.ends_with("t=0 0\r\nm=video 47998  \r\n"));
        assert!(text.contains("a=x-nv-general.serverAddress:rtsp://10.0.0.2:48010 \r\n"));
    }

    #[test]
    fn encryption_follows_support_requirement_and_wish() {
        let mut d = Describe {
            enc_supported: 7,
            enc_requested: 1,
            ..Describe::default()
        };
        let mut r = request();
        r.video_encryption = Encrypt::IfSupported;
        r.audio_encryption = Encrypt::Off;
        assert_eq!(encryption_enabled(&d, &r).unwrap(), 1 | 2);
        r.video_encryption = Encrypt::Off;
        assert_eq!(encryption_enabled(&d, &r).unwrap(), 1);
        d.enc_supported = 1;
        r.video_encryption = Encrypt::IfSupported;
        assert_eq!(encryption_enabled(&d, &r).unwrap(), 1);
        r.video_encryption = Encrypt::Required;
        assert!(encryption_enabled(&d, &r).is_err());
        // The host requiring audio encryption wins over our opt-out.
        d.enc_requested = 5;
        d.enc_supported = 7;
        r.video_encryption = Encrypt::Off;
        assert_eq!(encryption_enabled(&d, &r).unwrap(), 1 | 4);
    }

    #[test]
    fn high_quality_audio_follows_bitrate_and_the_hosts_offer() {
        let host = describe(&host_config());
        let d = Describe::parse(&host, 6).unwrap();
        let mut r = request();
        r.audio_channels = 6;
        r.bitrate_kbps = 20_000;
        assert!(wants_high_quality(&r, &d));
        r.bitrate_kbps = 10_000;
        assert!(!wants_high_quality(&r, &d));
        r.high_quality_audio = Some(true);
        assert!(wants_high_quality(&r, &d));
        r.high_quality_audio = Some(false);
        r.bitrate_kbps = 90_000;
        assert!(!wants_high_quality(&r, &d));
        r.high_quality_audio = None;
        r.audio_channels = 2;
        assert!(
            !wants_high_quality(&r, &d),
            "stereo stays normal unless asked"
        );
    }

    /// Whatever the host says in DESCRIBE, parsing returns.
    #[test]
    fn hostile_describe_never_panics() {
        let mut seed = 0x0DDB_A11C_0FFE_E123u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let atoms = [
            "a=fmtp:97 surround-params=",
            "6",
            "8",
            "642014235",
            "\n",
            "a=x-ss-general.encryptionSupported:",
            "a=",
            ":",
            "99999999999",
            "\u{e9}",
            "AV1/90000",
            "0x",
            "-1",
        ];
        for _ in 0..30_000 {
            let mut s = String::new();
            for _ in 0..(next() % 14) {
                s.push_str(atoms[(next() % atoms.len() as u64) as usize]);
            }
            for channels in [2, 6, 8] {
                let _ = Describe::parse(&s, channels);
            }
        }
    }
}

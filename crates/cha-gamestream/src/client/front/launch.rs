//! `/launch`, `/resume`, `/cancel` and the RTSP negotiation that follows,
//! ending in a [`StreamSetup`]. The query parameter names, stream ids and
//! request order are what hosts expect (per moonlight-common-c and
//! moonlight-qt's launch requests); the code is written from that behaviour.

use std::net::SocketAddr;

use aws_lc_rs::rand::{SecureRandom, SystemRandom};

use super::rtsp::Client as Rtsp;
use super::sdp::{self, Announce, Describe};
use super::{ClientError, HostClient, HostInfo};
use crate::client::{PyrowaveSetup, StreamSetup};
use crate::front::rtsp::sdp::enc;
use crate::handoff::{Chroma, Encryption, MediaPorts, SessionKeys, VideoCodec};

/// The `X-GS-ClientVersion` of hosts at app version 7, per Moonlight's RTSP requests.
const RTSP_CLIENT_VERSION: u32 = 14;
/// Bytes video encryption adds to each packet (IV, frame number, tag), as
/// Moonlight clients budget for it.
const ENC_VIDEO_HEADER: usize = 32;

/// Whether a stream's video or audio is encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encrypt {
    /// Not unless the host insists.
    Off,
    /// When the host supports it.
    IfSupported,
    /// Always: the launch fails if the host can't.
    Required,
}

/// What the encoder's colour space is, as `encoderCscMode` numbers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ColorSpace {
    Rec601 = 0,
    Rec709 = 1,
    Rec2020 = 2,
}

/// What the client asks of a stream, for `/launch` and RTSP `ANNOUNCE`.
#[derive(Clone, Debug)]
pub struct StreamRequest {
    pub app_id: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// What the user chose; the host adds FEC to it.
    pub bitrate_kbps: u32,
    /// Codecs we can decode, most wanted first. The stream uses the first
    /// the host offers in the dynamic range and chroma format asked for.
    pub codecs: Vec<VideoCodec>,
    /// Stream PyroWave (Vibepollo's contract: 8-bit, 4:2:0 or 4:4:4 as
    /// [`chroma`](Self::chroma) says) instead of any of `codecs`. Never
    /// chosen for the caller: only an explicit request gets it, and `hdr`
    /// must be off (HDR10 PyroWave isn't spoken yet). The host refuses
    /// unless its `serverinfo` offers the profile and its DESCRIBE names a
    /// bitstream in [`PYROWAVE_BITSTREAMS`](super::PYROWAVE_BITSTREAMS).
    pub pyrowave: bool,
    pub hdr: bool,
    pub chroma: Chroma,
    pub full_range: bool,
    /// Rec. 709 unless changed; HDR is always Rec. 2020.
    pub color_space: ColorSpace,
    /// 2, 6 or 8.
    pub audio_channels: u8,
    /// `None`: as moonlight-common-c decides (surround above 15 Mbps, when the
    /// host offers it); `Some` forces it on or off.
    pub high_quality_audio: Option<bool>,
    /// Milliseconds of audio per packet (5 or 10).
    pub audio_packet_ms: u32,
    /// Video packet size in bytes before encryption overhead: what the
    /// network carries comfortably (1392 on a LAN, 1024 across the internet).
    pub packet_size: usize,
    /// Parity packets a small frame is given at least.
    pub min_fec_packets: u32,
    pub video_encryption: Encrypt,
    pub audio_encryption: Encrypt,
    /// Whether the client can ask the host to invalidate lost reference
    /// frames, which lets the host use several of them.
    pub reference_frame_invalidation: bool,
    /// `sops`: let the host change its resolution to ours.
    pub optimize_game_settings: bool,
    /// Also play the audio on the host.
    pub local_audio: bool,
    /// Pads the client has (`gcmap`, a bit per pad).
    pub gamepad_mask: u32,
    /// A stream across the internet: tighter bitrate headroom, no QoS tagging.
    pub remote: bool,
}

impl StreamRequest {
    /// 1392-byte packets, 20 Mbps, HEVC then H.264, stereo, SDR, video and
    /// audio encrypted when the host can.
    pub fn new(app_id: u32, width: u32, height: u32, fps: u32) -> Self {
        Self {
            app_id,
            width,
            height,
            fps,
            bitrate_kbps: 20_000,
            codecs: vec![VideoCodec::Hevc, VideoCodec::H264],
            pyrowave: false,
            hdr: false,
            chroma: Chroma::Yuv420,
            full_range: false,
            color_space: ColorSpace::Rec709,
            audio_channels: 2,
            high_quality_audio: None,
            audio_packet_ms: 5,
            packet_size: 1392,
            min_fec_packets: 2,
            video_encryption: Encrypt::IfSupported,
            audio_encryption: Encrypt::IfSupported,
            reference_frame_invalidation: false,
            optimize_game_settings: false,
            local_audio: false,
            gamepad_mask: 0,
            remote: false,
        }
    }

    fn validate(&self) -> Result<(), ClientError> {
        let bad = |what: &str| {
            Err(ClientError::Unsupported(format!(
                "bad stream request: {what}"
            )))
        };
        if !(64..=16_384).contains(&self.width) || !(64..=16_384).contains(&self.height) {
            return bad("the picture size");
        }
        if !(1..=1000).contains(&self.fps) {
            return bad("the frame rate");
        }
        if !(1..=4_000_000).contains(&self.bitrate_kbps) {
            return bad("the bitrate");
        }
        if !(256..=16_384).contains(&self.packet_size) {
            return bad("the packet size");
        }
        if !matches!(self.audio_channels, 2 | 6 | 8) {
            return bad("the audio channel count");
        }
        if self.pyrowave {
            if self.hdr {
                return bad("HDR10 PyroWave isn't supported (8-bit only)");
            }
        } else if self.codecs.is_empty() {
            return bad("no codec");
        }
        Ok(())
    }

    /// `surroundAudioInfo`: channel mask in the high 16 bits, count in the low.
    fn surround_audio_info(&self) -> u32 {
        (sdp::channel_mask(self.audio_channels) << 16) | u32::from(self.audio_channels)
    }
}

fn random<const N: usize>() -> Result<[u8; N], ClientError> {
    let mut bytes = [0u8; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ClientError::Tls("no randomness".into()))?;
    Ok(bytes)
}

/// The port in `rtsp://host:port`, with `rtsp` as the only scheme we speak.
fn rtsp_port(url: &str) -> Result<u16, ClientError> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(ClientError::Malformed(format!(
            "sessionUrl0 {url:?} isn't a URL"
        )));
    };
    if scheme != "rtsp" {
        return Err(ClientError::Unsupported(format!(
            "the host's session URL is {scheme}://, and only plain rtsp:// is supported"
        )));
    }
    let authority = rest.split('/').next().unwrap_or("");
    // `[v6]:port`, `v4:port` or a bare host (the default port).
    let port = match authority.rsplit_once(':') {
        Some((host, port)) if !host.ends_with(':') && !port.contains(']') => Some(port),
        _ => None,
    };
    match port {
        None => Ok(48010),
        Some(p) => {
            p.parse::<u16>().ok().filter(|&p| p != 0).ok_or_else(|| {
                ClientError::Malformed(format!("sessionUrl0 {url:?} has a bad port"))
            })
        }
    }
}

fn transport_port(transport: Option<&str>) -> Option<u16> {
    let rest = transport?.split("server_port=").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u16>().ok().filter(|&p| p != 0)
}

fn ping_payload(value: Option<&str>) -> Option<[u8; 16]> {
    <[u8; 16]>::try_from(value?.as_bytes()).ok()
}

impl HostClient {
    /// Starts `request.app_id` on the host and negotiates its stream. The
    /// host must be paired and idle. After an error past `/launch`, the app
    /// runs on: [`resume`](Self::resume) it, or [`cancel`](Self::cancel).
    pub async fn launch(&self, request: &StreamRequest) -> Result<StreamSetup, ClientError> {
        self.start("launch", request).await
    }

    /// Reconnects to the running session this client launched, with a fresh key.
    pub async fn resume(&self, request: &StreamRequest) -> Result<StreamSetup, ClientError> {
        self.start("resume", request).await
    }

    /// Quits the running app, whatever session it was.
    pub async fn cancel(&self) -> Result<(), ClientError> {
        let (root, _) = self
            .nvhttp(true, "cancel", Vec::new(), self.timeouts.cancel)
            .await?;
        // `cancel` is 0 when the host couldn't quit the app.
        if root.text_of("cancel") == Some("0") {
            return Err(ClientError::Host {
                code: 0,
                message: "the host didn't quit the app".into(),
            });
        }
        Ok(())
    }

    async fn start(
        &self,
        verb: &'static str,
        request: &StreamRequest,
    ) -> Result<StreamSetup, ClientError> {
        request.validate()?;
        if self.pinned().is_none() {
            return Err(ClientError::NoPinnedCert);
        }
        let info = self.server_info().await?;
        if !info.paired {
            return Err(ClientError::NotPaired);
        }
        let quad = info.app_version_quad();
        if quad[0] < 7 {
            return Err(ClientError::Unsupported(format!(
                "the host is GameStream generation {}, and only 7 and later are supported",
                quad[0]
            )));
        }
        if !info.app_version_at_least(7, 1, 431) {
            return Err(ClientError::Unsupported(
                "the host is older than GFE 3.22 (app version 7.1.431), which isn't supported"
                    .into(),
            ));
        }
        let candidates = if request.pyrowave {
            if !info.is_sunshine() {
                return Err(ClientError::Unsupported(
                    "PyroWave needs a Sunshine-family host (its ANNOUNCE attributes are extensions)"
                        .into(),
                ));
            }
            if !info.supports_pyrowave(request.chroma) {
                return Err(ClientError::Unsupported(format!(
                    "the host doesn't offer 8-bit PyroWave in {}",
                    if request.chroma == Chroma::Yuv444 {
                        "4:4:4"
                    } else {
                        "4:2:0"
                    }
                )));
            }
            Vec::new()
        } else {
            candidates(&info, request)
        };
        if candidates.is_empty() && !request.pyrowave {
            return Err(ClientError::Unsupported(format!(
                "the host can't encode {}{} with any codec we can decode",
                if request.hdr { "HDR " } else { "" },
                if request.chroma == Chroma::Yuv444 {
                    "4:4:4"
                } else {
                    "video"
                }
            )));
        }

        let key: [u8; 16] = random()?;
        let key_id = i64::from(i32::from_be_bytes(random()?));
        let keys = SessionKeys { key, key_id };
        let mut params = vec![
            ("appid", request.app_id.to_string()),
            (
                "mode",
                format!("{}x{}x{}", request.width, request.height, request.fps),
            ),
            ("additionalStates", "1".into()),
            ("sops", u8::from(request.optimize_game_settings).to_string()),
            ("rikey", hex::encode(key)),
            ("rikeyid", key_id.to_string()),
        ];
        // The HDR capability parameters are sent as Moonlight's launch
        // requests send them (a zeroed capability set), for compatibility.
        if request.hdr {
            params.extend([
                ("hdrMode", "1".to_owned()),
                ("clientHdrCapVersion", "0".into()),
                ("clientHdrCapSupportedFlagsInUint32", "0".into()),
                ("clientHdrCapMetaDataId", "NV_STATIC_METADATA_TYPE_1".into()),
                ("clientHdrCapDisplayData", "0x0x0x0x0x0x0x0x0x0x0".into()),
            ]);
        }
        params.extend([
            (
                "localAudioPlayMode",
                u8::from(request.local_audio).to_string(),
            ),
            (
                "surroundAudioInfo",
                request.surround_audio_info().to_string(),
            ),
            ("remoteControllersBitmap", request.gamepad_mask.to_string()),
            ("gcmap", request.gamepad_mask.to_string()),
            ("gcpersist", "0".into()),
            // Per Moonlight's launch requests.
            ("corever", "1".into()),
        ]);
        let (root, peer) = self
            .nvhttp(true, verb, params, self.timeouts.launch)
            .await?;
        let url = root
            .text_of("sessionUrl0")
            .filter(|u| !u.is_empty())
            .ok_or_else(|| {
                ClientError::Malformed(format!("the {verb} answer has no sessionUrl0"))
            })?;
        let port = rtsp_port(url)?;
        // The host's own idea of its address (`sessionUrl0`) can be a private
        // one when we came through a port forward; the address we reached it
        // at is the one that works.
        let rtsp_addr = SocketAddr::new(peer.ip(), port);
        self.negotiate(request, &info, &candidates, keys, rtsp_addr)
            .await
    }

    async fn negotiate(
        &self,
        request: &StreamRequest,
        info: &HostInfo,
        candidates: &[VideoCodec],
        keys: SessionKeys,
        rtsp_addr: SocketAddr,
    ) -> Result<StreamSetup, ClientError> {
        let mut rtsp = Rtsp::new(rtsp_addr, RTSP_CLIENT_VERSION, self.timeouts.rtsp);
        let url = rtsp.url().to_owned();
        rtsp.request("OPTIONS", "OPTIONS", &url, &[], "").await?;
        let described = rtsp
            .request(
                "DESCRIBE",
                "DESCRIBE",
                &url,
                &[
                    ("Accept", "application/sdp".to_owned()),
                    (
                        "If-Modified-Since",
                        "Thu, 01 Jan 1970 00:00:00 GMT".to_owned(),
                    ),
                ],
                "",
            )
            .await?;
        let describe = Describe::parse(&described.body_text(), request.audio_channels)?;
        let pyrowave = if request.pyrowave {
            if !describe.pyrowave_marker {
                return Err(ClientError::Unsupported(
                    "the host's RTSP doesn't offer PyroWave".into(),
                ));
            }
            // The id decides: a host on another bitstream is refused before
            // anything is announced.
            Some(describe.pyrowave_bitstream_allowed()?)
        } else {
            None
        };
        let codec = if pyrowave.is_some() {
            // Not consulted for PyroWave; see `StreamSetup::pyrowave`.
            VideoCodec::H264
        } else {
            candidates
                .iter()
                .copied()
                .find(|&c| describe.offers(c))
                .ok_or_else(|| {
                    ClientError::Unsupported(
                        "the host's RTSP offers none of the codecs we asked for".into(),
                    )
                })?
        };
        let sunshine = info.is_sunshine();
        let enabled = if sunshine {
            sdp::encryption_enabled(&describe, request)?
        } else {
            // Without Sunshine's flags only audio can be asked for.
            if request.audio_encryption == Encrypt::Off {
                0
            } else {
                enc::AUDIO
            }
        };
        let encryption = Encryption {
            control: true,
            video: enabled & enc::VIDEO != 0,
            audio: enabled & enc::AUDIO != 0,
        };
        let high = sdp::wants_high_quality(request, &describe);
        let audio_ms = if high { 5 } else { request.audio_packet_ms };
        let audio = sdp::audio_params(&describe, request.audio_channels, high, audio_ms)?;
        // A multiple of 16 that the encryption header still fits in.
        let mut packet_size = request.packet_size / 16 * 16;
        if encryption.video {
            packet_size -= ENC_VIDEO_HEADER;
        }

        // The Transport line and the `streamid=` values below (audio/0/0,
        // video/0/0, control/13/0) are per Moonlight's RTSP requests.
        let setup_headers = [
            (
                "Transport",
                "unicast;X-GS-ClientPort=50000-50001".to_owned(),
            ),
            (
                "If-Modified-Since",
                "Thu, 01 Jan 1970 00:00:00 GMT".to_owned(),
            ),
        ];
        let audio_setup = rtsp
            .request(
                "SETUP audio",
                "SETUP",
                "streamid=audio/0/0",
                &setup_headers,
                "",
            )
            .await?;
        let token = audio_setup
            .header("session")
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| ClientError::Rtsp {
                step: "SETUP audio",
                status: Some(200),
                detail: "the answer has no session".into(),
            })?;
        rtsp.session = Some(token.to_owned());
        let port = |response: &super::rtsp::Response, step: &'static str| {
            transport_port(response.header("transport")).ok_or_else(|| ClientError::Rtsp {
                step,
                status: Some(200),
                detail: "the answer has no server_port".into(),
            })
        };
        let audio_port = port(&audio_setup, "SETUP audio")?;
        let video_setup = rtsp
            .request(
                "SETUP video",
                "SETUP",
                "streamid=video/0/0",
                &setup_headers,
                "",
            )
            .await?;
        let video_port = port(&video_setup, "SETUP video")?;
        let control_stream = "streamid=control/13/0";
        let control_setup = rtsp
            .request("SETUP control", "SETUP", control_stream, &setup_headers, "")
            .await?;
        let control_port = port(&control_setup, "SETUP control")?;
        let control_connect_data = control_setup
            .header("x-ss-connect-data")
            .and_then(sdp::c_uint)
            .unwrap_or(0);
        let ping = ping_payload(video_setup.header("x-ss-ping-payload"))
            .or_else(|| ping_payload(audio_setup.header("x-ss-ping-payload")))
            .unwrap_or([0; 16]);

        let address = match rtsp_addr.ip() {
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
            v4 => v4.to_string(),
        };
        let announce = Announce {
            request,
            sunshine,
            codec,
            pyrowave: pyrowave.is_some(),
            hdr: request.hdr,
            chroma: request.chroma,
            encryption: enabled,
            audio: &audio,
            packet_size,
            video_port,
            rtsp_port: rtsp_addr.port(),
            address: &address,
            ip_version: if rtsp_addr.is_ipv6() { "IPv6" } else { "IPv4" },
            rtsp_client_version: RTSP_CLIENT_VERSION,
            // Never for PyroWave: every frame stands alone.
            ref_invalidation: pyrowave.is_none()
                && request.reference_frame_invalidation
                && describe.ref_invalidation,
        };
        let sdp_text = announce.sdp();
        rtsp.request(
            "ANNOUNCE",
            "ANNOUNCE",
            control_stream,
            &[("Content-type", "application/sdp".to_owned())],
            &sdp_text,
        )
        .await?;
        rtsp.request("PLAY", "PLAY", "/", &[], "").await?;

        Ok(StreamSetup {
            host: rtsp_addr.ip(),
            ports: MediaPorts {
                video: video_port,
                control: control_port,
                audio: audio_port,
            },
            keys,
            encryption,
            control_connect_data,
            ping_payload: ping,
            codec,
            width: request.width,
            height: request.height,
            fps: request.fps,
            packet_size,
            hdr: request.hdr,
            chroma: request.chroma,
            audio,
            pyrowave: pyrowave.map(|bitstream| PyrowaveSetup {
                bitstream: bitstream.to_owned(),
                record_framing: true,
            }),
        })
    }
}

/// The codecs of the request the host encodes in the dynamic range and chroma asked for.
fn candidates(info: &HostInfo, request: &StreamRequest) -> Vec<VideoCodec> {
    let mut out = Vec::new();
    for &codec in &request.codecs {
        if !out.contains(&codec) && info.supports(codec, request.hdr, request.chroma) {
            out.push(codec);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rtsp_port_comes_from_the_session_url() {
        assert_eq!(rtsp_port("rtsp://192.168.1.5:48010").unwrap(), 48010);
        assert_eq!(rtsp_port("rtsp://192.168.1.5:50000/x").unwrap(), 50000);
        assert_eq!(rtsp_port("rtsp://[fe80::1]:48010").unwrap(), 48010);
        assert_eq!(rtsp_port("rtsp://[fe80::1]").unwrap(), 48010);
        assert_eq!(rtsp_port("rtsp://host").unwrap(), 48010);
        assert!(rtsp_port("rtspenc://h:1").is_err());
        assert!(rtsp_port("rtspru://h:1").is_err());
        assert!(rtsp_port("nonsense").is_err());
        assert!(rtsp_port("rtsp://h:0").is_err());
        assert!(rtsp_port("rtsp://h:99999").is_err());
    }

    #[test]
    fn transport_and_ping_headers() {
        assert_eq!(
            transport_port(Some("unicast;server_port=48000-48001;source=1.2.3.4")),
            Some(48000)
        );
        assert_eq!(transport_port(Some("server_port=47998")), Some(47998));
        assert_eq!(transport_port(Some("server_port=0")), None);
        assert_eq!(transport_port(Some("server_port=70000")), None);
        assert_eq!(transport_port(Some("unicast")), None);
        assert_eq!(transport_port(None), None);
        assert_eq!(
            ping_payload(Some("0123456789ABCDEF")),
            Some(*b"0123456789ABCDEF")
        );
        assert_eq!(ping_payload(Some("short")), None);
    }

    #[test]
    fn requests_are_checked() {
        assert!(StreamRequest::new(1, 1920, 1080, 60).validate().is_ok());
        for r in [
            StreamRequest::new(1, 10, 1080, 60),
            StreamRequest::new(1, 1920, 1080, 0),
            StreamRequest {
                audio_channels: 3,
                ..StreamRequest::new(1, 1920, 1080, 60)
            },
            StreamRequest {
                codecs: vec![],
                ..StreamRequest::new(1, 1920, 1080, 60)
            },
            StreamRequest {
                packet_size: 100,
                ..StreamRequest::new(1, 1920, 1080, 60)
            },
        ] {
            assert!(r.validate().is_err());
        }
        // PyroWave: needs no codec list, never HDR (10-bit isn't spoken).
        let pyro = StreamRequest {
            pyrowave: true,
            codecs: vec![],
            ..StreamRequest::new(1, 1920, 1080, 60)
        };
        assert!(pyro.validate().is_ok());
        assert!(StreamRequest { hdr: true, ..pyro }.validate().is_err());
        assert!(
            !StreamRequest::new(1, 1920, 1080, 60).pyrowave,
            "never the default"
        );
        let r = StreamRequest {
            audio_channels: 6,
            ..StreamRequest::new(1, 1920, 1080, 60)
        };
        assert_eq!(r.surround_audio_info(), 0x3F0006);
        assert_eq!(
            StreamRequest::new(1, 1920, 1080, 60).surround_audio_info(),
            0x30002
        );
    }
}

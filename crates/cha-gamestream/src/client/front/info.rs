//! What `serverinfo` says about a host.

use super::ClientError;
use super::xml::Node;
use crate::front::nvhttp::xml::codec_support as bits;
use crate::handoff::{Chroma, VideoCodec};

/// `serverinfo`, parsed. Only `appversion` is required; a host that leaves
/// out the rest (old GFE) gets empty or zero values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostInfo {
    pub name: String,
    /// The host's id, which clients remember it by.
    pub unique_id: String,
    pub app_version: String,
    pub gfe_version: String,
    /// `ExternalPort`: the plain HTTP port.
    pub http_port: u16,
    pub https_port: u16,
    pub mac: String,
    pub local_ip: String,
    /// `ServerCodecModeSupport`, (bit values per moonlight-common-c's `SCM_*` constants).
    pub codec_mode_support: u32,
    pub max_luma_pixels_hevc: u32,
    /// Whether the host knows our certificate. Only an HTTPS answer says so;
    /// over plain HTTP it is `false`.
    pub paired: bool,
    /// The app of the running session, 0 when none runs.
    pub current_game: u32,
    /// The host's `state`, e.g. `SUNSHINE_SERVER_BUSY`.
    pub state: String,
}

impl HostInfo {
    pub(super) fn from_xml(root: &Node) -> Result<Self, ClientError> {
        let text = |name: &str| root.text_of(name).unwrap_or("").to_owned();
        let number = |name: &str| {
            root.text_of(name)
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(0)
        };
        let port = |name: &str| {
            root.text_of(name)
                .and_then(|v| v.parse::<u16>().ok())
                .unwrap_or(0)
        };
        let app_version = text("appversion");
        if app_version.is_empty() {
            return Err(ClientError::Malformed(
                "serverinfo has no appversion".into(),
            ));
        }
        let state = text("state");
        // GFE 2.8 left `currentgame` at the last game played; only a busy
        // host is running it.
        let current_game = if state.ends_with("_SERVER_BUSY") {
            number("currentgame")
        } else {
            0
        };
        Ok(Self {
            name: text("hostname"),
            unique_id: text("uniqueid"),
            gfe_version: text("GfeVersion"),
            http_port: port("ExternalPort"),
            https_port: port("HttpsPort"),
            mac: text("mac"),
            local_ip: text("LocalIP"),
            codec_mode_support: number("ServerCodecModeSupport"),
            max_luma_pixels_hevc: number("MaxLumaPixelsHEVC"),
            paired: root.text_of("PairStatus") == Some("1"),
            current_game,
            state,
            app_version,
        })
    }

    /// The four numbers of `appversion` (0 for any that isn't one; `-1` in
    /// the fourth marks Sunshine and its kin).
    pub fn app_version_quad(&self) -> [i32; 4] {
        let mut quad = [0; 4];
        for (slot, part) in quad.iter_mut().zip(self.app_version.split('.')) {
            *slot = part.trim().parse().unwrap_or(0);
        }
        quad
    }

    /// Sunshine, Apollo and our host: their extensions (encryption flags,
    /// ping payloads, feature flags) are on.
    pub fn is_sunshine(&self) -> bool {
        self.app_version_quad()[3] < 0
    }

    pub(super) fn app_version_at_least(&self, a: i32, b: i32, c: i32) -> bool {
        let q = self.app_version_quad();
        (q[0], q[1], q[2]) >= (a, b, c)
    }

    pub fn busy(&self) -> bool {
        self.current_game != 0
    }

    /// The codecs the host encodes in the plain (SDR, 4:2:0) form, best first.
    /// Empty `codec_mode_support` means a host that predates the field: H.264.
    pub fn codecs(&self) -> Vec<VideoCodec> {
        [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264]
            .into_iter()
            .filter(|&c| self.supports(c, false, Chroma::Yuv420))
            .collect()
    }

    pub fn supports_hdr(&self, codec: VideoCodec) -> bool {
        self.supports(codec, true, Chroma::Yuv420)
    }

    pub fn supports_yuv444(&self, codec: VideoCodec) -> bool {
        self.supports(codec, false, Chroma::Yuv444)
    }

    /// Whether the host encodes `codec` in this dynamic range and chroma format.
    pub fn supports(&self, codec: VideoCodec, hdr: bool, chroma: Chroma) -> bool {
        let bit = match (codec, hdr, chroma) {
            (VideoCodec::H264, false, Chroma::Yuv420) => bits::H264,
            (VideoCodec::H264, false, Chroma::Yuv444) => bits::H264_HIGH8_444,
            (VideoCodec::H264, true, _) => return false,
            (VideoCodec::Hevc, false, Chroma::Yuv420) => bits::HEVC,
            (VideoCodec::Hevc, true, Chroma::Yuv420) => bits::HEVC_MAIN10,
            (VideoCodec::Hevc, false, Chroma::Yuv444) => bits::HEVC_REXT8_444,
            (VideoCodec::Hevc, true, Chroma::Yuv444) => bits::HEVC_REXT10_444,
            (VideoCodec::Av1, false, Chroma::Yuv420) => bits::AV1_MAIN8,
            (VideoCodec::Av1, true, Chroma::Yuv420) => bits::AV1_MAIN10,
            (VideoCodec::Av1, false, Chroma::Yuv444) => bits::AV1_HIGH8_444,
            (VideoCodec::Av1, true, Chroma::Yuv444) => bits::AV1_HIGH10_444,
        };
        if self.codec_mode_support == 0 {
            // A host that doesn't say: plain H.264, and plain HEVC if RTSP
            // `DESCRIBE` offers it.
            return !hdr && chroma == Chroma::Yuv420 && codec != VideoCodec::Av1;
        }
        self.codec_mode_support & bit != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::front::xml;

    fn info(body: &str) -> Result<HostInfo, ClientError> {
        let doc = xml::parse(body);
        HostInfo::from_xml(doc.find("root").unwrap())
    }

    #[test]
    fn serverinfo_is_read_as_the_host_writes_it() {
        let i = info("<root status_code=\"200\"><hostname>Cha &amp; Co</hostname><appversion>7.1.431.-1</appversion>\
            <GfeVersion>3.23.0.74</GfeVersion><uniqueid>ABCDEF0123456789ABCDEF0123456789</uniqueid><HttpsPort>47984</HttpsPort>\
            <ExternalPort>47989</ExternalPort><mac>00:00:00:00:00:00</mac><MaxLumaPixelsHEVC>1869449984</MaxLumaPixelsHEVC>\
            <LocalIP>192.168.1.5</LocalIP><ServerCodecModeSupport>65793</ServerCodecModeSupport>\
            <SupportedDisplayMode></SupportedDisplayMode><PairStatus>1</PairStatus><currentgame>7</currentgame>\
            <state>CHA_SERVER_BUSY</state></root>").unwrap();
        assert_eq!(i.name, "Cha & Co");
        assert_eq!((i.https_port, i.http_port), (47984, 47989));
        assert!(i.paired && i.busy() && i.is_sunshine());
        assert_eq!(i.current_game, 7);
        assert_eq!(i.app_version_quad(), [7, 1, 431, -1]);
        assert!(i.app_version_at_least(7, 1, 431) && !i.app_version_at_least(7, 1, 432));
        // AV1 main 8, HEVC, H.264
        assert_eq!(
            i.codecs(),
            [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::H264]
        );
        assert!(!i.supports_hdr(VideoCodec::Hevc) && !i.supports_yuv444(VideoCodec::H264));
    }

    #[test]
    fn a_free_host_runs_nothing_whatever_it_remembers() {
        let i = info("<root><appversion>7.1.0.0</appversion><currentgame>5</currentgame><state>GFE_SERVER_FREE</state></root>")
            .unwrap();
        assert_eq!(i.current_game, 0);
        assert!(!i.is_sunshine() && !i.paired);
        // No codec mask: H.264 (and HEVC, if RTSP says so) is assumed.
        assert_eq!(i.codecs(), [VideoCodec::Hevc, VideoCodec::H264]);
    }

    #[test]
    fn serverinfo_without_a_version_is_refused() {
        assert!(info("<root><hostname>x</hostname></root>").is_err());
    }

    #[test]
    fn the_codec_mask_decides_dynamic_range_and_chroma() {
        let mut i = info("<root><appversion>7.1.431.-1</appversion></root>").unwrap();
        i.codec_mode_support = bits::HEVC | bits::HEVC_MAIN10 | bits::AV1_HIGH8_444 | bits::H264;
        assert!(i.supports_hdr(VideoCodec::Hevc));
        assert!(!i.supports_hdr(VideoCodec::Av1));
        assert!(i.supports_yuv444(VideoCodec::Av1));
        assert!(!i.supports_hdr(VideoCodec::H264));
        assert!(!i.supports(VideoCodec::Av1, false, Chroma::Yuv420));
    }
}

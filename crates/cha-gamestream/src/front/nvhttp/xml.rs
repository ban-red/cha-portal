// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: the documents are built here from typed values (golden-tested) instead of inline in the handlers;
// codec support is derived from the host's capabilities.

//! The XML documents nvhttp answers with. Moonlight reads them with a
//! forgiving parser and looks elements up by name, so the shape is a flat
//! `<root status_code="200">` of elements.

use std::fmt::Write;

use crate::directory::App;
use crate::handoff::{Capabilities, VideoCodec};

/// What `serverinfo` reports for `appversion`: the fourth component is -1 to
/// say "Sunshine protocol", which enables its extensions in clients.
pub const APP_VERSION: &str = "7.1.431.-1";
pub const GFE_VERSION: &str = "3.23.0.74";

/// `ServerCodecModeSupport` bits, as moonlight-common-c defines them.
pub mod codec_support {
    pub const H264: u32 = 0x0_0001;
    pub const H264_HIGH8_444: u32 = 0x0_0004;
    pub const HEVC: u32 = 0x0_0100;
    pub const HEVC_MAIN10: u32 = 0x0_0200;
    pub const HEVC_REXT8_444: u32 = 0x0_0400;
    pub const HEVC_REXT10_444: u32 = 0x0_0800;
    pub const AV1_MAIN8: u32 = 0x1_0000;
    pub const AV1_MAIN10: u32 = 0x2_0000;
    pub const AV1_HIGH8_444: u32 = 0x4_0000;
    pub const AV1_HIGH10_444: u32 = 0x8_0000;
}

/// The `ServerCodecModeSupport` mask for what the host can encode.
pub fn codec_mode_support(caps: &Capabilities) -> u32 {
    use codec_support::*;
    let mut mask = 0;
    for codec in &caps.codecs {
        mask |= match codec {
            VideoCodec::H264 => H264 | if caps.yuv444 { H264_HIGH8_444 } else { 0 },
            VideoCodec::Hevc => {
                HEVC | if caps.hdr { HEVC_MAIN10 } else { 0 }
                    | if caps.yuv444 {
                        HEVC_REXT8_444 | if caps.hdr { HEVC_REXT10_444 } else { 0 }
                    } else {
                        0
                    }
            }
            VideoCodec::Av1 => {
                AV1_MAIN8
                    | if caps.hdr { AV1_MAIN10 } else { 0 }
                    | if caps.yuv444 {
                        AV1_HIGH8_444 | if caps.hdr { AV1_HIGH10_444 } else { 0 }
                    } else {
                        0
                    }
            }
        };
    }
    mask
}

pub fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters other than whitespace aren't valid XML.
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// `<root status_code="200">` around already-built elements.
pub fn ok(inner: &str) -> String {
    format!("<root status_code=\"200\">{inner}</root>")
}

/// An error answer. nvhttp always uses HTTP 200 for these: Moonlight's Qt
/// client reads no body from a 4xx and would lose the message.
pub fn error(code: u16, message: &str) -> String {
    format!(
        "<root status_code=\"{code}\" status_message=\"{}\"></root>",
        escape(message)
    )
}

pub struct ServerInfo<'a> {
    pub hostname: &'a str,
    pub unique_id: &'a str,
    pub https_port: u16,
    pub http_port: u16,
    pub mac: &'a str,
    pub local_ip: &'a str,
    pub codec_mode_support: u32,
    /// Whether the asking client is paired (only HTTPS can tell).
    pub paired: bool,
    /// The app of the running session, 0 for none.
    pub current_game: u32,
}

pub fn server_info(info: &ServerInfo<'_>) -> String {
    let mut s = String::new();
    let _ = write!(
        s,
        "<hostname>{}</hostname><appversion>{APP_VERSION}</appversion><GfeVersion>{GFE_VERSION}</GfeVersion>\
         <uniqueid>{}</uniqueid><HttpsPort>{}</HttpsPort><ExternalPort>{}</ExternalPort><mac>{}</mac>\
         <MaxLumaPixelsHEVC>{}</MaxLumaPixelsHEVC><LocalIP>{}</LocalIP>\
         <ServerCodecModeSupport>{}</ServerCodecModeSupport><SupportedDisplayMode></SupportedDisplayMode>\
         <PairStatus>{}</PairStatus><currentgame>{}</currentgame><state>{}</state>",
        escape(info.hostname),
        escape(info.unique_id),
        info.https_port,
        info.http_port,
        escape(info.mac),
        if info.codec_mode_support & (codec_support::HEVC | codec_support::AV1_MAIN8) != 0 {
            1_869_449_984u32
        } else {
            0
        },
        escape(info.local_ip),
        info.codec_mode_support,
        u8::from(info.paired),
        info.current_game,
        if info.current_game != 0 {
            "CHA_SERVER_BUSY"
        } else {
            "CHA_SERVER_FREE"
        },
    );
    ok(&s)
}

pub fn app_list(apps: &[App]) -> String {
    let mut s = String::new();
    for app in apps {
        let _ = write!(
            s,
            "<App><IsHdrSupported>{}</IsHdrSupported><AppTitle>{}</AppTitle><ID>{}</ID></App>",
            u8::from(app.hdr),
            escape(&app.title),
            app.id
        );
    }
    ok(&s)
}

/// `<paired>` first, as Moonlight reads pairing answers.
pub fn paired(inner: &str) -> String {
    ok(&format!("<paired>1</paired>{inner}"))
}

pub fn not_paired() -> String {
    ok("<paired>0</paired>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_what_xml_needs() {
        assert_eq!(escape("a<b>&\"c'"), "a&lt;b&gt;&amp;&quot;c&apos;");
        assert_eq!(escape("tab\tok\u{1}gone"), "tab\tokgone");
    }

    #[test]
    fn serverinfo_shape() {
        let xml = server_info(&ServerInfo {
            hostname: "Cha & Co",
            unique_id: "ABCDEF0123456789ABCDEF0123456789",
            https_port: 47984,
            http_port: 47989,
            mac: "00:00:00:00:00:00",
            local_ip: "192.168.1.5",
            codec_mode_support: 0x101,
            paired: true,
            current_game: 7,
        });
        assert_eq!(
            xml,
            "<root status_code=\"200\"><hostname>Cha &amp; Co</hostname><appversion>7.1.431.-1</appversion>\
             <GfeVersion>3.23.0.74</GfeVersion><uniqueid>ABCDEF0123456789ABCDEF0123456789</uniqueid><HttpsPort>47984</HttpsPort>\
             <ExternalPort>47989</ExternalPort><mac>00:00:00:00:00:00</mac><MaxLumaPixelsHEVC>1869449984</MaxLumaPixelsHEVC>\
             <LocalIP>192.168.1.5</LocalIP><ServerCodecModeSupport>257</ServerCodecModeSupport>\
             <SupportedDisplayMode></SupportedDisplayMode><PairStatus>1</PairStatus><currentgame>7</currentgame>\
             <state>CHA_SERVER_BUSY</state></root>"
        );
    }

    #[test]
    fn applist_shape() {
        let xml = app_list(&[
            App {
                id: 1,
                title: "Desktop <1>".into(),
                hdr: false,
            },
            App {
                id: 2,
                title: "Steam".into(),
                hdr: true,
            },
        ]);
        assert_eq!(
            xml,
            "<root status_code=\"200\"><App><IsHdrSupported>0</IsHdrSupported><AppTitle>Desktop &lt;1&gt;</AppTitle><ID>1</ID></App>\
             <App><IsHdrSupported>1</IsHdrSupported><AppTitle>Steam</AppTitle><ID>2</ID></App></root>"
        );
        assert_eq!(app_list(&[]), "<root status_code=\"200\"></root>");
    }

    #[test]
    fn codec_mask_follows_capabilities() {
        let caps = Capabilities {
            codecs: vec![VideoCodec::H264, VideoCodec::Hevc],
            hdr: true,
            yuv444: false,
        };
        assert_eq!(codec_mode_support(&caps), 0x1 | 0x100 | 0x200);
        let caps = Capabilities {
            codecs: vec![VideoCodec::Av1],
            hdr: false,
            yuv444: true,
        };
        assert_eq!(codec_mode_support(&caps), 0x1_0000 | 0x4_0000);
        assert_eq!(codec_mode_support(&Capabilities::default()), 0);
    }

    #[test]
    fn errors_are_http_200_documents() {
        assert_eq!(
            error(503, "no <room>"),
            "<root status_code=\"503\" status_message=\"no &lt;room&gt;\"></root>"
        );
        assert_eq!(
            paired("<x>1</x>"),
            "<root status_code=\"200\"><paired>1</paired><x>1</x></root>"
        );
    }
}

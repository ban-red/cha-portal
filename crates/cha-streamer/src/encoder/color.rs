//! The colour every encoder says its pictures are in, kept in one table: the
//! H.273 code points for the H.264 and HEVC VUI and the AV1 colour config
//! that Cha writes itself, the same values as x264's and SVT-AV1's options,
//! and NVENC's VUI (docs/plans/hdr-and-vrr.md, step 2.1).
//!
//! The compositor's pixels are sRGB-encoded with BT.709 primaries, and every
//! encoder turns them into limited-range YCbCr without changing the curve.
//! What the streams say about that curve differs today: NVENC tags the sRGB
//! transfer (13), because with BT.709's (1) Chrome on a Mac showed midtones
//! about 11 levels too light (`cha-nvenc`), while the VA-API and CPU encoders
//! still tag 1. Which is right for them is to be measured before it changes
//! (the colour patches of step 2.2), so the defaults are today's and two
//! variables let a node's owner try the others without a rebuild:
//! `CHA_SDR_TRANSFER=srgb|bt709` and `CHA_SDR_MATRIX=bt709|bt601`. They apply
//! to every encoder of the streamer.

use std::sync::OnceLock;

use tracing::warn;

use super::Backend;

/// H.273 code points Cha writes.
pub mod code {
    /// Primaries, transfer and matrix: BT.709.
    pub const BT709: u8 = 1;
    /// Matrix: SMPTE 170M, BT.601's (5, BT.470BG, has the same coefficients).
    pub const BT601: u8 = 6;
    /// Transfer: IEC 61966-2-1, sRGB.
    pub const SRGB: u8 = 13;
}

/// What a stream says its pictures' colour is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorTags {
    pub primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub full_range: bool,
}

impl ColorTags {
    /// BT.709 primaries, transfer and matrix, limited range.
    pub const BT709: Self = Self {
        primaries: code::BT709,
        transfer: code::BT709,
        matrix: code::BT709,
        full_range: false,
    };

    /// What `backend` tags SDR pictures with before any override: today's.
    pub const fn default_for(backend: Backend) -> Self {
        match backend {
            Backend::Nvenc => Self {
                transfer: code::SRGB,
                ..Self::BT709
            },
            Backend::X264 | Backend::SvtAv1 | Backend::Vaapi => Self::BT709,
        }
    }

    fn with(self, overrides: Overrides) -> Self {
        Self {
            transfer: overrides.transfer.unwrap_or(self.transfer),
            matrix: overrides.matrix.unwrap_or(self.matrix),
            ..self
        }
    }

    /// x264's names for these (`colorprim`, `transfer`, `colormatrix`,
    /// `fullrange`).
    pub fn x264(self) -> [(&'static str, String); 4] {
        let name = |code: u8, names: &[(u8, &str)]| {
            names
                .iter()
                .find(|(c, _)| *c == code)
                .map_or("undef", |(_, n)| *n)
                .to_string()
        };
        [
            ("colorprim", name(self.primaries, &[(code::BT709, "bt709")])),
            (
                "transfer",
                name(
                    self.transfer,
                    &[(code::BT709, "bt709"), (code::SRGB, "iec61966-2-1")],
                ),
            ),
            (
                "colormatrix",
                name(
                    self.matrix,
                    &[(code::BT709, "bt709"), (code::BT601, "smpte170m")],
                ),
            ),
            (
                "fullrange",
                if self.full_range { "on" } else { "off" }.into(),
            ),
        ]
    }

    /// SVT-AV1's options for these (its code points as numbers).
    pub fn svtav1(self) -> [(&'static str, String); 4] {
        [
            ("color-primaries", self.primaries.to_string()),
            ("transfer-characteristics", self.transfer.to_string()),
            ("matrix-coefficients", self.matrix.to_string()),
            ("color-range", u8::from(self.full_range).to_string()),
        ]
    }

    /// The same for `cha-nvenc`'s VUI and AV1 colour config.
    pub fn nvenc(self) -> cha_nvenc::ColorDescription {
        cha_nvenc::ColorDescription {
            primaries: self.primaries.into(),
            transfer: self.transfer.into(),
            matrix: self.matrix.into(),
            full_range: self.full_range,
        }
    }
}

/// The overrides from the environment: a code point, or none.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Overrides {
    transfer: Option<u8>,
    matrix: Option<u8>,
}

impl Overrides {
    fn parse(transfer: Option<&str>, matrix: Option<&str>) -> Self {
        let pick = |var: &str, value: Option<&str>, names: &[(&str, u8)]| {
            let value = value?.trim();
            let found = names.iter().find(|(n, _)| n.eq_ignore_ascii_case(value));
            if found.is_none() {
                let known: Vec<&str> = names.iter().map(|(n, _)| *n).collect();
                warn!(%var, %value, ?known, "not a value this streamer knows; ignored");
            }
            found.map(|(_, c)| *c)
        };
        Self {
            transfer: pick(
                "CHA_SDR_TRANSFER",
                transfer,
                &[("srgb", code::SRGB), ("bt709", code::BT709)],
            ),
            matrix: pick(
                "CHA_SDR_MATRIX",
                matrix,
                &[("bt709", code::BT709), ("bt601", code::BT601)],
            ),
        }
    }
}

/// The tags `backend` writes into an SDR stream, with the environment's
/// overrides (read once).
pub fn sdr(backend: Backend) -> ColorTags {
    static OVERRIDES: OnceLock<Overrides> = OnceLock::new();
    let overrides = *OVERRIDES.get_or_init(|| {
        let var = |name| std::env::var(name).ok();
        Overrides::parse(
            var("CHA_SDR_TRANSFER").as_deref(),
            var("CHA_SDR_MATRIX").as_deref(),
        )
    });
    ColorTags::default_for(backend).with(overrides)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_what_each_encoder_wrote_before() {
        let nvenc = ColorTags::default_for(Backend::Nvenc);
        assert_eq!((nvenc.primaries, nvenc.transfer, nvenc.matrix), (1, 13, 1));
        for backend in [Backend::X264, Backend::SvtAv1, Backend::Vaapi] {
            assert_eq!(ColorTags::default_for(backend), ColorTags::BT709);
        }
        assert!(!nvenc.full_range);
    }

    /// Through every writer, the defaults give the bytes each wrote before
    /// this table existed (so this change alters no stream).
    #[test]
    fn every_writer_still_writes_what_it_did() {
        use crate::encoder::bitstream::{av1, h264, h265};
        // The VA-API H.264 and HEVC VUI, and the AV1 sequence header.
        let vaapi = ColorTags::default_for(Backend::Vaapi);
        assert_eq!(
            h264::Vui::bt709_low_latency(60, 1).with_color(vaapi),
            h264::Vui::bt709_low_latency(60, 1)
        );
        assert_eq!(h265::Vui::bt709(60).with_color(vaapi), h265::Vui::bt709(60));
        let mut seq = av1::Seq::new(1920, 1080);
        let before = av1::sequence_header_payload(&seq);
        seq.color = vaapi;
        assert_eq!(av1::sequence_header_payload(&seq), before);
        // NVENC's VUI and AV1 colour config: BT.709, the sRGB transfer, limited.
        assert_eq!(
            ColorTags::default_for(Backend::Nvenc).nvenc(),
            cha_nvenc::ColorDescription::default()
        );
        // x264 and SVT-AV1: the option values they were given before.
        assert_eq!(
            ColorTags::default_for(Backend::X264).x264().map(|(_, v)| v),
            ["bt709", "bt709", "bt709", "off"]
        );
        assert_eq!(
            ColorTags::default_for(Backend::SvtAv1)
                .svtav1()
                .map(|(_, v)| v),
            ["1", "1", "1", "0"]
        );
    }

    #[test]
    fn a_tag_reaches_the_bytes() {
        use crate::encoder::bitstream::av1;
        let mut seq = av1::Seq::new(1280, 720);
        let bt709 = av1::sequence_header_payload(&seq);
        seq.color = ColorTags {
            transfer: code::SRGB,
            ..ColorTags::BT709
        };
        let srgb = av1::sequence_header_payload(&seq);
        assert_ne!(srgb, bt709);
        assert_eq!(srgb.len(), bt709.len());
    }

    #[test]
    fn the_overrides_change_only_what_they_name() {
        let o = Overrides::parse(Some("srgb"), None);
        assert_eq!(
            ColorTags::default_for(Backend::Vaapi).with(o),
            ColorTags {
                transfer: code::SRGB,
                ..ColorTags::BT709
            }
        );
        let o = Overrides::parse(Some(" BT709 "), Some("bt601"));
        let nvenc = ColorTags::default_for(Backend::Nvenc).with(o);
        assert_eq!((nvenc.transfer, nvenc.matrix), (code::BT709, code::BT601));
        // Unknown values change nothing.
        let o = Overrides::parse(Some("pq"), Some("rgb"));
        assert_eq!(o, Overrides::default());
    }

    #[test]
    fn the_cpu_encoders_names_for_the_tags() {
        let x264 = ColorTags::BT709.x264();
        assert_eq!(
            x264.map(|(k, v)| format!("{k}={v}")),
            [
                "colorprim=bt709",
                "transfer=bt709",
                "colormatrix=bt709",
                "fullrange=off"
            ]
        );
        let srgb_601 = ColorTags {
            transfer: code::SRGB,
            matrix: code::BT601,
            ..ColorTags::BT709
        };
        assert_eq!(srgb_601.x264()[1].1, "iec61966-2-1");
        assert_eq!(srgb_601.x264()[2].1, "smpte170m");
        assert_eq!(
            ColorTags::BT709.svtav1().map(|(k, v)| format!("{k}={v}")),
            [
                "color-primaries=1",
                "transfer-characteristics=1",
                "matrix-coefficients=1",
                "color-range=0"
            ]
        );
    }
}

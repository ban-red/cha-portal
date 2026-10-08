//! What the toolbar remembers per app: the native twin of the portal's
//! `toolbarPrefs.ts`, which keeps it per template in the browser. Here an app
//! is the transport, the host and the app id, and the settings live in
//! `config.json` under `toolbar`.
//!
//! Every field is optional (never picked is not the same as picked). The fields and their limits
//! are `prefs.json`'s `toolbar` group, shared with the browser; [`StreamPrefs::from_json`] reads
//! them through the spec's validator, so a malformed field is dropped on its own and a hand edit
//! can't lose the rest of the config.

use std::collections::BTreeMap;

use cha_ui_spec::health::Platform;
use cha_ui_spec::prefs::{self, GroupName};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// One app's toolbar settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamPrefs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub muted: Option<bool>,
    /// 0 to 100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
    /// False: the mouse was switched off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mouse: Option<bool>,
    /// The frame rate asked for at launch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// The Steam Gamescope overlay level, 0 to 4.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay: Option<u8>,
}

/// The per-app map, keeping the entries that are objects.
pub fn lenient_map<'de, D>(d: D) -> Result<BTreeMap<String, StreamPrefs>, D::Error>
where
    D: Deserializer<'de>,
{
    let Value::Object(raw) = Value::deserialize(d)? else {
        return Ok(BTreeMap::new());
    };
    Ok(raw
        .into_iter()
        .filter(|(_, value)| value.is_object())
        .map(|(key, value)| (key, StreamPrefs::from_json(&value)))
        .collect())
}

impl StreamPrefs {
    /// Saved settings from parsed JSON: each field kept only if it is valid (`prefs.json`).
    pub fn from_json(saved: &Value) -> Self {
        let valid = prefs::parse(GroupName::Toolbar, saved, Platform::Native);
        serde_json::from_value(Value::Object(valid))
            .expect("prefs.json's toolbar fields are StreamPrefs's")
    }

    /// Values in the spec's range: a volume is brought into it, a frame rate or overlay level
    /// outside it is dropped.
    pub fn sanitized(self) -> Self {
        Self::from_json(&serde_json::to_value(&self).unwrap_or(Value::Null))
    }

    /// `patch`'s fields over these (a field it leaves `None` is kept).
    pub fn merge(&mut self, patch: &StreamPrefs) {
        let patch = patch.clone().sanitized();
        self.muted = patch.muted.or(self.muted);
        self.volume = patch.volume.or(self.volume);
        self.mouse = patch.mouse.or(self.mouse);
        self.fps = patch.fps.or(self.fps);
        self.overlay = patch.overlay.or(self.overlay);
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Who a setting belongs to: the transport, the host and the app on it. The
/// transport is its name ("Cha Portal"), which stays put where its index in
/// the list could move.
pub fn app_key(transport: &str, host: &str, app: u32) -> String {
    format!("{transport}|{host}|{app}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> StreamPrefs {
        StreamPrefs::from_json(&serde_json::from_str(text).unwrap_or(Value::Null))
    }

    #[test]
    fn a_field_is_kept_only_if_it_is_well_formed() {
        let p = parse(r#"{"muted": true, "volume": "loud", "mouse": 1, "fps": 90, "overlay": 7}"#);
        assert_eq!(p.muted, Some(true));
        assert_eq!(p.volume, None);
        assert_eq!(p.mouse, None);
        assert_eq!(p.fps, Some(90));
        assert_eq!(p.overlay, None, "past the full level");
        assert_eq!(parse(r#"{"volume": 250}"#).volume, Some(100));
        // A volume is brought into 0..=100 (as the browser does), a rate is not.
        assert_eq!(parse(r#"{"volume": -3}"#).volume, Some(0));
        assert_eq!(parse(r#"{"volume": 33.6}"#).volume, Some(34));
        assert_eq!(parse(r#"{"fps": 0}"#).fps, None);
        assert!(parse("{}").is_empty());
    }

    #[test]
    fn whatever_was_saved_loads() {
        for text in ["", "{nope", "null", "[]", "7"] {
            assert!(parse(text).is_empty(), "{text}");
        }
        // The browser's codec and transport aren't kept here.
        assert!(parse(r#"{"codec": "hevc", "transport": "auto"}"#).is_empty());
    }

    #[test]
    fn the_shared_cases() {
        use cha_ui_spec::prefs_cases::{cases, normalised};
        let mut run = 0;
        for c in cases()
            .iter()
            .filter(|c| c.group == GroupName::Toolbar && c.runs_on(Platform::Native))
        {
            let saved = c.saved_text().unwrap_or_default();
            let got = parse(&saved);
            assert_eq!(
                normalised(&serde_json::to_value(&got).unwrap()),
                Value::Object(c.expected(Platform::Native)),
                "{}: {saved}",
                c.name
            );
            run += 1;
        }
        assert!(run >= 10);
    }

    #[test]
    fn a_patch_changes_only_what_it_names() {
        let mut saved = StreamPrefs {
            muted: Some(false),
            volume: Some(40),
            fps: Some(60),
            ..StreamPrefs::default()
        };
        saved.merge(&StreamPrefs {
            volume: Some(80),
            mouse: Some(false),
            overlay: Some(9),
            ..StreamPrefs::default()
        });
        assert_eq!(
            saved,
            StreamPrefs {
                muted: Some(false),
                volume: Some(80),
                mouse: Some(false),
                fps: Some(60),
                overlay: None,
            }
        );
    }

    #[test]
    fn only_what_was_picked_is_written() {
        let p = StreamPrefs {
            muted: Some(true),
            ..StreamPrefs::default()
        };
        assert_eq!(serde_json::to_string(&p).unwrap(), r#"{"muted":true}"#);
    }

    #[test]
    fn the_map_survives_a_bad_entry() {
        #[derive(Deserialize)]
        struct Holder {
            #[serde(default, deserialize_with = "lenient_map")]
            toolbar: BTreeMap<String, StreamPrefs>,
        }
        let h: Holder = serde_json::from_str(
            r#"{"toolbar": {"a|b|1": {"volume": 30}, "x|y|2": "nope", "c|d|3": {"fps": 120, "volume": "x"}}}"#,
        )
        .unwrap();
        assert_eq!(h.toolbar.len(), 2);
        assert_eq!(h.toolbar["a|b|1"].volume, Some(30));
        assert_eq!(h.toolbar["c|d|3"].fps, Some(120));
        let none: Holder = serde_json::from_str(r#"{"toolbar": 5}"#).unwrap();
        assert!(none.toolbar.is_empty());
    }

    #[test]
    fn keys_name_the_transport_host_and_app() {
        assert_eq!(
            app_key("Cha Portal", "portal.lan", 3),
            "Cha Portal|portal.lan|3"
        );
    }
}

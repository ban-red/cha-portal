//! What the toolbar remembers per app: the native twin of the portal's
//! `toolbarPrefs.ts`, which keeps it per template in the browser. Here an app
//! is the transport, the host and the app id, and the settings live in
//! `config.json` under `toolbar`.
//!
//! Every field is optional (never picked is not the same as picked), and one
//! that is malformed is dropped on its own, so a hand edit can't lose the rest
//! of the config.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};

/// Frame rates the streamer offers (`FPS_CHOICES` in `cha-client-stream`).
pub const FRAME_RATES: [u32; 3] = [60, 90, 120];
/// The highest performance overlay level (4 is the full one).
pub const OVERLAY_MAX: u8 = 4;

/// One app's toolbar settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamPrefs {
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub muted: Option<bool>,
    /// 0 to 100.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub volume: Option<u8>,
    /// False: the mouse was switched off.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub mouse: Option<bool>,
    /// The frame rate asked for at launch.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub fps: Option<u32>,
    /// The Steam Gamescope overlay level, 0 to 4.
    #[serde(skip_serializing_if = "Option::is_none", deserialize_with = "lenient")]
    pub overlay: Option<u8>,
}

/// A field that fails to parse is `None`, not an error.
fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

/// The per-app map, keeping the entries that are objects.
pub fn lenient_map<'de, D>(d: D) -> Result<BTreeMap<String, StreamPrefs>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(d)?;
    let serde_json::Value::Object(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    Ok(raw
        .into_iter()
        .filter_map(|(key, value)| {
            let prefs: StreamPrefs = serde_json::from_value(value).ok()?;
            Some((key, prefs.sanitized()))
        })
        .collect())
}

impl StreamPrefs {
    /// Values in a range the player can use; out of range ones are dropped.
    pub fn sanitized(mut self) -> Self {
        self.volume = self.volume.map(|v| v.min(100));
        self.fps = self.fps.filter(|f| (1..=240).contains(f));
        self.overlay = self.overlay.filter(|l| *l <= OVERLAY_MAX);
        self
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
        serde_json::from_str::<StreamPrefs>(text)
            .unwrap()
            .sanitized()
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
        assert_eq!(parse(r#"{"volume": -3}"#).volume, None);
        assert_eq!(parse(r#"{"fps": 0}"#).fps, None);
        assert!(parse("{}").is_empty());
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

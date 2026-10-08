//! The player's saved settings, and where its files live.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cha_client::{Codec, StreamConfig};
use serde::{Deserialize, Serialize};

use crate::overlay_prefs::OverlayPrefs;
use crate::stream_prefs::StreamPrefs;
use crate::theme::ThemePrefs;

/// `~/Library/Application Support/Cha Player`: settings, and whatever a
/// transport keeps (its client identity and paired hosts).
pub fn data_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Application Support/Cha Player"))
}

/// What this Mac is called ("Alex's MacBook Pro"), for the portal's Devices
/// page.
#[cfg(feature = "portal")]
pub fn device_name() -> String {
    std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Mac".into())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Most preferred first.
    pub codecs: Vec<Codec>,
    /// Send Cmd as Ctrl, as the browser player does (Cmd+C copies in a Linux
    /// app); off sends it as Super.
    pub command_as_control: bool,
    /// Colours, appearance and UI scale.
    pub theme: ThemePrefs,
    /// The stats panel: shown or hidden, folded, where it sits, how opaque.
    #[serde(deserialize_with = "crate::overlay_prefs::lenient")]
    pub overlay: OverlayPrefs,
    /// The toolbar's choices per app (see [`crate::stream_prefs::app_key`]).
    #[serde(deserialize_with = "crate::stream_prefs::lenient_map")]
    pub toolbar: BTreeMap<String, StreamPrefs>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 2560,
            height: 1440,
            fps: 60,
            bitrate_kbps: 80_000,
            codecs: vec![Codec::Hevc, Codec::H264],
            command_as_control: true,
            theme: ThemePrefs::default(),
            overlay: OverlayPrefs::default(),
            toolbar: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Self {
        let read = || -> Result<Self> {
            let text = std::fs::read_to_string(dir.join("config.json"))?;
            Ok(serde_json::from_str(&text)?)
        };
        match read() {
            Ok(config) => config.sanitized(),
            Err(e) => {
                if dir.join("config.json").exists() {
                    tracing::warn!("config.json unreadable, using defaults: {e:#}");
                }
                Self::default()
            }
        }
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, dir.join("config.json"))?;
        Ok(())
    }

    /// Keep hand-edited values in a range a host can serve.
    fn sanitized(mut self) -> Self {
        let defaults = Self::default();
        self.width = self.width.clamp(320, 7680) & !1;
        self.height = self.height.clamp(240, 4320) & !1;
        if self.fps == 0 || self.fps > 240 {
            self.fps = defaults.fps;
        }
        self.bitrate_kbps = self.bitrate_kbps.clamp(1_000, 500_000);
        self.theme = self.theme.sanitized();
        self.overlay = self.overlay.sanitized();
        self.codecs.retain(|c| *c != Codec::Av1);
        if self.codecs.is_empty() {
            self.codecs = defaults.codecs;
        }
        self
    }

    /// What to ask a host for when launching the app `key`: the settings,
    /// with the frame rate the toolbar last picked for that app.
    pub fn stream_config_for(&self, key: &str) -> StreamConfig {
        let mut config = self.stream_config();
        if let Some(fps) = self.toolbar.get(key).and_then(|p| p.fps) {
            config.fps = fps;
        }
        config
    }

    pub fn stream_config(&self) -> StreamConfig {
        StreamConfig {
            width: self.width,
            height: self.height,
            fps: self.fps,
            bitrate_kbps: self.bitrate_kbps,
            codecs: self.codecs.clone(),
            audio_channels: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_repairs() {
        let dir = std::env::temp_dir().join(format!("cha-player-config-{}", std::process::id()));
        let config = Config {
            fps: 120,
            ..Config::default()
        };
        config.save(&dir).unwrap();
        assert_eq!(Config::load(&dir), config);

        std::fs::write(
            dir.join("config.json"),
            r#"{"fps": 0, "width": 1921, "codecs": ["av1"]}"#,
        )
        .unwrap();
        let repaired = Config::load(&dir);
        assert_eq!(repaired.fps, 60);
        assert_eq!(repaired.width, 1920);
        assert_eq!(repaired.codecs, vec![Codec::Hevc, Codec::H264]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn config_without_a_theme_key_gets_the_default_theme() {
        let old = r#"{"width": 1920, "height": 1080, "fps": 60, "bitrate_kbps": 50000,
                      "codecs": ["h264"], "command_as_control": false}"#;
        let config: Config = serde_json::from_str(old).unwrap();
        assert_eq!(config.theme, ThemePrefs::default());
        assert_eq!(config.overlay, OverlayPrefs::default());
        assert!(config.toolbar.is_empty());
        assert_eq!(config.theme.theme, "cha-magenta");
        assert_eq!(config.theme.scale, 1.0);
        assert!(!config.command_as_control);
    }

    #[test]
    fn a_broken_overlay_or_toolbar_never_fails_the_load() {
        let junk: Config = serde_json::from_str(r#"{"overlay": "junk", "toolbar": 5}"#).unwrap();
        assert_eq!(junk.overlay, OverlayPrefs::default());
        let partly: Config = serde_json::from_str(
            r#"{"fps": 90, "overlay": {"corner": "middle", "compact": true, "opacity": 12}}"#,
        )
        .unwrap();
        assert_eq!(partly.fps, 90);
        assert!(partly.overlay.compact);
        assert_eq!(partly.overlay.opacity, crate::overlay_prefs::OPACITY_MIN);
        assert_eq!(partly.overlay.corner, OverlayPrefs::default().corner);
    }

    #[test]
    fn toolbar_choices_round_trip_and_set_the_launch_rate() {
        let dir =
            std::env::temp_dir().join(format!("cha-player-toolbar-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"fps": 60, "toolbar": {"Cha Portal|p.lan|2": {"fps": 120, "volume": 35, "muted": "x"},
                                       "bad": 3}}"#,
        )
        .unwrap();
        let config = Config::load(&dir);
        let key = "Cha Portal|p.lan|2";
        assert_eq!(config.toolbar.len(), 1);
        assert_eq!(config.toolbar[key].volume, Some(35));
        assert_eq!(config.toolbar[key].muted, None);
        assert_eq!(config.stream_config_for(key).fps, 120);
        assert_eq!(config.stream_config_for("other").fps, 60);
        config.save(&dir).unwrap();
        assert_eq!(Config::load(&dir), config);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn theme_prefs_round_trip_and_partial_keys_default() {
        let dir = std::env::temp_dir().join(format!("cha-player-theme-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"theme": {"theme": "cha-jade", "appearance": "light", "scale": 9.0}}"#,
        )
        .unwrap();
        let config = Config::load(&dir);
        assert_eq!(config.theme.theme, "cha-jade");
        assert_eq!(config.theme.appearance, crate::theme::Appearance::Light);
        assert_eq!(config.theme.contrast, crate::theme::Contrast::System);
        assert_eq!(config.theme.scale, 2.0);
        config.save(&dir).unwrap();
        assert_eq!(Config::load(&dir), config);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn pyrowave_orders_survive_and_are_spelled_as_the_portal_does() {
        let config = Config {
            codecs: vec![Codec::PyroWave444, Codec::Hevc, Codec::H264],
            ..Config::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            json.contains(r#""codecs":["pyrowave444","hevc","h264"]"#),
            "{json}"
        );
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sanitized().codecs, config.codecs);
        // The default stays HEVC first.
        assert_eq!(Config::default().codecs[0], Codec::Hevc);
    }
}

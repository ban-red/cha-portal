//! The player's saved settings, and where its files live.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cha_client::{Codec, StreamConfig};
use serde::{Deserialize, Serialize};

/// `~/Library/Application Support/Cha Player`: settings, and whatever a
/// transport keeps (its client identity and paired hosts).
pub fn data_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Application Support/Cha Player"))
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
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: 2560,
            height: 1440,
            fps: 60,
            bitrate_kbps: 80_000,
            codecs: vec![Codec::Hevc, Codec::H264],
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
        self.codecs.retain(|c| *c != Codec::Av1);
        if self.codecs.is_empty() {
            self.codecs = defaults.codecs;
        }
        self
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
}

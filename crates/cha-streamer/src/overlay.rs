//! The app's performance overlay (MangoHud), set from the page.
//!
//! An app that has an overlay keeps its config in `mangohud.conf` in the
//! shared runtime dir (the Steam image points `MANGOHUD_CONFIGFILE` at it);
//! mangoapp re-reads the file whenever it's replaced. The file existing is
//! what says the app has an overlay: other apps don't have it, and the
//! streamer never creates it.
//! - `no_display` is level 0, off.
//! - `mangoapp_steam` and `preset=N` (N in 1..=4: FPS only, horizontal bar,
//!   extended, full; the user may redefine them in `presets.conf`) is level N.
//! - Anything else is the user's own config: reported as `"custom"`, and the
//!   page can still replace it with a level.
//!
//! A level is set by writing a temp file in the same dir and renaming it over
//! the config, so mangoapp never reads half of one. The page learns the level
//! from the hello and from every stats line (`overlay`, a number or
//! `"custom"`, left out when there's no file).

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Serialize, Serializer};

/// A config is a few lines; a bigger file is somebody's own.
const MAX_BYTES: usize = 4096;
/// The highest preset.
const MAX_LEVEL: u8 = 4;

/// What the config says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// 0 (off) to 4 (full).
    Preset(u8),
    /// A config we didn't write.
    Custom,
}

impl Serialize for Level {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Level::Preset(n) => s.serialize_u8(*n),
            Level::Custom => s.serialize_str("custom"),
        }
    }
}

fn parse(text: &str) -> Level {
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    match (lines.next(), lines.next(), lines.next()) {
        (Some("no_display"), None, None) => Level::Preset(0),
        (Some("mangoapp_steam"), Some(preset), None) => preset
            .strip_prefix("preset=")
            .and_then(|n| n.trim().parse::<u8>().ok())
            .filter(|n| (1..=MAX_LEVEL).contains(n))
            .map_or(Level::Custom, Level::Preset),
        _ => Level::Custom,
    }
}

/// The overlay config of the environment's app, if it has one.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    path: Option<PathBuf>,
    app_uid: Option<u32>,
}

impl Overlay {
    /// Follows `path`; new files go to `app_uid` when it's known.
    pub fn at(path: PathBuf, app_uid: Option<u32>) -> Self {
        Self {
            path: Some(path),
            app_uid,
        }
    }

    /// The level now, or None when the app has no overlay (no file).
    pub fn level(&self) -> Option<Level> {
        let mut file = File::open(self.path.as_ref()?).ok()?;
        let mut bytes = Vec::new();
        // One byte past the limit tells a file that's too big from one that fills it.
        Read::take(&mut file, MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_BYTES {
            return Some(Level::Custom);
        }
        Some(parse(&String::from_utf8_lossy(&bytes)))
    }

    /// Sets level 0..=4 and returns it. Refused when the app has no overlay.
    pub fn set(&self, level: u8) -> Result<u8, String> {
        if level > MAX_LEVEL {
            return Err(format!(
                "overlay level {level} isn't one of 0 to {MAX_LEVEL}"
            ));
        }
        let no_overlay = || "this app has no performance overlay".to_string();
        let path = self.path.as_ref().ok_or_else(no_overlay)?;
        if self.level().is_none() {
            return Err(no_overlay());
        }
        let text = if level == 0 {
            "no_display\n".to_string()
        } else {
            format!("mangoapp_steam\npreset={level}\n")
        };
        self.replace(path, &text)
            .map_err(|err| format!("couldn't write the overlay config: {err}"))?;
        Ok(level)
    }

    fn replace(&self, path: &Path, text: &str) -> std::io::Result<()> {
        let tmp = path.with_extension("conf.tmp");
        let written = (|| {
            let mut file = File::create(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.set_permissions(std::fs::Permissions::from_mode(0o644))?;
            if let Some(uid) = self.app_uid {
                std::os::unix::fs::chown(&tmp, Some(uid), Some(uid))?;
            }
            std::fs::rename(&tmp, path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("cha-overlay-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn conf(&self) -> PathBuf {
            self.0.join("mangohud.conf")
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_config_is_read_as_a_level_or_custom() {
        assert_eq!(parse("no_display\n"), Level::Preset(0));
        assert_eq!(parse("mangoapp_steam\npreset=3\n"), Level::Preset(3));
        assert_eq!(
            parse("# ours\nmangoapp_steam\n\npreset=1"),
            Level::Preset(1)
        );
        assert_eq!(parse("mangoapp_steam\npreset=5\n"), Level::Custom);
        assert_eq!(parse("mangoapp_steam\npreset=0\n"), Level::Custom);
        assert_eq!(parse("mangoapp_steam\n"), Level::Custom);
        assert_eq!(parse("fps\ngpu_stats\n"), Level::Custom);
        assert_eq!(parse("no_display\nfps\n"), Level::Custom);
        assert_eq!(parse(""), Level::Custom);
    }

    #[test]
    fn a_level_serializes_as_a_number_or_custom() {
        assert_eq!(serde_json::to_string(&Level::Preset(2)).unwrap(), "2");
        assert_eq!(serde_json::to_string(&Level::Custom).unwrap(), "\"custom\"");
    }

    #[test]
    fn an_app_without_the_file_has_no_overlay_and_never_gets_one() {
        let dir = Dir::new("absent");
        let overlay = Overlay::at(dir.conf(), None);
        assert_eq!(overlay.level(), None);
        assert!(overlay.set(2).is_err());
        assert!(!dir.conf().exists());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
        assert_eq!(Overlay::default().level(), None);
        assert!(Overlay::default().set(1).is_err());
    }

    #[test]
    fn levels_write_the_exact_formats() {
        let dir = Dir::new("set");
        std::fs::write(dir.conf(), "no_display\n").unwrap();
        let overlay = Overlay::at(dir.conf(), None);
        assert_eq!(overlay.set(3), Ok(3));
        assert_eq!(
            std::fs::read_to_string(dir.conf()).unwrap(),
            "mangoapp_steam\npreset=3\n"
        );
        assert_eq!(overlay.level(), Some(Level::Preset(3)));
        assert_eq!(overlay.set(0), Ok(0));
        assert_eq!(std::fs::read_to_string(dir.conf()).unwrap(), "no_display\n");
        let mode = std::fs::metadata(dir.conf()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
        // No temp file is left beside it.
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn a_level_out_of_range_changes_nothing() {
        let dir = Dir::new("range");
        std::fs::write(dir.conf(), "no_display\n").unwrap();
        let overlay = Overlay::at(dir.conf(), None);
        assert!(overlay.set(5).is_err());
        assert_eq!(overlay.level(), Some(Level::Preset(0)));
    }

    #[test]
    fn someone_elses_config_is_custom_until_a_level_replaces_it() {
        let dir = Dir::new("custom");
        std::fs::write(dir.conf(), "fps\ngpu_stats\n").unwrap();
        let overlay = Overlay::at(dir.conf(), None);
        assert_eq!(overlay.level(), Some(Level::Custom));
        assert_eq!(overlay.set(1), Ok(1));
        assert_eq!(overlay.level(), Some(Level::Preset(1)));
    }
}

//! Fonts: the system's by default (SF Pro and SF Mono, as the portal's
//! `system-ui` and `ui-monospace` resolve on a Mac), or the files a theme
//! names. Font files are never bundled; egui's built-in fonts stay behind
//! them as fallbacks for glyphs they lack (and for everything if a file
//! can't be used).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use egui::{FontData, FontDefinitions, FontFamily};

pub const SYSTEM_SANS: &str = "/System/Library/Fonts/SFNS.ttf";
pub const SYSTEM_MONO: &str = "/System/Library/Fonts/SFNSMono.ttf";

/// Largest font file read (a CJK family is far less than this).
const MAX_FONT_BYTES: u64 = 64 << 20;

/// Which font files a theme asks for. `None` is the system font.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FontChoice {
    pub proportional: Option<PathBuf>,
    pub monospace: Option<PathBuf>,
}

/// Font definitions to give egui, and what went wrong on the way.
pub struct FontSetup {
    pub definitions: FontDefinitions,
    pub warnings: Vec<String>,
}

/// Build the definitions for `choice`: each face that loads goes first in
/// its family; one that doesn't is skipped with a warning.
pub fn setup(choice: &FontChoice) -> FontSetup {
    let mut definitions = FontDefinitions::default();
    let mut warnings = Vec::new();
    for (family, name, path, system_default) in [
        (
            FontFamily::Proportional,
            "theme-proportional",
            choice.proportional.clone(),
            SYSTEM_SANS,
        ),
        (
            FontFamily::Monospace,
            "theme-monospace",
            choice.monospace.clone(),
            SYSTEM_MONO,
        ),
    ] {
        // A font the theme named must work; the system default may be
        // absent (another OS) without a word.
        let named = path.is_some();
        let path = path.unwrap_or_else(|| PathBuf::from(system_default));
        match load(&path) {
            Ok(data) => {
                definitions
                    .font_data
                    .insert(name.to_owned(), std::sync::Arc::new(data));
                definitions
                    .families
                    .entry(family)
                    .or_default()
                    .insert(0, name.to_owned());
            }
            Err(_) if !named && !path.exists() => {}
            Err(e) => {
                let message = format!("font {}: {e}; using egui's built-in font", path.display());
                tracing::warn!("{message}");
                warnings.push(message);
            }
        }
    }
    FontSetup {
        definitions,
        warnings,
    }
}

fn load(path: &Path) -> Result<FontData, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_FONT_BYTES {
        return Err("file is too large".into());
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if !looks_like_font(&bytes) {
        return Err("not a TrueType/OpenType file".into());
    }
    let data = FontData::from_owned(bytes);
    check_parses(&data)?;
    Ok(data)
}

/// sfnt magic: TrueType 1.0, `OTTO`, `true`, or a collection.
fn looks_like_font(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0, 1, 0, 0] | b"OTTO" | b"true" | b"ttcf")
    )
}

/// egui panics on a font it can't parse, at the first frame that lays text
/// out. Do that in a scratch context first, so a damaged file is a warning.
fn check_parses(data: &FontData) -> Result<(), String> {
    let mut definitions = FontDefinitions::default();
    definitions
        .font_data
        .insert("probe".into(), std::sync::Arc::new(data.clone()));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        definitions
            .families
            .entry(family)
            .or_default()
            .insert(0, "probe".into());
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let ctx = egui::Context::default();
        ctx.set_fonts(definitions);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("Aa 0");
            ui.monospace("Aa 0");
        });
    }));
    result.map_err(|_| "the file can't be parsed".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("cha-player-fonts-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn system_fonts_load_when_present() {
        if !Path::new(SYSTEM_SANS).exists() {
            return;
        }
        let setup = setup(&FontChoice::default());
        assert!(setup.warnings.is_empty(), "{:?}", setup.warnings);
        assert_eq!(
            setup.definitions.families[&FontFamily::Proportional][0],
            "theme-proportional"
        );
        // And egui can lay text out with them.
        let ctx = egui::Context::default();
        ctx.set_fonts(setup.definitions);
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("Cha Player 0123");
            ui.monospace("0123");
        });
    }

    #[test]
    fn broken_fonts_warn_and_fall_back() {
        let d = dir("broken");
        let garbage = d.join("garbage.ttf");
        std::fs::write(&garbage, b"not a font at all").unwrap();
        let truncated = d.join("truncated.ttf");
        std::fs::write(&truncated, [0u8, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        for file in [garbage, truncated, d.join("missing.ttf")] {
            let setup = setup(&FontChoice {
                proportional: Some(file.clone()),
                monospace: None,
            });
            assert!(
                setup
                    .warnings
                    .iter()
                    .any(|w| w.contains(&*file.to_string_lossy())),
                "{file:?}: {:?}",
                setup.warnings
            );
            assert!(
                !setup.definitions.families[&FontFamily::Proportional]
                    .contains(&"theme-proportional".to_string())
            );
        }
        std::fs::remove_dir_all(d).ok();
    }
}

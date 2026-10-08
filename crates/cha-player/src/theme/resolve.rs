//! Preferences against the system: which variant of which theme to show.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::fonts::FontChoice;
use super::metrics::Metrics;
use super::overlay::OverlayStyle;
use super::palette::Palette;
use super::registry::{DEFAULT_THEME, Registry};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Appearance {
    /// Follow macOS (light or dark).
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Contrast {
    /// Follow macOS "Increase contrast".
    #[default]
    System,
    Standard,
    More,
}

/// What the user chose in Settings; saved in `config.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemePrefs {
    /// A theme id from the registry.
    pub theme: String,
    pub appearance: Appearance,
    pub contrast: Contrast,
    /// UI scale (egui's zoom factor); 1.0 is the default size.
    pub scale: f32,
}

impl Default for ThemePrefs {
    fn default() -> Self {
        Self {
            theme: DEFAULT_THEME.into(),
            appearance: Appearance::System,
            contrast: Contrast::System,
            scale: 1.0,
        }
    }
}

pub const SCALE_RANGE: std::ops::RangeInclusive<f32> = 0.75..=2.0;

impl ThemePrefs {
    /// Keep a hand-edited scale in range.
    pub fn sanitized(mut self) -> Self {
        self.scale = if self.scale.is_finite() {
            self.scale.clamp(*SCALE_RANGE.start(), *SCALE_RANGE.end())
        } else {
            1.0
        };
        if self.theme.trim().is_empty() {
            self.theme = DEFAULT_THEME.into();
        }
        self
    }
}

/// What the system says, as far as it matters here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemEnv {
    pub dark: bool,
    pub increase_contrast: bool,
}

impl Default for SystemEnv {
    fn default() -> Self {
        // The portal is dark first; so is the player until the window says.
        Self {
            dark: true,
            increase_contrast: false,
        }
    }
}

/// A theme's four palettes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Variant {
    Dark,
    Light,
    DarkMore,
    LightMore,
}

impl Variant {
    pub const ALL: [Variant; 4] = [
        Variant::Dark,
        Variant::Light,
        Variant::DarkMore,
        Variant::LightMore,
    ];

    pub fn pick(dark: bool, more: bool) -> Self {
        match (dark, more) {
            (true, false) => Variant::Dark,
            (false, false) => Variant::Light,
            (true, true) => Variant::DarkMore,
            (false, true) => Variant::LightMore,
        }
    }

    pub fn is_dark(self) -> bool {
        matches!(self, Variant::Dark | Variant::DarkMore)
    }

    pub fn is_more(self) -> bool {
        matches!(self, Variant::DarkMore | Variant::LightMore)
    }
}

impl ThemePrefs {
    pub fn variant(&self, env: &SystemEnv) -> Variant {
        let dark = match self.appearance {
            Appearance::System => env.dark,
            Appearance::Dark => true,
            Appearance::Light => false,
        };
        let more = match self.contrast {
            Contrast::System => env.increase_contrast,
            Contrast::Standard => false,
            Contrast::More => true,
        };
        Variant::pick(dark, more)
    }
}

/// Everything [`super::apply`] needs: a theme chosen and resolved.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub theme_id: String,
    pub variant: Variant,
    pub palette: Arc<Palette>,
    pub metrics: Metrics,
    pub fonts: FontChoice,
    pub overlay: OverlayStyle,
    pub scale: f32,
    /// Set when the chosen theme was not found and the default stands in.
    pub fallback_from: Option<String>,
}

/// Resolve `prefs` against the registry and the system.
pub fn resolve(registry: &Registry, prefs: &ThemePrefs, env: &SystemEnv) -> Resolved {
    let prefs = prefs.clone().sanitized();
    let (theme, fallback_from) = match registry.get(&prefs.theme) {
        Some(t) => (t, None),
        None => (registry.default_theme(), Some(prefs.theme.clone())),
    };
    let variant = prefs.variant(env);
    let mut metrics = theme.metrics.clone();
    if variant.is_more() {
        // Contrast "more" also thickens borders.
        metrics.stroke_width *= 1.5;
        metrics.stroke_width_strong *= 1.5;
    }
    Resolved {
        theme_id: theme.id.clone(),
        variant,
        palette: Arc::new(theme.palette(variant).clone()),
        metrics,
        fonts: theme.fonts.clone(),
        overlay: OverlayStyle::from_palette(theme.palette(if variant.is_more() {
            Variant::DarkMore
        } else {
            Variant::Dark
        })),
        scale: prefs.scale,
        fallback_from,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(dark: bool, increase_contrast: bool) -> SystemEnv {
        SystemEnv {
            dark,
            increase_contrast,
        }
    }

    #[test]
    fn system_follows_the_environment() {
        let prefs = ThemePrefs::default();
        assert_eq!(prefs.variant(&env(true, false)), Variant::Dark);
        assert_eq!(prefs.variant(&env(false, false)), Variant::Light);
        assert_eq!(prefs.variant(&env(true, true)), Variant::DarkMore);
        assert_eq!(prefs.variant(&env(false, true)), Variant::LightMore);
    }

    #[test]
    fn explicit_choices_ignore_the_environment() {
        let prefs = ThemePrefs {
            appearance: Appearance::Light,
            contrast: Contrast::Standard,
            ..ThemePrefs::default()
        };
        assert_eq!(prefs.variant(&env(true, true)), Variant::Light);
        let prefs = ThemePrefs {
            appearance: Appearance::Dark,
            contrast: Contrast::More,
            ..ThemePrefs::default()
        };
        assert_eq!(prefs.variant(&env(false, false)), Variant::DarkMore);
    }

    #[test]
    fn resolve_picks_the_variant_and_falls_back_for_unknown_themes() {
        let registry = Registry::builtin();
        let light = resolve(&registry, &ThemePrefs::default(), &env(false, false));
        let dark = resolve(&registry, &ThemePrefs::default(), &env(true, false));
        assert_ne!(light.palette.canvas, dark.palette.canvas);
        assert!(light.fallback_from.is_none());

        let missing = resolve(
            &registry,
            &ThemePrefs {
                theme: "nope".into(),
                ..ThemePrefs::default()
            },
            &env(true, false),
        );
        assert_eq!(missing.theme_id, DEFAULT_THEME);
        assert_eq!(missing.fallback_from.as_deref(), Some("nope"));

        let more = resolve(&registry, &ThemePrefs::default(), &env(true, true));
        assert!(more.metrics.stroke_width > dark.metrics.stroke_width);
    }

    #[test]
    fn the_overlay_stays_dark_whatever_the_launcher_shows() {
        let registry = Registry::builtin();
        let theme = registry.default_theme();
        let light = resolve(&registry, &ThemePrefs::default(), &env(false, false));
        let dark = resolve(&registry, &ThemePrefs::default(), &env(true, false));
        assert_eq!(*light.overlay.palette, *theme.palette(Variant::Dark));
        assert_eq!(light.overlay, dark.overlay);
        // More contrast brings the dark-more palette, light launcher or not.
        let more = resolve(&registry, &ThemePrefs::default(), &env(false, true));
        assert_eq!(*more.overlay.palette, *theme.palette(Variant::DarkMore));
        assert_eq!(more.overlay.fill(100), more.overlay.palette.panel);
        assert_eq!(more.overlay.fill(0), egui::Color32::TRANSPARENT);
    }

    #[test]
    fn scale_is_clamped() {
        let prefs = ThemePrefs {
            scale: 40.0,
            ..ThemePrefs::default()
        }
        .sanitized();
        assert_eq!(prefs.scale, 2.0);
        let prefs = ThemePrefs {
            scale: f32::NAN,
            ..ThemePrefs::default()
        }
        .sanitized();
        assert_eq!(prefs.scale, 1.0);
    }
}

//! Look and feel of the launcher, matching the portal's design system.
//!
//! Four layers, each in its own file:
//!
//! 1. **Generated palettes** (`themes/*.json`, from the portal's CSS by
//!    `scripts/export-player-themes.ts`) hold the portal's colour roles in
//!    four variants: dark, light, and both again with more contrast. They are
//!    compiled in by [`registry`].
//! 2. **User themes** (`<data dir>/themes/*.json`) extend a theme and change
//!    some roles, [`metrics`] (radii, spacing, type sizes) or [`fonts`].
//!    [`registry`] loads them and reports files it had to skip.
//! 3. **Preferences and the system** (`ThemePrefs` in `config.json`, macOS
//!    light/dark and Increase contrast from [`system`]) choose one variant of
//!    one theme: [`resolve`].
//! 4. **egui style**: [`apply`] is the only place the result becomes an
//!    `egui::Style`. UI code reads roles through [`ThemeExt::palette`], and
//!    [`widgets`] has the portal's buttons and cards.
//!
//! The stats overlay sits on the video and stays dark: it takes the theme's
//! dark palette whatever the launcher shows ([`overlay`]).

mod apply;
mod fonts;
mod metrics;
mod overlay;
mod palette;
mod registry;
mod resolve;
mod system;
pub mod widgets;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use apply::ThemeExt;
pub use overlay::OverlayStyle;
pub use palette::Palette;
pub use registry::Theme;
pub use resolve::{Appearance, Contrast, SCALE_RANGE, ThemePrefs, Variant};

use fonts::FontChoice;
use registry::Registry;
use resolve::{Resolved, SystemEnv};

/// How often "Increase contrast" is asked for again.
const SYSTEM_POLL: Duration = Duration::from_secs(2);

/// Owns the registry and the system state, and keeps an egui context styled.
pub struct ThemeController {
    data_dir: PathBuf,
    registry: Registry,
    env: SystemEnv,
    polled: Instant,
    /// Bumped when the registry reloads, so the style is made again.
    generation: u64,
    applied: Option<(ThemePrefs, SystemEnv, u64)>,
    applied_fonts: Option<FontChoice>,
    font_warnings: Vec<String>,
    resolved: Option<Resolved>,
}

impl ThemeController {
    pub fn new(data_dir: PathBuf, window_theme: Option<winit::window::Theme>) -> Self {
        let registry = Registry::load(&data_dir);
        for issue in registry.issues() {
            tracing::warn!("theme {}: {}", issue.file, issue.reason);
        }
        let mut env = SystemEnv {
            increase_contrast: system::increase_contrast(),
            ..SystemEnv::default()
        };
        if let Some(theme) = window_theme {
            env.dark = theme == winit::window::Theme::Dark;
        }
        Self {
            data_dir,
            registry,
            env,
            polled: Instant::now(),
            generation: 0,
            applied: None,
            applied_fonts: None,
            font_warnings: Vec::new(),
            resolved: None,
        }
    }

    /// `WindowEvent::ThemeChanged`, or `Window::theme()` at start.
    pub fn set_window_theme(&mut self, theme: winit::window::Theme) {
        self.env.dark = theme == winit::window::Theme::Dark;
    }

    /// Ask macOS about Increase contrast, at most every couple of seconds.
    pub fn poll_system(&mut self) {
        if self.polled.elapsed() >= SYSTEM_POLL {
            self.polled = Instant::now();
            self.env.increase_contrast = system::increase_contrast();
        }
    }

    /// Read the themes folder again; the next [`Self::sync`] restyles.
    pub fn reload(&mut self) {
        self.registry = Registry::load(&self.data_dir);
        for issue in self.registry.issues() {
            tracing::warn!("theme {}: {}", issue.file, issue.reason);
        }
        self.generation += 1;
    }

    /// Style `ctx` for `prefs` if anything changed since the last call.
    /// True when it did (the caller should redraw).
    pub fn sync(&mut self, ctx: &egui::Context, prefs: &ThemePrefs) -> bool {
        let prefs = prefs.clone().sanitized();
        let key = (prefs.clone(), self.env, self.generation);
        if self.applied.as_ref() == Some(&key) {
            return false;
        }
        let resolved = resolve::resolve(&self.registry, &prefs, &self.env);
        if let Some(missing) = &resolved.fallback_from {
            tracing::warn!("theme {missing:?} not found, using {}", resolved.theme_id);
        }
        // Fonts are costly to set up (the atlas is rebuilt): only when the
        // choice changed.
        if self.applied_fonts.as_ref() != Some(&resolved.fonts) {
            let setup = fonts::setup(&resolved.fonts);
            ctx.set_fonts(setup.definitions);
            self.font_warnings = setup.warnings;
            self.applied_fonts = Some(resolved.fonts.clone());
        }
        apply::apply(ctx, &resolved);
        self.resolved = Some(resolved);
        self.applied = Some(key);
        true
    }

    /// The canvas colour, for clearing the window behind egui.
    pub fn canvas(&self) -> egui::Color32 {
        self.resolved
            .as_ref()
            .map_or_else(|| Palette::fallback().canvas, |r| r.palette.canvas)
    }

    pub fn themes(&self) -> &[Arc<Theme>] {
        self.registry.themes()
    }

    /// The variant in use, for swatches.
    pub fn variant(&self) -> Variant {
        self.resolved.as_ref().map_or(Variant::Dark, |r| r.variant)
    }

    /// Why a theme file was skipped, why a font wasn't used, and a pick that
    /// fell back to the default.
    pub fn problems(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .registry
            .issues()
            .iter()
            .map(|i| format!("{}: {}", i.file, i.reason))
            .collect();
        out.extend(self.font_warnings.iter().cloned());
        if let Some(missing) = self
            .resolved
            .as_ref()
            .and_then(|r| r.fallback_from.as_ref())
        {
            out.push(format!(
                "theme \"{missing}\" not found; showing the default"
            ));
        }
        out
    }

    pub fn themes_dir(&self) -> PathBuf {
        self.data_dir.join("themes")
    }

    /// Make the themes folder if needed and show it in Finder.
    pub fn open_themes_folder(&self) -> std::io::Result<()> {
        let dir = self.themes_dir();
        std::fs::create_dir_all(&dir)?;
        std::process::Command::new("open")
            .arg(&dir)
            .spawn()
            .map(|_| ())
    }
}

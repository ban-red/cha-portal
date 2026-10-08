//! Colour roles: the portal's design tokens as `Color32`s.

use egui::Color32;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

/// A colour written as `#rgb`, `#rrggbb` or `#rrggbbaa` in a theme file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hex(pub Color32);

impl<'de> Deserialize<'de> for Hex {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        parse_hex(&text).map(Hex).map_err(de::Error::custom)
    }
}

impl Serialize for Hex {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let [r, g, b, a] = self.0.to_srgba_unmultiplied();
        if a == 255 {
            s.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}"))
        } else {
            s.serialize_str(&format!("#{r:02x}{g:02x}{b:02x}{a:02x}"))
        }
    }
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa` (the alpha is not premultiplied).
pub fn parse_hex(text: &str) -> Result<Color32, String> {
    let digits = text
        .trim()
        .strip_prefix('#')
        .ok_or_else(|| format!("colour {text:?} must start with #"))?;
    if !digits.is_ascii() || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "colour {text:?} has characters that are not hex digits"
        ));
    }
    let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).unwrap_or(0);
    let nibble = |i: usize| {
        let n = u8::from_str_radix(&digits[i..i + 1], 16).unwrap_or(0);
        n << 4 | n
    };
    match digits.len() {
        3 => Ok(Color32::from_rgb(nibble(0), nibble(1), nibble(2))),
        6 => Ok(Color32::from_rgb(byte(0), byte(2), byte(4))),
        8 => Ok(Color32::from_rgba_unmultiplied(
            byte(0),
            byte(2),
            byte(4),
            byte(6),
        )),
        _ => Err(format!(
            "colour {text:?} must be #rgb, #rrggbb or #rrggbbaa"
        )),
    }
}

/// Declares [`Palette`] and [`PaletteOverride`] from one list of roles, so
/// the two cannot drift apart. JSON keys are the field names in kebab-case
/// (`panel_2` is `panel-2`).
macro_rules! roles {
    ($($(#[$doc:meta])* $field:ident),* $(,)?) => {
        /// Every role, resolved to a colour, for one appearance and contrast.
        #[derive(Clone, Debug, PartialEq)]
        pub struct Palette {
            $($(#[$doc])* pub $field: Color32,)*
        }

        /// Roles a theme file changes; the rest come from what it extends.
        #[derive(Clone, Debug, Default, PartialEq, Deserialize)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        pub struct PaletteOverride {
            $(#[serde(default)] pub $field: Option<Hex>,)*
        }

        impl PaletteOverride {
            /// Put this override's roles over `base`.
            pub fn apply_to(&self, base: &mut Palette) {
                $(if let Some(Hex(c)) = self.$field { base.$field = c; })*
            }

            /// The roles this override sets, as a palette; `Err` names the
            /// roles missing.
            pub fn complete(&self) -> Result<Palette, Vec<&'static str>> {
                let mut missing = Vec::new();
                $(if self.$field.is_none() { missing.push(stringify!($field)); })*
                if !missing.is_empty() {
                    return Err(missing);
                }
                Ok(Palette {
                    $($field: self.$field.map(|h| h.0).unwrap_or(Color32::PLACEHOLDER),)*
                })
            }
        }
    };
}

roles! {
    /// Window background.
    canvas,
    /// Raised surfaces: cards, windows.
    panel,
    /// Inputs, buttons, a second level of raising.
    panel_2,
    /// Borders.
    line,
    /// Borders that must be seen (inputs, focus neighbours).
    line_strong,
    /// Primary text.
    ink,
    /// Secondary text.
    ink_2,
    /// Tertiary text, hints.
    ink_3,
    /// Accent text, links, selection.
    accent,
    /// Primary button fill.
    accent_fill,
    accent_fill_hover,
    /// Text on `accent_fill`.
    on_accent,
    /// Subtle selected background.
    accent_soft,
    /// Focus ring.
    focus,
    ok,
    warn,
    danger,
    /// Destructive button fill.
    danger_fill,
    /// Text on `danger_fill`.
    on_danger,
    info,
    /// Modal backdrop (has alpha).
    scrim,
    chart_1,
    chart_2,
    chart_3,
    chart_4,
}

impl Palette {
    /// Cha – Magenta, dark: what UI code gets before any theme is applied.
    pub fn fallback() -> Self {
        static FALLBACK: std::sync::OnceLock<Palette> = std::sync::OnceLock::new();
        FALLBACK
            .get_or_init(|| {
                super::registry::Registry::builtin()
                    .default_theme()
                    .palette(super::resolve::Variant::Dark)
                    .clone()
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_forms() {
        assert_eq!(parse_hex("#fff").unwrap(), Color32::WHITE);
        assert_eq!(
            parse_hex("#1a2b3c").unwrap(),
            Color32::from_rgb(0x1a, 0x2b, 0x3c)
        );
        assert_eq!(
            parse_hex("#1A2B3C").unwrap(),
            Color32::from_rgb(0x1a, 0x2b, 0x3c)
        );
        assert_eq!(
            parse_hex("#f80").unwrap(),
            Color32::from_rgb(0xff, 0x88, 0x00)
        );
        assert_eq!(
            parse_hex("#0801069e").unwrap(),
            Color32::from_rgba_unmultiplied(0x08, 0x01, 0x06, 0x9e)
        );
        assert_eq!(parse_hex(" #000000ff ").unwrap(), Color32::BLACK);
    }

    #[test]
    fn bad_hex() {
        for bad in ["", "fff", "#", "#ff", "#ffff", "#ggg", "#12345", "#é12"] {
            assert!(parse_hex(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn override_merges_only_what_it_sets() {
        let base: PaletteOverride = serde_json::from_str(&full_json("#101010")).unwrap();
        let mut palette = base.complete().unwrap();
        let ov: PaletteOverride =
            serde_json::from_str(r##"{"accent": "#00ff88", "panel-2": "#222"}"##).unwrap();
        ov.apply_to(&mut palette);
        assert_eq!(palette.accent, Color32::from_rgb(0, 255, 136));
        assert_eq!(palette.panel_2, Color32::from_rgb(0x22, 0x22, 0x22));
        assert_eq!(palette.canvas, Color32::from_rgb(0x10, 0x10, 0x10));
    }

    #[test]
    fn incomplete_override_names_missing_roles() {
        let ov: PaletteOverride = serde_json::from_str(r##"{"canvas": "#000"}"##).unwrap();
        let missing = ov.complete().unwrap_err();
        assert!(missing.contains(&"accent") && !missing.contains(&"canvas"));
    }

    #[test]
    fn unknown_role_is_an_error() {
        assert!(serde_json::from_str::<PaletteOverride>(r##"{"acent": "#000"}"##).is_err());
    }

    /// Every role set to `colour`, as JSON.
    fn full_json(colour: &str) -> String {
        let roles = [
            "canvas",
            "panel",
            "panel-2",
            "line",
            "line-strong",
            "ink",
            "ink-2",
            "ink-3",
            "accent",
            "accent-fill",
            "accent-fill-hover",
            "on-accent",
            "accent-soft",
            "focus",
            "ok",
            "warn",
            "danger",
            "danger-fill",
            "on-danger",
            "info",
            "scrim",
            "chart-1",
            "chart-2",
            "chart-3",
            "chart-4",
        ];
        let body: Vec<String> = roles
            .iter()
            .map(|r| format!("\"{r}\": \"{colour}\""))
            .collect();
        format!("{{{}}}", body.join(","))
    }
}

//! Non-colour tokens: radii, spacing, stroke widths and type sizes. The
//! defaults follow the portal (Tailwind's `rounded-lg` is 8 px, the smallest
//! type is 11 px).

use serde::Deserialize;

macro_rules! metrics {
    ($($(#[$doc:meta])* $field:ident = $default:expr),* $(,)?) => {
        #[derive(Clone, Debug, PartialEq)]
        pub struct Metrics {
            $($(#[$doc])* pub $field: f32,)*
        }

        /// Metrics a theme file changes (`"metrics": { "radius_medium": 2.0 }`).
        #[derive(Clone, Debug, Default, PartialEq, Deserialize)]
        #[serde(deny_unknown_fields)]
        pub struct MetricsOverride {
            $(#[serde(default)] pub $field: Option<f32>,)*
        }

        impl Default for Metrics {
            fn default() -> Self {
                Self { $($field: $default,)* }
            }
        }

        impl MetricsOverride {
            pub fn apply_to(&self, base: &mut Metrics) {
                $(if let Some(v) = self.$field { base.$field = v; })*
            }
        }
    };
}

metrics! {
    /// Small things: badges, swatches.
    radius_small = 4.0,
    /// Buttons, inputs, cards.
    radius_medium = 8.0,
    /// Windows and dialogs.
    radius_large = 12.0,
    item_spacing_x = 8.0,
    item_spacing_y = 6.0,
    button_padding_x = 12.0,
    button_padding_y = 5.0,
    window_margin = 14.0,
    /// Height of a button or input.
    control_height = 28.0,
    /// Borders.
    stroke_width = 1.0,
    /// Focus rings, pressed and selected borders.
    stroke_width_strong = 1.5,
    font_heading = 20.0,
    font_body = 14.0,
    font_button = 14.0,
    font_small = 11.0,
    font_monospace = 13.0,
}

impl Metrics {
    /// Keep hand-edited values in a range egui lays out sanely.
    pub fn sanitized(mut self) -> Self {
        let clamp = |v: &mut f32, lo: f32, hi: f32| {
            *v = if v.is_finite() { v.clamp(lo, hi) } else { lo };
        };
        for r in [
            &mut self.radius_small,
            &mut self.radius_medium,
            &mut self.radius_large,
        ] {
            clamp(r, 0.0, 32.0);
        }
        for s in [
            &mut self.item_spacing_x,
            &mut self.item_spacing_y,
            &mut self.button_padding_x,
            &mut self.button_padding_y,
        ] {
            clamp(s, 0.0, 40.0);
        }
        clamp(&mut self.window_margin, 0.0, 48.0);
        clamp(&mut self.control_height, 18.0, 64.0);
        clamp(&mut self.stroke_width, 0.0, 4.0);
        clamp(&mut self.stroke_width_strong, 0.0, 6.0);
        for f in [
            &mut self.font_heading,
            &mut self.font_body,
            &mut self.font_button,
            &mut self.font_small,
            &mut self.font_monospace,
        ] {
            clamp(f, 8.0, 64.0);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_merges() {
        let mut m = Metrics::default();
        let ov: MetricsOverride =
            serde_json::from_str(r#"{"radius_medium": 2.0, "font_body": 15.0}"#).unwrap();
        ov.apply_to(&mut m);
        assert_eq!(m.radius_medium, 2.0);
        assert_eq!(m.font_body, 15.0);
        assert_eq!(m.radius_small, Metrics::default().radius_small);
    }

    #[test]
    fn sanitizing_clamps() {
        let m = Metrics {
            radius_medium: 900.0,
            font_body: f32::NAN,
            ..Metrics::default()
        }
        .sanitized();
        assert_eq!(m.radius_medium, 32.0);
        assert_eq!(m.font_body, 8.0);
    }
}

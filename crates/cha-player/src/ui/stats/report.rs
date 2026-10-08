//! Copying a warning with the numbers around it, for a bug report or a chat. The text is the
//! panel model's (`Panel::report`); this holds who is asking and the copy button.

use cha_ui_spec::panel::{Panel, ReportIssue};
use egui::{Rect, Sense, Ui, Vec2, pos2};

use super::{Cmd, View};
use crate::health::Issue;
use crate::theme::icons::{self, Icon};
use crate::ui::chrome;

/// What a copy button copies.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Copied {
    One(&'static str),
    All,
}

impl Copied {
    fn key(self) -> &'static str {
        match self {
            Copied::All => "all",
            Copied::One(id) => id,
        }
    }
}

/// "Cha Player 0.1.0, macOS 15.5", for a copied report.
pub(super) fn player_line() -> String {
    static LINE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LINE.get_or_init(|| {
        let macos = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".into());
        format!("Cha Player {}, macOS {macos}", env!("CARGO_PKG_VERSION"))
    })
    .clone()
}

/// The text a copy button puts on the clipboard: `issues` with their hints,
/// then the numbers around them.
pub(super) fn issue_report(panel: &Panel, issues: &[Issue], player: &str) -> String {
    let issues: Vec<ReportIssue<'_>> = issues
        .iter()
        .map(|i| ReportIssue {
            title: i.title,
            detail: &i.detail,
            hint: i.hint,
        })
        .collect();
    panel.report(&issues, player)
}

impl View<'_> {
    /// A 16 px copy button whose left edge is at `x`, centred on `y`.
    pub(super) fn copy_button(
        &self,
        ui: &mut Ui,
        x: f32,
        y: f32,
        what: Copied,
        cmds: &mut Vec<Cmd>,
    ) {
        let look = self.look;
        let b = Rect::from_min_size(pos2(x, y - 8.0), Vec2::splat(16.0));
        let resp = ui.interact(b, ui.id().with(("copy", what.key())), Sense::click());
        let done = self.copied == Some(what);
        if resp.hovered() {
            chrome::lit(ui.painter(), look, b, 4.0);
        }
        let color = if resp.hovered() {
            look.p.ink
        } else {
            look.ink2
        };
        let icon = if done { Icon::Check } else { Icon::Copy };
        icons::paint(
            ui.painter(),
            Rect::from_center_size(b.center(), Vec2::splat(12.0)),
            icon,
            color,
        );
        let tip = if done {
            "Copied"
        } else {
            "Copy this warning and the stream numbers"
        };
        if resp.on_hover_text(tip).clicked() {
            cmds.push(Cmd::Copy(what));
        }
    }
}

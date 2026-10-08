//! The stats panel over the picture: the native twin of the portal's
//! `StatsOverlay.vue`. A header (grade, compact/full, collapse, settings,
//! hide) over either one compact line or the full sections, in a corner you
//! can drag it out of. Ctrl+Alt+Shift+S shows and hides it.
//!
//! What the panel says (sections, rows, labels, tooltips, number formats, the
//! compact line and the copy report) is `cha_ui_spec::panel::build_panel`'s,
//! from `web/packages/ui-spec/stats-panel.json`, the same model the browser
//! draws. These files only draw it:
//!
//! - [`snapshot`]: [`StatsSnapshot`] and its mapping to the panel's value keys.
//! - [`look`]: colours and text drawing.
//! - [`placement`]: corners, free placement and the drag.
//! - [`header`], [`settings`], [`compact`], [`sections`], [`chip`]: the parts.
//! - [`report`]: the copy buttons and who is asking.
//!
//! [`StatsPanel`] owns the preferences and what is being dragged; the app
//! feeds it a [`StatsSnapshot`] and the health [`Assessment`] each frame and
//! asks it which pointer events are its own ([`StatsPanel::consumes`]).

mod chip;
mod compact;
mod header;
mod placement;
mod report;
mod sections;
mod settings;
mod snapshot;

use std::time::{Duration, Instant};

use cha_ui_spec::health::Platform;
use cha_ui_spec::panel::{Panel, PanelHealth, Severity as PanelSeverity, build_panel, spec};
use egui::{Align2, Area, Context, FontId, Frame, Id, Order, Pos2, Rect, Stroke, Ui, Vec2, vec2};
#[cfg(test)]
use winit::event::ElementState;
use winit::event::WindowEvent;

use crate::health::{Assessment, Severity};
use crate::overlay_prefs::{OPACITY_MIN, OverlayPrefs, Section};
use crate::theme::{OverlayStyle, ThemeExt};
use placement::{Drag, INSET};
use report::{Copied, issue_report, player_line};

use super::chrome::{self, Hold, Look, Press};
pub use snapshot::StatsSnapshot;

const HEADER_H: f32 = 24.0;
const BTN: f32 = 20.0;
const GAP: f32 = 4.0;
const PAD: f32 = 8.0;
/// Four buttons, the gaps before each and the padding at the right.
const BUTTONS_W: f32 = 4.0 * BTN + 4.0 * GAP + 4.0;
/// The settings strip closes itself after this long untouched.
const MENU_IDLE: Duration = Duration::from_secs(60);
/// How long a copy button shows its check.
const COPIED_FOR: Duration = Duration::from_millis(1500);
/// Preferences are saved this long after the last change.
const SAVE_AFTER: Duration = Duration::from_millis(600);
/// The compact view's issue line stops growing here.
const COMPACT_ISSUE_MAX: f32 = 288.0;
/// The settings strip's height.
const MENU_H: f32 = 80.0;

/// The panel's state: preferences, and what the user is doing to it.
pub struct StatsPanel {
    pub prefs: OverlayPrefs,
    saved: OverlayPrefs,
    changed: Option<Instant>,
    /// Where the panel (or the chip) was drawn last, in points.
    rect: Option<Rect>,
    dragging: Option<Drag>,
    /// A press began on the panel and its release hasn't come yet.
    press: Press,
    menu_until: Option<Instant>,
    copied: Option<(Copied, Instant)>,
}

/// What the user did, applied after the frame is drawn.
enum Cmd {
    Compact,
    Collapse,
    Menu,
    Hide,
    Show,
    Fold(Section),
    Copy(Copied),
    Opacity(u8),
    Snap(bool),
    KeepMenu,
    DragStart,
    DragMove,
    DragEnd,
}

/// The health grade as the panel model takes it.
fn panel_health(h: &Assessment) -> PanelHealth {
    PanelHealth {
        grade: h.grade.map(|g| g.letter().to_owned()),
        score: h.score,
        summary: h.summary.to_owned(),
        issues: h
            .issues
            .iter()
            .map(|i| {
                let s = match i.severity {
                    Severity::Minor => PanelSeverity::Minor,
                    Severity::Major => PanelSeverity::Major,
                    Severity::Critical => PanelSeverity::Critical,
                };
                (i.id.to_owned(), s)
            })
            .collect(),
    }
}

/// The panel to draw for a reading and its health.
fn model(stats: &StatsSnapshot, health: &Assessment) -> Panel {
    build_panel(
        spec(),
        &stats.values(),
        &panel_health(health),
        Platform::Native,
    )
}

impl StatsPanel {
    pub fn new(prefs: OverlayPrefs) -> Self {
        Self {
            saved: prefs.clone(),
            prefs,
            changed: None,
            rect: None,
            dragging: None,
            press: Press::default(),
            menu_until: None,
            copied: None,
        }
    }

    /// Ctrl+Alt+Shift+S.
    pub fn toggle_open(&mut self) {
        self.prefs.open = !self.prefs.open;
        self.touched();
    }

    fn touched(&mut self) {
        self.changed = Some(Instant::now());
    }

    /// The preferences to write to `config.json`, once they have settled
    /// (not on every frame of a slider or a drag). `force` skips the wait,
    /// for leaving the stream.
    pub fn take_save(&mut self, force: bool) -> Option<OverlayPrefs> {
        if self.prefs == self.saved {
            self.changed = None;
            return None;
        }
        let settled = self.changed.is_none_or(|t| t.elapsed() >= SAVE_AFTER);
        if !(force || settled && self.dragging.is_none() && !self.press.is_down()) {
            return None;
        }
        self.saved = self.prefs.clone();
        self.changed = None;
        Some(self.prefs.clone())
    }

    /// When a pending save is due, for waking the event loop.
    pub fn save_due(&self) -> Option<Duration> {
        (self.prefs != self.saved).then(|| {
            self.changed
                .map_or(Duration::ZERO, |t| SAVE_AFTER.saturating_sub(t.elapsed()))
        })
    }

    /// Whether a pointer event at `pos` (points) belongs to the panel: over
    /// it, or part of a press that began on it. Such events are not for the
    /// host, and a click there doesn't capture the pointer.
    pub fn consumes(&mut self, pos: Option<Pos2>, event: &WindowEvent) -> bool {
        let over = pos.is_some_and(|p| self.rect.is_some_and(|r| r.contains(p)));
        let hold = if self.dragging.is_some() {
            Hold::Moves
        } else {
            Hold::Nothing
        };
        self.press.claims(event, over, hold)
    }

    /// Opens the settings strip, for the snapshots.
    #[cfg(test)]
    pub fn open_menu(&mut self) {
        self.menu_until = Some(Instant::now() + MENU_IDLE);
    }

    fn pointer(ctx: &Context) -> Option<Pos2> {
        ctx.input(|i| i.pointer.interact_pos().or(i.pointer.latest_pos()))
    }

    /// Draw the panel (or its chip) for this frame. `top_inset` is the height
    /// of the toolbar while it shows (0 when folded): a panel in a top corner
    /// sits below it, 10 points clear, as in the portal.
    pub fn show(
        &mut self,
        ctx: &Context,
        stats: &StatsSnapshot,
        health: &Assessment,
        top_inset: f32,
    ) {
        let style = ctx.overlay_style();
        let screen = ctx.content_rect();
        let now = Instant::now();
        if self.copied.is_some_and(|(_, at)| at + COPIED_FOR <= now) {
            self.copied = None;
        }
        if self.menu_until.is_some_and(|t| t <= now) || self.prefs.collapsed || !self.prefs.open {
            self.menu_until = None;
        }
        let look = Look::new(&style, self.prefs.opacity);
        let panel = model(stats, health);
        let mut cmds = Vec::new();

        let size = self.rect.map_or(vec2(style.width, HEADER_H), |r| r.size());
        let pos = placement::position(&self.prefs, self.dragging.as_ref(), size, screen, top_inset);
        let area = Area::new(Id::new("cha-stats-panel"))
            .order(Order::Foreground)
            .fixed_pos(pos)
            // Room to grow: egui's default would cap the body at 400 points.
            .default_size(vec2(style.width, screen.height()));

        let response = if self.prefs.open {
            let view = View {
                prefs: &self.prefs,
                look: &look,
                style: &style,
                health,
                panel: &panel,
                copied: self.copied.map(|c| c.0),
                menu: self.menu_until.is_some(),
                max_body: (screen.height() - INSET - placement::top_y(top_inset) - HEADER_H - 2.0)
                    .max(80.0),
            };
            area.show(ctx, |ui| view.panel(ui, &mut cmds)).response
        } else {
            area.show(ctx, |ui| chip::chip(ui, &look, health, &panel, &mut cmds))
                .response
        };
        let rect = response.rect;
        if self.rect.is_some_and(|r| r.size() != rect.size()) {
            ctx.request_repaint();
        }
        self.rect = Some(rect);

        for cmd in cmds {
            self.apply(ctx, cmd, health, &panel, screen, rect, now);
        }
        if let Some(t) = self.menu_until {
            ctx.request_repaint_after(t.saturating_duration_since(now));
        }
        if let Some((_, at)) = self.copied {
            ctx.request_repaint_after((at + COPIED_FOR).saturating_duration_since(now));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply(
        &mut self,
        ctx: &Context,
        cmd: Cmd,
        health: &Assessment,
        panel: &Panel,
        screen: Rect,
        rect: Rect,
        now: Instant,
    ) {
        match cmd {
            Cmd::Compact => self.prefs.compact = !self.prefs.compact,
            Cmd::Collapse => self.prefs.collapsed = !self.prefs.collapsed,
            Cmd::Menu => {
                self.menu_until = match self.menu_until {
                    Some(_) => None,
                    None => Some(now + MENU_IDLE),
                }
            }
            Cmd::Hide => self.prefs.open = false,
            Cmd::Show => self.prefs.open = true,
            Cmd::Fold(section) => self.prefs.toggle_section(section),
            Cmd::Copy(what) => {
                let issues: Vec<_> = match what {
                    Copied::All => health.issues.clone(),
                    Copied::One(id) => health
                        .issues
                        .iter()
                        .filter(|i| i.id == id)
                        .cloned()
                        .collect(),
                };
                ctx.copy_text(issue_report(panel, &issues, &player_line()));
                self.copied = Some((what, now));
            }
            Cmd::Opacity(o) => {
                self.prefs.opacity = o.clamp(OPACITY_MIN, 100);
                self.menu_until = Some(now + MENU_IDLE);
            }
            Cmd::Snap(on) => {
                self.prefs.snap = on;
                self.menu_until = Some(now + MENU_IDLE);
            }
            Cmd::KeepMenu => {
                if self.menu_until.is_some() {
                    self.menu_until = Some(now + MENU_IDLE);
                }
            }
            Cmd::DragStart => {
                if let Some(p) = Self::pointer(ctx) {
                    self.dragging = Some(Drag::begin(p, rect));
                }
            }
            Cmd::DragMove => {
                if let (Some(p), Some(d)) = (Self::pointer(ctx), &mut self.dragging) {
                    d.to(p, rect, screen);
                }
            }
            Cmd::DragEnd => {
                if let Some(d) = self.dragging.take() {
                    d.drop_on(&mut self.prefs, rect, screen);
                }
            }
        }
        self.touched();
    }
}

/// One frame's worth of what to draw and how.
struct View<'a> {
    prefs: &'a OverlayPrefs,
    look: &'a Look,
    style: &'a OverlayStyle,
    health: &'a Assessment,
    /// The model: sections, rows, summaries, the compact line.
    panel: &'a Panel,
    copied: Option<Copied>,
    menu: bool,
    /// The tallest the body may be before it scrolls.
    max_body: f32,
}

impl View<'_> {
    fn panel(&self, ui: &mut Ui, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let p = &look.p;
        let compact_like = self.prefs.compact || self.prefs.collapsed;
        let (grade_w, text_w) = self.header_text_widths(ui);
        let handle_content = PAD + grade_w + 6.0 + text_w + PAD;
        let inner_w = if compact_like {
            let mut w = handle_content + BUTTONS_W;
            if self.prefs.compact && !self.prefs.collapsed {
                w = w.max(2.0 * PAD + self.compact_body_width(ui));
            }
            w
        } else {
            self.style.width - 2.0
        };
        chrome::overlay_frame(look, self.style.radius).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.set_width(inner_w);
            // The area is as tall as it was last frame, so lift that
            // limit: the body scrolls only past `max_body`.
            ui.set_max_height(self.max_body + HEADER_H + 2.0);
            self.header(ui, inner_w, compact_like.then_some(handle_content), cmds);
            if self.menu {
                self.menu_strip(ui, inner_w, cmds);
            }
            if self.prefs.collapsed {
                return;
            }
            // The line between the header and the body.
            let y = ui.cursor().top();
            ui.painter()
                .hline(ui.max_rect().x_range(), y, Stroke::new(1.0, p.line));
            ui.add_space(1.0);
            egui::ScrollArea::vertical()
                .max_height(self.max_body - if self.menu { MENU_H } else { 0.0 })
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    let w = inner_w - 2.0 * PAD - 6.0;
                    Frame::new()
                        .inner_margin(egui::Margin {
                            left: PAD as i8,
                            right: (PAD + 6.0) as i8,
                            top: 4,
                            bottom: 6,
                        })
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing = Vec2::ZERO;
                            ui.set_width(w);
                            if self.prefs.compact {
                                self.compact_body(ui, w, cmds);
                            } else {
                                self.full_body(ui, w, cmds);
                            }
                        });
                });
        });
    }
}

/// "Reconnecting…" over the last picture while a dropped stream comes back.
pub fn show_reconnecting(ctx: &egui::Context, text: &str, top_inset: f32) {
    let style = ctx.overlay_style();
    let p = &style.palette;
    // Below the toolbar while it shows (it always does while reconnecting).
    let y = if top_inset > 0.0 {
        top_inset + super::toolbar::TOP + 12.0
    } else {
        24.0
    };
    egui::Area::new(egui::Id::new("reconnecting"))
        .anchor(Align2::CENTER_TOP, [0.0, y])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(style.fill(90))
                .stroke(Stroke::new(1.0, p.line))
                .corner_radius(style.radius)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(text)
                            .font(FontId::proportional(style.font_size * 1.4))
                            .color(p.ink),
                    );
                });
        });
}

#[cfg(test)]
mod tests;

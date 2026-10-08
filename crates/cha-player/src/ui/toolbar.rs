//! The toolbar over the picture: the native twin of the portal's in-stream
//! toolbar (`SessionView.vue`). A bar across the top while the pointer is
//! free, folding into a thin bar you hover or click; Back, power off, the
//! stream settings, exclusive input, the mouse switch, sound, controllers,
//! full screen and the stats button. Ctrl+Alt+Shift+T shows it.
//!
//! [`Toolbar`] owns what the user is doing to it (folded or not, which menu is
//! open, a power-off countdown) and where it was drawn, so the app can ask
//! which pointer events are its own ([`Toolbar::consumes`]). It never changes
//! the stream itself: [`Toolbar::show`] returns [`ToolbarAction`]s for the app
//! to carry out, and takes the current state in a [`ToolbarView`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use cha_client::{PerfOverlay, TransportStats};
use egui::epaint::Shadow;
use egui::{
    Area, Color32, Context, FontId, Galley, Id, LayerId, Order, Painter, Pos2, Rect, RichText,
    Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2,
};
use winit::event::{ElementState, WindowEvent};

use super::overlay::{Look, StatsSnapshot};
use crate::health::Assessment;
use crate::input::pads::{InputAccess, PadInfo};
use crate::stream_prefs::{FRAME_RATES, OVERLAY_MAX};
use crate::theme::icons::{self, Icon};
use crate::theme::{Palette, ThemeExt};

/// The bar's top edge (`top-3`).
pub const TOP: f32 = 12.0;
const BTN: f32 = 28.0;
const GAP: f32 = 4.0;
const PAD: f32 = 6.0;
const RADIUS: f32 = 12.0;
/// The bar's height: a button and its padding.
const BAR_H: f32 = BTN + 2.0 * PAD;
const MENU_W: f32 = 256.0;
const MENU_PAD: f32 = 12.0;
/// Folds this long after the pointer leaves it.
const COLLAPSE_AFTER: Duration = Duration::from_millis(1200);
/// The pointer this close to the top keeps an open toolbar from folding.
const NEAR_TOP: f32 = 72.0;
/// "Powering off in 5…".
const POWER_OFF_SECONDS: u32 = 5;
const TEXT: f32 = 12.0;
const FADE: f32 = 0.15;

// ---- state the app feeds in, and what comes out -------------------------

/// What the toolbar shows, from the stream and the player, each frame.
pub struct ToolbarView<'a> {
    /// The app's name.
    pub title: &'a str,
    /// The transport's name ("Cha Portal", "Moonlight").
    pub transport: &'a str,
    pub stats: &'a StatsSnapshot,
    pub health: &'a Assessment,
    /// What the transport says right now, if it says anything.
    pub link: Option<&'a TransportStats>,
    /// False while the stream is coming back after a drop.
    pub connected: bool,
    /// The pointer is captured: the toolbar is folded and takes no input.
    pub locked: bool,
    pub mouse: bool,
    pub muted: bool,
    pub volume: u8,
    pub fullscreen: bool,
    pub stats_open: bool,
    /// The stats panel's opacity, which the bar shares.
    pub opacity: u8,
    pub pads: &'a [PadInfo],
    pub input_access: InputAccess,
}

/// What the user asked for; the app does it.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolbarAction {
    /// Leave the stream, keeping the app running.
    Back,
    /// The countdown ran out: quit the app on the host and leave.
    PowerOff,
    /// Capture the pointer, as a click on the picture does.
    Capture,
    SetMouse(bool),
    SetMuted(bool),
    SetVolume(u8),
    RestartSound,
    SetFps(u32),
    SetOverlay(u8),
    TakeControl,
    ToggleFullScreen,
    ToggleStats,
    OpenInputSettings,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Stream,
    Sound,
    Controllers,
}

/// The countdown before the app is stopped.
#[derive(Clone, Copy, Debug)]
struct PowerOff {
    started: Instant,
}

impl PowerOff {
    fn total() -> Duration {
        Duration::from_secs(u64::from(POWER_OFF_SECONDS))
    }

    /// Seconds still to wait, rounded up: 5 at the start, 1 just before the end.
    fn left(&self, now: Instant) -> u32 {
        let elapsed = now.saturating_duration_since(self.started);
        let left = Self::total().saturating_sub(elapsed);
        left.as_secs_f32().ceil() as u32
    }

    /// 1 at the start, 0 when it is time.
    fn fraction(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        (1.0 - elapsed.as_secs_f32() / Self::total().as_secs_f32()).clamp(0.0, 1.0)
    }

    fn due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= Self::total()
    }
}

/// The toolbar's state between frames.
pub struct Toolbar {
    /// Open: shown in full. Closed, it is a thin bar at the top.
    expanded: bool,
    /// The pointer is on the bar, its tab or an open menu.
    hover: bool,
    collapse_at: Option<Instant>,
    menu: Option<Menu>,
    power_off: Option<PowerOff>,
    /// Where things were drawn last frame, in points; `None` when not shown.
    bar: Option<Rect>,
    tab: Option<Rect>,
    thin: Option<Rect>,
    menu_rect: Option<Rect>,
    menu_button: Option<Rect>,
    /// A press began on the toolbar and its release hasn't come yet.
    pressed: bool,
    /// The bar's height when it showed last: the stats panel sits below it.
    height: f32,
    /// Whether the last frame had the bar up (the fade has finished starting).
    shown: bool,
}

impl Toolbar {
    pub fn new() -> Self {
        Self {
            expanded: true,
            hover: false,
            collapse_at: None,
            menu: None,
            power_off: None,
            bar: None,
            tab: None,
            thin: None,
            menu_rect: None,
            menu_button: None,
            pressed: false,
            height: 0.0,
            shown: false,
        }
    }

    /// A new stream: shown in full, as the portal starts it.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    // ---- visibility ----

    fn visible(&self, connected: bool, locked: bool) -> bool {
        !locked
            && (!connected
                || self.expanded
                || self.hover
                || self.menu.is_some()
                || self.power_off.is_some())
    }

    fn collapse_soon(&mut self, now: Instant) {
        self.collapse_at = Some(now + COLLAPSE_AFTER);
    }

    fn expand(&mut self) {
        self.collapse_at = None;
        self.expanded = true;
    }

    /// Fold now, without waiting for the pointer to leave.
    fn hide(&mut self) {
        self.collapse_at = None;
        self.hover = false;
        self.menu = None;
        self.expanded = false;
    }

    /// The pointer was captured: nothing shows, and nothing stays open.
    pub fn on_capture(&mut self) {
        self.hide();
        self.power_off = None;
        self.pressed = false;
        self.clear_rects();
    }

    /// Ctrl+Alt+Shift+T: show the bar and keep it up until the pointer has
    /// been over it and gone.
    pub fn show_now(&mut self) {
        self.expand();
    }

    fn clear_rects(&mut self) {
        (self.bar, self.tab, self.thin) = (None, None, None);
        (self.menu_rect, self.menu_button) = (None, None);
        self.shown = false;
    }

    /// The pointer entered or left the bar.
    fn set_hover(&mut self, hover: bool, now: Instant) {
        if hover == self.hover {
            return;
        }
        self.hover = hover;
        if hover {
            self.collapse_at = None;
        } else {
            self.collapse_soon(now);
        }
    }

    /// The pointer moved to `y` while the bar is up: near the top it stays,
    /// further down it folds shortly.
    fn pointer_y(&mut self, y: f32, now: Instant) {
        if !self.expanded || self.hover {
            return;
        }
        if y < NEAR_TOP {
            self.collapse_at = None;
        } else {
            self.collapse_soon(now);
        }
    }

    fn tick(&mut self, now: Instant) {
        if self.collapse_at.is_some_and(|t| now >= t) {
            self.collapse_at = None;
            self.expanded = false;
        }
    }

    /// How tall the bar is while it shows (0 when folded): the stats panel's
    /// top corners sit below it.
    pub fn inset(&self) -> f32 {
        if self.shown { self.height } else { 0.0 }
    }

    /// Esc: closes an open menu or cancels the countdown. True when it did, so
    /// the key is not sent on.
    pub fn escape(&mut self) -> bool {
        if self.power_off.take().is_some() {
            return true;
        }
        self.menu.take().is_some()
    }

    #[cfg(test)]
    pub fn open_menu_for_test(&mut self, name: &str) {
        self.menu = match name {
            "stream" => Some(Menu::Stream),
            "sound" => Some(Menu::Sound),
            "controllers" => Some(Menu::Controllers),
            _ => None,
        };
    }

    #[cfg(test)]
    pub fn power_off_for_test(&mut self, started: Instant) {
        self.power_off = Some(PowerOff { started });
    }

    /// The bar is waiting to fold: a redraw has to arm its timer.
    pub fn timer_pending(&self) -> bool {
        self.collapse_at.is_some()
    }

    /// Whether a power-off countdown is running.
    #[cfg(test)]
    pub fn counting_down(&self) -> bool {
        self.power_off.is_some()
    }

    // ---- pointer routing ----

    fn regions(&self) -> impl Iterator<Item = Rect> {
        [self.bar, self.tab, self.menu_rect].into_iter().flatten()
    }

    /// Whether a pointer event at `pos` (points) belongs to the toolbar: over
    /// the bar, its tab, an open menu or the thin bar, or part of a press that
    /// began on one, or anything at all while a countdown holds the screen.
    /// Such events are not for the host, and a click there doesn't capture
    /// the pointer.
    pub fn consumes(&mut self, pos: Option<Pos2>, event: &WindowEvent) -> bool {
        self.consumes_at(pos, event, Instant::now())
    }

    fn consumes_at(&mut self, pos: Option<Pos2>, event: &WindowEvent, now: Instant) -> bool {
        let inside = |r: Option<Rect>| pos.is_some_and(|p| r.is_some_and(|r| r.contains(p)));
        let over = self.regions().any(|r| pos.is_some_and(|p| r.contains(p)));
        let over_thin = inside(self.thin);
        let held = self.power_off.is_some();
        match event {
            WindowEvent::MouseInput { state, .. } => match state {
                ElementState::Pressed => {
                    // A press anywhere but the open menu (and its button) shuts it.
                    if self.menu.is_some() && !inside(self.menu_rect) && !inside(self.menu_button) {
                        self.menu = None;
                    }
                    let claimed = over || over_thin || held;
                    self.pressed |= claimed;
                    claimed
                }
                ElementState::Released => {
                    std::mem::take(&mut self.pressed) || over || over_thin || held
                }
            },
            WindowEvent::MouseWheel { .. } => over || over_thin || held,
            WindowEvent::CursorMoved { .. } => {
                if over_thin {
                    // Hovering the thin bar brings the toolbar back.
                    self.expand();
                }
                if let Some(p) = pos {
                    self.pointer_y(p.y, now);
                }
                self.pressed || over || over_thin || held
            }
            _ => self.pressed || over || over_thin || held,
        }
    }

    // ---- drawing ----

    /// Draw the toolbar (or the thin bar), its open menu and a countdown.
    pub fn show(&mut self, ctx: &Context, v: &ToolbarView) -> Vec<ToolbarAction> {
        self.show_at(ctx, v, Instant::now())
    }

    fn show_at(&mut self, ctx: &Context, v: &ToolbarView, now: Instant) -> Vec<ToolbarAction> {
        let style = ctx.overlay_style();
        let look = Look::new(&style, v.opacity);
        let screen = ctx.content_rect();
        let mut actions = Vec::new();

        if v.locked {
            self.on_capture();
            return actions;
        }
        self.tick(now);
        // Hover from where the pointer is against what was drawn last frame.
        let pointer = ctx.pointer_latest_pos();
        let hovering = pointer.is_some_and(|p| self.regions().any(|r| r.contains(p)));
        self.set_hover(hovering, now);
        if let Some(t) = self.collapse_at {
            ctx.request_repaint_after(t.saturating_duration_since(now));
        }

        let visible = self.visible(v.connected, v.locked);
        let fade = ctx.animate_bool_with_time(Id::new("cha-toolbar-fade"), visible, FADE);
        if visible || fade > 0.0 {
            self.draw_bar(ctx, v, &look, screen, fade, visible, &mut actions);
        } else {
            self.clear_bar();
        }
        self.shown = visible;
        if visible {
            self.thin = None;
        } else {
            self.draw_thin(ctx, &look, screen);
        }
        if self.power_off.is_some() {
            self.draw_power_off(ctx, v, &look, screen, now, &mut actions);
        }
        actions
    }

    fn clear_bar(&mut self) {
        (self.bar, self.tab) = (None, None);
        (self.menu_rect, self.menu_button) = (None, None);
    }

    fn draw_thin(&mut self, ctx: &Context, look: &Look, screen: Rect) {
        let p = &look.p;
        let hit = Rect::from_min_size(pos2(screen.center().x - 72.0, 0.0), vec2(144.0, 24.0));
        let mut clicked = false;
        Area::new(Id::new("cha-toolbar-thin"))
            .order(Order::Foreground)
            .fixed_pos(hit.min)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(hit.size(), Sense::hover());
                let resp = ui.interact(rect, ui.id().with("thin"), Sense::click());
                let pill = Rect::from_min_size(
                    pos2(rect.center().x - 48.0, rect.top() + 6.0),
                    vec2(96.0, 6.0),
                );
                let m = if resp.hovered() { 1.0 } else { 0.85 };
                ui.painter().rect(
                    pill,
                    3.0,
                    look.fill.gamma_multiply(m),
                    Stroke::new(1.0, p.line),
                    StrokeKind::Inside,
                );
                clicked = resp.on_hover_text("Show the toolbar").clicked();
            });
        if clicked {
            self.expand();
        }
        self.thin = Some(hit);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_bar(
        &mut self,
        ctx: &Context,
        v: &ToolbarView,
        look: &Look,
        screen: Rect,
        fade: f32,
        interactive: bool,
        actions: &mut Vec<ToolbarAction>,
    ) {
        let layout = Layout::new(ctx, look, v, screen);
        let bar = layout.bar;
        self.height = bar.height();
        let mut menu_click = None;
        let mut hide_click = false;
        let mut power_click = false;
        let menu = self.menu;
        let mut open_rect = None;
        let mut menu_button = None;

        Area::new(Id::new("cha-toolbar"))
            .order(Order::Foreground)
            .fixed_pos(bar.min)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(bar.size(), Sense::hover());
                let mut painter = ui.painter().clone();
                painter.set_opacity(fade);
                painter.add(
                    Shadow {
                        offset: [0, 4],
                        blur: 14,
                        spread: 0,
                        color: Color32::from_black_alpha(70),
                    }
                    .as_shape(rect, RADIUS),
                );
                painter.rect(
                    rect,
                    RADIUS,
                    look.fill,
                    Stroke::new(1.0, look.p.line),
                    StrokeKind::Inside,
                );
                let mut b = Buttons {
                    ui: &mut *ui,
                    painter: &painter,
                    look,
                    live: interactive,
                    actions: &mut *actions,
                };
                for (item, r) in &layout.items {
                    match item {
                        Item::Back => {
                            if b.text_button(*r, "back", "← Back", "Back to the dashboard (the app keeps running)", None) {
                                b.actions.push(ToolbarAction::Back);
                            }
                        }
                        Item::Power => {
                            let hot = look.p.danger;
                            if b.icon_button(
                                *r,
                                "power",
                                Icon::Power,
                                "Power off this app (a 5 second countdown, which you can cancel)",
                                Style { hover: Some(hot), ..Style::default() },
                            ) {
                                power_click = true;
                            }
                        }
                        Item::Title(text) => b.label(*r, text, look.p.ink),
                        Item::Watching(n) => b.label_hover(
                            *r,
                            &format!("{n} watching"),
                            &format!("{n} sessions are watching this app"),
                        ),
                        Item::TakeBack => {
                            if b.text_button(
                                *r,
                                "take",
                                "Viewing · Take back",
                                "Someone else has the keyboard and mouse; take them back",
                                Some(look.p.accent),
                            ) {
                                b.actions.push(ToolbarAction::TakeControl);
                            }
                        }
                        Item::Menu(kind) => {
                            let open = menu == Some(*kind);
                            let (icon, id, tip, style) = match kind {
                                Menu::Stream => (
                                    Icon::StreamSettings,
                                    "stream",
                                    "Stream settings: frame rate, overlay, codec and transport",
                                    Style { active: open, color: open.then_some(look.p.accent), ..Style::default() },
                                ),
                                Menu::Sound => (
                                    if v.muted {
                                        Icon::SoundMuted
                                    } else {
                                        Icon::SoundOn
                                    },
                                    "sound",
                                    if v.muted {
                                        "Sound off"
                                    } else {
                                        "Sound on"
                                    },
                                    Style {
                                        active: open,
                                        color: v.muted.then_some(look.ink2),
                                        ..Style::default()
                                    },
                                ),
                                Menu::Controllers => (
                                    Icon::Controllers,
                                    "controllers",
                                    if v.pads.is_empty() {
                                        "No controller yet"
                                    } else {
                                        "Controllers sending input"
                                    },
                                    Style {
                                        active: open,
                                        color: (!v.pads.is_empty()).then_some(look.p.accent),
                                        badge: (!v.pads.is_empty()).then_some(v.pads.len()),
                                        ..Style::default()
                                    },
                                ),
                            };
                            if b.icon_button(*r, id, icon, tip, style) {
                                menu_click = Some(*kind);
                            }
                            if open {
                                menu_button = Some(*r);
                            }
                        }
                        Item::Capture => {
                            let tip = if v.mouse {
                                "Exclusive input: capture the mouse for games. Ctrl+Alt+Shift+T lets it go"
                            } else {
                                "Turn the mouse on to use exclusive input"
                            };
                            if b.icon_button(
                                *r,
                                "capture",
                                Icon::ExclusiveInput,
                                tip,
                                Style { enabled: v.mouse, ..Style::default() },
                            ) {
                                b.actions.push(ToolbarAction::Capture);
                            }
                        }
                        Item::Mouse => {
                            let on = v.mouse;
                            if b.icon_button(
                                *r,
                                "mouse",
                                if on { Icon::MouseOn } else { Icon::MouseOff },
                                if on {
                                    "Mouse on: click to stop sending the mouse (the keyboard and controllers still work)"
                                } else {
                                    "Mouse off: click to send the mouse again"
                                },
                                Style { color: (!on).then_some(look.p.warn), ..Style::default() },
                            ) {
                                b.actions.push(ToolbarAction::SetMouse(!on));
                            }
                        }
                        Item::Fullscreen => {
                            let tip = if v.fullscreen {
                                "Exit full screen (Cmd+Ctrl+F)"
                            } else {
                                "Full screen (Cmd+Ctrl+F)"
                            };
                            if b.icon_button(
                                *r,
                                "fullscreen",
                                if v.fullscreen {
                                    Icon::FullscreenExit
                                } else {
                                    Icon::FullscreenEnter
                                },
                                tip,
                                Style::default(),
                            ) {
                                b.actions.push(ToolbarAction::ToggleFullScreen);
                            }
                        }
                        Item::Stats => {
                            if b.stats_button(*r, v, layout.letter_w) {
                                b.actions.push(ToolbarAction::ToggleStats);
                            }
                        }
                    }
                }
                let actions = b.actions;
                // The tab that hangs from the bar's lower edge, in the middle.
                let tab = Rect::from_center_size(pos2(rect.center().x, rect.bottom()), vec2(36.0, 20.0));
                let resp = ui.interact(tab, ui.id().with("tab"), Sense::click());
                let live = interactive && v.connected;
                let hot = live && resp.hovered();
                painter.rect(
                    tab,
                    10.0,
                    look.p.panel,
                    Stroke::new(1.0, look.p.line),
                    StrokeKind::Inside,
                );
                icons::paint(
                    &painter,
                    Rect::from_center_size(tab.center(), Vec2::splat(14.0)),
                    Icon::ToolbarFold,
                    if !live {
                        look.p.ink_3
                    } else if hot {
                        look.p.ink
                    } else {
                        look.ink2
                    },
                );
                let tip = "Hide the toolbar (hover the thin bar at the top to bring it back)";
                if resp.on_hover_text(tip).clicked() && live {
                    hide_click = true;
                }
                self.tab = Some(tab);

                if let (Some(kind), true, Some(btn)) = (menu, interactive, menu_button) {
                    let at = menu_origin(kind, btn, screen);
                    open_rect = Some(draw_menu(ui, look, v, kind, at, actions));
                }
            });
        self.bar = Some(bar);
        self.menu_rect = open_rect;
        self.menu_button = menu_button;

        if power_click {
            self.menu = None;
            self.power_off = Some(PowerOff {
                started: Instant::now(),
            });
        }
        if let Some(kind) = menu_click {
            self.menu = if self.menu == Some(kind) {
                None
            } else {
                Some(kind)
            };
            ctx.request_repaint();
        }
        if hide_click {
            self.hide();
            ctx.request_repaint();
        }
    }

    fn draw_power_off(
        &mut self,
        ctx: &Context,
        v: &ToolbarView,
        look: &Look,
        screen: Rect,
        now: Instant,
        actions: &mut Vec<ToolbarAction>,
    ) {
        let Some(power) = self.power_off else { return };
        let p = &look.p;
        let mut cancel = false;
        let scrim = Id::new("cha-power-scrim");
        Area::new(scrim)
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::hover());
                let resp = ui.interact(rect, ui.id().with("scrim"), Sense::click());
                ui.painter().rect_filled(rect, 0.0, p.scrim);
                // A click outside the card cancels, as in the browser's dialog.
                cancel |= resp.clicked();
            });
        ctx.move_to_top(LayerId::new(Order::Foreground, scrim));
        let card = Id::new("cha-power-card");
        let name = if v.title.is_empty() {
            "the app"
        } else {
            v.title
        };
        Area::new(card)
            .order(Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let inner = egui::Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.line))
                    .corner_radius(RADIUS)
                    .inner_margin(20)
                    .show(ui, |ui| {
                        ui.set_width(344.0);
                        ui.spacing_mut().item_spacing = vec2(0.0, 12.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new(format!(
                                    "Powering off {name} in {}…",
                                    power.left(now)
                                ))
                                .font(FontId::proportional(16.0))
                                .strong()
                                .color(p.ink),
                            );
                            let (bar, _) = ui.allocate_exact_size(vec2(344.0, 4.0), Sense::hover());
                            ui.painter().rect_filled(bar, 2.0, p.line);
                            let mut full = bar;
                            full.set_width(bar.width() * power.fraction(now));
                            ui.painter().rect_filled(full, 2.0, p.danger);
                            ui.label(
                                RichText::new(
                                    "The app stops and its session ends. Saved data stays.",
                                )
                                .font(FontId::proportional(13.0))
                                .color(look.ink2),
                            );
                            let w = Buttons::wide_width(ui, "Cancel");
                            let (rect, _) = ui.allocate_exact_size(vec2(w, 28.0), Sense::hover());
                            let painter = ui.painter().clone();
                            let mut b = Buttons {
                                ui,
                                painter: &painter,
                                look,
                                live: true,
                                actions: &mut *actions,
                            };
                            cancel |= b.wide_button(
                                rect,
                                "cancel",
                                "Cancel",
                                true,
                                "Cancel the power off",
                            );
                        });
                    });
                let _ = inner;
            });
        ctx.move_to_top(LayerId::new(Order::Foreground, card));
        if cancel {
            self.power_off = None;
            ctx.request_repaint();
        } else if power.due(now) {
            self.power_off = None;
            actions.push(ToolbarAction::PowerOff);
        } else {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}

impl Default for Toolbar {
    fn default() -> Self {
        Self::new()
    }
}

// ---- layout ----------------------------------------------------------------

enum Item {
    Back,
    Power,
    Title(String),
    Watching(u32),
    TakeBack,
    Menu(Menu),
    Capture,
    Mouse,
    Fullscreen,
    Stats,
}

struct Layout {
    bar: Rect,
    items: Vec<(Item, Rect)>,
    /// The grade letter's width, for the stats button.
    letter_w: f32,
}

fn galley(painter: &Painter, text: &str, size: f32, color: Color32) -> Arc<Galley> {
    painter.layout_no_wrap(text.to_string(), FontId::proportional(size), color)
}

impl Layout {
    fn new(ctx: &Context, look: &Look, v: &ToolbarView, screen: Rect) -> Self {
        let painter = ctx.layer_painter(LayerId::new(
            Order::Foreground,
            Id::new("cha-toolbar-measure"),
        ));
        let text_w = |s: &str| galley(&painter, s, TEXT, look.p.ink).size().x;
        let grade = v.health.grade.map(|g| g.letter());
        let letter_w = grade.map_or(0.0, |g| {
            painter
                .layout_no_wrap(g.to_string(), FontId::monospace(TEXT), look.p.ink)
                .size()
                .x
        });
        let viewers = v.link.and_then(|l| l.viewers).filter(|n| *n > 1);
        let viewing = v.link.is_some_and(|l| l.control == Some(false));
        let build = |with_labels: bool| {
            let mut items: Vec<(Item, f32)> =
                vec![(Item::Back, text_w("← Back") + 24.0), (Item::Power, BTN)];
            if with_labels && !v.title.is_empty() {
                let w = galley(&painter, v.title, 13.0, look.p.ink)
                    .size()
                    .x
                    .min(192.0)
                    + 16.0;
                items.push((Item::Title(v.title.to_string()), w));
            }
            if with_labels && let Some(n) = viewers {
                items.push((Item::Watching(n), text_w(&format!("{n} watching")) + 16.0));
            }
            if viewing {
                items.push((Item::TakeBack, text_w("Viewing · Take back") + 24.0));
            }
            items.extend([
                (Item::Menu(Menu::Stream), BTN),
                (Item::Capture, BTN),
                (Item::Mouse, BTN),
                (Item::Menu(Menu::Sound), BTN),
                (Item::Menu(Menu::Controllers), BTN),
                (Item::Fullscreen, BTN),
                (
                    Item::Stats,
                    (8.0 + 16.0 + 8.0 + if grade.is_some() { GAP + letter_w } else { 0.0 })
                        .max(BTN),
                ),
            ]);
            let width: f32 = items.iter().map(|(_, w)| w).sum::<f32>()
                + GAP * (items.len() - 1) as f32
                + 2.0 * PAD;
            (items, width)
        };
        let (mut items, mut width) = build(true);
        if width > screen.width() - 16.0 {
            (items, width) = build(false);
        }
        let x0 = (screen.center().x - width / 2.0).max(screen.left() + 8.0);
        let bar = Rect::from_min_size(pos2(x0, screen.top() + TOP), vec2(width, BAR_H));
        let mut x = bar.left() + PAD;
        let items = items
            .into_iter()
            .map(|(item, w)| {
                let r = Rect::from_min_size(pos2(x, bar.top() + PAD), vec2(w, BTN));
                x += w + GAP;
                (item, r)
            })
            .collect();
        Self {
            bar,
            items,
            letter_w,
        }
    }
}

/// Where a menu's top left goes: under its button, the sound and stream ones
/// from its left edge and the controllers one from its right edge.
fn menu_origin(kind: Menu, button: Rect, screen: Rect) -> Pos2 {
    let x = match kind {
        Menu::Stream | Menu::Sound => button.left(),
        Menu::Controllers => button.right() - MENU_W,
    };
    let x = x.clamp(
        screen.left() + 8.0,
        (screen.right() - 8.0 - MENU_W).max(screen.left() + 8.0),
    );
    pos2(x, button.bottom() + 8.0)
}

// ---- buttons ---------------------------------------------------------------

#[derive(Clone, Copy)]
struct Style {
    /// The icon's colour at rest (ink-2 if none).
    color: Option<Color32>,
    /// The icon's colour under the pointer (ink if none).
    hover: Option<Color32>,
    /// The button's menu is open, or it is switched on: a lit background.
    active: bool,
    enabled: bool,
    /// A count in a dot at the corner.
    badge: Option<usize>,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            color: None,
            hover: None,
            active: false,
            enabled: true,
            badge: None,
        }
    }
}

struct Buttons<'a, 'u> {
    ui: &'a mut Ui,
    painter: &'a Painter,
    look: &'a Look,
    /// False while the bar is fading out: it takes no clicks.
    live: bool,
    actions: &'u mut Vec<ToolbarAction>,
}

impl Buttons<'_, '_> {
    fn id(&self, name: &str) -> Id {
        self.ui.id().with(("tb", name))
    }

    fn lit(&self, rect: Rect) {
        self.painter
            .rect_filled(rect, 6.0, self.look.p.line.gamma_multiply(0.6));
    }

    fn icon_button(&mut self, rect: Rect, name: &str, icon: Icon, tip: &str, style: Style) -> bool {
        let resp = self.ui.interact(rect, self.id(name), Sense::click());
        let live = self.live && style.enabled;
        let hot = live && resp.hovered();
        if hot || style.active {
            self.lit(rect);
        }
        let look = self.look;
        let color = if !style.enabled {
            look.p.ink_3
        } else if hot {
            style.hover.unwrap_or(look.p.ink)
        } else {
            style.color.unwrap_or(look.ink2)
        };
        icons::paint(
            self.painter,
            Rect::from_center_size(rect.center(), Vec2::splat(16.0)),
            icon,
            color,
        );
        if let Some(n) = style.badge {
            let g = self.painter.layout_no_wrap(
                n.to_string(),
                FontId::proportional(9.0),
                look.p.on_accent,
            );
            let w = g.size().x.max(6.0) + 8.0;
            let dot = Rect::from_min_size(
                pos2(rect.right() - w + 2.0, rect.top() - 2.0),
                vec2(w, 14.0),
            );
            self.painter.rect_filled(dot, 7.0, look.p.accent);
            self.painter
                .galley(dot.center() - g.size() / 2.0, g, look.p.on_accent);
        }
        resp.on_hover_text(tip).clicked() && live
    }

    /// A ghost button with text, like "← Back".
    fn text_button(
        &mut self,
        rect: Rect,
        name: &str,
        text: &str,
        tip: &str,
        color: Option<Color32>,
    ) -> bool {
        let resp = self.ui.interact(rect, self.id(name), Sense::click());
        let hot = self.live && resp.hovered();
        if hot {
            self.lit(rect);
        }
        let ink = if hot {
            self.look.p.ink
        } else {
            color.unwrap_or(self.look.p.ink)
        };
        let g = galley(self.painter, text, TEXT, ink);
        self.painter.galley(rect.center() - g.size() / 2.0, g, ink);
        resp.on_hover_text(tip).clicked() && self.live
    }

    fn label(&mut self, rect: Rect, text: &str, color: Color32) {
        let mut job = egui::text::LayoutJob::single_section(
            text.to_string(),
            egui::TextFormat::simple(FontId::proportional(13.0), color),
        );
        job.wrap.max_width = rect.width() - 16.0;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
        let g = self.painter.layout_job(job);
        self.painter.galley(
            pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0),
            g,
            color,
        );
    }

    fn label_hover(&mut self, rect: Rect, text: &str, tip: &str) {
        let color = self.look.ink2;
        let g = galley(self.painter, text, TEXT, color);
        self.painter
            .galley(rect.center() - g.size() / 2.0, g, color);
        self.ui
            .interact(rect, self.id(text), Sense::hover())
            .on_hover_text(tip);
    }

    /// The stats button: bars, and the health grade's letter in its colour.
    fn stats_button(&mut self, rect: Rect, v: &ToolbarView, letter_w: f32) -> bool {
        let look = self.look;
        let resp = self.ui.interact(rect, self.id("stats"), Sense::click());
        let hot = self.live && resp.hovered();
        if hot || v.stats_open {
            self.lit(rect);
        }
        let ink = if v.stats_open {
            look.p.accent
        } else if hot {
            look.p.ink
        } else {
            look.ink2
        };
        let icon_at = pos2(
            if v.health.grade.is_some() {
                rect.left() + 8.0 + 8.0
            } else {
                rect.center().x
            },
            rect.center().y,
        );
        icons::paint(
            self.painter,
            Rect::from_center_size(icon_at, Vec2::splat(16.0)),
            Icon::Stats,
            ink,
        );
        if let Some(grade) = v.health.grade {
            let g = self.painter.layout_no_wrap(
                grade.letter().to_string(),
                FontId::monospace(TEXT),
                look.grade_color(Some(grade)),
            );
            let at = pos2(
                rect.right() - 8.0 - letter_w,
                rect.center().y - g.size().y / 2.0,
            );
            self.painter.galley(at, g, look.grade_color(Some(grade)));
        }
        let mut tip = if v.stats_open {
            "Hide the stats panel".to_string()
        } else {
            "Show the stats panel".to_string()
        };
        if v.health.grade.is_some() {
            tip.push_str(&format!("\nStream health: {}", v.health.summary));
        }
        resp.on_hover_text(tip).clicked() && self.live
    }

    fn wide_width(ui: &Ui, text: &str) -> f32 {
        ui.painter()
            .layout_no_wrap(text.to_string(), FontId::proportional(TEXT), Color32::WHITE)
            .size()
            .x
            + 24.0
    }

    /// A bordered ghost button with text, in a menu.
    fn wide_button(
        &mut self,
        rect: Rect,
        name: &str,
        text: &str,
        enabled: bool,
        tip: &str,
    ) -> bool {
        let look = self.look;
        let resp = self.ui.interact(rect, self.id(name), Sense::click());
        let hot = enabled && resp.hovered();
        self.painter.rect(
            rect,
            8.0,
            if hot {
                look.p.panel_2
            } else {
                Color32::TRANSPARENT
            },
            Stroke::new(1.0, look.p.line_strong),
            StrokeKind::Inside,
        );
        let ink = if enabled { look.p.ink } else { look.p.ink_3 };
        let g = galley(self.painter, text, TEXT, ink);
        self.painter.galley(rect.center() - g.size() / 2.0, g, ink);
        resp.on_hover_text(tip).clicked() && enabled
    }
}

// ---- menus -----------------------------------------------------------------

/// Draws the open menu at `at` and returns its rect.
fn draw_menu(
    ui: &mut Ui,
    look: &Look,
    v: &ToolbarView,
    kind: Menu,
    at: Pos2,
    actions: &mut Vec<ToolbarAction>,
) -> Rect {
    let p = look.p.clone();
    let area = Rect::from_min_size(at, vec2(MENU_W, 600.0));
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(("tb-menu", kind as u8))
            .max_rect(area)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    let shape = child.painter().add(Shape::Noop);
    let inner = egui::Frame::new()
        .inner_margin(MENU_PAD)
        .show(&mut child, |ui| {
            ui.set_width(MENU_W - 2.0 * MENU_PAD);
            style_menu(ui, &p);
            match kind {
                Menu::Stream => stream_menu(ui, look, v, actions),
                Menu::Sound => sound_menu(ui, look, v, actions),
                Menu::Controllers => controllers_menu(ui, look, v, actions),
            }
        });
    let rect = inner.response.rect;
    child.painter().set(
        shape,
        egui::epaint::RectShape::new(
            rect,
            RADIUS,
            p.panel,
            Stroke::new(1.0, p.line),
            StrokeKind::Inside,
        ),
    );
    child.painter().add(
        Shadow {
            offset: [0, 4],
            blur: 14,
            spread: 0,
            color: Color32::from_black_alpha(60),
        }
        .as_shape(rect, RADIUS),
    );
    rect
}

/// The widgets' look inside a menu: the overlay palette.
fn style_menu(ui: &mut Ui, p: &Palette) {
    ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
    ui.spacing_mut().interact_size.y = 16.0;
    ui.spacing_mut().slider_rail_height = 3.0;
    ui.spacing_mut().slider_width = 112.0;
    let v = ui.visuals_mut();
    v.override_text_color = Some(p.ink);
    v.widgets.inactive.bg_fill = p.line_strong;
    v.widgets.inactive.weak_bg_fill = p.panel_2;
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.ink);
    v.widgets.hovered.bg_fill = p.panel_2;
    v.widgets.hovered.weak_bg_fill = p.panel_2;
    v.widgets.active.bg_fill = p.panel_2;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, p.ink);
    ui.style_mut().override_font_id = Some(FontId::proportional(TEXT));
}

fn dim(look: &Look, text: &str) -> RichText {
    RichText::new(text).color(look.ink2)
}

/// A paragraph of small print in a menu.
fn note(ui: &mut Ui, look: &Look, text: &str, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).color(color).size(11.0)).wrap());
    let _ = look;
}

/// A value that can't be changed here, in a box like the portal's select.
fn read_only(ui: &mut Ui, look: &Look, text: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    ui.painter().rect(
        rect,
        8.0,
        look.p.canvas,
        Stroke::new(1.0, look.p.line_strong),
        StrokeKind::Inside,
    );
    let g = galley(ui.painter(), text, TEXT, look.ink2);
    ui.painter().galley(
        pos2(rect.left() + 8.0, rect.center().y - g.size().y / 2.0),
        g,
        look.ink2,
    );
}

/// Chips side by side, one lit: the picker for a few fixed choices.
fn segmented<T: Copy + PartialEq>(
    ui: &mut Ui,
    look: &Look,
    name: &str,
    options: &[(&str, T)],
    selected: Option<T>,
    enabled: bool,
    tip: &str,
) -> Option<T> {
    let gap = 4.0;
    let w = (ui.available_width() - gap * (options.len() as f32 - 1.0)) / options.len() as f32;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let mut picked = None;
    for (i, (label, value)) in options.iter().enumerate() {
        let rect = Rect::from_min_size(
            pos2(row.left() + i as f32 * (w + gap), row.top()),
            vec2(w, 24.0),
        );
        let resp = ui.interact(rect, ui.id().with((name, i)), Sense::click());
        let on = selected == Some(*value);
        let hot = enabled && resp.hovered();
        let (fill, border, ink) = if !enabled {
            (look.p.canvas, look.p.line, look.p.ink_3)
        } else if on {
            (look.p.accent_soft, look.p.accent, look.p.ink)
        } else if hot {
            (look.p.panel_2, look.p.accent, look.p.ink)
        } else {
            (look.p.canvas, look.p.line_strong, look.ink2)
        };
        ui.painter().rect(
            rect,
            8.0,
            fill,
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let g = galley(ui.painter(), label, 10.5, ink);
        ui.painter().galley(rect.center() - g.size() / 2.0, g, ink);
        if resp.on_hover_text(tip).clicked() && enabled {
            picked = Some(*value);
        }
    }
    picked
}

fn overlay_label(level: u8) -> &'static str {
    match level {
        0 => "Off",
        1 => "FPS",
        2 => "Bar",
        3 => "Detailed",
        _ => "Full",
    }
}

/// How the stream travels, for the read-only line.
pub fn transport_text(name: &str, tag: &str) -> String {
    match tag {
        "WT" => "WebTransport".into(),
        "" if name == "Moonlight" => "GameStream (Moonlight)".into(),
        "" => name.to_string(),
        other => other.to_string(),
    }
}

fn stream_menu(ui: &mut Ui, look: &Look, v: &ToolbarView, actions: &mut Vec<ToolbarAction>) {
    let link = v.link;
    let has_floor = link.and_then(|l| l.control);
    let can_change = has_floor == Some(true) && v.connected;
    // The rate the streamer runs at now, else the one asked for.
    let fps = link
        .and_then(|l| l.target_fps)
        .or(v.stats.target_fps)
        .unwrap_or(0);

    ui.label(dim(look, "Video codec"));
    let codec = v.stats.codec.to_uppercase();
    read_only(ui, look, if codec.is_empty() { "–" } else { &codec });

    ui.label(dim(look, "Frame rate"));
    if has_floor.is_some() && FRAME_RATES.contains(&fps) {
        let options: Vec<(String, u32)> = FRAME_RATES
            .iter()
            .map(|r| (format!("{r} fps"), *r))
            .collect();
        let options: Vec<(&str, u32)> = options.iter().map(|(s, r)| (s.as_str(), *r)).collect();
        let tip = if has_floor == Some(true) {
            "Frame rate"
        } else {
            "Only the session with the controls changes the frame rate"
        };
        if let Some(rate) = segmented(ui, look, "fps", &options, Some(fps), can_change, tip) {
            actions.push(ToolbarAction::SetFps(rate));
        }
    } else {
        read_only(
            ui,
            look,
            &if fps > 0 {
                format!("{fps} fps")
            } else {
                "–".into()
            },
        );
    }

    if let Some(overlay) = link.and_then(|l| l.overlay) {
        ui.label(dim(look, "Steam Gamescope Overlay"));
        let levels: Vec<(&str, u8)> = (0..=OVERLAY_MAX).map(|l| (overlay_label(l), l)).collect();
        let selected = match overlay {
            PerfOverlay::Preset(l) => Some(l),
            PerfOverlay::Custom => None,
        };
        let tip = if has_floor == Some(true) {
            "Steam Gamescope Overlay"
        } else {
            "Only the session with the controls changes the overlay"
        };
        if let Some(level) = segmented(ui, look, "overlay", &levels, selected, can_change, tip) {
            actions.push(ToolbarAction::SetOverlay(level));
        }
        if overlay == PerfOverlay::Custom {
            note(
                ui,
                look,
                "A hand-written overlay config is in use.",
                look.ink2,
            );
        }
    }

    ui.label(dim(look, "Transport"));
    read_only(
        ui,
        look,
        &transport_text(v.transport, v.stats.transport_tag),
    );

    if has_floor.is_none() {
        note(
            ui,
            look,
            "The frame rate and codec are set in Settings and apply to the next launch.",
            look.p.ink_3,
        );
    } else {
        note(
            ui,
            look,
            "The codec is chosen when the stream starts (Settings).",
            look.p.ink_3,
        );
    }
}

fn sound_menu(ui: &mut Ui, look: &Look, v: &ToolbarView, actions: &mut Vec<ToolbarAction>) {
    let p = &look.p;
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
    let painter = ui.painter().clone();
    let mut b = Buttons {
        ui,
        painter: &painter,
        look,
        live: true,
        actions,
    };
    let text = if v.muted {
        "Turn sound on"
    } else {
        "Turn sound off"
    };
    if b.wide_button(rect, "mute", text, true, "Sound on or off") {
        b.actions.push(ToolbarAction::SetMuted(!v.muted));
    }
    let ui = b.ui;
    let shown = if v.muted { 0 } else { v.volume };
    let mut volume = u32::from(shown);
    ui.horizontal(|ui| {
        ui.label(dim(look, "Volume"));
        ui.add(
            egui::Slider::new(&mut volume, 0..=100)
                .step_by(5.0)
                .trailing_fill(true)
                .show_value(false),
        );
        ui.add_sized(vec2(30.0, 16.0), egui::Label::new(format!("{volume}%")));
    });
    if volume != u32::from(shown) {
        actions.push(ToolbarAction::SetVolume(volume as u8));
    }
    if !v.muted {
        let (rect, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
        let painter = ui.painter().clone();
        let mut b = Buttons {
            ui,
            painter: &painter,
            look,
            live: true,
            actions,
        };
        if b.wide_button(
            rect,
            "restart",
            "Restart sound",
            true,
            "Sound went quiet? This opens the sound output again without reconnecting",
        ) {
            b.actions.push(ToolbarAction::RestartSound);
        }
    }
    let _ = p;
}

fn controllers_menu(ui: &mut Ui, look: &Look, v: &ToolbarView, actions: &mut Vec<ToolbarAction>) {
    let p = &look.p;
    if v.pads.is_empty() {
        note(
            ui,
            look,
            "Press a button on a controller. If nothing shows, connect it in macOS (a cable, or Bluetooth in System Settings) and press again.",
            look.ink2,
        );
    } else {
        for pad in v.pads {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
            let slot = format!("slot {}", pad.slot + 1);
            let sg = galley(ui.painter(), &slot, TEXT, p.ink_3);
            let sx = rect.right() - sg.size().x;
            ui.painter()
                .galley(pos2(sx, rect.center().y - sg.size().y / 2.0), sg, p.ink_3);
            let mut job = egui::text::LayoutJob::single_section(
                pad.name.clone(),
                egui::TextFormat::simple(FontId::proportional(TEXT), p.ink),
            );
            job.wrap.max_width = sx - rect.left() - 8.0;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            job.wrap.overflow_character = Some('…');
            let g = ui.painter().layout_job(job);
            ui.painter().galley(
                pos2(rect.left(), rect.center().y - g.size().y / 2.0),
                g,
                p.ink,
            );
        }
        note(
            ui,
            look,
            "Every controller shown here is sending input to the stream.",
            look.p.ink_3,
        );
    }
    if v.input_access == InputAccess::Denied && v.pads.is_empty() {
        note(
            ui,
            look,
            "Some controllers (Steam Controller) need Input Monitoring: allow Cha Player in System Settings → Privacy & Security → Input Monitoring, then reopen it.",
            p.warn,
        );
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(vec2(w, 26.0), Sense::hover());
        let painter = ui.painter().clone();
        let mut b = Buttons {
            ui,
            painter: &painter,
            look,
            live: true,
            actions,
        };
        if b.wide_button(
            rect,
            "input-settings",
            "Open Settings",
            true,
            "Open System Settings at Input Monitoring",
        ) {
            b.actions.push(ToolbarAction::OpenInputSettings);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{DeviceId, MouseButton};

    fn click(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        }
    }

    fn moved() -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(0.0, 0.0),
        }
    }

    fn drawn(bar: Rect) -> Toolbar {
        let mut t = Toolbar::new();
        t.bar = Some(bar);
        t.tab = Some(Rect::from_center_size(
            pos2(bar.center().x, bar.bottom()),
            vec2(36.0, 20.0),
        ));
        t.shown = true;
        t
    }

    fn bar() -> Rect {
        Rect::from_min_size(pos2(300.0, 12.0), vec2(600.0, 40.0))
    }

    #[test]
    fn it_shows_while_there_is_a_reason_to() {
        let mut t = Toolbar::new();
        assert!(t.visible(true, false), "a new stream starts with it up");
        assert!(!t.visible(true, true), "captured: folded");
        let now = Instant::now();
        t.hide();
        assert!(!t.visible(true, false));
        assert!(t.visible(false, false), "reconnecting always shows it");
        t.set_hover(true, now);
        assert!(t.visible(true, false));
        t.set_hover(false, now);
        t.menu = Some(Menu::Sound);
        assert!(t.visible(true, false), "a menu keeps it up");
        t.menu = None;
        t.expand();
        assert!(t.visible(true, false), "clicking the thin bar opens it");
        t.on_capture();
        assert!(!t.visible(true, false), "capturing folds it");
        t.show_now();
        assert!(t.visible(true, false), "Ctrl+Alt+Shift+T");
    }

    #[test]
    fn it_folds_a_moment_after_the_pointer_leaves() {
        let mut t = Toolbar::new();
        let t0 = Instant::now();
        t.set_hover(true, t0);
        t.set_hover(false, t0);
        t.tick(t0 + Duration::from_millis(1000));
        assert!(t.expanded, "not yet");
        t.tick(t0 + Duration::from_millis(1200));
        assert!(!t.expanded);
    }

    #[test]
    fn the_pointer_near_the_top_keeps_it_and_further_down_folds_it() {
        let mut t = Toolbar::new();
        let t0 = Instant::now();
        t.pointer_y(500.0, t0);
        assert!(t.collapse_at.is_some());
        t.pointer_y(20.0, t0 + Duration::from_millis(100));
        assert!(t.collapse_at.is_none(), "back near the top");
        t.pointer_y(500.0, t0);
        t.set_hover(true, t0);
        assert!(t.collapse_at.is_none(), "on the bar it stays");
        t.pointer_y(500.0, t0);
        assert!(
            t.collapse_at.is_none(),
            "hovering ignores the pointer's height"
        );
    }

    #[test]
    fn hiding_closes_menus_and_waits_for_the_thin_bar() {
        let mut t = Toolbar::new();
        t.menu = Some(Menu::Stream);
        t.hide();
        assert!(t.menu.is_none() && !t.expanded && !t.hover);
        let thin = Rect::from_min_size(pos2(568.0, 0.0), vec2(144.0, 24.0));
        t.thin = Some(thin);
        let now = Instant::now();
        // The pointer on the thin bar is the toolbar's, and brings it back.
        assert!(t.consumes_at(Some(pos2(600.0, 10.0)), &moved(), now));
        assert!(t.expanded);
        // Away from it, in the picture, it is not.
        t.thin = None;
        assert!(!t.consumes_at(Some(pos2(600.0, 400.0)), &moved(), now));
    }

    #[test]
    fn clicks_on_the_bar_never_reach_the_host() {
        let mut t = drawn(bar());
        let on = Some(pos2(400.0, 30.0));
        let off = Some(pos2(400.0, 400.0));
        let now = Instant::now();
        assert!(!t.consumes_at(off, &click(ElementState::Pressed), now));
        assert!(!t.consumes_at(off, &click(ElementState::Released), now));
        assert!(t.consumes_at(on, &click(ElementState::Pressed), now));
        // Dragged off the bar, the release is still the bar's.
        assert!(t.consumes_at(off, &moved(), now));
        assert!(t.consumes_at(off, &click(ElementState::Released), now));
        assert!(!t.consumes_at(off, &moved(), now));
        // The tab hangs below the bar.
        assert!(t.consumes_at(Some(pos2(600.0, 58.0)), &click(ElementState::Pressed), now));
        t.consumes_at(None, &click(ElementState::Released), now);
        assert!(t.consumes_at(
            on,
            &WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: winit::event::MouseScrollDelta::LineDelta(0.0, 1.0),
                phase: winit::event::TouchPhase::Moved,
            },
            now
        ));
    }

    #[test]
    fn a_menu_is_the_toolbars_and_a_press_outside_it_shuts_it() {
        let mut t = drawn(bar());
        t.menu = Some(Menu::Controllers);
        t.menu_rect = Some(Rect::from_min_size(pos2(644.0, 60.0), vec2(256.0, 120.0)));
        t.menu_button = Some(Rect::from_min_size(pos2(850.0, 18.0), vec2(28.0, 28.0)));
        let now = Instant::now();
        let in_menu = Some(pos2(700.0, 100.0));
        assert!(t.consumes_at(in_menu, &click(ElementState::Pressed), now));
        assert!(t.menu.is_some(), "inside the menu it stays");
        t.consumes_at(in_menu, &click(ElementState::Released), now);
        // On its own button the toolbar's click handler toggles it.
        let on_button = Some(pos2(860.0, 30.0));
        assert!(t.consumes_at(on_button, &click(ElementState::Pressed), now));
        assert!(t.menu.is_some());
        t.consumes_at(on_button, &click(ElementState::Released), now);
        // In the picture it shuts, and the click goes on to the picture.
        let picture = Some(pos2(100.0, 500.0));
        assert!(!t.consumes_at(picture, &click(ElementState::Pressed), now));
        assert!(t.menu.is_none());
    }

    #[test]
    fn escape_closes_a_menu_or_cancels_the_countdown_and_nothing_else() {
        let mut t = Toolbar::new();
        assert!(!t.escape(), "nothing open: the key goes to the stream");
        t.menu = Some(Menu::Sound);
        assert!(t.escape());
        assert!(t.menu.is_none());
        assert!(!t.escape());
        t.power_off = Some(PowerOff {
            started: Instant::now(),
        });
        assert!(t.escape());
        assert!(!t.counting_down());
    }

    #[test]
    fn a_countdown_holds_every_pointer_event() {
        let mut t = Toolbar::new();
        t.power_off = Some(PowerOff {
            started: Instant::now(),
        });
        let now = Instant::now();
        let anywhere = Some(pos2(5.0, 700.0));
        assert!(t.consumes_at(anywhere, &click(ElementState::Pressed), now));
        assert!(t.consumes_at(anywhere, &click(ElementState::Released), now));
        assert!(t.consumes_at(anywhere, &moved(), now));
    }

    #[test]
    fn the_countdown_runs_five_seconds() {
        let t0 = Instant::now();
        let p = PowerOff { started: t0 };
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        assert_eq!(p.left(at(0)), 5);
        assert_eq!(p.left(at(900)), 5);
        assert_eq!(p.left(at(1000)), 4);
        assert_eq!(p.left(at(4900)), 1);
        assert!(!p.due(at(4900)));
        assert_eq!(p.left(at(5000)), 0);
        assert!(p.due(at(5000)));
        assert_eq!(p.fraction(at(0)), 1.0);
        assert!((p.fraction(at(2500)) - 0.5).abs() < 1e-6);
        assert_eq!(p.fraction(at(9000)), 0.0);
    }

    #[test]
    fn a_capture_clears_what_was_up() {
        let mut t = drawn(bar());
        t.menu = Some(Menu::Stream);
        t.power_off = Some(PowerOff {
            started: Instant::now(),
        });
        t.on_capture();
        assert!(t.bar.is_none() && t.menu.is_none() && !t.counting_down());
        assert_eq!(t.inset(), 0.0);
        let now = Instant::now();
        assert!(!t.consumes_at(Some(pos2(400.0, 30.0)), &click(ElementState::Pressed), now));
    }

    #[test]
    fn the_transport_reads_as_a_person_would_say_it() {
        assert_eq!(transport_text("Cha Portal", "WT"), "WebTransport");
        assert_eq!(transport_text("Moonlight", ""), "GameStream (Moonlight)");
        assert_eq!(transport_text("Other", ""), "Other");
    }
}

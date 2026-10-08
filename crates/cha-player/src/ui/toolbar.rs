//! The toolbar over the picture: the native twin of the portal's in-stream
//! toolbar (`SessionToolbar.vue`). A bar across the top while the pointer is
//! free, folding into a thin bar you hover or click; Back, power off, the
//! stream settings, exclusive input, the mouse switch, sound, controllers,
//! full screen and the stats button. Ctrl+Alt+Shift+T shows it.
//!
//! What the toolbar says and when each control shows (its icon, tooltip and
//! state, the menus' rows, the countdown's words and length, the fold timing)
//! is `cha_ui_spec::toolbar::build_toolbar`'s, from
//! `web/packages/ui-spec/toolbar.json`: the same model the browser draws.
//! This file only lays it out and draws it, and carries out what the user
//! picks. The session reports what it can do as capabilities
//! ([`capabilities`]); the parts the two players share (the look, the icon
//! button, the menu box, the chips, which pointer events are an overlay's) are
//! in [`super::chrome`].
//!
//! [`Toolbar`] owns what the user is doing to it (folded or not, which menu is
//! open, a power-off countdown) and where it was drawn, so the app can ask
//! which pointer events are its own ([`Toolbar::consumes`]). It never changes
//! the stream itself: [`Toolbar::show`] returns [`ToolbarAction`]s for the app
//! to carry out, and takes the current state in a [`ToolbarView`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use cha_client::{PerfOverlay, TransportStats};
use cha_ui_spec::health::Platform;
use cha_ui_spec::toolbar::{
    Align as MenuAlign, ControlKind, RowKind, State, Toolbar as Model, ToolbarControl, ToolbarMenu,
    ToolbarRow, build_toolbar, spec,
};
use egui::{
    Area, Color32, Context, FontId, Galley, Id, LayerId, Order, Painter, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};
use winit::event::WindowEvent;

use super::chrome::{self, Hold, IconStyle, Look, MenuFrame, Press};
use super::stats::StatsSnapshot;
use crate::health::Assessment;
use crate::input::pads::{InputAccess, PadInfo};
use crate::theme::ThemeExt;
use crate::theme::icons::{self, Icon};

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
const TEXT: f32 = 12.0;
const FADE: f32 = 0.15;

/// Folds this long after the pointer leaves it (the spec's `timing`).
fn collapse_after() -> Duration {
    Duration::from_millis(spec().timing.fold_after_ms)
}

/// The pointer this close to the top keeps an open toolbar from folding.
fn near_top() -> f32 {
    spec().timing.near_top_px
}

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

/// The capabilities a session reports to the toolbar (the spec's vocabulary).
/// The portal's streamer reports a keyboard-and-mouse floor, so those
/// controls, the frame rate and the sound restart show; Moonlight's transport
/// says nothing of the kind, so the rest does not. Sound restarts locally and
/// so is always on. Nothing here is the browser's (Share, the GPU badge, Hand
/// controls, the codec and transport pickers, the probe, WebHID).
pub fn capabilities(link: Option<&TransportStats>) -> Vec<String> {
    let mut caps = vec!["sound-restart".to_string()];
    if let Some(l) = link {
        if l.viewers.is_some() {
            caps.push("viewers".into());
        }
        if l.control.is_some() {
            caps.push("control-handoff".into());
            caps.push("fps-change".into());
        }
        if l.overlay.is_some() {
            caps.push("steam-overlay".into());
        }
    }
    caps
}

impl ToolbarView<'_> {
    /// The session's state as the spec's model reads it (the bar's own, expanded or open, is added
    /// by [`Toolbar`]).
    fn state(&self) -> State {
        let link = self.link;
        let mut s = State::new();
        s.set("connected", self.connected)
            .set("captured", self.locked)
            .set("title", self.title)
            .set("viewers", link.and_then(|l| l.viewers))
            .set("has_control", link.is_none_or(|l| l.control != Some(false)))
            .set("mouse_on", self.mouse)
            .set("muted", self.muted)
            .set("volume", self.volume)
            .set("fullscreen", self.fullscreen)
            .set("stats_open", self.stats_open)
            .set("transport_name", self.transport)
            .set("transport_tag", self.stats.transport_tag)
            .set("input_denied", self.input_access == InputAccess::Denied);
        // The codec the stream decodes, upper case; the rate the host runs at now, else the one asked for.
        s.set("codec", self.stats.codec.to_uppercase());
        let fps = link
            .and_then(|l| l.target_fps)
            .or(self.stats.target_fps)
            .filter(|f| *f > 0);
        s.set("fps", fps);
        s.set(
            "overlay",
            link.and_then(|l| l.overlay).map(|o| match o {
                PerfOverlay::Preset(level) => level.to_string(),
                PerfOverlay::Custom => "custom".to_string(),
            }),
        );
        s.set(
            "controllers",
            self.pads
                .iter()
                .map(|p| serde_json::json!({ "name": p.name, "slot": p.slot }))
                .collect::<Vec<_>>(),
        );
        s.set("grade", self.health.grade.map_or("", |g| g.letter()));
        s.set("grade_summary", self.health.summary);
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Stream,
    Sound,
    Controllers,
}

impl Menu {
    /// The spec's id for the control that opens it.
    fn id(self) -> &'static str {
        match self {
            Menu::Stream => "stream",
            Menu::Sound => "sound",
            Menu::Controllers => "controllers",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        [Menu::Stream, Menu::Sound, Menu::Controllers]
            .into_iter()
            .find(|m| m.id() == id)
    }
}

/// The countdown before the app is stopped.
#[derive(Clone, Copy, Debug)]
struct PowerOff {
    started: Instant,
}

impl PowerOff {
    fn total() -> Duration {
        Duration::from_secs(u64::from(spec().power_off.seconds))
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
    press: Press,
    /// The bar's height when it showed last: the stats panel sits below it.
    height: f32,
    /// Whether the last frame had the bar up (the fade has finished starting).
    shown: bool,
    /// Controls and rows the spec has that this file did not draw (tests read it).
    #[cfg(test)]
    unhandled: Vec<String>,
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
            press: Press::default(),
            height: 0.0,
            shown: false,
            #[cfg(test)]
            unhandled: Vec::new(),
        }
    }

    /// A new stream: shown in full, as the portal starts it.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    // ---- visibility ----

    /// The model for this view: the session's state with the bar's own laid over.
    fn model(&self, v: &ToolbarView, countdown: Option<u32>) -> Model {
        let mut s = v.state();
        self.bar_state(&mut s, countdown);
        build_toolbar(spec(), &s, &capabilities(v.link), Platform::Native)
    }

    fn bar_state(&self, s: &mut State, countdown: Option<u32>) {
        s.set("expanded", self.expanded)
            .set("hover", self.hover)
            .set("menu_open", self.menu.map_or("", Menu::id))
            .set("countdown", countdown);
    }

    /// Whether the bar is up (the spec's `visibility` rules), given only the bar's own state.
    #[cfg(test)]
    fn visible(&self, connected: bool, locked: bool) -> bool {
        let mut s = State::new();
        s.set("connected", connected).set("captured", locked);
        self.bar_state(&mut s, self.power_off.map(|p| p.left(Instant::now())));
        build_toolbar(spec(), &s, &[], Platform::Native).visible
    }

    fn collapse_soon(&mut self, now: Instant) {
        self.collapse_at = Some(now + collapse_after());
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
        self.press.clear();
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
        if y < near_top() {
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
        self.menu = Menu::from_id(name);
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
        // A press anywhere but the open menu (and its button) shuts it.
        if let WindowEvent::MouseInput {
            state: winit::event::ElementState::Pressed,
            ..
        } = event
            && self.menu.is_some()
            && !inside(self.menu_rect)
            && !inside(self.menu_button)
        {
            self.menu = None;
        }
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            if over_thin {
                // Hovering the thin bar brings the toolbar back.
                self.expand();
            }
            if let Some(p) = pos {
                self.pointer_y(p.y, now);
            }
        }
        let hold = if held { Hold::All } else { Hold::Nothing };
        self.press.claims(event, over || over_thin, hold)
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

        let model = self.model(v, self.power_off.map(|p| p.left(now)));
        let visible = model.visible;
        let fade = ctx.animate_bool_with_time(Id::new("cha-toolbar-fade"), visible, FADE);
        if visible || fade > 0.0 {
            self.draw_bar(ctx, v, &model, &look, screen, fade, visible, &mut actions);
        } else {
            self.clear_bar();
        }
        self.shown = visible;
        if visible {
            self.thin = None;
        } else {
            self.draw_thin(ctx, &look, screen, &model.folded_bar.tooltip);
        }
        if self.power_off.is_some() {
            self.draw_power_off(ctx, &model, &look, screen, now, &mut actions);
        }
        actions
    }

    fn clear_bar(&mut self) {
        (self.bar, self.tab) = (None, None);
        (self.menu_rect, self.menu_button) = (None, None);
    }

    fn draw_thin(&mut self, ctx: &Context, look: &Look, screen: Rect, tooltip: &str) {
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
                clicked = resp.on_hover_text(tooltip).clicked();
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
        model: &Model,
        look: &Look,
        screen: Rect,
        fade: f32,
        interactive: bool,
        actions: &mut Vec<ToolbarAction>,
    ) {
        let layout = Layout::new(ctx, look, model, screen);
        let bar = layout.bar;
        self.height = bar.height();
        let mut menu_click = None;
        let mut hide_click = false;
        let mut power_click = false;
        let mut open: Option<(&ToolbarMenu, Rect)> = None;
        let mut open_rect = None;
        #[cfg(test)]
        let mut unhandled = Vec::new();

        Area::new(Id::new("cha-toolbar"))
            .order(Order::Foreground)
            .fixed_pos(bar.min)
            .show(ctx, |ui| {
                let (rect, _) = ui.allocate_exact_size(bar.size(), Sense::hover());
                let mut painter = ui.painter().clone();
                painter.set_opacity(fade);
                painter.add(chrome::shadow(rect, RADIUS, 70));
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
                for (c, r) in &layout.items {
                    let clicked = match (c.kind, c.id.as_str()) {
                        (ControlKind::Label, "name") => {
                            b.label(*r, c.label.as_deref().unwrap_or_default(), look.p.ink);
                            false
                        }
                        (ControlKind::Label, "viewers") => {
                            b.label_hover(
                                *r,
                                c.label.as_deref().unwrap_or_default(),
                                c.tooltip.as_deref().unwrap_or_default(),
                            );
                            false
                        }
                        (ControlKind::Button, "back" | "take-control") => b.text_button(*r, c),
                        (ControlKind::Toggle, "stats") => b.stats_button(*r, c, layout.letter_w),
                        (
                            ControlKind::Button | ControlKind::Toggle | ControlKind::Menu,
                            "power" | "capture" | "mouse" | "fullscreen" | "stream" | "sound"
                            | "controllers",
                        ) => b.control_button(*r, c),
                        _ => {
                            #[cfg(test)]
                            unhandled.push(c.id.clone());
                            false
                        }
                    };
                    if let Some(menu) = &c.menu {
                        // The open menu hangs from its button.
                        open = Some((menu, *r));
                    }
                    if !clicked {
                        continue;
                    }
                    match c.id.as_str() {
                        "back" => b.actions.push(ToolbarAction::Back),
                        "power" => power_click = true,
                        "take-control" => b.actions.push(ToolbarAction::TakeControl),
                        "capture" => b.actions.push(ToolbarAction::Capture),
                        "mouse" => b.actions.push(ToolbarAction::SetMouse(!v.mouse)),
                        "fullscreen" => b.actions.push(ToolbarAction::ToggleFullScreen),
                        "stats" => b.actions.push(ToolbarAction::ToggleStats),
                        id => menu_click = Menu::from_id(id),
                    }
                }
                let actions = b.actions;
                // The tab that hangs from the bar's lower edge, in the middle.
                let tab =
                    Rect::from_center_size(pos2(rect.center().x, rect.bottom()), vec2(36.0, 20.0));
                let resp = ui.interact(tab, ui.id().with("tab"), Sense::click());
                let live = interactive && !model.hide_tab.disabled;
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
                if resp.on_hover_text(&model.hide_tab.tooltip).clicked() && live {
                    hide_click = true;
                }
                self.tab = Some(tab);

                if let (true, Some((menu, btn))) = (interactive, open) {
                    let at = menu_origin(menu.align, btn, screen);
                    open_rect = Some(draw_menu(ui, look, v, menu, at, actions));
                }
            });
        #[cfg(test)]
        self.unhandled.extend(unhandled);
        self.bar = Some(bar);
        self.menu_rect = open_rect;
        self.menu_button = open.map(|(_, r)| r);

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
        model: &Model,
        look: &Look,
        screen: Rect,
        now: Instant,
        actions: &mut Vec<ToolbarAction>,
    ) {
        let (Some(power), Some(count)) = (self.power_off, model.countdown.as_ref()) else {
            return;
        };
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
        Area::new(card)
            .order(Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(p.panel)
                    .stroke(Stroke::new(1.0, p.line))
                    .corner_radius(RADIUS)
                    .inner_margin(20)
                    .show(ui, |ui| {
                        ui.set_width(344.0);
                        ui.spacing_mut().item_spacing = vec2(0.0, 12.0);
                        ui.vertical_centered(|ui| {
                            ui.label(
                                RichText::new(&count.title)
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
                                RichText::new(&count.note)
                                    .font(FontId::proportional(13.0))
                                    .color(look.ink2),
                            );
                            let w = Buttons::wide_width(ui, &count.cancel_label);
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
                                &count.cancel_label,
                                true,
                                count.cancel_tooltip.as_deref().unwrap_or_default(),
                            );
                        });
                    });
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

struct Layout<'m> {
    bar: Rect,
    items: Vec<(&'m ToolbarControl, Rect)>,
    /// The grade letter's width, for the stats button.
    letter_w: f32,
}

fn galley(painter: &Painter, text: &str, size: f32, color: Color32) -> Arc<Galley> {
    painter.layout_no_wrap(text.to_string(), FontId::proportional(size), color)
}

impl<'m> Layout<'m> {
    fn new(ctx: &Context, look: &Look, model: &'m Model, screen: Rect) -> Self {
        let painter = ctx.layer_painter(LayerId::new(
            Order::Foreground,
            Id::new("cha-toolbar-measure"),
        ));
        let text_w = |s: &str| galley(&painter, s, TEXT, look.p.ink).size().x;
        let stats = model.control("stats");
        let letter_w = stats.and_then(|c| c.badge.as_ref()).map_or(0.0, |b| {
            painter
                .layout_no_wrap(b.text.clone(), FontId::monospace(TEXT), look.p.ink)
                .size()
                .x
        });
        let width_of = |c: &ToolbarControl| -> f32 {
            let label = c.label.as_deref().unwrap_or_default();
            match (c.kind, c.id.as_str()) {
                (ControlKind::Label, "name") => {
                    galley(&painter, label, 13.0, look.p.ink)
                        .size()
                        .x
                        .min(192.0)
                        + 16.0
                }
                (ControlKind::Label, _) => text_w(label) + 16.0,
                (_, "stats") => (8.0
                    + 16.0
                    + 8.0
                    + if c.badge.is_some() {
                        GAP + letter_w
                    } else {
                        0.0
                    })
                .max(BTN),
                _ if c.icon.is_none() => text_w(label) + 24.0,
                _ => BTN,
            }
        };
        let build = |with_labels: bool| {
            let items: Vec<(&ToolbarControl, f32)> = model
                .controls
                .iter()
                .filter(|c| with_labels || !c.droppable)
                .map(|c| (c, width_of(c)))
                .collect();
            let width: f32 = items.iter().map(|(_, w)| w).sum::<f32>()
                + GAP * (items.len().saturating_sub(1)) as f32
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

/// Where a menu's top left goes: under its button, from its left edge or its right edge.
fn menu_origin(align: MenuAlign, button: Rect, screen: Rect) -> Pos2 {
    let x = match align {
        MenuAlign::Left => button.left(),
        MenuAlign::Right => button.right() - MENU_W,
    };
    let x = x.clamp(
        screen.left() + 8.0,
        (screen.right() - 8.0 - MENU_W).max(screen.left() + 8.0),
    );
    pos2(x, button.bottom() + 8.0)
}

// ---- buttons ---------------------------------------------------------------

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

    /// An icon button for a control: its icon, tone, lit state and tooltip from the model.
    fn control_button(&mut self, rect: Rect, c: &ToolbarControl) -> bool {
        let look = self.look;
        let style = IconStyle {
            color: look.toolbar_tone(c.tone),
            hover: look.toolbar_tone(c.hover_tone),
            active: c.active,
            enabled: !c.disabled,
            live: self.live,
            ..IconStyle::new(16.0, 6.0)
        };
        let icon = c
            .icon
            .as_deref()
            .and_then(Icon::from_id)
            .unwrap_or(Icon::Stats);
        let resp = chrome::icon_button(
            self.ui,
            self.painter,
            look,
            rect,
            self.id(&c.id),
            icon,
            style,
        );
        if let Some(badge) = &c.badge {
            let g = self.painter.layout_no_wrap(
                badge.text.clone(),
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
        let resp = resp.on_hover_text(c.tooltip.as_deref().unwrap_or_default());
        style.fires(&resp)
    }

    /// A ghost button with text, like "← Back".
    fn text_button(&mut self, rect: Rect, c: &ToolbarControl) -> bool {
        let resp = self.ui.interact(rect, self.id(&c.id), Sense::click());
        let hot = self.live && resp.hovered();
        if hot {
            chrome::lit(self.painter, self.look, rect, 6.0);
        }
        let ink = if hot {
            self.look.p.ink
        } else {
            self.look.toolbar_tone(c.tone).unwrap_or(self.look.p.ink)
        };
        let g = galley(
            self.painter,
            c.label.as_deref().unwrap_or_default(),
            TEXT,
            ink,
        );
        self.painter.galley(rect.center() - g.size() / 2.0, g, ink);
        resp.on_hover_text(c.tooltip.as_deref().unwrap_or_default())
            .clicked()
            && self.live
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
    fn stats_button(&mut self, rect: Rect, c: &ToolbarControl, letter_w: f32) -> bool {
        let look = self.look;
        let resp = self.ui.interact(rect, self.id("stats"), Sense::click());
        let hot = self.live && resp.hovered();
        let on = c.pressed == Some(true);
        if hot || on {
            chrome::lit(self.painter, look, rect, 6.0);
        }
        let ink = if on {
            look.p.accent
        } else if hot {
            look.p.ink
        } else {
            look.ink2
        };
        let icon_at = pos2(
            if c.badge.is_some() {
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
        let mut tip = c.tooltip.clone().unwrap_or_default();
        if let Some(badge) = &c.badge {
            let color = look.toolbar_tone(badge.tone).unwrap_or(look.p.ink);
            let g = self
                .painter
                .layout_no_wrap(badge.text.clone(), FontId::monospace(TEXT), color);
            let at = pos2(
                rect.right() - 8.0 - letter_w,
                rect.center().y - g.size().y / 2.0,
            );
            self.painter.galley(at, g, color);
            if let Some(t) = &badge.tooltip {
                tip.push('\n');
                tip.push_str(t);
            }
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
    menu: &ToolbarMenu,
    at: Pos2,
    actions: &mut Vec<ToolbarAction>,
) -> Rect {
    chrome::menu_box(
        ui,
        look,
        ("tb-menu", menu.id.clone()),
        MenuFrame {
            at,
            width: MENU_W,
            pad: MENU_PAD,
            radius: RADIUS,
        },
        |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.spacing_mut().interact_size.y = 16.0;
            ui.spacing_mut().slider_rail_height = 3.0;
            ui.spacing_mut().slider_width = 112.0;
            ui.style_mut().override_font_id = Some(FontId::proportional(TEXT));
            for row in &menu.rows {
                draw_row(ui, look, v, row, actions);
            }
        },
    )
}

fn dim(look: &Look, text: &str) -> RichText {
    RichText::new(text).color(look.ink2)
}

/// A paragraph of small print in a menu.
fn note(ui: &mut Ui, text: &str, color: Color32) {
    ui.add(egui::Label::new(RichText::new(text).color(color).size(11.0)).wrap());
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
fn segmented(
    ui: &mut Ui,
    look: &Look,
    name: &str,
    options: &[(&str, &str)],
    selected: Option<&str>,
    enabled: bool,
    tip: Option<&str>,
) -> Option<String> {
    let gap = 4.0;
    let w = (ui.available_width() - gap * (options.len() as f32 - 1.0)) / options.len() as f32;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let mut picked = None;
    for (i, (label, value)) in options.iter().enumerate() {
        let rect = Rect::from_min_size(
            pos2(row.left() + i as f32 * (w + gap), row.top()),
            vec2(w, 24.0),
        );
        let resp = chrome::chip(
            ui,
            look,
            rect,
            ui.id().with((name, i)),
            label,
            selected == Some(*value),
            enabled,
        );
        let resp = match tip {
            Some(t) => resp.on_hover_text(t),
            None => resp,
        };
        if resp.clicked() && enabled {
            picked = Some((*value).to_string());
        }
    }
    picked
}

/// One row of an open menu, drawn by what kind of row the spec says it is; what a button or a
/// choice does is by its id.
fn draw_row(
    ui: &mut Ui,
    look: &Look,
    v: &ToolbarView,
    row: &ToolbarRow,
    actions: &mut Vec<ToolbarAction>,
) {
    let tone = look.toolbar_tone(row.tone).unwrap_or(look.p.ink);
    let text = row.text.as_deref().unwrap_or_default();
    match row.kind {
        RowKind::Note => note(ui, text, tone),
        RowKind::Choice => {
            ui.label(dim(look, row.label.as_deref().unwrap_or_default()));
            let options = row.options.as_deref().unwrap_or_default();
            if row.editable {
                let chips: Vec<(&str, &str)> = options
                    .iter()
                    .map(|o| (o.label.as_str(), o.value.as_str()))
                    .collect();
                let picked = segmented(
                    ui,
                    look,
                    &row.id,
                    &chips,
                    row.value.as_deref(),
                    !row.disabled,
                    row.tooltip.as_deref(),
                );
                match (row.id.as_str(), picked) {
                    ("fps", Some(rate)) => {
                        if let Ok(rate) = rate.parse() {
                            actions.push(ToolbarAction::SetFps(rate));
                        }
                    }
                    ("overlay", Some(level)) => {
                        if let Ok(level) = level.parse() {
                            actions.push(ToolbarAction::SetOverlay(level));
                        }
                    }
                    _ => {}
                }
            } else {
                read_only(ui, look, row.read_only.as_deref().unwrap_or("–"));
            }
        }
        RowKind::Button => {
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
            let label = row.label.as_deref().unwrap_or_default();
            let tip = row.tooltip.as_deref().unwrap_or_default();
            if b.wide_button(rect, &row.id, label, !row.disabled, tip) {
                match row.id.as_str() {
                    "sound-toggle" => b.actions.push(ToolbarAction::SetMuted(!v.muted)),
                    "restart" => b.actions.push(ToolbarAction::RestartSound),
                    "input-settings" => b.actions.push(ToolbarAction::OpenInputSettings),
                    _ => {}
                }
            }
        }
        RowKind::Slider => {
            let Some(slider) = &row.slider else { return };
            let shown = slider.value as u32;
            let mut volume = shown;
            ui.horizontal(|ui| {
                ui.label(dim(look, row.label.as_deref().unwrap_or_default()));
                ui.add(
                    egui::Slider::new(&mut volume, slider.min as u32..=slider.max as u32)
                        .step_by(slider.step)
                        .trailing_fill(true)
                        .show_value(false),
                );
                ui.add_sized(vec2(30.0, 16.0), egui::Label::new(&slider.display));
            });
            if volume != shown {
                actions.push(ToolbarAction::SetVolume(volume as u8));
            }
        }
        RowKind::List => {
            for item in row.items.as_deref().unwrap_or_default() {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
                let slot = item.detail.as_deref().unwrap_or_default();
                let sg = galley(ui.painter(), slot, TEXT, look.p.ink_3);
                let sx = rect.right() - sg.size().x;
                ui.painter().galley(
                    pos2(sx, rect.center().y - sg.size().y / 2.0),
                    sg,
                    look.p.ink_3,
                );
                let mut job = egui::text::LayoutJob::single_section(
                    item.text.clone(),
                    egui::TextFormat::simple(FontId::proportional(TEXT), look.p.ink),
                );
                job.wrap.max_width = sx - rect.left() - 8.0;
                job.wrap.max_rows = 1;
                job.wrap.break_anywhere = true;
                job.wrap.overflow_character = Some('…');
                let g = ui.painter().layout_job(job);
                ui.painter().galley(
                    pos2(rect.left(), rect.center().y - g.size().y / 2.0),
                    g,
                    look.p.ink,
                );
            }
        }
        // The browser's link to its controllers page.
        RowKind::Link => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::event::{DeviceId, ElementState, MouseButton};

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
    fn the_timing_is_the_specs() {
        assert_eq!(collapse_after(), Duration::from_millis(1200));
        assert_eq!(near_top(), 72.0);
        assert_eq!(PowerOff::total(), Duration::from_secs(5));
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
    fn capabilities_follow_what_the_link_reports() {
        // Moonlight says nothing: only the local sound restart.
        assert_eq!(capabilities(None), ["sound-restart"]);
        // The portal's streamer: a floor, viewers and an overlay.
        let link = TransportStats {
            control: Some(true),
            viewers: Some(2),
            overlay: Some(PerfOverlay::Preset(1)),
            ..Default::default()
        };
        let caps = capabilities(Some(&link));
        for cap in [
            "viewers",
            "control-handoff",
            "fps-change",
            "steam-overlay",
            "sound-restart",
        ] {
            assert!(caps.iter().any(|c| c == cap), "{cap}");
        }
        // Whatever it reports is a capability the native side can ever report.
        let reports = spec().reports.for_platform(Platform::Native);
        assert!(caps.iter().all(|c| reports.contains(c)));
        // A stream with no overlay and no floor has neither.
        let bare = TransportStats::default();
        assert_eq!(capabilities(Some(&bare)), ["sound-restart"]);
    }

    /// A link with everything on, for the draw test.
    fn full_link() -> TransportStats {
        TransportStats {
            tag: "WT",
            target_fps: Some(120),
            control: Some(true),
            viewers: Some(2),
            can_take: false,
            overlay: Some(PerfOverlay::Preset(2)),
            ..Default::default()
        }
    }

    /// Draws the toolbar headless, as the player does, with each menu and kind of session the spec
    /// has rows for, and checks that nothing the spec shows for the native player went undrawn:
    /// every control and row the shared native cases expect is among what came up.
    #[test]
    fn every_control_and_row_the_spec_has_for_native_is_drawn() {
        use crate::health::{Assessment, Grade};
        use cha_ui_spec::toolbar_cases::toolbar_cases;
        let stats = StatsSnapshot::default();
        let health = Assessment {
            grade: Some(Grade::B),
            ..Assessment::default()
        };
        let pad = [PadInfo {
            slot: 0,
            name: "Pad".into(),
        }];
        let full = full_link();
        let viewing = TransportStats {
            control: Some(false),
            ..full_link()
        };
        let custom = TransportStats {
            overlay: Some(PerfOverlay::Custom),
            ..full_link()
        };
        let ctx = Context::default();
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        let sessions: [(&str, Option<&TransportStats>, bool, &[PadInfo]); 9] = [
            ("", Some(&full), false, &pad),
            ("stream", Some(&full), false, &pad),
            ("sound", Some(&full), false, &pad),
            ("controllers", Some(&full), false, &pad),
            ("controllers", Some(&full), true, &[]),
            ("stream", Some(&viewing), false, &[]),
            ("stream", Some(&custom), false, &[]),
            ("stream", None, false, &[]),
            ("sound", None, false, &[]),
        ];
        for (menu, link, denied, pads) in sessions {
            let mut t = Toolbar::new();
            t.open_menu_for_test(menu);
            let v = ToolbarView {
                title: "Chrome",
                transport: "Cha Portal",
                stats: &stats,
                health: &health,
                link,
                connected: true,
                locked: false,
                mouse: true,
                muted: false,
                volume: 60,
                fullscreen: false,
                stats_open: true,
                opacity: 90,
                pads,
                input_access: if denied {
                    InputAccess::Denied
                } else {
                    InputAccess::Granted
                },
            };
            for c in &t.model(&v, None).controls {
                seen.insert(c.id.clone());
                seen.extend(
                    c.menu
                        .iter()
                        .flat_map(|m| m.rows.iter().map(|r| r.id.clone())),
                );
            }
            for _ in 0..3 {
                let input = egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 720.0))),
                    ..Default::default()
                };
                let _ = ctx.run_ui(input, |ui| {
                    t.show_at(ui.ctx(), &v, Instant::now());
                });
            }
            assert!(t.unhandled.is_empty(), "not drawn: {:?}", t.unhandled);
        }
        for case in toolbar_cases()
            .iter()
            .filter(|c| c.runs_on(Platform::Native))
        {
            let want = case.expected(Platform::Native);
            let ids = want["controls"].as_array().into_iter().flatten();
            let rows = want["menu"]["rows"].as_array().into_iter().flatten();
            for id in ids.chain(rows).filter_map(|v| v.as_str()) {
                assert!(
                    seen.contains(id),
                    "case {:?}: {id} was never drawn",
                    case.name
                );
            }
        }
    }

    #[test]
    fn the_state_comes_from_the_view() {
        use crate::health::Assessment;
        let stats = StatsSnapshot {
            codec: "hevc".into(),
            ..StatsSnapshot::default()
        };
        let health = Assessment::default();
        let link = TransportStats {
            tag: "WT",
            target_fps: Some(90),
            control: Some(false),
            viewers: Some(3),
            overlay: Some(PerfOverlay::Custom),
            ..Default::default()
        };
        let v = ToolbarView {
            title: "Desktop",
            transport: "Cha Portal",
            stats: &stats,
            health: &health,
            link: Some(&link),
            connected: true,
            locked: false,
            mouse: false,
            muted: true,
            volume: 40,
            fullscreen: true,
            stats_open: false,
            opacity: 90,
            pads: &[],
            input_access: InputAccess::Granted,
        };
        let model = Toolbar::new().model(&v, None);
        assert_eq!(
            model.control("viewers").unwrap().label.as_deref(),
            Some("3 watching")
        );
        assert!(
            model.control("take-control").is_some(),
            "another session has the controls"
        );
        assert_eq!(
            model.control("mouse").unwrap().icon.as_deref(),
            Some("mouse-off")
        );
        assert_eq!(
            model.control("sound").unwrap().icon.as_deref(),
            Some("sound-muted")
        );
        assert_eq!(
            model.control("fullscreen").unwrap().icon.as_deref(),
            Some("fullscreen-exit")
        );
        assert_eq!(model.control("stats").unwrap().pressed, Some(false));
        assert!(
            model.control("stats").unwrap().badge.is_none(),
            "no grade yet"
        );
    }
}

//! The stats panel over the picture: the native twin of the portal's
//! `StatsOverlay.vue`. A header (grade, compact/full, collapse, settings,
//! hide) over either one compact line or the full sections, in a corner you
//! can drag it out of. Ctrl+Alt+Shift+S shows and hides it.
//!
//! [`StatsPanel`] owns the preferences and what is being dragged; the app
//! feeds it a [`StatsSnapshot`] and the health [`Assessment`] each frame and
//! asks it which pointer events are its own ([`StatsPanel::consumes`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use cha_client::NodeStats;
use egui::epaint::text::{LayoutJob, TextFormat};
use egui::{
    Align, Align2, Area, Color32, Context, FontId, Frame, Galley, Id, Order, Painter, Pos2, Rect,
    Sense, Stroke, Ui, Vec2, pos2, vec2,
};
use winit::event::{ElementState, WindowEvent};

use crate::health::{Assessment, Grade, Issue, Severity};
use crate::overlay_prefs::{Corner, OPACITY_MIN, OverlayPrefs, Pos, Section};
use crate::theme::icons::{self, Icon};
use crate::theme::{OverlayStyle, Palette, ThemeExt};

/// One reading of the running stream, for the overlay and the health grade.
/// A reading a transport can't give is `None` (or, for the counters that only
/// some transports keep, `None` instead of 0); the panel hides what is `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StatsSnapshot {
    pub width: u32,
    pub height: u32,
    /// The codec as the core names it ("Hevc", "PyroWave444").
    pub codec: String,
    /// How the stream travels ("WT"), if the transport says.
    pub transport_tag: &'static str,
    /// Frames put on screen per second.
    pub present_fps: f32,
    pub decode_fps: f32,
    /// The frame rate the host encodes at (or was asked for).
    pub target_fps: Option<u32>,
    /// Frames per second the host sent over its last reports, and the same
    /// span's frames shown here.
    pub sent_fps: Option<f32>,
    pub shown_sent_fps: Option<f32>,
    /// The host's composited to encoded p99, ms.
    pub encode_p99_ms: Option<f32>,
    /// Received video, Mbit/s.
    pub mbps: Option<f32>,
    pub decode_ms: Option<f32>,
    /// Received to shown, ms.
    pub latency_ms: Option<f32>,
    /// The longest wait between two frames, over the last second, ms.
    pub frame_gap_ms: Option<f32>,
    pub rtt_ms: Option<f32>,
    /// Frames given up on and rebuilt from parity, since the session began.
    pub lost: Option<u64>,
    pub recovered: Option<u64>,
    /// Decoded frames replaced by a newer one before being shown.
    pub dropped: u64,
    pub decode_errors: u64,
    /// PyroWave frames decoded from only some of their packets.
    pub partial: u64,
    pub audio_buffer_ms: Option<f32>,
    pub audio_underruns: u64,
    pub audio_dropped_ms: u64,
    /// Times the stream came back after dropping.
    pub reconnects: u32,
    pub node: Option<NodeStats>,
    /// Periodic arrival gaps typical of AWDL were seen over the last seconds.
    pub awdl_suspected: bool,
}

impl StatsSnapshot {
    pub fn is_pyrowave(&self) -> bool {
        self.codec.starts_with("PyroWave")
    }

    /// "HEVC/WT": the codec and a short transport name, whichever are known.
    pub fn codec_text(&self) -> String {
        let name = self.codec.to_uppercase();
        [name.as_str(), self.transport_tag]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join("/")
    }

    fn reconnect_text(&self) -> String {
        match self.reconnects {
            0 => String::new(),
            1 => "1 reconnect".into(),
            n => format!("{n} reconnects"),
        }
    }

    /// "60 fps · 11.6 ms · 58.2 Mbit/s · HEVC/WT · 1 reconnect".
    pub fn compact_line(&self) -> String {
        [
            format!("{} fps", num(Some(self.present_fps), 0)),
            format!("{} ms", num(self.latency_ms, 1)),
            format!("{} Mbit/s", num(self.mbps, 1)),
            self.codec_text(),
            self.reconnect_text(),
        ]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
    }
}

/// A number with fixed digits, or an en dash when there is none.
pub fn num(v: Option<f32>, digits: usize) -> String {
    match v {
        Some(v) if v.is_finite() => format!("{v:.digits$}"),
        _ => "–".into(),
    }
}

fn gb(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1024f64.powi(3))
}

fn pct(used: f64, total: f64) -> f64 {
    if total > 0.0 {
        used / total * 100.0
    } else {
        0.0
    }
}

/// Amber for a node value at or over `limit` percent (°C for the temperature).
fn hot(v: f64, limit: f64) -> bool {
    v >= limit
}

/// "Cha Player 0.1.0, macOS 15.5", for a copied report.
fn player_line() -> String {
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
/// then the numbers around them (`issueReport` in `StatsOverlay.vue`).
pub fn issue_report(
    issues: &[Issue],
    stats: &StatsSnapshot,
    health: &Assessment,
    player: &str,
) -> String {
    let ms = |v: Option<f32>, d: usize| format!("{} ms", num(v, d));
    let mut lines: Vec<String> = Vec::new();
    for i in issues {
        lines.push(format!("{}: {}", i.title, i.detail));
        lines.push(i.hint.to_string());
        lines.push(String::new());
    }
    let grade = health.grade.map_or("not measured", Grade::letter);
    let score = health
        .score
        .map_or(String::new(), |s| format!(" ({s}/100)"));
    lines.push(format!("Health: {grade}{score}, {}", health.summary));
    let codec = stats.codec_text();
    let target = stats
        .target_fps
        .map_or(String::new(), |t| format!(" of {t}"));
    lines.push(format!(
        "Stream: {}, {}×{}, {}{target} fps, {} Mbit/s",
        if codec.is_empty() { "–" } else { &codec },
        stats.width,
        stats.height,
        num(Some(stats.present_fps), 0),
        num(stats.mbps, 1),
    ));
    lines.push(format!(
        "Latency: received → shown {}, decode {}, audio buffer {}",
        ms(stats.latency_ms, 1),
        ms(stats.decode_ms, 2),
        ms(stats.audio_buffer_ms, 0),
    ));
    lines.push(format!(
        "Network: round trip {}, {} lost, {} dropped",
        ms(stats.rtt_ms, 1),
        stats.lost.unwrap_or(0),
        stats.dropped,
    ));
    lines.push(format!(
        "Playback: {} decode errors, {} partial frames, {} audio underruns, {} ms audio dropped",
        stats.decode_errors, stats.partial, stats.audio_underruns, stats.audio_dropped_ms
    ));
    if let Some(n) = &stats.node {
        let f = |v: f32| f64::from(v);
        let mut parts = vec![
            format!("CPU {}%", num(Some(n.cpu), 0)),
            format!("RAM {}/{} GB", gb(n.mem_used), gb(n.mem_total)),
        ];
        if let Some(gpu) = n.gpu {
            parts.push(format!("GPU {}%", num(Some(gpu), 0)));
        }
        if let (Some(used), Some(total)) = (n.vram_used, n.vram_total) {
            parts.push(format!("VRAM {}/{} GB", gb(used), gb(total)));
        }
        if let Some(t) = n.temp {
            parts.push(format!("{} °C", f(t)));
        }
        if let Some(p) = n.power {
            let limit = n
                .power_limit
                .map_or(String::new(), |l| format!("/{}", num(Some(l), 0)));
            parts.push(format!("{}{limit} W", num(Some(p), 0)));
        }
        lines.push(format!("Node: {}", parts.join(", ")));
    }
    lines.push(format!("Player: {player}"));
    lines.join("\n")
}

// ---- the panel -----------------------------------------------------------

const INSET: f32 = 12.0;
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Copied {
    One(&'static str),
    All,
}

struct Drag {
    /// Pointer minus the panel's top left, when the drag began.
    grab: Vec2,
    at: Pos,
}

/// The panel's state: preferences, and what the user is doing to it.
pub struct StatsPanel {
    pub prefs: OverlayPrefs,
    saved: OverlayPrefs,
    changed: Option<Instant>,
    /// Where the panel (or the chip) was drawn last, in points.
    rect: Option<Rect>,
    dragging: Option<Drag>,
    /// A press began on the panel and its release hasn't come yet.
    pressed: bool,
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

impl StatsPanel {
    pub fn new(prefs: OverlayPrefs) -> Self {
        Self {
            saved: prefs.clone(),
            prefs,
            changed: None,
            rect: None,
            dragging: None,
            pressed: false,
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
        if !(force || settled && self.dragging.is_none() && !self.pressed) {
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
        match event {
            WindowEvent::MouseInput { state, .. } => match state {
                ElementState::Pressed => {
                    self.pressed |= over;
                    over
                }
                ElementState::Released => std::mem::take(&mut self.pressed) || over,
            },
            WindowEvent::MouseWheel { .. } => over,
            _ => self.pressed || self.dragging.is_some() || over,
        }
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
        let mut cmds = Vec::new();

        let size = self.rect.map_or(vec2(style.width, HEADER_H), |r| r.size());
        let corner = self.prefs.corner;
        let top_y = if top_inset > 0.0 {
            top_inset + super::toolbar::TOP + 10.0
        } else {
            INSET
        };
        // Where the top left goes, from the size it had last frame (the
        // first frame is invisible while egui measures it).
        let place = |left: bool, top: bool| {
            pos2(
                if left {
                    INSET
                } else {
                    screen.right() - INSET - size.x
                },
                if top {
                    top_y
                } else {
                    screen.bottom() - INSET - size.y
                },
            )
        };
        let pos = match (&self.dragging, self.prefs.snap, self.prefs.pos) {
            (Some(d), _, _) => pos2(d.at.left, d.at.top),
            (None, false, Some(p)) => {
                let p = p.clamped(size.x, size.y, screen.width(), screen.height());
                pos2(p.left, p.top)
            }
            (None, _, _) => match corner {
                Corner::TopLeft => place(true, true),
                Corner::TopRight => place(false, true),
                Corner::BottomLeft => place(true, false),
                Corner::BottomRight => place(false, false),
            },
        };
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
                stats,
                health,
                copied: self.copied.map(|c| c.0),
                menu: self.menu_until.is_some(),
                max_body: (screen.height() - INSET - top_y - HEADER_H - 2.0).max(80.0),
            };
            area.show(ctx, |ui| view.panel(ui, &mut cmds)).response
        } else {
            area.show(ctx, |ui| chip(ui, &look, health, &mut cmds))
                .response
        };
        let rect = response.rect;
        if self.rect.is_some_and(|r| r.size() != rect.size()) {
            ctx.request_repaint();
        }
        self.rect = Some(rect);

        for cmd in cmds {
            self.apply(ctx, cmd, stats, health, screen, rect, now);
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
        stats: &StatsSnapshot,
        health: &Assessment,
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
                let issues: Vec<Issue> = match what {
                    Copied::All => health.issues.clone(),
                    Copied::One(id) => health
                        .issues
                        .iter()
                        .filter(|i| i.id == id)
                        .cloned()
                        .collect(),
                };
                ctx.copy_text(issue_report(&issues, stats, health, &player_line()));
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
                    self.dragging = Some(Drag {
                        grab: p - rect.min,
                        at: Pos {
                            left: rect.min.x,
                            top: rect.min.y,
                        },
                    });
                }
            }
            Cmd::DragMove => {
                if let (Some(p), Some(d)) = (Self::pointer(ctx), &mut self.dragging) {
                    d.at = Pos {
                        left: p.x - d.grab.x,
                        top: p.y - d.grab.y,
                    }
                    .clamped(
                        rect.width(),
                        rect.height(),
                        screen.width(),
                        screen.height(),
                    );
                }
            }
            Cmd::DragEnd => {
                if let Some(d) = self.dragging.take() {
                    let at =
                        d.at.clamped(rect.width(), rect.height(), screen.width(), screen.height());
                    if self.prefs.snap {
                        self.prefs.corner = Corner::nearest(
                            at.left + rect.width() / 2.0,
                            at.top + rect.height() / 2.0,
                            screen.width(),
                            screen.height(),
                        );
                    } else {
                        self.prefs.pos = Some(Pos {
                            left: at.left.round(),
                            top: at.top.round(),
                        });
                    }
                }
            }
        }
        self.touched();
    }
}

/// Colours and text drawing for one frame: the palette, with the dimmer text
/// moved toward full ink and given a dark halo as the background gets more
/// see-through.
pub(super) struct Look {
    pub(super) p: Arc<Palette>,
    pub(super) fill: Color32,
    pub(super) ink2: Color32,
    /// 0 at full opacity, 1 at the lowest.
    halo: f32,
    pub(super) font: FontId,
}

impl Look {
    pub(super) fn new(style: &OverlayStyle, opacity: u8) -> Self {
        let p = style.palette.clone();
        let t = ((100.0 - f32::from(opacity)) / (100.0 - f32::from(OPACITY_MIN))).clamp(0.0, 1.0);
        Self {
            ink2: p.ink_2.lerp_to_gamma(p.ink, t),
            fill: style.fill(opacity),
            halo: t,
            font: FontId::monospace(style.font_size),
            p,
        }
    }

    fn galley(&self, painter: &Painter, text: &str, color: Color32) -> Arc<Galley> {
        painter.layout_no_wrap(text.to_string(), self.font.clone(), color)
    }

    fn paint(&self, painter: &Painter, at: Pos2, galley: Arc<Galley>) {
        if self.halo > 0.02 {
            let halo = Color32::from_black_alpha(((0.3 + 0.5 * self.halo) * 255.0) as u8);
            for d in [
                vec2(-1.0, 0.0),
                vec2(1.0, 0.0),
                vec2(0.0, -1.0),
                vec2(0.0, 1.0),
            ] {
                painter.galley_with_override_text_color(at + d * 0.8, galley.clone(), halo);
            }
        }
        painter.galley(at, galley, self.p.ink);
    }

    /// Draw `text` with its left (or right) edge at `x`, centred on `y`.
    fn text(
        &self,
        painter: &Painter,
        x: f32,
        y: f32,
        align: Align,
        text: &str,
        color: Color32,
    ) -> Rect {
        let galley = self.galley(painter, text, color);
        let size = galley.size();
        let left = if align == Align::Max { x - size.x } else { x };
        let at = pos2(left, y - size.y / 2.0);
        let rect = Rect::from_min_size(at, size);
        self.paint(painter, at, galley);
        rect
    }

    /// `text` on one line within `max_w`, ending in "…" if cut.
    fn fitted(&self, painter: &Painter, text: &str, color: Color32, max_w: f32) -> Arc<Galley> {
        let mut job = LayoutJob::single_section(
            text.to_string(),
            TextFormat::simple(self.font.clone(), color),
        );
        job.wrap.max_width = max_w.max(0.0);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        job.wrap.overflow_character = Some('…');
        painter.layout_job(job)
    }

    pub(super) fn grade_color(&self, grade: Option<Grade>) -> Color32 {
        match grade {
            Some(Grade::A | Grade::B) => self.p.ok,
            Some(Grade::C | Grade::D) => self.p.warn,
            Some(Grade::F) => self.p.danger,
            None => self.ink2,
        }
    }

    /// An issue's title: minor in the dim ink, major amber, critical red.
    fn title_color(&self, s: Severity) -> Color32 {
        match s {
            Severity::Minor => self.ink2,
            Severity::Major => self.p.warn,
            Severity::Critical => self.p.danger,
        }
    }

    /// A value whose signal health found bad: amber, or red when critical.
    fn bad_color(&self, s: Severity) -> Color32 {
        match s {
            Severity::Minor | Severity::Major => self.p.warn,
            Severity::Critical => self.p.danger,
        }
    }

    fn section_color(&self, s: Section) -> Color32 {
        match s {
            Section::Stream => self.p.accent,
            Section::Latency => self.p.chart_1,
            Section::Network => self.p.chart_2,
            Section::Node => self.p.chart_3,
        }
    }
}

/// A value on a row, with the colour health gives it (ink when fine).
type Seg = (String, Option<Color32>);

struct View<'a> {
    prefs: &'a OverlayPrefs,
    look: &'a Look,
    style: &'a OverlayStyle,
    stats: &'a StatsSnapshot,
    health: &'a Assessment,
    copied: Option<Copied>,
    menu: bool,
    /// The tallest the body may be before it scrolls.
    max_body: f32,
}

impl View<'_> {
    fn bad(&self, ids: &[&str]) -> Option<Color32> {
        self.health
            .bad(ids)
            .map(|i| self.look.bad_color(i.severity))
    }

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
        Frame::new()
            .fill(look.fill)
            .stroke(Stroke::new(1.0, p.line))
            .corner_radius(self.style.radius)
            .show(ui, |ui| {
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

    fn word(&self) -> &str {
        self.health.summary
    }

    /// The letter's width and the width of the text beside it, which is the
    /// compact line when collapsed and the summary word otherwise.
    fn header_text_widths(&self, ui: &Ui) -> (f32, f32) {
        let look = self.look;
        let painter = ui.painter();
        let grade = look
            .galley(painter, self.grade_letter(), look.p.ink)
            .size()
            .x;
        let text = if self.prefs.collapsed {
            self.stats.compact_line()
        } else {
            self.word().to_string()
        };
        (grade, look.galley(painter, &text, look.p.ink).size().x)
    }

    fn grade_letter(&self) -> &'static str {
        self.health.grade.map_or("–", Grade::letter)
    }

    fn compact_body_width(&self, ui: &Ui) -> f32 {
        let look = self.look;
        let painter = ui.painter();
        let line = format!("{} · {}", self.grade_letter(), self.stats.compact_line());
        let mut w = look.galley(painter, &line, look.p.ink).size().x;
        if let Some(issue) = self.health.issues.first() {
            let text = format!("{} · {}", issue.title, issue.detail);
            let issue_w = look.galley(painter, &text, look.p.ink).size().x + 4.0 + 16.0;
            w = w.max(issue_w.min(COMPACT_ISSUE_MAX));
        }
        w
    }

    fn header(&self, ui: &mut Ui, inner_w: f32, handle_content: Option<f32>, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let (rect, _) = ui.allocate_exact_size(vec2(inner_w, HEADER_H), Sense::hover());
        let painter = ui.painter().clone();
        let cy = rect.center().y;
        let handle_w = handle_content.unwrap_or(inner_w - BUTTONS_W);
        let handle = Rect::from_min_size(rect.min, vec2(handle_w, HEADER_H));
        let r = ui.interact(handle, ui.id().with("handle"), Sense::click_and_drag());
        if r.drag_started() {
            cmds.push(Cmd::DragStart);
        }
        if r.dragged() {
            cmds.push(Cmd::DragMove);
        }
        if r.drag_stopped() {
            cmds.push(Cmd::DragEnd);
        }
        let r = r.on_hover_text("Drag to move");
        let grade_color = look.grade_color(self.health.grade);
        let letter = look.text(
            &painter,
            handle.left() + PAD,
            cy,
            Align::Min,
            self.grade_letter(),
            grade_color,
        );
        let x = letter.right() + 6.0;
        let avail = handle.right() - PAD - x;
        if self.prefs.collapsed {
            let g = look.fitted(&painter, &self.stats.compact_line(), look.p.ink, avail);
            look.paint(&painter, pos2(x, cy - g.size().y / 2.0), g);
        } else {
            let g = look.fitted(&painter, self.word(), look.ink2, avail);
            look.paint(&painter, pos2(x, cy - g.size().y / 2.0), g);
        }
        drop(r);

        let mut bx = handle.right() + GAP;
        let mut button = |ui: &mut Ui, id: &str, tip: &str, icon: Icon, active: bool, cmd: Cmd| {
            let b = Rect::from_min_size(pos2(bx, cy - BTN / 2.0), vec2(BTN, BTN));
            bx += BTN + GAP;
            let resp = ui.interact(b, ui.id().with(id), Sense::click());
            let hot = resp.hovered() || active;
            if hot {
                painter.rect_filled(b, 4.0, look.p.line.gamma_multiply(0.6));
            }
            let color = if hot { look.p.ink } else { look.ink2 };
            icons::paint(
                &painter,
                Rect::from_center_size(b.center(), Vec2::splat(12.0)),
                icon,
                color,
            );
            if resp.on_hover_text(tip).clicked() {
                cmds.push(cmd);
            }
        };
        button(
            ui,
            "compact",
            if self.prefs.compact {
                "Full view"
            } else {
                "Compact view"
            },
            if self.prefs.compact {
                Icon::StatsFull
            } else {
                Icon::StatsCompact
            },
            false,
            Cmd::Compact,
        );
        button(
            ui,
            "collapse",
            if self.prefs.collapsed {
                "Expand"
            } else {
                "Collapse to the header"
            },
            if self.prefs.collapsed {
                Icon::PanelExpand
            } else {
                Icon::PanelCollapse
            },
            false,
            Cmd::Collapse,
        );
        button(
            ui,
            "menu",
            "Panel settings",
            Icon::SettingsDots,
            self.menu,
            Cmd::Menu,
        );
        button(
            ui,
            "hide",
            "Hide (click the grade chip or press Ctrl+Alt+Shift+S to bring it back)",
            Icon::EyeOff,
            false,
            Cmd::Hide,
        );
    }

    fn menu_strip(&self, ui: &mut Ui, inner_w: f32, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let p = &look.p;
        let top = ui.cursor().top();
        ui.painter()
            .hline(ui.max_rect().x_range(), top, Stroke::new(1.0, p.line));
        let strip = ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
            ui.spacing_mut().interact_size.y = 16.0;
            ui.spacing_mut().slider_rail_height = 3.0;
            ui.spacing_mut().slider_width = inner_w - 2.0 * PAD - 8.0 - 8.0 - 62.0 - 32.0;
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
            ui.style_mut().override_font_id = Some(look.font.clone());
            ui.add_space(6.0);
            let mut opacity = self.prefs.opacity;
            let mut snap = self.prefs.snap;
            ui.horizontal(|ui| {
                ui.add_space(PAD);
                let label = ui.label(egui::RichText::new("Opacity").color(look.ink2));
                label.on_hover_text(
                    "How see-through the panel's background is. The text stays solid.",
                );
                let slider = egui::Slider::new(&mut opacity, OPACITY_MIN..=100)
                    .step_by(5.0)
                    .trailing_fill(true)
                    .show_value(false);
                ui.add(slider);
                ui.label(format!("{opacity}%"));
            });
            ui.horizontal(|ui| {
                ui.add_space(PAD);
                ui.checkbox(&mut snap, "Snap to corners").on_hover_text(
                    "On: dropping the panel snaps it to the nearest corner. Off: it stays exactly where you drop it.",
                );
            });
            ui.add_space(6.0);
            (opacity, snap)
        });
        let (opacity, snap) = strip.inner;
        if opacity != self.prefs.opacity {
            cmds.push(Cmd::Opacity(opacity));
        }
        if snap != self.prefs.snap {
            cmds.push(Cmd::Snap(snap));
        }
        if ui.rect_contains_pointer(strip.response.rect)
            && ui.input(|i| i.pointer.is_moving() || i.pointer.any_down())
        {
            cmds.push(Cmd::KeepMenu);
        }
    }

    // ---- compact ----

    fn compact_body(&self, ui: &mut Ui, w: f32, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let painter = ui.painter().clone();
        let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
        let g = look.grade_color(self.health.grade);
        let letter = look.text(
            &painter,
            rect.left(),
            rect.center().y,
            Align::Min,
            self.grade_letter(),
            g,
        );
        look.text(
            &painter,
            letter.right(),
            rect.center().y,
            Align::Min,
            &format!(" · {}", self.stats.compact_line()),
            look.p.ink,
        );
        if let Some(issue) = self.health.issues.first() {
            let (rect, _) =
                ui.allocate_exact_size(vec2(w.min(COMPACT_ISSUE_MAX - 0.0), 20.0), Sense::hover());
            let text = format!("{} · {}", issue.title, issue.detail);
            let galley = look.fitted(&painter, &text, look.ink2, rect.width() - 20.0);
            let gw = galley.size().x;
            look.paint(
                &painter,
                pos2(rect.left(), rect.center().y - galley.size().y / 2.0),
                galley,
            );
            let tip = ui.interact(rect, ui.id().with("compact-issue"), Sense::hover());
            tip.on_hover_text(format!("{}: {}", issue.title, issue.detail));
            self.copy_button(
                ui,
                rect.left() + gw + 4.0,
                rect.center().y,
                Copied::One(issue.id),
                cmds,
            );
        }
    }

    /// A 16 px copy button whose left edge is at `x`, centred on `y`.
    fn copy_button(&self, ui: &mut Ui, x: f32, y: f32, what: Copied, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let b = Rect::from_min_size(pos2(x, y - 8.0), Vec2::splat(16.0));
        let resp = ui.interact(b, ui.id().with(("copy", what_key(what))), Sense::click());
        let done = self.copied == Some(what);
        if resp.hovered() {
            ui.painter()
                .rect_filled(b, 4.0, look.p.line.gamma_multiply(0.6));
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

    // ---- full ----

    fn full_body(&self, ui: &mut Ui, w: f32, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let painter = ui.painter().clone();
        let issues = &self.health.issues;
        if issues.len() > 1 {
            let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
            look.text(
                &painter,
                rect.left(),
                rect.center().y,
                Align::Min,
                &format!("{} warnings", issues.len()),
                look.ink2,
            );
            let all = self.copied == Some(Copied::All);
            let label = if all { "Copied" } else { "Copy all" };
            let galley = look.galley(&painter, label, look.ink2);
            let b = Rect::from_min_size(
                pos2(rect.right() - galley.size().x - 8.0, rect.top()),
                vec2(galley.size().x + 8.0, 20.0),
            );
            let resp = ui.interact(b, ui.id().with("copy-all"), Sense::click());
            let color = if resp.hovered() {
                painter.rect_filled(
                    b.shrink2(vec2(0.0, 2.0)),
                    4.0,
                    look.p.line.gamma_multiply(0.6),
                );
                look.p.ink
            } else {
                look.ink2
            };
            look.text(
                &painter,
                b.right() - 4.0,
                b.center().y,
                Align::Max,
                label,
                color,
            );
            if resp
                .on_hover_text("Copy every warning and the stream numbers")
                .clicked()
            {
                cmds.push(Cmd::Copy(Copied::All));
            }
        }
        if !issues.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("issues")
                .max_height(160.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing = Vec2::ZERO;
                    for issue in issues {
                        self.issue(ui, w - 6.0, issue, cmds);
                        ui.add_space(4.0);
                    }
                });
        }
        if let Some(score) = self.health.score {
            let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
            look.text(
                &painter,
                rect.left(),
                rect.center().y,
                Align::Min,
                &format!("Health {score}/100"),
                look.ink2,
            );
        }

        let s = self.stats;
        let codec = s.codec_text();
        let fps_target = s.target_fps.map_or(String::new(), |t| format!(" of {t}"));
        let stutter = self.bad(&["stutter", "freeze"]);

        // Stream
        let summary = [
            codec.clone(),
            format!("{} fps", num(Some(s.present_fps), 0)),
            format!("{} Mbit/s", num(s.mbps, 1)),
            s.reconnect_text(),
        ]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
        if self.section(ui, w, Section::Stream, "Stream", (summary, stutter), cmds) {
            self.row(ui, w, "Codec", "How the video is compressed on the node. The suffix is how it travels: WT is WebTransport.", &[(if codec.is_empty() { "–".into() } else { codec }, None)]);
            self.row(
                ui,
                w,
                "Size",
                "The resolution of the picture this player is decoding, in pixels.",
                &[(format!("{}×{}", s.width, s.height), None)],
            );
            self.row(ui, w, "Frame rate", "Frames per second shown here, against the rate the node encodes at. A still screen sends few frames, so a low number on an idle desktop is normal.", &[(format!("{}{fps_target} fps", num(Some(s.present_fps), 0)), stutter)]);
            self.row(ui, w, "Bitrate", "How much video data is arriving each second, in megabits. It rises with motion and falls on a still screen.", &[(format!("{} Mbit/s", num(s.mbps, 1)), None)]);
            if s.reconnects > 0 {
                self.row(
                    ui,
                    w,
                    "Reconnects",
                    "Times the stream dropped and came back by itself during this session.",
                    &[(s.reconnects.to_string(), None)],
                );
            }
        }

        // Latency
        let summary = format!(
            "{} ms · decode {} ms",
            num(s.latency_ms, 1),
            num(s.decode_ms, 2)
        );
        let latency_bad = self.bad(&["latency", "jitter"]);
        if self.section(
            ui,
            w,
            Section::Latency,
            "Latency",
            (summary, self.bad(&["latency", "decode", "jitter"])),
            cmds,
        ) {
            self.row(ui, w, "Received → shown", "Time from a frame arriving here to it appearing on your screen (the mean over the last second). It adds decoding and waiting for the display, but not the network or the time the app took to react to your input.", &[(format!("{} ms", num(s.latency_ms, 1)), latency_bad)]);
            self.row(ui, w, "Decode", "Average time this Mac takes to decode one frame (VideoToolbox, or the GPU for PyroWave). If this approaches the gap between frames, the picture will stutter.", &[(format!("{} ms", num(s.decode_ms, 2)), self.bad(&["decode"]))]);
            if s.audio_buffer_ms.is_some() {
                self.row(ui, w, "Audio buffer", "Sound waiting in the player's buffer, which it keeps near 30 ms. It evens out uneven arrival: more is steadier sound but later sound.", &[(format!("{} ms", num(s.audio_buffer_ms, 0)), None)]);
            }
            self.row(
                ui,
                w,
                "Audio underruns",
                "Times the sound ran dry and played silence until the buffer refilled.",
                &[(s.audio_underruns.to_string(), None)],
            );
            self.row(ui, w, "Audio dropped", "Sound thrown away because it arrived faster than it played, to keep it from falling behind the picture.", &[(format!("{} ms", s.audio_dropped_ms), None)]);
        }

        // Network
        let summary = [
            s.rtt_ms.map(|r| format!("{} ms", num(Some(r), 1))),
            s.lost.map(|l| format!("{l} lost")),
            Some(format!("{} dropped", s.dropped)),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        let summary_bad = self.bad(&["rtt", "loss", "skipped", "recovered", "dropped"]);
        if self.section(
            ui,
            w,
            Section::Network,
            "Network",
            (summary, summary_bad),
            cmds,
        ) {
            if let Some(rtt) = s.rtt_ms {
                self.row(ui, w, "Round trip", "How long a message takes to reach the node and come back. It is the network's base delay, with no processing included.", &[(format!("{} ms", num(Some(rtt), 1)), self.bad(&["rtt"]))]);
            }
            if let Some(lost) = s.lost {
                self.row(ui, w, "Lost", "Frames given up on because their packets never arrived (PyroWave: skipped). A few are harmless: lost video data is rebuilt from spare data or skipped.", &[(lost.to_string(), self.bad(&["loss", "skipped"]))]);
            }
            if let Some(recovered) = s.recovered {
                self.row(ui, w, "Recovered", "Frames that lost packets but were rebuilt from spare (parity) data. No harm, but the network is dropping packets.", &[(recovered.to_string(), self.bad(&["recovered"]))]);
            }
            self.row(ui, w, "Dropped", "Frames that arrived and were decoded but replaced by a newer one before they could be shown, usually because this Mac fell behind.", &[(s.dropped.to_string(), self.bad(&["dropped"]))]);
            if s.is_pyrowave() || s.partial > 0 {
                self.row(ui, w, "Partial frames", "PyroWave frames shown from only some of their packets: softer where blocks are missing, whole again on the next frame.", &[(s.partial.to_string(), self.bad(&["partial"]))]);
            }
            self.row(ui, w, "Decode errors", "Frames the decoder could not decode. The player asks the node for a new key frame after each.", &[(s.decode_errors.to_string(), None)]);
        }

        // Node
        if let Some(n) = &s.node {
            self.node(ui, w, n, cmds);
        }
    }

    fn node(&self, ui: &mut Ui, w: f32, n: &NodeStats, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let amber = Some(look.p.warn);
        let f = |v: f32| f64::from(v);
        let ram_hot = hot(pct(n.mem_used as f64, n.mem_total as f64), 90.0);
        let vram = match (n.vram_used, n.vram_total) {
            (Some(u), Some(t)) => Some((u, t)),
            _ => None,
        };
        let any_hot = hot(f(n.cpu), 90.0)
            || n.gpu.is_some_and(|g| hot(f(g), 90.0))
            || vram.is_some_and(|(u, t)| hot(pct(u as f64, t as f64), 90.0))
            || n.temp.is_some_and(|t| hot(f(t), 85.0));
        let summary = [
            Some(format!("CPU {}%", num(Some(n.cpu), 0))),
            n.gpu.map(|g| format!("GPU {}%", num(Some(g), 0))),
            vram.filter(|(_, t)| *t > 0)
                .map(|(u, t)| format!("VRAM {}/{} GB", gb(u), gb(t))),
            n.temp.map(|t| format!("{} °C", f(t))),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        if !self.section(
            ui,
            w,
            Section::Node,
            "Node",
            (summary, any_hot.then_some(look.p.warn)),
            cmds,
        ) {
            return;
        }
        let tint = |h: bool| if h { amber } else { None };
        self.row(
            ui,
            w,
            "CPU",
            "How busy the node's processor is overall, across all cores.",
            &[(
                format!("{}%", num(Some(n.cpu), 0)),
                tint(hot(f(n.cpu), 90.0)),
            )],
        );
        if n.cores > 0 {
            self.row(ui, w, "Load", "The node's average number of busy processes over the last minute, next to its core count. Steadily above the core count means it is overloaded.", &[(num(Some(n.load1), 1), None), (format!(" on {} cores", n.cores), Some(look.ink2))]);
        }
        self.row(
            ui,
            w,
            "RAM",
            "Memory in use on the node out of its total.",
            &[
                (gb(n.mem_used), tint(ram_hot)),
                (format!(" / {} GB", gb(n.mem_total)), None),
            ],
        );
        if let Some(gpu) = n.gpu {
            self.row(
                ui,
                w,
                "GPU",
                "How busy the node's graphics card is. Games and the desktop's rendering use this.",
                &[(format!("{}%", num(Some(gpu), 0)), tint(hot(f(gpu), 90.0)))],
            );
        }
        if let Some((used, total)) = vram {
            self.row(
                ui,
                w,
                "VRAM",
                "Graphics card memory in use out of its total.",
                &[
                    (gb(used), tint(hot(pct(used as f64, total as f64), 90.0))),
                    (format!(" / {} GB", gb(total)), None),
                ],
            );
        }
        if let Some(t) = n.temp {
            self.row(
                ui,
                w,
                "Temp",
                "The graphics card's temperature. Much above 85 °C it may slow itself down.",
                &[(format!("{} °C", f(t)), tint(hot(f(t), 85.0)))],
            );
        }
        if let Some(p) = n.power {
            let limit_hot = n.power_limit.is_some_and(|l| hot(pct(f(p), f(l)), 90.0));
            let mut segs = vec![(num(Some(p), 0), tint(limit_hot))];
            if let Some(l) = n.power_limit.filter(|l| *l > 0.0) {
                segs.push((format!(" / {}", num(Some(l), 0)), None));
            }
            segs.push((" W".into(), None));
            self.row(
                ui,
                w,
                "Power",
                "What the graphics card is drawing now, out of the limit it is allowed.",
                &segs,
            );
        }
        if let Some(c) = n.clock {
            self.row(ui, w, "Clock", "The graphics card's current core speed. It drops when the card is idle, hot or at its power limit.", &[(format!("{} MHz", f(c)), None)]);
        }
        if let Some(e) = n.enc {
            self.row(ui, w, "NVENC", "How busy the card's video encoder is. This is the part that compresses the stream.", &[(format!("{}%", num(Some(e), 0)), tint(hot(f(e), 90.0)))]);
        }
        if let Some(d) = n.dec {
            self.row(ui, w, "NVDEC", "How busy the card's video decoder is. It is used when the environment itself plays video.", &[(format!("{}%", num(Some(d), 0)), tint(hot(f(d), 90.0)))]);
        }
        self.row(ui, w, "Streamer CPU", "Processor used by the streaming program alone, in percent of one core, so it can pass 100.", &[(format!("{}%", num(Some(n.streamer_cpu), 0)), None)]);
    }

    /// One issue: title and detail with a copy button, then the hint.
    fn issue(&self, ui: &mut Ui, w: f32, issue: &Issue, cmds: &mut Vec<Cmd>) {
        let look = self.look;
        let painter = ui.painter().clone();
        let mut job = LayoutJob::default();
        job.append(
            issue.title,
            0.0,
            TextFormat::simple(look.font.clone(), look.title_color(issue.severity)),
        );
        job.append(
            &format!(" · {}", issue.detail),
            0.0,
            TextFormat::simple(look.font.clone(), look.ink2),
        );
        job.wrap.max_width = w - 20.0;
        let galley = painter.layout_job(job);
        let (rect, _) = ui.allocate_exact_size(vec2(w, galley.size().y.max(20.0)), Sense::hover());
        look.paint(&painter, rect.min, galley.clone());
        let last = galley.rows.last().map_or(0.0, |r| r.rect().right());
        let line_h = look.font.size * 1.25;
        let y = rect.top() + galley.size().y - line_h / 2.0;
        self.copy_button(ui, rect.left() + last + 4.0, y, Copied::One(issue.id), cmds);
        let hint = painter.layout(issue.hint.to_string(), look.font.clone(), look.ink2, w);
        let (rect, _) = ui.allocate_exact_size(hint.size(), Sense::hover());
        look.paint(&painter, rect.min, hint);
    }

    /// A foldable heading; true when the section is open.
    fn section(
        &self,
        ui: &mut Ui,
        w: f32,
        section: Section,
        title: &str,
        summary: (String, Option<Color32>),
        cmds: &mut Vec<Cmd>,
    ) -> bool {
        let look = self.look;
        let folded = self.prefs.is_folded(section);
        let color = look.section_color(section);
        ui.add_space(6.0);
        let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
        let resp = ui.interact(rect, ui.id().with(("fold", title)), Sense::click());
        let shown = if resp.hovered() {
            color.lerp_to_gamma(Color32::WHITE, 0.25)
        } else {
            color
        };
        let painter = ui.painter().clone();
        let c = pos2(rect.left() + 5.0, rect.center().y);
        let chevron = Rect::from_center_size(c, Vec2::splat(10.0));
        if folded {
            icons::paint(&painter, chevron, Icon::SectionChevronRight, shown);
        } else {
            icons::paint(&painter, chevron, Icon::SectionChevron, shown);
        }
        let label = look.text(
            &painter,
            rect.left() + 14.0,
            rect.center().y,
            Align::Min,
            &title.to_uppercase(),
            shown,
        );
        if folded && !summary.0.is_empty() {
            let avail = rect.right() - label.right() - 8.0;
            let g = look.fitted(&painter, &summary.0, summary.1.unwrap_or(look.p.ink), avail);
            let at = pos2(
                rect.right() - g.size().x,
                rect.center().y - g.size().y / 2.0,
            );
            look.paint(&painter, at, g);
        }
        if resp.clicked() {
            cmds.push(Cmd::Fold(section));
        }
        !folded
    }

    /// Label ... leader ... value, with the label's tooltip.
    fn row(&self, ui: &mut Ui, w: f32, label: &str, tip: &str, value: &[Seg]) {
        let look = self.look;
        let painter = ui.painter().clone();
        let (rect, _) = ui.allocate_exact_size(vec2(w, 20.0), Sense::hover());
        let cy = rect.center().y;
        let lg = look.galley(&painter, label, look.ink2);
        let lw = lg.size().x;
        let galleys: Vec<Arc<Galley>> = value
            .iter()
            .map(|(t, c)| look.galley(&painter, t, c.unwrap_or(look.p.ink)))
            .collect();
        let vw: f32 = galleys.iter().map(|g| g.size().x).sum();
        look.paint(&painter, pos2(rect.left(), cy - lg.size().y / 2.0), lg);
        let mut x = rect.right() - vw;
        for g in galleys {
            let width = g.size().x;
            look.paint(&painter, pos2(x, cy - g.size().y / 2.0), g);
            x += width;
        }
        let (x0, x1) = (rect.left() + lw + 6.0, rect.right() - vw - 6.0);
        if x1 > x0 {
            painter.hline(
                x0..=x1,
                cy.round() + 0.5,
                Stroke::new(1.0, look.ink2.gamma_multiply(0.5)),
            );
        }
        let hover = Rect::from_min_size(rect.min, vec2(lw, 20.0));
        ui.interact(hover, ui.id().with(("row", label)), Sense::hover())
            .on_hover_ui(|ui| {
                ui.set_max_width(260.0);
                ui.label(tip);
            });
    }
}

fn what_key(what: Copied) -> &'static str {
    match what {
        Copied::All => "all",
        Copied::One(id) => id,
    }
}

/// The hidden panel: a faint grade chip in the same corner.
fn chip(ui: &mut Ui, look: &Look, health: &Assessment, cmds: &mut Vec<Cmd>) {
    let p = &look.p;
    let letter = health.grade.map_or("–", Grade::letter);
    let galley = look.galley(ui.painter(), letter, p.ink);
    let size = vec2(galley.size().x + 12.0 + 2.0, 22.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let resp = ui.interact(rect, ui.id().with("chip"), Sense::click());
    let m = if resp.hovered() { 1.0 } else { 0.3 };
    let painter = ui.painter();
    painter.rect(
        rect,
        6.0,
        p.panel.gamma_multiply(0.6 * m),
        Stroke::new(1.0, p.line.gamma_multiply(m)),
        egui::StrokeKind::Inside,
    );
    let color = look.grade_color(health.grade).gamma_multiply(m);
    let g = look.galley(painter, letter, color);
    painter.galley(rect.center() - g.size() / 2.0, g, color);
    if resp.on_hover_text("Show stats").clicked() {
        cmds.push(Cmd::Show);
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
mod tests {
    use super::*;
    use crate::health;

    #[test]
    fn the_compact_line_names_what_it_has() {
        let s = StatsSnapshot {
            codec: "Hevc".into(),
            transport_tag: "WT",
            present_fps: 59.8,
            latency_ms: Some(11.64),
            mbps: Some(58.21),
            reconnects: 2,
            ..StatsSnapshot::default()
        };
        assert_eq!(
            s.compact_line(),
            "60 fps · 11.6 ms · 58.2 Mbit/s · HEVC/WT · 2 reconnects"
        );
        let bare = StatsSnapshot::default();
        assert_eq!(bare.compact_line(), "0 fps · – ms · – Mbit/s");
        assert_eq!(
            StatsSnapshot {
                reconnects: 1,
                ..bare
            }
            .compact_line(),
            "0 fps · – ms · – Mbit/s · 1 reconnect"
        );
    }

    #[test]
    fn the_report_has_the_numbers_around_the_issues() {
        let s = StatsSnapshot {
            codec: "Hevc".into(),
            transport_tag: "WT",
            width: 2560,
            height: 1440,
            present_fps: 58.0,
            target_fps: Some(60),
            mbps: Some(60.0),
            latency_ms: Some(30.0),
            decode_ms: Some(3.0),
            rtt_ms: Some(2.0),
            lost: Some(4),
            dropped: 7,
            node: Some(NodeStats {
                cpu: 12.0,
                mem_used: 8 << 30,
                mem_total: 32 << 30,
                gpu: Some(50.0),
                ..NodeStats::default()
            }),
            ..StatsSnapshot::default()
        };
        let mut history = vec![s.clone(); 8];
        for (i, h) in history.iter_mut().enumerate() {
            h.dropped = i as u64 * 7;
        }
        let health = health::assess(&history);
        assert!(health.issues.iter().any(|i| i.id == "dropped"));
        let text = issue_report(&health.issues[..1], &s, &health, "Cha Player 9, macOS 99");
        assert!(text.starts_with(&format!("{}: ", health.issues[0].title)));
        assert!(text.contains("\nHealth: "));
        assert!(text.contains("Stream: HEVC/WT, 2560×1440, 58 of 60 fps, 60.0 Mbit/s"));
        assert!(text.contains("Network: round trip 2.0 ms, 4 lost, 7 dropped"));
        assert!(text.contains("Node: CPU 12%, RAM 8.0/32.0 GB, GPU 50%"));
        assert!(text.ends_with("Player: Cha Player 9, macOS 99"));
    }

    fn click(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: winit::event::MouseButton::Left,
        }
    }

    #[test]
    fn a_press_on_the_panel_is_the_panels_until_it_is_released() {
        let mut panel = StatsPanel::new(OverlayPrefs::default());
        panel.rect = Some(Rect::from_min_size(pos2(10.0, 10.0), vec2(100.0, 50.0)));
        let inside = Some(pos2(20.0, 20.0));
        let outside = Some(pos2(500.0, 500.0));
        assert!(!panel.consumes(outside, &click(ElementState::Pressed)));
        assert!(!panel.consumes(outside, &click(ElementState::Released)));
        assert!(panel.consumes(inside, &click(ElementState::Pressed)));
        // Dragged out of the panel: still the panel's.
        let moved = WindowEvent::CursorLeft {
            device_id: winit::event::DeviceId::dummy(),
        };
        assert!(panel.consumes(outside, &moved));
        assert!(panel.consumes(outside, &click(ElementState::Released)));
        assert!(!panel.consumes(outside, &moved));
    }

    #[test]
    fn preferences_save_only_once_they_settle() {
        let mut panel = StatsPanel::new(OverlayPrefs::default());
        assert!(panel.take_save(false).is_none());
        panel.toggle_open();
        assert!(panel.take_save(false).is_none(), "just changed");
        assert!(panel.save_due().is_some());
        let saved = panel.take_save(true).expect("forced");
        assert!(!saved.open);
        assert!(panel.take_save(true).is_none());
        assert!(panel.save_due().is_none());
    }
}

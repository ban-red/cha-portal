//! The control channel, the same on every transport (plan §3.1): JSON lines
//! carrying the page's pings, input, resize, keyframe requests and clipboard
//! text in, and pongs, acknowledgements, stats, the apps' clipboard and their
//! setup status out.
//! Only the session with the floor (`viewers`) acts on the environment; the
//! others watch. WebRTC carries it on the `control`
//! DataChannel, WebTransport on the session's first bidirectional stream.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde::Serialize;
use tokio::sync::broadcast;

use crate::codec::VideoCodec;
use crate::compositor::{ClipboardWatch, CursorShape, CursorWatch, PointerSpot, PointerWatch};
use crate::gamepad::{EVENT_CLASSES, Gamepads, MAX_PADS, PadEvent, PadMemory, PadState};
use crate::input::BrowserInput;
use crate::media::Media;
use crate::overlay::Level;
use crate::status::{Status, StatusWatch};
use crate::system::SystemSample;
use crate::viewers::Seat;

#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        stream: serde_json::Value,
    },
    Pong {
        c: f64,
        s_us: u64,
    },
    /// Frame `id` went out with RTP timestamp `rtp` at `s_us` (session clock).
    /// `c_us`/`e_us`: when it was composited and encoded, same clock.
    Sent {
        id: u32,
        rtp: u32,
        s_us: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        c_us: Option<u64>,
        e_us: u64,
    },
    /// Input carrying a `probe` id reached the streamer at `s_us`.
    Probe {
        id: u64,
        s_us: u64,
    },
    /// Video now comes as `codec`, its datagrams tagged `stream` (WebTransport;
    /// the answer to the page's `codec` request). With `error`, the switch
    /// didn't happen and these are the codec and stream still running.
    Codec {
        codec: &'static str,
        stream: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The frame rate is now `fps` (the answer to the page's `fps` request).
    /// With `error`, it didn't change and `fps` is what still runs.
    Fps {
        fps: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The performance overlay is now at `level` (the answer to the page's
    /// `overlay` request). With `error`, it didn't change and `level` is what
    /// it still is (left out when the app has no overlay).
    Overlay {
        #[serde(skip_serializing_if = "Option::is_none")]
        level: Option<Level>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// An app set pad `i`'s motors: `lo` the strong one, `hi` the weak one
    /// (0..1), for `ms`; 0 stops them.
    Rumble {
        i: usize,
        lo: f32,
        hi: f32,
        ms: u32,
    },
    /// An app pulsed a trackpad of pad `i`'s (Steam Controller): `count`
    /// pulses of `on_us`, `off_us` apart, at `amp` (0..1); 0 stops them.
    Haptic {
        i: usize,
        side: &'static str,
        amp: f32,
        on_us: u32,
        off_us: u32,
        count: u32,
    },
    /// An app set the lightbar of pad `i` (DualSense), 0..255.
    Led {
        i: usize,
        r: u8,
        g: u8,
        b: u8,
    },
    /// An app set the player LEDs of pad `i`, bits 0..5 (DualSense).
    Players {
        i: usize,
        mask: u8,
    },
    /// An app set an adaptive trigger effect on pad `i` (DualSense): the
    /// output report's block, the type and ten parameters.
    Trigger {
        i: usize,
        side: &'static str,
        effect: [u8; 11],
    },
    /// An app copied this text (P2.6).
    Clipboard {
        text: String,
    },
    /// The cursor's shape, for a page that draws it (P2.6).
    Cursor(CursorMsg),
    /// What the app's long setup is doing (P2.1); with no `label`, nothing now.
    /// Every session gets it, not only the controller's.
    Status {
        #[serde(flatten)]
        status: Option<Status>,
    },
    /// Whether this session has the controls, and how many sessions watch.
    Floor {
        control: bool,
        viewers: usize,
    },
    /// Where the pointer is (0..1), for a viewer's page to draw it when the
    /// picture doesn't show it (`drawn` false).
    Pointer {
        x: f32,
        y: f32,
        drawn: bool,
    },
    /// The output was resized to `w`×`h` at `s_us` (as asked, rounded to fit).
    Resized {
        w: u32,
        h: u32,
        s_us: u64,
    },
    /// The node's CPU, RAM and GPU use, about once a second (`system.rs`).
    System(SystemSample),
    Stats {
        elapsed_ms: u64,
        #[serde(flatten)]
        stats: StreamerStats,
    },
    Done {
        #[serde(flatten)]
        stats: StreamerStats,
    },
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct StreamerStats {
    pub frames_generated: u64,
    pub frames_sent: u64,
    pub bytes_sent: u64,
    pub keyframes: u64,
    /// Browser PLI/FIR or keyframe requests, each turned into a force-key-unit.
    pub keyframe_requests: u64,
    /// Frames not sent because the transport's queue was full (WebTransport).
    #[serde(skip_serializing_if = "is_zero")]
    pub frames_dropped: u64,
    /// From the session being ready to the first frame.
    pub first_frame_ms: Option<u64>,
    /// Composited frame ready → encoded (WebTransport: over the last report's
    /// window, so a codec switch shows within one).
    pub composite_to_encoded_ms_p50: Option<f64>,
    pub composite_to_encoded_ms_p99: Option<f64>,
    /// Encoded → handed to the transport.
    pub encoded_to_sent_us_p50: Option<u64>,
    pub encoded_to_sent_us_p99: Option<u64>,
    pub frame_interval_ms_p50: Option<f64>,
    pub frame_interval_ms_p99: Option<f64>,
    /// The frame rate now (the page can change it: `{"t":"fps"}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// The performance overlay's level (0 off to 4 full, or "custom"), only
    /// for an app that has one (the page can change it: `{"t":"overlay"}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Level>,
    /// Rate control (WebTransport): the rate it aims at, and the path's
    /// RTT growth over its minimum.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_mbps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_ms: Option<f64>,
    pub inputs: u64,
    pub inputs_unmapped: u64,
    pub resizes: u64,
    pub audio_packets: u64,
    pub audio_bytes: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Applies the page's messages to the environment.
pub struct Control {
    pub epoch: Instant,
    pub codec: VideoCodec,
    pub media: Arc<Media>,
    pub gamepads: Option<Arc<Gamepads>>,
    /// This session's place among the viewers; leaving when dropped.
    pub seat: Seat,
}

impl Control {
    /// Handles one line from the page; replies go through `reply`.
    pub fn handle_line(
        &self,
        line: &str,
        stats: &mut StreamerStats,
        reply: &mut dyn FnMut(ServerMsg),
    ) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        let kind = msg.get("t").and_then(|t| t.as_str());
        // A viewer watches: what it sends doesn't reach the environment.
        if matches!(
            kind,
            Some("input" | "resize" | "clipboard" | "cursor" | "fps" | "overlay")
        ) && !self.seat.has_control()
        {
            return;
        }
        match kind {
            Some("take_control") => {
                self.seat.take_control();
            }
            Some("presence") => {
                // A page in front or heard is active; one hidden and silent isn't.
                if let Some(active) = msg.get("active").and_then(|a| a.as_bool()) {
                    self.seat.set_active(active);
                }
            }
            Some("ping") => {
                if let Some(c) = msg.get("c").and_then(|c| c.as_f64()) {
                    reply(ServerMsg::Pong {
                        c,
                        s_us: self.now_us(),
                    });
                }
            }
            Some("resize") => {
                let dim = |k: &str| msg.get(k).and_then(|v| v.as_u64()).map(|v| v as u32);
                if let (Some(w), Some(h)) = (dim("w"), dim("h")) {
                    let (w, h) = self.media.resize(w, h);
                    stats.resizes += 1;
                    reply(ServerMsg::Resized {
                        w,
                        h,
                        s_us: self.now_us(),
                    });
                }
            }
            // The frame rate, for every viewer: 60, 90 or 120.
            Some("fps") => reply(fps_reply(&self.media, &msg)),
            // The app's performance overlay, for every viewer: 0 to 4.
            Some("overlay") => reply(overlay_reply(&self.media, &msg)),
            // The page draws the cursor (desktop) or wants it in the picture.
            Some("cursor") => {
                if let Some(client) = msg.get("client").and_then(|c| c.as_bool()) {
                    self.media.set_client_cursor(client);
                }
            }
            // The browser's clipboard, sent before the paste keys that use it.
            Some("clipboard") => {
                if let Some(text) = msg.get("text").and_then(|t| t.as_str()) {
                    self.media.set_clipboard(text);
                }
            }
            // The page lost a frame (WebTransport), or its WebRTC decoder
            // stalled waiting on retransmissions: start over from a keyframe.
            Some("keyframe") => {
                stats.keyframe_requests += 1;
                self.media.request_keyframe(self.codec);
            }
            Some("input") => {
                let received_us = self.now_us();
                stats.inputs += 1;
                if msg.get("k").and_then(|k| k.as_str()) == Some("pad") {
                    match (&self.gamepads, serde_json::from_value::<PadState>(msg)) {
                        (Some(pads), Ok(state)) => pads.update(&state),
                        _ => stats.inputs_unmapped += 1,
                    }
                    return;
                }
                let probe = msg.get("probe").and_then(|p| p.as_u64());
                let (w, h) = self.media.size();
                match serde_json::from_value::<BrowserInput>(msg)
                    .ok()
                    .and_then(|i| i.to_compositor(w, h))
                {
                    Some(input) => self.media.input(input),
                    None => stats.inputs_unmapped += 1,
                }
                // The page's latency probe: echo when its click arrived.
                if let Some(id) = probe {
                    reply(ServerMsg::Probe {
                        id,
                        s_us: received_us,
                    });
                }
            }
            _ => {}
        }
    }

    pub fn now_us(&self) -> u64 {
        self.epoch.elapsed().as_micros() as u64
    }
}

/// The answer to a `{"t":"fps","fps":N}` request: the rate now, or why it
/// didn't change.
fn fps_reply(media: &Media, msg: &serde_json::Value) -> ServerMsg {
    answer_fps(msg, |fps| media.set_fps(fps), || media.fps())
}

/// `set` applies a valid request; `current` is the rate that runs now.
fn answer_fps(
    msg: &serde_json::Value,
    set: impl FnOnce(u32) -> Result<u32, String>,
    current: impl FnOnce() -> u32,
) -> ServerMsg {
    let requested = msg
        .get("fps")
        .and_then(|f| f.as_u64())
        .and_then(|f| u32::try_from(f).ok())
        .ok_or_else(|| "no fps".to_string());
    match requested.and_then(set) {
        Ok(fps) => ServerMsg::Fps { fps, error: None },
        Err(error) => ServerMsg::Fps {
            fps: current(),
            error: Some(error),
        },
    }
}

/// The answer to a `{"t":"overlay","level":N}` request: the level now, or
/// why it didn't change.
fn overlay_reply(media: &Media, msg: &serde_json::Value) -> ServerMsg {
    answer_overlay(msg, |level| media.set_overlay(level), || media.overlay())
}

/// `set` applies a valid request; `current` is the level that is set now.
fn answer_overlay(
    msg: &serde_json::Value,
    set: impl FnOnce(u8) -> Result<u8, String>,
    current: impl FnOnce() -> Option<Level>,
) -> ServerMsg {
    let requested = msg
        .get("level")
        .and_then(|l| l.as_u64())
        // Out of u8 range is out of range, not "no level".
        .map(|l| u8::try_from(l).unwrap_or(u8::MAX))
        .ok_or_else(|| "no level".to_string());
    match requested.and_then(set) {
        Ok(level) => ServerMsg::Overlay {
            level: Some(Level::Preset(level)),
            error: None,
        },
        Err(error) => ServerMsg::Overlay {
            level: current(),
            error: Some(error),
        },
    }
}

/// `hidden`, a CSS keyword (`named`), or an `image`: RGBA as base64, sent
/// only the first time a session sends its `id`.
#[derive(Debug, Default, Serialize)]
pub struct CursorMsg {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    w: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    h: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    y: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rgba: Option<String>,
}

/// A cursor shape as a control message; `sent` holds the image ids this
/// session already sent.
pub fn cursor_msg(shape: &CursorShape, sent: &mut HashSet<u64>) -> ServerMsg {
    ServerMsg::Cursor(match shape {
        CursorShape::Hidden => CursorMsg {
            kind: "hidden",
            ..CursorMsg::default()
        },
        CursorShape::Named(name) => CursorMsg {
            kind: "named",
            name: Some(name),
            ..CursorMsg::default()
        },
        CursorShape::Image {
            id,
            width,
            height,
            hotspot,
            rgba,
        } => CursorMsg {
            kind: "image",
            id: Some(format!("{id:016x}")),
            w: Some(*width),
            h: Some(*height),
            x: Some(hotspot.0),
            y: Some(hotspot.1),
            rgba: sent
                .insert(*id)
                .then(|| base64::engine::general_purpose::STANDARD.encode(rgba)),
            ..CursorMsg::default()
        },
    })
}

/// The cursor's next shape; never resolves once the compositor is gone.
pub async fn next_cursor(watch: &mut Option<CursorWatch>) -> Option<CursorShape> {
    match watch {
        Some(rx) => match rx.changed().await {
            Ok(()) => Some(rx.borrow_and_update().clone()),
            Err(_) => {
                *watch = None;
                None
            }
        },
        None => std::future::pending().await,
    }
}

/// The app's next setup status as a message (a cleared one has no label);
/// never resolves once the watcher is gone.
pub async fn next_status(watch: &mut Option<StatusWatch>) -> Option<ServerMsg> {
    match watch {
        Some(rx) => match rx.changed().await {
            Ok(()) => Some(ServerMsg::Status {
                status: rx.borrow_and_update().clone(),
            }),
            Err(_) => {
                *watch = None;
                None
            }
        },
        None => std::future::pending().await,
    }
}

/// What the pads' apps do goes out at most this often per pad and kind.
const PAD_EVENT_INTERVAL: Duration = Duration::from_millis(16);

/// Keeps pad events to about 60 messages a second per pad and kind (motors,
/// each trackpad, the lightbar, ...): the newest of what comes in between waits
/// for its turn, and a stop never waits.
#[derive(Default)]
struct PadLimit {
    slots: [[PadSlot; EVENT_CLASSES]; MAX_PADS],
}

#[derive(Default)]
struct PadSlot {
    sent: Option<Instant>,
    waiting: Option<PadEvent>,
}

impl PadLimit {
    /// The message to send now, if any.
    fn push(&mut self, event: PadEvent, now: Instant) -> Option<PadEvent> {
        let slot = self.slots.get_mut(event.slot())?.get_mut(event.class())?;
        if event.is_stop() {
            *slot = PadSlot::default();
            return Some(event);
        }
        if slot.sent.is_none_or(|at| now >= at + PAD_EVENT_INTERVAL) {
            slot.sent = Some(now);
            slot.waiting = None;
            return Some(event);
        }
        slot.waiting = Some(event);
        None
    }

    /// When a waiting message may go.
    fn due(&self) -> Option<Instant> {
        self.slots
            .iter()
            .flatten()
            .filter(|s| s.waiting.is_some())
            .filter_map(|s| s.sent.map(|at| at + PAD_EVENT_INTERVAL))
            .min()
    }

    /// A waiting message whose time has come.
    fn take_due(&mut self, now: Instant) -> Option<PadEvent> {
        let slot = self.slots.iter_mut().flatten().find(|s| {
            s.waiting.is_some() && s.sent.is_some_and(|at| now >= at + PAD_EVENT_INTERVAL)
        })?;
        slot.sent = Some(now);
        slot.waiting.take()
    }
}

/// A session's view of what apps do to the pads, rate-limited.
pub struct PadFeed {
    rx: Option<broadcast::Receiver<PadEvent>>,
    limit: PadLimit,
    /// What the apps last set (lightbar, player LEDs, triggers).
    memory: Option<Arc<Mutex<PadMemory>>>,
    /// Whether the session had the controls when last checked.
    had_control: bool,
}

impl PadFeed {
    pub fn new(gamepads: Option<&Gamepads>) -> Self {
        Self {
            rx: gamepads.map(Gamepads::events),
            limit: PadLimit::default(),
            memory: gamepads.map(Gamepads::memory),
            had_control: false,
        }
    }

    /// Call whenever the session's floor may have changed (and once when its
    /// control channel opens): the lightbar, player LEDs and trigger effects
    /// the apps set before the session had the controls, when it has just
    /// gained them. Empty otherwise.
    pub fn replay_on_gain(&mut self, has_control: bool) -> Vec<ServerMsg> {
        let gained = has_control && !self.had_control;
        self.had_control = has_control;
        let Some(memory) = self.memory.as_ref().filter(|_| gained) else {
            return Vec::new();
        };
        let events = memory.lock().unwrap_or_else(|e| e.into_inner()).replay();
        events.into_iter().map(pad_msg).collect()
    }

    /// The next message (rumble, haptics, lightbar, ...); never resolves
    /// without pads.
    pub async fn next(&mut self) -> ServerMsg {
        loop {
            let due = self.limit.due();
            let Some(rx) = &mut self.rx else {
                return std::future::pending().await;
            };
            let mut closed = false;
            let ready = tokio::select! {
                got = rx.recv() => match got {
                    Ok(event) => self.limit.push(event, Instant::now()),
                    Err(broadcast::error::RecvError::Lagged(_)) => None,
                    Err(broadcast::error::RecvError::Closed) => {
                        closed = true;
                        None
                    }
                },
                () = tokio::time::sleep_until(due.unwrap_or_else(Instant::now).into()), if due.is_some() => {
                    self.limit.take_due(Instant::now())
                }
            };
            if closed {
                self.rx = None;
            }
            if let Some(event) = ready {
                return pad_msg(event);
            }
        }
    }
}

/// An event for the page.
fn pad_msg(event: PadEvent) -> ServerMsg {
    match event {
        PadEvent::Rumble(r) => ServerMsg::Rumble {
            i: r.slot,
            lo: r.lo,
            hi: r.hi,
            ms: r.ms,
        },
        PadEvent::Haptic(h) => ServerMsg::Haptic {
            i: h.slot,
            side: h.side.name(),
            amp: h.amp,
            on_us: h.on_us,
            off_us: h.off_us,
            count: h.count,
        },
        PadEvent::Led(l) => ServerMsg::Led {
            i: l.slot,
            r: l.r,
            g: l.g,
            b: l.b,
        },
        PadEvent::Players(p) => ServerMsg::Players {
            i: p.slot,
            mask: p.mask,
        },
        PadEvent::Trigger(t) => ServerMsg::Trigger {
            i: t.slot,
            side: t.side.name(),
            effect: t.effect,
        },
    }
}

/// The floor as this session sees it.
pub fn floor_msg(seat: &Seat) -> ServerMsg {
    ServerMsg::Floor {
        control: seat.has_control(),
        viewers: seat.viewers(),
    }
}

/// Where the pointer moved; never resolves once the compositor is gone.
pub async fn next_pointer(watch: &mut Option<PointerWatch>) -> Option<PointerSpot> {
    match watch {
        Some(rx) => match rx.changed().await {
            Ok(()) => Some(*rx.borrow_and_update()),
            Err(_) => {
                *watch = None;
                None
            }
        },
        None => std::future::pending().await,
    }
}

/// The next text apps copy (None: an empty clipboard, or the compositor is
/// gone, after which this never resolves).
pub async fn next_clipboard(watch: &mut Option<ClipboardWatch>) -> Option<Arc<str>> {
    match watch {
        Some(rx) => match rx.changed().await {
            Ok(()) => rx.borrow_and_update().clone(),
            Err(_) => {
                *watch = None;
                None
            }
        },
        None => std::future::pending().await,
    }
}

pub fn percentile<T: Copy>(sorted: &[T], q: f64) -> Option<T> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((sorted.len() as f64 * q) as usize).min(sorted.len() - 1);
    Some(sorted[i])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::{Haptic, Led, Players, Rumble, Side, Trigger};

    fn line(msg: &ServerMsg) -> String {
        serde_json::to_string(msg).unwrap()
    }

    #[test]
    fn rumble_serializes_and_is_limited_per_pad() {
        assert_eq!(
            line(&ServerMsg::Rumble {
                i: 1,
                lo: 1.0,
                hi: 0.5,
                ms: 200
            }),
            r#"{"t":"rumble","i":1,"lo":1.0,"hi":0.5,"ms":200}"#
        );
        let r = |slot, lo, ms| {
            PadEvent::Rumble(Rumble {
                slot,
                lo,
                hi: 0.0,
                ms,
            })
        };
        let lo_of = |e: PadEvent| match e {
            PadEvent::Rumble(r) => r.lo,
            other => panic!("{other:?}"),
        };
        let mut limit = PadLimit::default();
        let t0 = Instant::now();
        assert!(limit.push(r(0, 0.1, 100), t0).is_some());
        // Within the interval the newest waits; another pad is its own.
        assert!(
            limit
                .push(r(0, 0.2, 100), t0 + Duration::from_millis(2))
                .is_none()
        );
        assert!(
            limit
                .push(r(0, 0.3, 100), t0 + Duration::from_millis(4))
                .is_none()
        );
        assert!(
            limit
                .push(r(1, 0.9, 100), t0 + Duration::from_millis(4))
                .is_some()
        );
        assert_eq!(limit.due(), Some(t0 + PAD_EVENT_INTERVAL));
        assert!(limit.take_due(t0 + Duration::from_millis(8)).is_none());
        assert_eq!(lo_of(limit.take_due(t0 + PAD_EVENT_INTERVAL).unwrap()), 0.3);
        assert!(limit.due().is_none());
        // A stop goes at once and cancels what waited.
        assert!(
            limit
                .push(r(0, 0.4, 100), t0 + PAD_EVENT_INTERVAL)
                .is_none()
        );
        let stop = limit.push(r(0, 0.0, 0), t0 + PAD_EVENT_INTERVAL).unwrap();
        assert!(stop.is_stop());
        assert!(limit.due().is_none());
    }

    #[test]
    fn the_other_pad_messages_serialize_and_are_limited_per_kind() {
        let events = [
            PadEvent::Haptic(Haptic {
                slot: 0,
                side: Side::Left,
                amp: 0.5,
                on_us: 100,
                off_us: 200,
                count: 3,
            }),
            PadEvent::Led(Led {
                slot: 2,
                r: 1,
                g: 2,
                b: 3,
            }),
            PadEvent::Players(Players { slot: 1, mask: 4 }),
            PadEvent::Trigger(Trigger {
                slot: 0,
                side: Side::Right,
                effect: [2, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10],
            }),
        ];
        let lines: Vec<String> = events.iter().map(|e| line(&pad_msg(*e))).collect();
        assert_eq!(
            lines,
            [
                r#"{"t":"haptic","i":0,"side":"left","amp":0.5,"on_us":100,"off_us":200,"count":3}"#,
                r#"{"t":"led","i":2,"r":1,"g":2,"b":3}"#,
                r#"{"t":"players","i":1,"mask":4}"#,
                r#"{"t":"trigger","i":0,"side":"right","effect":[2,1,2,3,4,5,6,7,8,9,10]}"#,
            ]
        );
        // Each kind (and each side) keeps its own newest; none holds back another.
        let mut limit = PadLimit::default();
        let t0 = Instant::now();
        for e in events {
            assert!(limit.push(e, t0).is_some());
        }
        assert!(
            limit
                .push(events[1], t0 + Duration::from_millis(1))
                .is_none()
        );
        assert!(
            limit
                .push(events[0], t0 + Duration::from_millis(1))
                .is_none()
        );
        // A haptic stop goes at once.
        let stop = PadEvent::Haptic(Haptic {
            count: 0,
            ..match events[0] {
                PadEvent::Haptic(h) => h,
                _ => unreachable!(),
            }
        });
        assert!(limit.push(stop, t0 + Duration::from_millis(2)).is_some());
    }

    #[test]
    fn status_messages_carry_the_status_flat() {
        let status = Status {
            label: "Downloading Steam".into(),
            done: Some(123.0),
            total: Some(496.0),
            unit: Some("MB".into()),
        };
        assert_eq!(
            line(&ServerMsg::Status {
                status: Some(status)
            }),
            r#"{"t":"status","label":"Downloading Steam","done":123.0,"total":496.0,"unit":"MB"}"#
        );
        let bare = Status {
            label: "Unpacking Steam".into(),
            done: None,
            total: None,
            unit: None,
        };
        assert_eq!(
            line(&ServerMsg::Status { status: Some(bare) }),
            r#"{"t":"status","label":"Unpacking Steam"}"#
        );
        assert_eq!(
            line(&ServerMsg::Status { status: None }),
            r#"{"t":"status"}"#
        );
    }

    #[tokio::test]
    async fn a_session_hears_every_status_change_and_the_clearing() {
        let (publish, watch) = tokio::sync::watch::channel(None);
        let mut watch = Some(watch);
        let say = |label: &str| {
            publish.send_replace(Some(Status {
                label: label.into(),
                done: None,
                total: None,
                unit: None,
            }));
        };
        say("Installing");
        assert_eq!(
            line(&next_status(&mut watch).await.unwrap()),
            r#"{"t":"status","label":"Installing"}"#
        );
        publish.send_replace(None);
        assert_eq!(
            line(&next_status(&mut watch).await.unwrap()),
            r#"{"t":"status"}"#
        );
        // The watcher is gone: no more news, and none ever again.
        drop(publish);
        assert!(next_status(&mut watch).await.is_none());
        assert!(watch.is_none());
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            next_status(&mut watch),
        )
        .await
        .expect_err("pending for good");
    }

    #[test]
    fn the_frame_rate_request_is_answered_with_the_rate_or_the_reason() {
        let ask = |text: &str| {
            let msg: serde_json::Value = serde_json::from_str(text).unwrap();
            let reply = answer_fps(&msg, crate::framerate::validate, || 60);
            serde_json::to_string(&reply).unwrap()
        };
        assert_eq!(ask(r#"{"t":"fps","fps":120}"#), r#"{"t":"fps","fps":120}"#);
        assert_eq!(ask(r#"{"t":"fps","fps":90}"#), r#"{"t":"fps","fps":90}"#);
        assert_eq!(
            ask(r#"{"t":"fps","fps":144}"#),
            r#"{"t":"fps","fps":60,"error":"144 fps isn't one of 60, 90, 120"}"#
        );
        assert_eq!(
            ask(r#"{"t":"fps"}"#),
            r#"{"t":"fps","fps":60,"error":"no fps"}"#
        );
        assert_eq!(
            ask(r#"{"t":"fps","fps":"fast"}"#),
            r#"{"t":"fps","fps":60,"error":"no fps"}"#
        );
    }

    #[test]
    fn the_overlay_request_is_answered_with_the_level_or_the_reason() {
        let ask = |text: &str, has_overlay: bool| {
            let msg: serde_json::Value = serde_json::from_str(text).unwrap();
            let reply = answer_overlay(
                &msg,
                |level| match (has_overlay, level) {
                    (false, _) => Err("this app has no performance overlay".to_string()),
                    (true, 5..) => Err(format!("overlay level {level} isn't one of 0 to 4")),
                    (true, level) => Ok(level),
                },
                || has_overlay.then_some(Level::Preset(1)),
            );
            serde_json::to_string(&reply).unwrap()
        };
        assert_eq!(
            ask(r#"{"t":"overlay","level":3}"#, true),
            r#"{"t":"overlay","level":3}"#
        );
        assert_eq!(
            ask(r#"{"t":"overlay","level":0}"#, true),
            r#"{"t":"overlay","level":0}"#
        );
        assert_eq!(
            ask(r#"{"t":"overlay","level":9}"#, true),
            r#"{"t":"overlay","level":1,"error":"overlay level 9 isn't one of 0 to 4"}"#
        );
        assert_eq!(
            ask(r#"{"t":"overlay","level":1000}"#, true),
            r#"{"t":"overlay","level":1,"error":"overlay level 255 isn't one of 0 to 4"}"#
        );
        assert_eq!(
            ask(r#"{"t":"overlay"}"#, true),
            r#"{"t":"overlay","level":1,"error":"no level"}"#
        );
        assert_eq!(
            ask(r#"{"t":"overlay","level":2}"#, false),
            r#"{"t":"overlay","error":"this app has no performance overlay"}"#
        );
    }

    #[test]
    fn the_stats_carry_the_overlay_only_for_an_app_that_has_one() {
        let line = |stats: StreamerStats| serde_json::to_value(stats).unwrap();
        assert!(line(StreamerStats::default()).get("overlay").is_none());
        let on = StreamerStats {
            overlay: Some(Level::Preset(2)),
            ..StreamerStats::default()
        };
        assert_eq!(line(on)["overlay"], 2);
        let custom = StreamerStats {
            overlay: Some(Level::Custom),
            ..StreamerStats::default()
        };
        assert_eq!(line(custom)["overlay"], "custom");
    }

    #[test]
    fn the_stats_carry_the_frame_rate_once_known() {
        let line = |stats: StreamerStats| serde_json::to_value(stats).unwrap();
        assert!(line(StreamerStats::default()).get("fps").is_none());
        let known = StreamerStats {
            fps: Some(90),
            ..StreamerStats::default()
        };
        assert_eq!(line(known)["fps"], 90);
    }

    #[test]
    fn a_session_that_gains_the_controls_hears_the_last_lightbar_and_leds() {
        use crate::gamepad::{Led, Players, Rumble, Side, Trigger};
        let memory = Arc::new(Mutex::new(PadMemory::default()));
        {
            let mut m = memory.lock().unwrap();
            m.remember(PadEvent::Led(Led {
                slot: 0,
                r: 1,
                g: 2,
                b: 3,
            }));
            m.remember(PadEvent::Led(Led {
                slot: 0,
                r: 9,
                g: 8,
                b: 7,
            }));
            m.remember(PadEvent::Led(Led {
                slot: 1,
                r: 4,
                g: 5,
                b: 6,
            }));
            m.remember(PadEvent::Players(Players { slot: 0, mask: 4 }));
            m.remember(PadEvent::Trigger(Trigger {
                slot: 0,
                side: Side::Left,
                effect: [1; 11],
            }));
            // Moments, not state: not kept.
            m.remember(PadEvent::Rumble(Rumble {
                slot: 0,
                lo: 1.0,
                hi: 1.0,
                ms: 100,
            }));
        }
        let mut feed = PadFeed {
            rx: None,
            limit: PadLimit::default(),
            memory: Some(memory),
            had_control: false,
        };
        let lines = |msgs: Vec<ServerMsg>| msgs.iter().map(line).collect::<Vec<_>>();
        assert!(
            feed.replay_on_gain(false).is_empty(),
            "a watcher hears nothing"
        );
        let gained = lines(feed.replay_on_gain(true));
        assert_eq!(
            gained,
            [
                r#"{"t":"led","i":0,"r":9,"g":8,"b":7}"#,
                r#"{"t":"players","i":0,"mask":4}"#,
                r#"{"t":"trigger","i":0,"side":"left","effect":[1,1,1,1,1,1,1,1,1,1,1]}"#,
                r#"{"t":"led","i":1,"r":4,"g":5,"b":6}"#,
            ],
            "the newest of each, pad by pad"
        );
        assert!(feed.replay_on_gain(true).is_empty(), "once per gain");
        assert!(feed.replay_on_gain(false).is_empty());
        assert_eq!(
            feed.replay_on_gain(true).len(),
            4,
            "again after a take-over"
        );
        let mut none = PadFeed::new(None);
        assert!(
            none.replay_on_gain(true).is_empty(),
            "no pads, nothing to say"
        );
    }
}

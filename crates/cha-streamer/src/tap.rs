//! The frame tap: the latest composited picture, small, on a local Unix socket.
//!
//! For a process on the node that wants to look at the screen without a viewer
//! connected: a health check, a thumbnail, an agent. It is off unless the
//! streamer starts with `--frame-tap <socket>`, and it costs nothing until a
//! reader asks: the compositor only captures for [`KEEP_ALIVE`] after a request,
//! and then at most `--frame-tap-fps` times a second.
//!
//! The socket is mode 0600, owned by the streamer's user. A line protocol, one
//! request per line:
//! - `FRAME [after]` waits for a frame newer than `after` (the newest one at the
//!   time of the request if it is left out), for up to [`MAX_WAIT`] (300 ms). It answers
//!   `OK <seq> <width> <height> <bytes> <age_ms>\n` and then `bytes` of RGB,
//!   top row first. `age_ms` is how long ago the frame was captured: a screen
//!   that isn't changing makes no new frames, so the newest one is returned
//!   once the wait is over, and it is still what's on screen. `NONE\n` means no
//!   frame has been captured yet.
//! - `INFO` answers one JSON line: the tap's size, rate and newest sequence number.
//! - `PAD <json>` sets a virtual gamepad's state, only when the streamer was
//!   started with `--frame-tap-pad <index>` (otherwise it answers `ERR`). The JSON
//!   is a browser's pad message: `{"b":[...buttons 0..1],"a":[lx,ly,rx,ry],"ty":"xbox"}`,
//!   plus `hold_ms` (default 250, at most 2000): how long the state stands if
//!   nothing newer comes. It goes to that pad index as a session's would, and
//!   answers `OK`. When the hold runs out, when `PADOFF` is sent, or when the
//!   connection that sent it closes, the pad goes back to rest.
//! - `QUIT`, or closing the socket, ends the connection.
//!
//! The picture is the compositor's output, scaled to the tap's size (a plain
//! stretch if the aspect differs), with the cursor in it if it is drawn into
//! the picture.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::gamepad::{Gamepads, PadState};

/// How long after a request the compositor keeps capturing for the tap.
const KEEP_ALIVE: Duration = Duration::from_secs(3);
/// The longest a `FRAME` request waits for a newer frame.
const MAX_WAIT: Duration = Duration::from_millis(300);
/// Readers at once; the picture is a few hundred KB, so this is about sanity.
const MAX_CLIENTS: usize = 8;
/// How long a `PAD` state stands when its `hold_ms` is left out, and the most it may ask for.
const DEFAULT_HOLD: Duration = Duration::from_millis(250);
const MAX_HOLD: Duration = Duration::from_millis(2000);
/// How often the watchdog looks for a `PAD` state whose hold has run out.
const WATCHDOG: Duration = Duration::from_millis(20);

/// Somewhere pad states go: the gamepads, or a recorder in the tests.
pub trait PadTarget: Send + Sync {
    fn update(&self, state: &PadState);
}

impl PadTarget for Gamepads {
    fn update(&self, state: &PadState) {
        Gamepads::update(self, state);
    }
}

/// The one virtual pad the tap's `PAD` command drives, at a fixed index.
pub struct PadSink {
    target: Arc<dyn PadTarget>,
    index: usize,
    /// When the state last set stops standing; none while the pad is at rest.
    deadline: Mutex<Option<Instant>>,
}

impl PadSink {
    pub fn new(target: Arc<dyn PadTarget>, index: usize) -> Arc<Self> {
        Arc::new(Self {
            target,
            index,
            deadline: Mutex::new(None),
        })
    }

    /// Applies one `PAD` line's JSON.
    fn set(&self, json: &str) -> Result<(), String> {
        let mut value: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
        let object = value.as_object_mut().ok_or("PAD wants a JSON object")?;
        let hold = object
            .remove("hold_ms")
            .and_then(|v| v.as_u64())
            .map_or(DEFAULT_HOLD, |ms| Duration::from_millis(ms).min(MAX_HOLD));
        // The index is ours, whatever the message says; and a message can't say "gone".
        object.insert("i".into(), self.index.into());
        object.remove("gone");
        let state: PadState =
            serde_json::from_value(value).map_err(|e| format!("not a pad state: {e}"))?;
        if state.b.len() > 32 || state.a.len() > 8 {
            return Err("too many buttons or axes".into());
        }
        self.target.update(&state);
        *self.deadline.lock().expect("pad lock") = Some(Instant::now() + hold);
        Ok(())
    }

    /// Puts the pad back to rest, if it isn't.
    fn release(&self) {
        if self.deadline.lock().expect("pad lock").take().is_some() {
            self.target.update(&PadState {
                i: self.index,
                gone: true,
                ..PadState::default()
            });
        }
    }

    /// Lets go if the state set last has stood for its hold.
    fn expire(&self) {
        let over = self
            .deadline
            .lock()
            .expect("pad lock")
            .is_some_and(|at| Instant::now() >= at);
        if over {
            self.release();
        }
    }
}

/// One captured picture.
pub struct TapFrame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` bytes, top row first.
    pub rgb: Vec<u8>,
    pub captured: Instant,
}

pub struct Tap {
    pub width: u32,
    pub height: u32,
    fps: u32,
    keep_alive: Duration,
    start: Instant,
    /// When a reader last asked, in milliseconds since `start`, plus one; 0 for never.
    asked: AtomicU64,
    /// A request came after an idle spell: composite once even if nothing changed,
    /// so the first answer is the screen as it is now.
    stale: AtomicBool,
    seq: AtomicU64,
    latest: Mutex<Option<Arc<TapFrame>>>,
    fresh: Condvar,
}

impl Tap {
    pub fn new(width: u32, height: u32, fps: u32) -> Arc<Self> {
        Self::with_keep_alive(width, height, fps, KEEP_ALIVE)
    }

    fn with_keep_alive(width: u32, height: u32, fps: u32, keep_alive: Duration) -> Arc<Self> {
        Arc::new(Self {
            width,
            height,
            fps: fps.max(1),
            keep_alive,
            start: Instant::now(),
            asked: AtomicU64::new(0),
            stale: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            latest: Mutex::new(None),
            fresh: Condvar::new(),
        })
    }

    /// The gap between captures.
    pub fn interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / f64::from(self.fps))
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64 + 1
    }

    /// A reader asked for a frame.
    pub fn touch(&self) {
        let now = self.now_ms();
        let before = self.asked.swap(now, Ordering::AcqRel);
        if before == 0 || now - before > self.keep_alive.as_millis() as u64 {
            self.stale.store(true, Ordering::Release);
        }
    }

    /// Whether a reader asked recently enough that the compositor should capture.
    pub fn wanted(&self) -> bool {
        let asked = self.asked.load(Ordering::Acquire);
        asked != 0 && self.now_ms() - asked <= self.keep_alive.as_millis() as u64
    }

    /// Whether this is the first request after an idle spell (once per spell).
    pub fn take_stale(&self) -> bool {
        self.stale.swap(false, Ordering::AcqRel)
    }

    /// Stores a picture that is already the tap's size: RGBA, `width * height * 4` bytes.
    pub fn publish_rgba(&self, rgba: &[u8]) {
        let pixels = self.width as usize * self.height as usize;
        let mut rgb = Vec::with_capacity(pixels * 3);
        for px in rgba[..pixels * 4].chunks_exact(4) {
            rgb.extend_from_slice(&px[..3]);
        }
        self.store(rgb);
    }

    /// Stores a bigger RGBA picture (tightly packed) scaled down to the tap's size by averaging.
    pub fn publish_downscaled(&self, rgba: &[u8], src_w: usize, src_h: usize) {
        self.store(box_downscale(
            rgba,
            src_w,
            src_h,
            self.width as usize,
            self.height as usize,
        ));
    }

    fn store(&self, rgb: Vec<u8>) {
        let seq = self.seq.fetch_add(1, Ordering::AcqRel) + 1;
        let frame = Arc::new(TapFrame {
            seq,
            width: self.width,
            height: self.height,
            rgb,
            captured: Instant::now(),
        });
        *self.latest.lock().expect("tap lock") = Some(frame);
        self.fresh.notify_all();
    }

    /// The newest sequence number, 0 before the first capture.
    pub fn newest(&self) -> u64 {
        self.latest
            .lock()
            .expect("tap lock")
            .as_ref()
            .map_or(0, |f| f.seq)
    }

    /// Waits for a frame newer than `after`, up to `wait`; then the newest frame
    /// there is (which is `after`'s own if the screen made no new ones), or none
    /// before the first capture.
    pub fn wait_newer(&self, after: u64, wait: Duration) -> Option<Arc<TapFrame>> {
        let guard = self.latest.lock().expect("tap lock");
        let (guard, _) = self
            .fresh
            .wait_timeout_while(guard, wait, |l| l.as_ref().is_none_or(|f| f.seq <= after))
            .expect("tap lock");
        guard.clone()
    }
}

/// Averages `src` (RGBA, tightly packed) down to `w` by `h`, as RGB.
fn box_downscale(src: &[u8], src_w: usize, src_h: usize, w: usize, h: usize) -> Vec<u8> {
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        let y0 = y * src_h / h;
        let y1 = ((y + 1) * src_h / h).clamp(y0 + 1, src_h);
        for x in 0..w {
            let x0 = x * src_w / w;
            let x1 = ((x + 1) * src_w / w).clamp(x0 + 1, src_w);
            let mut sum = [0u32; 3];
            for sy in y0..y1 {
                for px in src[(sy * src_w + x0) * 4..(sy * src_w + x1) * 4].chunks_exact(4) {
                    sum[0] += u32::from(px[0]);
                    sum[1] += u32::from(px[1]);
                    sum[2] += u32::from(px[2]);
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let at = (y * w + x) * 3;
            rgb[at] = (sum[0] / n) as u8;
            rgb[at + 1] = (sum[1] / n) as u8;
            rgb[at + 2] = (sum[2] / n) as u8;
        }
    }
    rgb
}

const PAD_OFF: &str = "pad injection is off (start the streamer with --frame-tap-pad)";

/// Reads a `WIDTHxHEIGHT` size for `--frame-tap-size`.
pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let (w, h) = text
        .split_once('x')
        .ok_or_else(|| "use WIDTHxHEIGHT, for example 640x360".to_string())?;
    let number = |s: &str| {
        s.trim()
            .parse::<u32>()
            .map_err(|_| format!("{s:?} isn't a number"))
    };
    let (w, h) = (number(w)?, number(h)?);
    if !(16..=1920).contains(&w) || !(16..=1080).contains(&h) {
        return Err("the size must be between 16x16 and 1920x1080".to_string());
    }
    Ok((w, h))
}

/// Listens on `path` and answers readers until the process ends. With `pad`, the `PAD` command works too.
pub fn serve(tap: Arc<Tap>, path: &Path, pad: Option<Arc<PadSink>>) -> Result<()> {
    // A socket left by a run that was killed would make the bind fail.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)
        .with_context(|| format!("binding the frame tap at {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", path.display()))?;
    info!(socket = %path.display(), width = tap.width, height = tap.height, fps = tap.fps, "frame tap ready");
    if let Some(pad) = &pad {
        info!(
            index = pad.index,
            "frame tap: PAD commands drive this virtual gamepad"
        );
        let pad = Arc::clone(pad);
        std::thread::Builder::new()
            .name("frame-tap-pad".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(WATCHDOG);
                    pad.expire();
                }
            })
            .context("starting the frame tap's pad watchdog")?;
    }
    let clients = Arc::new(AtomicUsize::new(0));
    std::thread::Builder::new()
        .name("frame-tap".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(s) => s,
                    Err(err) => {
                        warn!("frame tap accept: {err}");
                        continue;
                    }
                };
                if clients.fetch_add(1, Ordering::AcqRel) >= MAX_CLIENTS {
                    clients.fetch_sub(1, Ordering::AcqRel);
                    continue; // dropping the stream closes it
                }
                let (tap, clients, pad) = (Arc::clone(&tap), Arc::clone(&clients), pad.clone());
                let spawned = std::thread::Builder::new()
                    .name("frame-tap-client".into())
                    .spawn(move || {
                        if let Err(err) = client(&tap, pad.as_deref(), stream) {
                            debug!("frame tap client: {err}");
                        }
                        clients.fetch_sub(1, Ordering::AcqRel);
                    });
                if let Err(err) = spawned {
                    warn!("frame tap client thread: {err}");
                }
            }
        })
        .context("starting the frame tap thread")?;
    Ok(())
}

fn client(tap: &Tap, pad: Option<&PadSink>, stream: UnixStream) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(300)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    converse(tap, pad, &mut reader, &mut writer)
}

/// One connection's requests and answers; apart from the sockets, what the tests run.
fn converse<R: BufRead, W: Write>(
    tap: &Tap,
    pad: Option<&PadSink>,
    input: &mut R,
    out: &mut W,
) -> io::Result<()> {
    // A connection that set the pad lets go of it when it ends, however it ends.
    struct Release<'a>(Option<&'a PadSink>, bool);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            if let (Some(pad), true) = (self.0, self.1) {
                pad.release();
            }
        }
    }
    let mut held = Release(pad, false);
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let mut words = line.split_whitespace();
        match words.next() {
            None => continue,
            Some("QUIT") => return Ok(()),
            Some("PAD") => match pad {
                None => writeln!(out, "ERR {PAD_OFF}")?,
                Some(pad) => {
                    let json = line.trim().strip_prefix("PAD").unwrap_or("").trim();
                    match pad.set(json) {
                        Ok(()) => {
                            held.1 = true;
                            writeln!(out, "OK")?;
                        }
                        Err(why) => writeln!(out, "ERR {why}")?,
                    }
                }
            },
            Some("PADOFF") => match pad {
                None => writeln!(out, "ERR {PAD_OFF}")?,
                Some(pad) => {
                    pad.release();
                    writeln!(out, "OK")?;
                }
            },
            Some("INFO") => writeln!(
                out,
                "{{\"width\":{},\"height\":{},\"fps\":{},\"seq\":{}}}",
                tap.width,
                tap.height,
                tap.fps,
                tap.newest()
            )?,
            Some("FRAME") => {
                let after = match words.next() {
                    None => tap.newest(),
                    Some(word) => match word.parse::<u64>() {
                        Ok(n) => n,
                        Err(_) => {
                            writeln!(out, "ERR FRAME wants a sequence number")?;
                            out.flush()?;
                            continue;
                        }
                    },
                };
                tap.touch();
                match tap.wait_newer(after, MAX_WAIT) {
                    Some(f) => {
                        writeln!(
                            out,
                            "OK {} {} {} {} {}",
                            f.seq,
                            f.width,
                            f.height,
                            f.rgb.len(),
                            f.captured.elapsed().as_millis()
                        )?;
                        out.write_all(&f.rgb)?;
                    }
                    None => writeln!(out, "NONE")?,
                }
            }
            Some(other) => writeln!(out, "ERR unknown command {other}")?,
        }
        out.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tap() -> Arc<Tap> {
        Tap::with_keep_alive(4, 2, 15, Duration::from_millis(60))
    }

    fn rgba(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&f(x, y));
            }
        }
        out
    }

    fn ask(tap: &Tap, request: &str) -> Vec<u8> {
        ask_with(tap, None, request)
    }

    fn ask_with(tap: &Tap, pad: Option<&PadSink>, request: &str) -> Vec<u8> {
        let mut out = Vec::new();
        converse(
            tap,
            pad,
            &mut Cursor::new(request.as_bytes().to_vec()),
            &mut out,
        )
        .expect("conversation");
        out
    }

    /// A pad target that remembers what it was told.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<PadState>>);

    impl PadTarget for Recorder {
        fn update(&self, state: &PadState) {
            self.0.lock().expect("recorder").push(state.clone());
        }
    }

    fn pad_sink(index: usize) -> (Arc<Recorder>, Arc<PadSink>) {
        let recorder = Arc::new(Recorder::default());
        let sink = PadSink::new(Arc::clone(&recorder) as Arc<dyn PadTarget>, index);
        (recorder, sink)
    }

    fn text(out: Vec<u8>) -> String {
        String::from_utf8(out).expect("text")
    }

    #[test]
    fn pad_commands_are_refused_unless_the_streamer_was_started_with_them() {
        let tap = tap();
        let out = text(ask(&tap, "PAD {\"b\":[1]}\nPADOFF\n"));
        let lines: Vec<&str> = out.lines().collect();
        assert!(
            lines[0].starts_with("ERR pad injection is off"),
            "{}",
            lines[0]
        );
        assert!(
            lines[1].starts_with("ERR pad injection is off"),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn a_pad_state_goes_to_the_sinks_index_whatever_it_says() {
        let tap = tap();
        let (recorder, sink) = pad_sink(2);
        let out = text(ask_with(
            &tap,
            Some(&sink),
            "PAD {\"i\":0,\"gone\":true,\"b\":[1,0,0.5],\"a\":[0.25,0,0,-1],\"ty\":\"xbox\"}\n",
        ));
        assert_eq!(out, "OK\n");
        let states = recorder.0.lock().expect("recorder");
        // The state, then the release when the connection ended.
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].i, 2, "the sink's index, not the message's");
        assert!(!states[0].gone, "a message can't release the pad itself");
        assert_eq!(states[0].b, vec![1.0, 0.0, 0.5]);
        assert_eq!(states[0].a, vec![0.25, 0.0, 0.0, -1.0]);
        assert!(states[1].gone && states[1].i == 2);
    }

    #[test]
    fn what_isnt_a_pad_state_is_refused_and_changes_nothing() {
        let tap = tap();
        let (recorder, sink) = pad_sink(0);
        let too_many = format!("PAD {{\"b\":[{}]}}\n", vec!["0"; 33].join(","));
        let request = format!("PAD nonsense\nPAD [1,2]\nPAD {{\"b\":\"yes\"}}\n{too_many}");
        let out = text(ask_with(&tap, Some(&sink), &request));
        assert_eq!(out.lines().count(), 4);
        assert!(out.lines().all(|l| l.starts_with("ERR ")), "{out}");
        assert!(recorder.0.lock().expect("recorder").is_empty());
    }

    #[test]
    fn a_state_stands_for_its_hold_and_then_the_watchdog_lets_go() {
        let (recorder, sink) = pad_sink(1);
        sink.set("{\"b\":[1],\"hold_ms\":40}").expect("set");
        sink.expire();
        assert_eq!(
            recorder.0.lock().expect("recorder").len(),
            1,
            "still inside the hold"
        );
        std::thread::sleep(Duration::from_millis(60));
        sink.expire();
        let states = recorder.0.lock().expect("recorder");
        assert_eq!(states.len(), 2);
        assert!(states[1].gone && states[1].i == 1);
        drop(states);
        sink.expire(); // nothing more to let go of
        assert_eq!(recorder.0.lock().expect("recorder").len(), 2);
    }

    #[test]
    fn a_newer_state_pushes_the_hold_back() {
        let (recorder, sink) = pad_sink(0);
        sink.set("{\"b\":[1],\"hold_ms\":80}").expect("set");
        std::thread::sleep(Duration::from_millis(50));
        sink.set("{\"b\":[1],\"hold_ms\":80}").expect("set");
        std::thread::sleep(Duration::from_millis(50));
        sink.expire();
        assert_eq!(
            recorder.0.lock().expect("recorder").len(),
            2,
            "two states, not yet let go"
        );
    }

    #[test]
    fn a_hold_is_capped() {
        let (_recorder, sink) = pad_sink(0);
        sink.set("{\"b\":[1],\"hold_ms\":99999999}").expect("set");
        let at = sink.deadline.lock().expect("pad lock").expect("a deadline");
        assert!(at <= Instant::now() + MAX_HOLD + Duration::from_millis(20));
    }

    #[test]
    fn padoff_lets_go_at_once_and_only_once() {
        let tap = tap();
        let (recorder, sink) = pad_sink(3);
        let out = text(ask_with(
            &tap,
            Some(&sink),
            "PAD {\"b\":[1]}\nPADOFF\nPADOFF\n",
        ));
        assert_eq!(out, "OK\nOK\nOK\n");
        let states = recorder.0.lock().expect("recorder");
        assert_eq!(
            states.len(),
            2,
            "the state, one release; the second PADOFF and the close add none"
        );
        assert!(states[1].gone);
    }

    #[test]
    fn sizes_are_read_and_bounded() {
        assert_eq!(parse_size("640x360"), Ok((640, 360)));
        assert_eq!(parse_size(" 512 x 288 "), Ok((512, 288)));
        assert!(parse_size("640").is_err());
        assert!(parse_size("axb").is_err());
        assert!(parse_size("8x8").is_err());
        assert!(parse_size("4000x2000").is_err());
    }

    #[test]
    fn nothing_is_wanted_until_asked_then_for_a_while() {
        let tap = tap();
        assert!(!tap.wanted());
        tap.touch();
        assert!(tap.wanted());
        std::thread::sleep(Duration::from_millis(90));
        assert!(
            !tap.wanted(),
            "an idle tap stops asking the compositor for frames"
        );
    }

    #[test]
    fn the_first_request_after_an_idle_spell_asks_for_a_fresh_frame_once() {
        let tap = tap();
        tap.touch();
        assert!(tap.take_stale());
        assert!(!tap.take_stale(), "once per spell");
        tap.touch(); // still inside the spell
        assert!(!tap.take_stale());
        std::thread::sleep(Duration::from_millis(90));
        tap.touch(); // a new spell
        assert!(tap.take_stale());
    }

    #[test]
    fn rgba_becomes_rgb_and_gets_a_sequence_number() {
        let tap = tap();
        assert_eq!(tap.newest(), 0);
        tap.publish_rgba(&rgba(4, 2, |x, y| [x as u8, y as u8, 9, 255]));
        let f = tap
            .wait_newer(0, Duration::from_millis(10))
            .expect("a frame");
        assert_eq!((f.seq, f.width, f.height), (1, 4, 2));
        assert_eq!(f.rgb.len(), 4 * 2 * 3);
        assert_eq!(
            &f.rgb[..6],
            &[0, 0, 9, 1, 0, 9],
            "alpha is dropped, rows run top first"
        );
        assert_eq!(&f.rgb[(4 + 3) * 3..(4 + 3) * 3 + 3], &[3, 1, 9]);
    }

    #[test]
    fn a_wait_ends_when_a_newer_frame_arrives_and_otherwise_gives_the_newest() {
        let tap = tap();
        tap.publish_rgba(&rgba(4, 2, |_, _| [1, 1, 1, 255]));
        // Nothing newer than 1: the wait runs out and the newest comes back.
        let started = Instant::now();
        let f = tap
            .wait_newer(1, Duration::from_millis(40))
            .expect("the newest");
        assert_eq!(f.seq, 1);
        assert!(started.elapsed() >= Duration::from_millis(35));
        // A frame published during the wait ends it early.
        let publisher = {
            let tap = Arc::clone(&tap);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                tap.publish_rgba(&rgba(4, 2, |_, _| [2, 2, 2, 255]));
            })
        };
        let started = Instant::now();
        let f = tap
            .wait_newer(1, Duration::from_secs(2))
            .expect("a newer one");
        publisher.join().expect("publisher");
        assert_eq!(f.seq, 2);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn averaging_down_keeps_the_picture_and_its_orientation() {
        // 4x4: the top half red, the bottom half blue, scaled to 2x2.
        let src = rgba(4, 4, |_, y| {
            if y < 2 {
                [200, 0, 0, 255]
            } else {
                [0, 0, 100, 255]
            }
        });
        let out = box_downscale(&src, 4, 4, 2, 2);
        assert_eq!(out, [200, 0, 0, 200, 0, 0, 0, 0, 100, 0, 0, 100]);
        // A 2x2 average of four different pixels.
        let src = rgba(2, 2, |x, y| [(x * 100 + y * 50) as u8, 0, 0, 255]);
        assert_eq!(box_downscale(&src, 2, 2, 1, 1), [75, 0, 0]);
    }

    #[test]
    fn frame_replies_with_a_header_and_exactly_the_bytes_it_names() {
        let tap = tap();
        tap.publish_rgba(&rgba(4, 2, |x, _| [x as u8, 0, 0, 255]));
        let out = ask(&tap, "FRAME 0\n");
        let header_end = out.iter().position(|&b| b == b'\n').expect("a header line");
        let header = String::from_utf8(out[..header_end].to_vec()).expect("text");
        let words: Vec<&str> = header.split(' ').collect();
        assert_eq!(&words[..5], ["OK", "1", "4", "2", "24"]);
        assert_eq!(
            out.len() - header_end - 1,
            24,
            "the picture follows, and nothing else"
        );
    }

    #[test]
    fn a_request_before_any_frame_says_none_and_asks_for_one() {
        let tap = Tap::with_keep_alive(4, 2, 15, Duration::from_millis(500));
        // With nothing captured the wait runs its course (shortened here by publishing nothing).
        let started = Instant::now();
        let waiter = {
            let tap = Arc::clone(&tap);
            std::thread::spawn(move || ask(&tap, "FRAME\nINFO\n"))
        };
        std::thread::sleep(Duration::from_millis(50));
        assert!(tap.wanted(), "asking is what turns the capture on");
        assert!(tap.take_stale());
        tap.publish_rgba(&rgba(4, 2, |_, _| [5, 5, 5, 255]));
        let out = waiter.join().expect("reader");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(out.starts_with(b"OK 1 4 2 24 "));
        assert!(out.ends_with(b"{\"width\":4,\"height\":2,\"fps\":15,\"seq\":1}\n"));
    }

    #[test]
    fn unknown_words_and_bad_numbers_get_an_error_and_the_connection_goes_on() {
        let tap = tap();
        let out = ask(&tap, "HELLO\nFRAME soon\nINFO\nQUIT\nINFO\n");
        let text = String::from_utf8(out).expect("text");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "ERR unknown command HELLO");
        assert_eq!(lines[1], "ERR FRAME wants a sequence number");
        assert!(lines[2].starts_with("{\"width\":4"));
        assert_eq!(lines.len(), 3, "nothing after QUIT");
    }

    #[test]
    fn the_socket_serves_a_frame_to_a_real_client() {
        let dir = std::env::temp_dir().join(format!("cha-tap-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("tap.sock");
        let tap = Tap::with_keep_alive(4, 2, 15, Duration::from_millis(500));
        tap.publish_rgba(&rgba(4, 2, |_, _| [7, 8, 9, 255]));
        serve(Arc::clone(&tap), &path, None).expect("serve");
        let mode = std::fs::metadata(&path)
            .expect("socket")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "only the streamer's user may read the screen");

        let mut stream = UnixStream::connect(&path).expect("connect");
        stream.write_all(b"FRAME 0\n").expect("write");
        let mut reader = BufReader::new(stream);
        let mut header = String::new();
        reader.read_line(&mut header).expect("header");
        assert!(header.starts_with("OK 1 4 2 24 "), "{header}");
        let mut body = [0u8; 24];
        io::Read::read_exact(&mut reader, &mut body).expect("body");
        assert_eq!(&body[..3], &[7, 8, 9]);
        std::fs::remove_dir_all(&dir).ok();
    }
}

//! One `cha-stream/1` session on WebTransport: the task that owns the
//! connection, the control stream and the receive logic, and the handles the
//! player gets ([`cha_client::Session`]).
//!
//! The task is one `select!` loop: datagrams into [`Receiver`], control lines
//! in, 10 ms ticks (loss, rumble expiry), 100 ms ticks (the rate-control
//! report, the silence watchdog) and 1 s ticks (ping). Writes to the control
//! stream go through a writer task so a slow stream never holds up datagrams.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use cha_client::{
    AudioPacket, BoxFuture, Codec, Ended, Feedback, Input, Session, SessionControl, VideoFrame,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, interval_at};
use tracing::{debug, info, warn};
use wtransport::error::ConnectionError;
use wtransport::{RecvStream, SendStream};

use crate::control::{self, Hello, LineBuf, ServerMsg};
use crate::input::{InputMapper, PADS};
use crate::net::{self, Link};
use crate::receiver::{Numbering, Receiver};
use crate::reconnect::{Reacquired, Refresh, RefreshFn, reacquire};
use crate::video::Ask;

/// Frames the player may be behind by before the stream skips to a keyframe.
const VIDEO_BACKLOG: usize = 32;
const AUDIO_BACKLOG: usize = 128;
const FEEDBACK_BACKLOG: usize = 64;
/// From the control stream opening to `hello` and the first sizing, at most.
const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// After `hello`, how long to wait for `floor` before going on without it.
const FLOOR_WAIT: Duration = Duration::from_secs(1);
/// How long a `resize` has to be answered before the session starts anyway.
const RESIZE_WAIT: Duration = Duration::from_secs(2);
/// How long a stopping session lets its last lines (key-ups) go out.
const FLUSH: Duration = Duration::from_millis(300);
/// The streamer's Opus: 48 kHz stereo, 10 ms packets.
const SAMPLE_RATE: u32 = 48_000;
const PACKET_SAMPLES: u32 = 480;

/// Where to connect: the portal's `/connect` answer.
#[derive(Clone, Debug)]
pub struct Target {
    /// The streamer's WebTransport URLs, one per address, token included.
    pub urls: Vec<String>,
    /// The streamer certificate's SHA-256, 64 hex characters.
    pub cert_hash: String,
    /// The codec that was asked for (the streamer's `hello` has the last word).
    pub codec: Codec,
}

pub(crate) enum Command {
    Stop,
}

/// What goes to the control stream.
enum Out {
    Line(String),
    /// Answered once everything before it has been written.
    Barrier(oneshot::Sender<()>),
}

/// What the task tells `connect` when the stream is ready to play.
struct Ready {
    codec: Codec,
    width: u32,
    height: u32,
}

struct Control {
    out: mpsc::UnboundedSender<Out>,
    input: Arc<Mutex<InputMapper>>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Control {
    fn say(&self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            let _ = self.out.send(Out::Line(line));
        }
    }
}

impl SessionControl for Control {
    fn input(&self, input: Input) {
        let lines = self.input.lock().expect("input lock").map(&input);
        self.say(lines);
    }

    fn release_all(&self) {
        let lines = self.input.lock().expect("input lock").release_all();
        self.say(lines);
    }

    fn request_keyframe(&self) {
        self.say([control::keyframe()]);
    }

    fn stop(&self, _quit_app: bool) {
        // Quitting the environment is the portal transport's job: this only
        // ends the stream.
        self.release_all();
        let _ = self.commands.send(Command::Stop);
    }
}

/// Connects to a streamer and returns the running session once it has said
/// `hello` (and, when this session controls, once it has sized the picture
/// to `width` x `height` or been told it can't).
///
/// `width` and `height` are what the player would like; the streamer rounds
/// them down to a multiple of 8 within 320x240 .. 3840x2160 and the returned
/// session says what it made of them.
///
/// The session ends when the connection does; see [`connect_with`] for one
/// that connects again.
pub async fn connect(target: &Target, width: u32, height: u32) -> Result<Session> {
    connect_at(target, width, height, None).await
}

/// [`connect`], also asking for `fps` (60, 90 or 120) once this session has
/// the controls, as the browser's frame-rate menu does. The environment
/// starts at its own rate; the streamer answers whether it changed.
pub async fn connect_at(
    target: &Target,
    width: u32,
    height: u32,
    fps: Option<u32>,
) -> Result<Session> {
    start(target, width, height, fps, None).await
}

/// [`connect_at`] for a session that heals itself. When the connection ends
/// for any reason other than the player's `stop()` or the streamer closing on
/// purpose (see [`crate::reconnect`] and the end reasons below), the session
/// connects again, inside the same [`Session`]: `video`, `audio`, `feedback`
/// and `control` stay valid and the player sees a pause, announced by
/// [`Feedback::Reconnecting`] and ended by [`Feedback::Reconnected`].
///
/// `refresh` returns a fresh [`Target`] (a new media token from the portal's
/// `/connect`, valid 60 s and checked once, at the start of a session) and is
/// called before every attempt, after a wait of 1, 2, 4, 8, then 15 s, as the
/// browser backs off. [`Refresh::Retry`] tries again later;
/// [`Refresh::GiveUp`] ends the session with [`Ended::Failed`].
///
/// What ends the session for good: the player's `stop()`, close code 1
/// (another session of this user took the seat), code 2 (the encoder
/// stopped, e.g. the app quit), `refresh` giving up. Everything else
/// (the 4 s silence watchdog, the QUIC idle timeout, a reset, a broken control
/// stream, the streamer's other closes) reconnects.
///
/// Each reconnect is a new streamer session: frame numbers go on increasing,
/// but the receiver, clock sync, report window and watchdog start over, the
/// cursor mode, the size and frame rate asks go out again, the pads' states
/// are sent when the floor comes, and keys and buttons held at the drop are
/// let go. Video resumes at a keyframe (an INTRA frame for PyroWave).
pub async fn connect_with(
    first: Target,
    width: u32,
    height: u32,
    fps: Option<u32>,
    refresh: impl Fn() -> BoxFuture<'static, Result<Target, Refresh>> + Send + Sync + 'static,
) -> Result<Session> {
    start(&first, width, height, fps, Some(Arc::new(refresh))).await
}

/// A connection made but not yet started: the link, the control stream with
/// its first `ping` already written, and a fresh receiver.
struct Opened {
    link: Link,
    send: SendStream,
    recv: RecvStream,
    rx: Receiver,
}

async fn open(target: &Target) -> Result<Opened> {
    let hash = net::parse_hash(&target.cert_hash)?;
    let link = net::race(&target.urls, hash, net::CONNECT_TIMEOUT).await?;
    let began = Instant::now();
    let rx = Receiver::new(began);
    let opened = async {
        let (mut send, recv) = link
            .connection
            .open_bi()
            .await
            .map_err(|e| anyhow!("opening the control stream: {e}"))?
            .await
            .map_err(|e| anyhow!("opening the control stream: {e}"))?;
        // The streamer closes a session that says nothing in 5 s, and doesn't
        // see the stream before this first write.
        let first = format!("{}\n", control::ping(rx.ping_value(Instant::now())));
        send.write_all(first.as_bytes())
            .await
            .map_err(|e| anyhow!("writing to the control stream: {e}"))?;
        anyhow::Ok((send, recv))
    }
    .await;
    match opened {
        Ok((send, recv)) => Ok(Opened {
            link,
            send,
            recv,
            rx,
        }),
        Err(e) => {
            link.connection.close(0u32.into(), b"no control stream");
            Err(e)
        }
    }
}

/// [`open`], but a streamer that connects and then says nothing doesn't hold
/// a reconnect for ever.
async fn open_within(target: Target) -> Result<Opened> {
    tokio::time::timeout(READY_TIMEOUT, open(&target))
        .await
        .map_err(|_| anyhow!("the streamer didn't answer in {READY_TIMEOUT:?}"))?
}

async fn start(
    target: &Target,
    width: u32,
    height: u32,
    fps: Option<u32>,
    refresh: Option<RefreshFn>,
) -> Result<Session> {
    let Opened {
        link,
        send,
        recv,
        rx,
    } = open(target).await?;
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (video_tx, video) = mpsc::channel(VIDEO_BACKLOG);
    let (audio_tx, audio) = mpsc::channel(AUDIO_BACKLOG);
    let (feedback_tx, feedback) = mpsc::channel(FEEDBACK_BACKLOG);
    let (ended_tx, ended) = oneshot::channel();
    let (ready_tx, ready_rx) = oneshot::channel();
    let input = Arc::new(Mutex::new(InputMapper::new()));
    // Dropping this (a failed connect) stops the task.
    let control = Control {
        out: out_tx.clone(),
        input: input.clone(),
        commands: commands_tx,
    };
    // We don't draw the cursor: the picture has it.
    control.say([control::cursor(false)]);

    let writer = Writer::spawn(send, out_rx);
    let task = Task {
        out: out_tx,
        rx,
        parser: LineBuf::default(),
        video: video_tx,
        audio: audio_tx,
        feedback: feedback_tx,
        input,
        numbering: Numbering::default(),
        codec: target.codec,
        want: (width, height),
        want_fps: fps.filter(|f| control::FPS_CHOICES.contains(f)),
        ready: Some(ready_tx),
        resuming: None,
        fatal: None,
        attempts: 0,
        hello: None,
        hello_at: None,
        got_floor: false,
        resize_deadline: None,
        last_resize: None,
        size: (0, 0),
        rumble_until: [None; PADS],
        stats: Stats::default(),
    };
    tokio::spawn(task.supervise(Wire { link, recv, writer }, commands_rx, ended_tx, refresh));

    let ready = match tokio::time::timeout(READY_TIMEOUT, ready_rx).await {
        Ok(Ok(Ok(ready))) => ready,
        Ok(Ok(Err(why))) => bail!("{why}"),
        Ok(Err(_)) => bail!("the stream ended before it started"),
        Err(_) => bail!("the streamer didn't start the stream in {READY_TIMEOUT:?}"),
    };
    info!(
        codec = ?ready.codec,
        width = ready.width,
        height = ready.height,
        "cha-stream/1 session up"
    );
    Ok(Session {
        video,
        audio,
        feedback,
        ended,
        codec: ready.codec,
        width: ready.width,
        height: ready.height,
        control: Box::new(control),
    })
}

/// The task that writes lines to the control stream, so a slow stream never
/// holds up datagrams. It hands the queue back when it is done, so the
/// next connection goes on with the same channel the player writes to.
struct Writer {
    stop: oneshot::Sender<()>,
    task: JoinHandle<mpsc::UnboundedReceiver<Out>>,
}

impl Writer {
    fn spawn(send: SendStream, lines: mpsc::UnboundedReceiver<Out>) -> Self {
        let (stop, stopped) = oneshot::channel();
        Self {
            stop,
            task: tokio::spawn(write_lines(send, lines, stopped)),
        }
    }

    /// Stops writing (what is queued and not yet written stays queued) and
    /// returns the queue.
    async fn finish(self) -> Option<mpsc::UnboundedReceiver<Out>> {
        let _ = self.stop.send(());
        self.task.await.ok()
    }
}

/// Writes lines to the control stream, several to a write when they queue up,
/// until `stopped` fires or a write fails.
async fn write_lines(
    mut send: SendStream,
    mut lines: mpsc::UnboundedReceiver<Out>,
    mut stopped: oneshot::Receiver<()>,
) -> mpsc::UnboundedReceiver<Out> {
    let mut buf = Vec::new();
    'outer: loop {
        let first = tokio::select! {
            _ = &mut stopped => break,
            first = lines.recv() => match first {
                Some(first) => first,
                None => break,
            },
        };
        let mut barriers = Vec::new();
        let mut item = Some(first);
        while let Some(out) = item.take() {
            match out {
                Out::Line(line) => {
                    buf.extend_from_slice(line.as_bytes());
                    buf.push(b'\n');
                }
                Out::Barrier(done) => barriers.push(done),
            }
            if buf.len() < 64 * 1024 {
                item = lines.try_recv().ok();
            }
        }
        if !buf.is_empty() {
            let written = tokio::select! {
                _ = &mut stopped => break 'outer,
                written = send.write_all(&buf) => written,
            };
            if let Err(e) = written {
                debug!("control stream write: {e}");
                break 'outer;
            }
            buf.clear();
        }
        for done in barriers {
            let _ = done.send(());
        }
    }
    let _ = tokio::time::timeout(Duration::from_millis(200), send.finish()).await;
    lines
}

#[derive(Default)]
struct Stats {
    frames: u64,
    lost: u64,
    recovered: u64,
    partial: u64,
    dropped_for_player: u64,
    /// The longest time between two datagrams reaching the receiver.
    max_datagram_gap: Duration,
    last_datagram: Option<Instant>,
}

/// A connection's end: what it means for the player, and whether a session
/// that can reconnect should.
struct End {
    ended: Ended,
    retry: bool,
    /// The connection itself is closed, so nothing more can be said on it.
    gone: bool,
}

impl End {
    fn over(ended: Ended) -> Self {
        Self {
            ended,
            retry: false,
            gone: false,
        }
    }

    fn drop(ended: Ended) -> Self {
        Self {
            ended,
            retry: true,
            gone: false,
        }
    }

    fn gone(mut self) -> Self {
        self.gone = true;
        self
    }

    fn reason(&self) -> String {
        match &self.ended {
            Ended::Stopped => "stopped".to_string(),
            Ended::ByHost(m) | Ended::Failed(m) => m.clone(),
        }
    }
}

/// One connection's pieces that the task takes apart at its end.
struct Wire {
    link: Link,
    recv: RecvStream,
    writer: Writer,
}

struct Task {
    out: mpsc::UnboundedSender<Out>,
    rx: Receiver,
    parser: LineBuf,
    video: mpsc::Sender<VideoFrame>,
    audio: mpsc::Sender<AudioPacket>,
    feedback: mpsc::Sender<Feedback>,
    input: Arc<Mutex<InputMapper>>,
    numbering: Numbering,
    codec: Codec,
    /// The size the player asked for.
    want: (u32, u32),
    /// The frame rate the player asked for, if the streamer offers it.
    want_fps: Option<u32>,
    ready: Option<oneshot::Sender<Result<Ready, String>>>,
    /// A reconnected connection that hasn't started yet, and when to give up
    /// on it.
    resuming: Option<Instant>,
    /// Something that ends the session for good, found by a reconnected
    /// connection (nobody is waiting on `ready` then).
    fatal: Option<String>,
    /// Reconnect attempts since the stream was last up.
    attempts: u32,
    hello: Option<Hello>,
    hello_at: Option<Instant>,
    got_floor: bool,
    resize_deadline: Option<Instant>,
    /// What we last asked the streamer to size the picture to.
    last_resize: Option<(u32, u32)>,
    /// The size the streamer says the picture has.
    size: (u32, u32),
    rumble_until: [Option<Instant>; PADS],
    stats: Stats,
}

impl Task {
    /// Owns the session: runs a connection, and when it drops for a reason
    /// that deserves it, connects again.
    async fn supervise(
        mut self,
        first: Wire,
        mut commands: mpsc::UnboundedReceiver<Command>,
        ended: oneshot::Sender<Ended>,
        refresh: Option<RefreshFn>,
    ) {
        let mut wire = first;
        let reason = loop {
            let end = self.run(&wire.link, &mut wire.recv, &mut commands).await;
            if let Some(ready) = self.ready.take() {
                let _ = ready.send(Err(match &end.ended {
                    Ended::Stopped => "the session was stopped".to_string(),
                    Ended::ByHost(m) | Ended::Failed(m) => m.clone(),
                }));
            }
            info!(
                frames = self.stats.frames,
                lost = self.stats.lost,
                recovered = self.stats.recovered,
                partial = self.stats.partial,
                dropped_for_the_player = self.stats.dropped_for_player,
                bad_datagrams = self.rx.dropped(),
                max_datagram_gap_ms = self.stats.max_datagram_gap.as_secs_f64() * 1000.0,
                reason = ?end.ended,
                reconnecting = end.retry && refresh.is_some(),
                "cha-stream/1 session over"
            );
            let Wire { link, writer, .. } = wire;
            let conn = link.connection.clone();
            let reconnect = end.retry && refresh.is_some();
            if end.ended == Ended::Stopped {
                // Let the last key-ups out before the connection goes.
                flush(&self.out, FLUSH).await;
            } else if reconnect && !end.gone {
                // The connection looks alive (the watchdog fired, say): tell
                // the streamer what is no longer held, if it can still hear.
                let lines = self.input.lock().expect("input lock").drop_floor();
                self.say_all(lines);
                flush(&self.out, Duration::from_millis(100)).await;
            }
            let queue = writer.finish().await;
            conn.close(0u32.into(), b"bye");
            if !reconnect {
                let _ = tokio::time::timeout(Duration::from_millis(500), link.endpoint.wait_idle())
                    .await;
                break end.ended;
            }
            // The old endpoint winds down on its own.
            tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_millis(500), link.endpoint.wait_idle())
                    .await;
            });
            let Some(mut queue) = queue else {
                break Ended::Failed("the control stream's writer failed".into());
            };
            let refresh = refresh.as_ref().expect("checked above");

            self.on_drop();
            let mut attempts = self.attempts;
            let feedback = self.feedback.clone();
            let up = reacquire(
                refresh,
                open_within,
                &mut commands,
                end.reason(),
                &mut attempts,
                |attempt, delay, why| {
                    info!(
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        reason = %why,
                        "the stream dropped; reconnecting"
                    );
                    let _ = feedback.try_send(Feedback::Reconnecting {
                        attempt,
                        reason: why.to_string(),
                    });
                },
            )
            .await;
            self.attempts = attempts;
            match up {
                Reacquired::Up(opened) => {
                    // What was asked while the link was down means nothing to
                    // the new session.
                    while queue.try_recv().is_ok() {}
                    let Opened {
                        link,
                        send,
                        recv,
                        rx,
                    } = opened;
                    info!(url = %net::redact(&link.url), "reconnected; waiting for the stream");
                    self.begin(rx);
                    self.say(control::cursor(false));
                    wire = Wire {
                        link,
                        recv,
                        writer: Writer::spawn(send, queue),
                    };
                }
                Reacquired::Stopped => break Ended::Stopped,
                Reacquired::GiveUp(why) => {
                    info!(%why, "not reconnecting");
                    break Ended::Failed(why);
                }
            }
        };
        let _ = ended.send(reason);
    }

    /// The connection is gone and a new one is wanted: the player's view of
    /// what was held and running stops where the old session did.
    fn on_drop(&mut self) {
        // Idempotent; the lines for the old connection went out (or not) already.
        let _ = self.input.lock().expect("input lock").drop_floor();
        for (slot, until) in self.rumble_until.iter_mut().enumerate() {
            if until.take().is_some() {
                let _ = self.feedback.try_send(Feedback::Rumble {
                    index: slot as u8,
                    low: 0.0,
                    high: 0.0,
                });
            }
        }
    }

    /// Starts over for a new streamer session: its frame ids, stream
    /// generation and clock epoch are new, and it knows nothing we told the
    /// last one.
    fn begin(&mut self, rx: Receiver) {
        self.rx = rx;
        self.parser = LineBuf::default();
        self.numbering.restart();
        self.hello = None;
        self.hello_at = None;
        self.got_floor = false;
        self.resize_deadline = None;
        self.last_resize = None;
        self.size = (0, 0);
        self.stats = Stats::default();
        self.fatal = None;
        self.resuming = Some(Instant::now() + READY_TIMEOUT);
        // Nobody awaits this; `finish_ready` announces the return instead.
        self.ready = Some(oneshot::channel().0);
    }

    /// One connection: datagrams into the receiver, control lines in, the
    /// timers, until it ends.
    async fn run(
        &mut self,
        link: &Link,
        recv: &mut RecvStream,
        commands: &mut mpsc::UnboundedReceiver<Command>,
    ) -> End {
        let conn = link.connection.clone();
        let started = tokio::time::Instant::now();
        let mut t10 = interval(Duration::from_millis(10));
        let mut t100 = interval(Duration::from_millis(100));
        let mut t1000 = interval_at(started + Duration::from_secs(1), Duration::from_secs(1));
        for t in [&mut t10, &mut t100, &mut t1000] {
            t.set_missed_tick_behavior(MissedTickBehavior::Skip);
        }
        let closed = conn.closed();
        tokio::pin!(closed);
        let mut buf = vec![0u8; 16 * 1024];

        loop {
            tokio::select! {
                datagram = conn.receive_datagram() => match datagram {
                    Ok(d) => {
                        let now = Instant::now();
                        if let Some(last) = self.stats.last_datagram.replace(now) {
                            let gap = now.saturating_duration_since(last);
                            self.stats.max_datagram_gap = self.stats.max_datagram_gap.max(gap);
                        }
                        self.rx.on_datagram(now, &d.payload());
                        self.drain();
                    }
                    Err(e) => break end_of(&e),
                },
                read = recv.read(&mut buf) => match read {
                    Ok(Some(n)) => {
                        match self.parser.push(&buf[..n]) {
                            Ok(lines) => for line in lines { self.on_line(&line); },
                            Err(why) => break End::drop(Ended::Failed(format!("the streamer's control stream is broken: {why}"))),
                        }
                        self.drain();
                    }
                    // The connection's own reason, if it is closing, beats the stream's.
                    Ok(None) => break closing(&mut closed, End::drop(Ended::ByHost("the streamer closed the control stream".into()))).await,
                    Err(e) => break closing(&mut closed, End::drop(Ended::Failed(format!("the control stream failed: {e}")))).await,
                },
                _ = t10.tick() => {
                    let now = Instant::now();
                    self.rx.tick(now);
                    self.expire_rumble(now);
                    self.drain();
                }
                _ = t100.tick() => {
                    let now = Instant::now();
                    if let Some(why) = self.fatal.take() {
                        break End::over(Ended::Failed(why));
                    }
                    if !self.rx.alive(now) {
                        break End::drop(Ended::Failed("the streamer went silent: nothing came for 4 seconds".into()));
                    }
                    if self.resuming.is_some_and(|deadline| now >= deadline) {
                        break End::drop(Ended::Failed(format!("the streamer didn't start the stream in {READY_TIMEOUT:?}")));
                    }
                    let report = self.rx.report(now).line();
                    self.say(report);
                    self.check_ready(now);
                }
                _ = t1000.tick() => {
                    let c = self.rx.ping_value(Instant::now());
                    self.say(control::ping(c));
                }
                command = commands.recv() => match command {
                    Some(Command::Stop) | None => break End::over(Ended::Stopped),
                },
                e = &mut closed => break end_of(&e),
            }
        }
    }

    fn say_all(&self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            self.say(line);
        }
    }

    fn say(&self, line: String) {
        let _ = self.out.send(Out::Line(line));
    }

    /// Sends what the receiver produced on to the player and the streamer.
    fn drain(&mut self) {
        let out = self.rx.take();
        let now = Instant::now();
        self.stats.lost += u64::from(out.lost);
        self.stats.recovered += u64::from(out.recovered);
        self.stats.partial += u64::from(out.partial);
        if out.lost > 0 {
            debug!(frames = out.lost, "frames given up on");
        }
        let mut behind = false;
        for f in out.frames {
            if behind {
                break;
            }
            self.stats.frames += 1;
            let frame = VideoFrame {
                codec: self.codec,
                data: f.data,
                key: f.key,
                partial: f.partial,
                number: self.numbering.number(f.id),
                received: f.last_at,
            };
            if let Err(mpsc::error::TrySendError::Full(_)) = self.video.try_send(frame) {
                // The player can't keep up: what it already has is a
                // decodable run; start the next from a keyframe.
                self.stats.dropped_for_player += 1;
                behind = true;
            }
        }
        let mut asks = out.asks;
        if behind {
            self.rx.resync(now);
            asks.extend(self.rx.take().asks);
        }
        for ask in asks {
            match ask {
                Ask::Rfi(id) => self.say(control::rfi(id)),
                Ask::Keyframe => self.say(control::keyframe()),
            }
        }
        for a in out.audio {
            // A full channel is a player that isn't playing; Opus conceals a gap.
            let _ = self.audio.try_send(AudioPacket {
                data: a.data,
                channels: 2,
                sample_rate: SAMPLE_RATE,
                samples: PACKET_SAMPLES,
            });
        }
    }

    fn on_line(&mut self, line: &str) {
        let now = Instant::now();
        self.rx.heard(now);
        let Some(msg) = control::parse(line) else {
            debug!("unreadable control line");
            return;
        };
        match msg {
            ServerMsg::Hello(h) => self.on_hello(now, h),
            ServerMsg::Floor { control, viewers } => self.on_floor(now, control, viewers),
            ServerMsg::Pong { c, s_us } => self.rx.on_pong(now, c, s_us),
            ServerMsg::Resized { w, h } => {
                self.size = (w, h);
                debug!(w, h, "the streamer sized the picture");
                if self.resize_deadline.take().is_some() {
                    self.finish_ready(Some((w, h)));
                }
            }
            ServerMsg::Rumble { i, lo, hi, ms } => self.on_rumble(now, i, lo, hi, ms),
            ServerMsg::Led { i, r, g, b } => {
                let _ = self.feedback.try_send(Feedback::Led { index: i, r, g, b });
            }
            ServerMsg::Codec { codec, error } => {
                debug!(%codec, ?error, "codec message (switching isn't asked for)");
            }
            ServerMsg::Fps { fps, error: None } => info!(fps, "the streamer's frame rate"),
            ServerMsg::Fps {
                fps,
                error: Some(error),
            } => warn!(fps, %error, "the streamer kept its frame rate"),
            ServerMsg::Clipboard | ServerMsg::Cursor | ServerMsg::Other(_) => {}
        }
    }

    fn on_hello(&mut self, now: Instant, h: Hello) {
        let Some(codec) = h.codec() else {
            self.fail_ready(format!(
                "the streamer sends {}, which this player can't decode",
                h.codec
            ));
            return;
        };
        if codec != self.codec {
            info!(asked = ?self.codec, got = ?codec, "the streamer's codec differs from the one asked for");
        }
        self.codec = codec;
        self.size = (h.width, h.height);
        debug!(?h, "hello");
        self.hello_at = Some(now);
        self.hello = Some(h);
    }

    fn on_floor(&mut self, now: Instant, control: bool, viewers: u32) {
        debug!(control, viewers, "floor");
        let lines = self.input.lock().expect("input lock").set_floor(control);
        if control {
            // The streamer applies none of this until it's ours.
            self.say(control::cursor(false));
            if self.got_floor
                && let Some((w, h)) = self.last_resize
            {
                self.say(control::resize(w, h));
            }
            for line in lines {
                self.say(line);
            }
        }
        let first = !self.got_floor;
        self.got_floor = true;
        let hello_fps = self.hello.as_ref().map(|h| h.fps);
        if first
            && control
            && let Some(want) = self.want_fps
            && hello_fps.is_some_and(|f| f != want)
        {
            self.say(control::fps(want));
        }
        let hello_size = self.hello.as_ref().map(|h| (h.width, h.height));
        if first && let Some(hello_size) = hello_size {
            let target = control::fit_size(self.want.0, self.want.1);
            if control && target != hello_size {
                self.last_resize = Some(self.want);
                self.say(control::resize(self.want.0, self.want.1));
                self.resize_deadline = Some(now + RESIZE_WAIT);
                return;
            }
        }
        self.check_ready(now);
    }

    fn on_rumble(&mut self, now: Instant, i: u8, lo: f32, hi: f32, ms: u32) {
        let slot = usize::from(i);
        if slot >= PADS {
            return;
        }
        let (low, high) = if ms == 0 { (0.0, 0.0) } else { (lo, hi) };
        // The player's motors run until told otherwise: the duration is ours.
        self.rumble_until[slot] = (ms > 0).then(|| now + Duration::from_millis(u64::from(ms)));
        let _ = self.feedback.try_send(Feedback::Rumble {
            index: i,
            low,
            high,
        });
    }

    fn expire_rumble(&mut self, now: Instant) {
        for (slot, until) in self.rumble_until.iter_mut().enumerate() {
            if until.is_some_and(|t| now >= t) {
                *until = None;
                let _ = self.feedback.try_send(Feedback::Rumble {
                    index: slot as u8,
                    low: 0.0,
                    high: 0.0,
                });
            }
        }
    }

    /// Starts the session when `hello` is in and the sizing is settled (or
    /// has run out of time).
    fn check_ready(&mut self, now: Instant) {
        if self.ready.is_none() {
            return;
        }
        let Some(hello_at) = self.hello_at else {
            return;
        };
        if let Some(deadline) = self.resize_deadline {
            if now >= deadline {
                warn!("the streamer didn't answer the resize; going on with its own size");
                self.resize_deadline = None;
                self.finish_ready(None);
            }
            return;
        }
        // The floor said, or not said in time (then we are not controlling).
        if self.got_floor || now.saturating_duration_since(hello_at) >= FLOOR_WAIT {
            self.finish_ready(None);
        }
    }

    fn finish_ready(&mut self, size: Option<(u32, u32)>) {
        let Some(ready) = self.ready.take() else {
            return;
        };
        let (width, height) = size.unwrap_or(self.size);
        if self.resuming.take().is_some() {
            info!(width, height, "cha-stream/1 session back up");
            self.attempts = 0;
            let _ = self.feedback.try_send(Feedback::Reconnected);
        }
        let _ = ready.send(Ok(Ready {
            codec: self.codec,
            width,
            height,
        }));
    }

    fn fail_ready(&mut self, why: String) {
        if self.resuming.is_some() {
            // Nobody is waiting on a reconnected connection's start.
            self.fatal = Some(why.clone());
        }
        if let Some(ready) = self.ready.take() {
            let _ = ready.send(Err(why));
        }
    }
}

/// Waits a moment for the connection's own close, which says more than the
/// stream's end did; else the stream's end is the answer.
async fn closing(
    closed: &mut (impl std::future::Future<Output = ConnectionError> + Unpin),
    otherwise: End,
) -> End {
    match tokio::time::timeout(Duration::from_millis(300), closed).await {
        Ok(e) => end_of(&e),
        Err(_) => otherwise,
    }
}

/// Lets the lines queued so far out (or gives up after `within`).
async fn flush(out: &mpsc::UnboundedSender<Out>, within: Duration) {
    let (done, flushed) = oneshot::channel();
    if out.send(Out::Barrier(done)).is_ok() {
        let _ = tokio::time::timeout(within, flushed).await;
    }
}

/// What a connection ending means for the player, and for reconnecting.
///
/// The streamer closing on purpose ends the session: code 1 (another session
/// of this user took the seat; reconnecting would take it back and the two
/// would fight) and code 2 (the encoder stopped, as when the app quit). Our
/// own close is the player's stop. Everything else is a drop: a timeout, a
/// reset, a transport error, code 3 (we were too slow with the first line),
/// and the streamer's other closes.
fn end_of(e: &ConnectionError) -> End {
    ended_by(e).gone()
}

fn ended_by(e: &ConnectionError) -> End {
    match e {
        ConnectionError::ApplicationClosed(close) => {
            let reason = String::from_utf8_lossy(close.reason()).to_string();
            match close.code().into_inner() {
                1 => End::over(Ended::ByHost("another session took this seat".into())),
                2 => End::over(Ended::ByHost(if reason.is_empty() {
                    "the streamer's encoder stopped".into()
                } else {
                    format!("the streamer stopped: {reason}")
                })),
                3 => End::drop(Ended::Failed("the streamer saw no control stream".into())),
                _ if reason.is_empty() => {
                    End::drop(Ended::ByHost("the streamer closed the session".into()))
                }
                _ => End::drop(Ended::ByHost(format!(
                    "the streamer closed the session: {reason}"
                ))),
            }
        }
        ConnectionError::LocallyClosed => End::over(Ended::Stopped),
        ConnectionError::TimedOut => End::drop(Ended::Failed(
            "the connection to the streamer timed out (the network dropped, or the streamer died)"
                .into(),
        )),
        other => End::drop(Ended::Failed(format!(
            "the connection to the streamer failed: {other}"
        ))),
    }
}

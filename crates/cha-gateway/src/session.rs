//! One WebRTC viewer: the host's frames out as an RTP video track with
//! playout-delay 0, its Opus packets as an audio track, and the `control`
//! DataChannel (JSON lines) carrying the player's pings, input and keyframe
//! requests in, and pongs, send times, stats and rumble out. The shapes are
//! cha-streamer's, so the player can't tell the two apart.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use cha_moonlight_input::InputState;
use serde::Serialize;
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::format::Codec as RtpCodec;
use str0m::media::{Frequency, MediaKind, MediaTime, Mid};
use str0m::net::{Protocol, Receive};
use str0m::rtp::{AbsCaptureTime, Extension, ExtensionMap};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::sleep_until;
use tracing::{info, warn};

use crate::host::{Codec, HostAudio, HostFrame, HostRumble, Link, StreamInfo};
use crate::hub::{Datagram, Hub, Peer};
use crate::input::BrowserInput;
use crate::viewers::Seat;

const STATS_INTERVAL: Duration = Duration::from_millis(500);
/// How long a peer has, from its offer, to finish ICE and DTLS. An ICE-lite
/// peer that never sends a check isn't reported disconnected, and would hold
/// its seat forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// While waiting for a keyframe, ask the host again this often.
const IDR_RETRY: Duration = Duration::from_secs(1);
/// Give up on a viewer that gets no keyframe for this long.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);
/// A host rumble has no end, only a next packet: replay the motors this
/// often, for a bit longer than that, so a held rumble keeps going and a
/// lost stop packet can't leave it stuck.
const RUMBLE_REPEAT: Duration = Duration::from_millis(500);
const RUMBLE_MS: u32 = 1000;

pub struct SessionParams {
    pub info: StreamInfo,
    pub link: Link,
    pub hub: Arc<Hub>,
    /// Port-forwarded public addresses, announced on the first socket's port.
    pub public: Vec<IpAddr>,
}

/// Starts a viewer's session; it ends (freeing the seat) when the browser
/// leaves or fails to connect.
pub fn start(params: SessionParams, seat: Seat, offer: SdpOffer) -> Result<SdpAnswer> {
    let locals = params.hub.locals.clone();
    let (mut rtc, rtp_codec) = build_rtc(
        params.info.codec,
        params.info.audio,
        &locals,
        &params.public,
    )?;
    let answer = rtc.sdp_api().accept_offer(offer)?;
    // Listening before the answer goes out, so the browser's first check
    // finds this session.
    let peer = params.hub.join(seat.id);
    info!(
        ?locals,
        codec = params.info.codec.name(),
        id = seat.id,
        "webrtc session"
    );
    tokio::spawn(async move {
        if let Err(err) = Session::new(rtc, peer, locals, rtp_codec, params, seat)
            .run()
            .await
        {
            warn!("webrtc session ended: {err:#}");
        }
    });
    Ok(answer)
}

/// A str0m instance for one viewer: ICE-lite, our extensions, the one video
/// codec the stream is, Opus if the host's audio is playable, and the
/// addresses the browser may reach us on.
fn build_rtc(
    codec: Codec,
    audio: bool,
    locals: &[SocketAddr],
    public: &[IpAddr],
) -> Result<(Rtc, RtpCodec)> {
    let mut exts = ExtensionMap::standard();
    exts.set(5, Extension::PlayoutDelay);
    exts.set(6, Extension::AbsoluteCaptureTime);
    let builder = Rtc::builder()
        .set_ice_lite(true)
        .clear_codecs()
        .set_extension_map(exts);
    let (builder, rtp_codec) = match codec {
        Codec::H264 => (builder.enable_h264(true), RtpCodec::H264),
        Codec::Hevc => (builder.enable_h265(true), RtpCodec::H265),
    };
    let builder = builder.enable_opus(audio, false);
    let mut rtc = builder.build(Instant::now());
    for local in locals {
        rtc.add_local_candidate(Candidate::host(*local, "udp")?)
            .context("adding host candidate")?;
    }
    // A port-forward's public address: announced as a host candidate on the
    // first socket's port (ICE-lite takes host candidates only). The
    // browser's checks arrive, NAT-translated, on that socket.
    for ip in public {
        let announced = SocketAddr::new(*ip, locals[0].port());
        rtc.add_local_candidate(Candidate::host(announced, "udp")?)
            .context("adding the public address")?;
    }
    Ok((rtc, rtp_codec))
}

/// Gateway → player, as cha-streamer sends them (the subset that applies).
#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum ServerMsg {
    Hello {
        stream: serde_json::Value,
    },
    Pong {
        c: f64,
        s_us: u64,
    },
    /// Frame `id` went out with RTP timestamp `rtp` at `s_us` (session clock);
    /// `e_us` is when the gateway had it from the host, same clock.
    Sent {
        id: u32,
        rtp: u32,
        s_us: u64,
        e_us: u64,
    },
    Probe {
        id: u64,
        s_us: u64,
    },
    Rumble {
        i: usize,
        lo: f32,
        hi: f32,
        ms: u32,
    },
    Floor {
        control: bool,
        viewers: usize,
    },
    Stats {
        elapsed_ms: u64,
        #[serde(flatten)]
        stats: Stats,
    },
}

#[derive(Clone, Debug, Default, Serialize)]
struct Stats {
    frames_generated: u64,
    frames_sent: u64,
    bytes_sent: u64,
    keyframes: u64,
    /// Browser PLI/FIR or `keyframe` messages, each turned into an IDR request.
    keyframe_requests: u64,
    /// Frames this viewer skipped because it fell behind the host's stream.
    frames_dropped: u64,
    first_frame_ms: Option<u64>,
    /// The stream's frame rate.
    fps: u32,
    inputs: u64,
    inputs_unmapped: u64,
    audio_packets: u64,
    audio_bytes: u64,
}

struct Session {
    rtc: Rtc,
    peer: Peer,
    locals: Vec<SocketAddr>,
    epoch: Instant,
    rtp_codec: RtpCodec,
    params: SessionParams,
    seat: Seat,
    mid: Option<Mid>,
    audio_mid: Option<Mid>,
    control: Option<ChannelId>,
    connected: bool,
    frames: Option<broadcast::Receiver<Arc<HostFrame>>>,
    audio: Option<broadcast::Receiver<Arc<HostAudio>>>,
    rumble: Option<broadcast::Receiver<HostRumble>>,
    /// Frames are skipped until a keyframe (a new viewer, or one that lagged).
    waiting_key: bool,
    subscribed_at: Option<Instant>,
    next_idr_at: Instant,
    /// What the host's rumble is doing now, per pad.
    rumble_on: BTreeMap<usize, (f32, f32)>,
    next_rumble_at: Option<Instant>,
    had_control: bool,
    input: InputState,
    next_stats_at: Instant,
    frame_id: u32,
    stats: Stats,
    /// The host clock's first audio packet and the RTP time it got.
    audio_base: Option<Duration>,
    last_audio_rtp: u64,
}

impl Session {
    fn new(
        rtc: Rtc,
        peer: Peer,
        locals: Vec<SocketAddr>,
        rtp_codec: RtpCodec,
        params: SessionParams,
        seat: Seat,
    ) -> Self {
        let epoch = Instant::now();
        let stats = Stats {
            fps: params.info.fps,
            ..Stats::default()
        };
        Self {
            rtc,
            peer,
            locals,
            epoch,
            rtp_codec,
            params,
            seat,
            mid: None,
            audio_mid: None,
            control: None,
            connected: false,
            frames: None,
            audio: None,
            rumble: None,
            waiting_key: true,
            subscribed_at: None,
            next_idr_at: epoch,
            rumble_on: BTreeMap::new(),
            next_rumble_at: None,
            had_control: false,
            input: InputState::default(),
            next_stats_at: epoch + STATS_INTERVAL,
            frame_id: 0,
            stats,
            audio_base: None,
            last_audio_rtp: 0,
        }
    }

    async fn run(mut self) -> Result<()> {
        loop {
            let now = Instant::now();
            if !self.connected && now >= self.epoch + CONNECT_TIMEOUT {
                bail!("no connection {CONNECT_TIMEOUT:?} after the offer");
            }
            if let Some(at) = self.subscribed_at
                && self.stats.frames_sent == 0
                && now >= at + FIRST_FRAME_TIMEOUT
            {
                bail!("no keyframe from the host {FIRST_FRAME_TIMEOUT:?} after subscribing");
            }
            if now >= self.next_stats_at {
                self.next_stats_at = now + STATS_INTERVAL;
                let elapsed_ms = self.epoch.elapsed().as_millis() as u64;
                let stats = self.stats.clone();
                self.send_control(&ServerMsg::Stats { elapsed_ms, stats });
            }
            if self.frames.is_some() && self.waiting_key && now >= self.next_idr_at {
                self.next_idr_at = now + IDR_RETRY;
                self.params.link.request_idr();
            }
            if self.next_rumble_at.is_some_and(|at| now >= at) {
                self.repeat_rumble(now);
            }

            let Some(rtc_deadline) = self.drive()? else {
                return Ok(());
            };
            self.maybe_subscribe();
            let mut deadline = rtc_deadline.min(self.next_stats_at);
            if !self.connected {
                deadline = deadline.min(self.epoch + CONNECT_TIMEOUT);
            }
            if self.frames.is_some() && self.waiting_key {
                deadline = deadline.min(self.next_idr_at);
            }
            if let Some(at) = self.next_rumble_at {
                deadline = deadline.min(at);
            }

            tokio::select! {
                datagram = self.peer.inbox.recv() => {
                    let Some(datagram) = datagram else { return Ok(()) };
                    self.receive(datagram)?;
                }
                frame = next(&mut self.frames) => match frame {
                    Ok(frame) => self.send_frame(&frame)?,
                    Err(RecvError::Lagged(n)) => {
                        // Skipped ahead: what follows can't be decoded
                        // without a keyframe.
                        self.stats.frames_dropped += n;
                        self.waiting_key = true;
                        self.next_idr_at = Instant::now();
                    }
                    Err(RecvError::Closed) => bail!("the host stream is gone"),
                },
                packet = next(&mut self.audio) => match packet {
                    Ok(packet) => self.send_audio(&packet)?,
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => self.audio = None,
                },
                rumble = next(&mut self.rumble) => match rumble {
                    Ok(rumble) => self.on_rumble(rumble),
                    Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => self.rumble = None,
                },
                () = self.seat.changed() => self.floor_changed(),
                _ = sleep_until(deadline.into()) => {
                    self.rtc.handle_input(Input::Timeout(Instant::now()))?;
                }
            }
        }
    }

    /// Starts taking frames once ICE, the track and the control channel are up.
    fn maybe_subscribe(&mut self) {
        if self.subscribed_at.is_some()
            || !self.connected
            || self.mid.is_none()
            || self.control.is_none()
        {
            return;
        }
        let now = Instant::now();
        self.subscribed_at = Some(now);
        self.frames = Some(self.params.link.frames());
        if self.params.info.audio && self.audio_mid.is_some() {
            self.audio = Some(self.params.link.audio());
        }
        self.rumble = Some(self.params.link.rumble());
        self.next_idr_at = now;
    }

    fn send_frame(&mut self, frame: &HostFrame) -> Result<()> {
        let now = Instant::now();
        let Some(mid) = self.mid else { return Ok(()) };
        if self.waiting_key {
            if !frame.key {
                return Ok(());
            }
            self.waiting_key = false;
            if self.stats.first_frame_ms.is_none() {
                let first = self
                    .subscribed_at
                    .map(|t| now.duration_since(t).as_millis() as u64);
                self.stats.first_frame_ms = first;
                info!(first_frame_ms = ?first, bytes = frame.data.len(), "first frame to the browser");
            }
        }
        self.stats.frames_generated += 1;
        let Some(writer) = self.rtc.writer(mid) else {
            bail!("video media {mid:?} disappeared");
        };
        let Some(pt) = writer
            .payload_params()
            .find(|p| p.spec().codec == self.rtp_codec)
            .map(|p| p.pt())
        else {
            bail!("browser did not negotiate {:?}", self.rtp_codec);
        };
        // RTP time from when the frame reached the gateway: hosts stamp
        // their video packets with a clock Moonlight clients ignore (Wolf
        // sends zeros), so it can't be passed through.
        let rtp_time = MediaTime::new(
            (frame
                .received
                .saturating_duration_since(self.epoch)
                .as_micros()
                * 9
                / 100) as u64,
            Frequency::NINETY_KHZ,
        );
        let s_us = self.epoch.elapsed().as_micros() as u64;
        writer
            .playout_delay(MediaTime::ZERO, MediaTime::ZERO)
            .abs_capture_time(AbsCaptureTime {
                capture_time: SystemTime::now(),
                clock_offset: None,
            })
            .write(pt, now, rtp_time, &frame.data[..])?;

        let id = self.frame_id;
        self.frame_id += 1;
        self.stats.frames_sent += 1;
        self.stats.bytes_sent += frame.data.len() as u64;
        self.stats.keyframes += u64::from(frame.key);
        let e_us = frame
            .received
            .saturating_duration_since(self.epoch)
            .as_micros() as u64;
        self.send_control(&ServerMsg::Sent {
            id,
            rtp: rtp_time.numer() as u32,
            s_us,
            e_us,
        });
        Ok(())
    }

    /// The host's Opus packet, unchanged. Its own clock times it (at 48 kHz),
    /// which the gateway's arrival jitter can't disturb.
    fn send_audio(&mut self, packet: &HostAudio) -> Result<()> {
        let Some(mid) = self.audio_mid else {
            return Ok(());
        };
        let Some(writer) = self.rtc.writer(mid) else {
            bail!("audio media {mid:?} disappeared");
        };
        let Some(pt) = writer
            .payload_params()
            .find(|p| p.spec().codec == RtpCodec::Opus)
            .map(|p| p.pt())
        else {
            // The browser offered audio without Opus: video only.
            self.audio = None;
            return Ok(());
        };
        let base = *self.audio_base.get_or_insert(packet.timestamp);
        let ticks = (packet.timestamp.saturating_sub(base).as_micros() * 48 / 1000) as u64;
        // Strictly forward, whatever the host's clock does.
        let rtp = if self.stats.audio_packets == 0 {
            ticks
        } else {
            ticks.max(self.last_audio_rtp + 1)
        };
        self.last_audio_rtp = rtp;
        writer.write(
            pt,
            Instant::now(),
            MediaTime::new(rtp, Frequency::FORTY_EIGHT_KHZ),
            &packet.data[..],
        )?;
        self.stats.audio_packets += 1;
        self.stats.audio_bytes += packet.data.len() as u64;
        Ok(())
    }

    fn on_rumble(&mut self, rumble: HostRumble) {
        if rumble.lo == 0.0 && rumble.hi == 0.0 {
            self.rumble_on.remove(&rumble.pad);
        } else {
            self.rumble_on.insert(rumble.pad, (rumble.lo, rumble.hi));
        }
        self.next_rumble_at = (!self.rumble_on.is_empty()).then(|| Instant::now() + RUMBLE_REPEAT);
        if self.seat.has_control() {
            let ms = if rumble.lo == 0.0 && rumble.hi == 0.0 {
                0
            } else {
                RUMBLE_MS
            };
            self.send_control(&ServerMsg::Rumble {
                i: rumble.pad,
                lo: rumble.lo,
                hi: rumble.hi,
                ms,
            });
        }
    }

    fn repeat_rumble(&mut self, now: Instant) {
        self.next_rumble_at = (!self.rumble_on.is_empty()).then(|| now + RUMBLE_REPEAT);
        if !self.seat.has_control() {
            return;
        }
        for (i, (lo, hi)) in self.rumble_on.clone() {
            self.send_control(&ServerMsg::Rumble {
                i,
                lo,
                hi,
                ms: RUMBLE_MS,
            });
        }
    }

    /// The floor or the viewer count moved: say so, and if this session lost
    /// the controls, let go of what it held.
    fn floor_changed(&mut self) {
        let control = self.seat.has_control();
        if self.control.is_some() {
            self.send_control(&ServerMsg::Floor {
                control,
                viewers: self.seat.viewers(),
            });
        }
        if self.had_control && !control {
            self.release_input();
        }
        self.had_control = control;
    }

    fn release_input(&mut self) {
        let events = self.input.release_all();
        self.params.link.input(events);
    }

    /// Polls output until str0m wants to wait; `None` ends the session.
    fn drive(&mut self) -> Result<Option<Instant>> {
        loop {
            match self.rtc.poll_output()? {
                Output::Timeout(at) => return Ok(Some(at)),
                Output::Transmit(t) => {
                    self.peer.hub.send(t.source, t.destination, &t.contents)?;
                }
                Output::Event(event) => {
                    if !self.handle_event(event) {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return false,
            Event::Connected => self.connected = true,
            Event::MediaAdded(m) if m.kind == MediaKind::Video => self.mid = Some(m.mid),
            Event::MediaAdded(m) if m.kind == MediaKind::Audio => self.audio_mid = Some(m.mid),
            Event::KeyframeRequest(_) => {
                self.stats.keyframe_requests += 1;
                self.params.link.request_idr();
            }
            Event::ChannelOpen(id, label) if label == "control" => {
                self.control = Some(id);
                let info = self.params.info;
                let stream = serde_json::json!({
                    "codec": info.codec.name(),
                    "width": info.width,
                    "height": info.height,
                    "input": true,
                    "audio": info.audio && self.audio_mid.is_some(),
                    "gamepads": true,
                    "fps": info.fps,
                    "overlay": false,
                });
                self.send_control(&ServerMsg::Hello { stream });
                self.had_control = self.seat.has_control();
                self.send_control(&ServerMsg::Floor {
                    control: self.had_control,
                    viewers: self.seat.viewers(),
                });
            }
            Event::ChannelData(data) if Some(data.id) == self.control => {
                for line in String::from_utf8_lossy(&data.data).lines() {
                    self.handle_client_line(line);
                }
            }
            _ => {}
        }
        true
    }

    /// One line from the page.
    fn handle_client_line(&mut self, line: &str) {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else {
            return;
        };
        match msg.get("t").and_then(|t| t.as_str()) {
            Some("ping") => {
                if let Some(c) = msg.get("c").and_then(|c| c.as_f64()) {
                    let s_us = self.epoch.elapsed().as_micros() as u64;
                    self.send_control(&ServerMsg::Pong { c, s_us });
                }
            }
            // The page lost a frame, or its decoder stalled: start over.
            Some("keyframe") => {
                self.stats.keyframe_requests += 1;
                self.params.link.request_idr();
            }
            Some("input") => {
                // A viewer watches: what it sends doesn't reach the host.
                if !self.seat.has_control() {
                    return;
                }
                self.stats.inputs += 1;
                let received_us = self.epoch.elapsed().as_micros() as u64;
                let probe = msg.get("probe").and_then(|p| p.as_u64());
                let info = self.params.info;
                match serde_json::from_value::<BrowserInput>(msg) {
                    Ok(input) => {
                        let events = input
                            .into_input()
                            .map(|input| self.input.apply(&input, info.width, info.height))
                            .unwrap_or_default();
                        if events.is_empty() {
                            self.stats.inputs_unmapped += 1;
                        }
                        self.params.link.input(events);
                    }
                    Err(_) => self.stats.inputs_unmapped += 1,
                }
                // The page's latency probe: echo when its click arrived.
                if let Some(id) = probe {
                    self.send_control(&ServerMsg::Probe {
                        id,
                        s_us: received_us,
                    });
                }
            }
            // resize, report, clipboard, cursor, fps, overlay, take_control:
            // nothing here changes with them.
            _ => {}
        }
    }

    /// One datagram from the shared socket. Only a session whose `Rtc`
    /// accepts it (an ICE check with its username, traffic from its peer's
    /// address) handles it; the others leave it alone.
    fn receive(&mut self, datagram: Datagram) -> Result<()> {
        let source = datagram.source;
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.locals[datagram.socket],
                contents: datagram.data.as_slice().try_into()?,
            },
        );
        if !self.rtc.accepts(&input) {
            self.peer.release(source);
            return Ok(());
        }
        self.peer.claim(source);
        self.rtc.handle_input(input)?;
        Ok(())
    }

    fn send_control(&mut self, msg: &ServerMsg) {
        let Some(id) = self.control else { return };
        let Some(mut channel) = self.rtc.channel(id) else {
            return;
        };
        let mut line = serde_json::to_string(msg).expect("control messages serialize");
        line.push('\n');
        if !matches!(channel.write(false, line.as_bytes()), Ok(true)) {
            warn!("control channel refused a message");
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // What this viewer held must not stay held on the host.
        self.release_input();
    }
}

/// The next message of a broadcast, or never while not subscribed.
async fn next<T: Clone>(rx: &mut Option<broadcast::Receiver<T>>) -> Result<T, RecvError> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

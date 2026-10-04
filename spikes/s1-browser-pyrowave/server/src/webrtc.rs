//! WebRTC path: an ICE-lite str0m peer per session. The browser creates two
//! DataChannels: `media` (unordered, maxRetransmits 0) and `control`
//! (reliable, JSON lines). One thread per session drives the sans-IO state
//! machine and the synthetic frame clock.

use std::io::ErrorKind;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use str0m::change::{SdpAnswer, SdpOffer};
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tracing::{info, warn};

use crate::traffic::{
    ClientMsg, FrameSource, SenderStats, ServerMsg, TrafficConfig, TransportStats,
};

const STATS_INTERVAL: Duration = Duration::from_millis(500);
/// Linger after the run so the final stats reach the client.
const DRAIN: Duration = Duration::from_secs(1);
/// SCTP buffer space kept free for control messages, which share the association.
const CONTROL_HEADROOM: usize = 64 * 1024;

pub struct SessionParams {
    pub config: TrafficConfig,
    /// Address the browser used to reach us; becomes the only host candidate.
    pub host: IpAddr,
    /// Preferred UDP port (firewall-friendly); falls back to an ephemeral port
    /// if a previous session still holds it.
    pub port: u16,
    pub sctp_buffer: usize,
}

/// Accepts an offer, spawns the session thread and returns the answer.
pub fn start(params: SessionParams, offer: SdpOffer) -> Result<SdpAnswer> {
    let socket = UdpSocket::bind(SocketAddr::new(params.host, params.port))
        .or_else(|_| UdpSocket::bind(SocketAddr::new(params.host, 0)))
        .with_context(|| format!("binding a UDP socket on {}", params.host))?;
    let local = socket.local_addr()?;

    let mut rtc = Rtc::builder()
        .set_ice_lite(true)
        .set_sctp_max_buffered_amount(params.sctp_buffer)
        .build(Instant::now());
    rtc.add_local_candidate(Candidate::host(local, "udp")?)
        .context("adding host candidate")?;
    let answer = rtc.sdp_api().accept_offer(offer)?;

    info!(%local, config = ?params.config, "webrtc session");
    std::thread::Builder::new()
        .name(format!("webrtc-{}", local.port()))
        .spawn(move || {
            if let Err(err) = Session::new(rtc, socket, params).and_then(Session::run) {
                warn!("webrtc session ended: {err:#}");
            }
        })?;
    Ok(answer)
}

struct Session {
    rtc: Rtc,
    socket: UdpSocket,
    local: SocketAddr,
    epoch: Instant,
    source: FrameSource,
    sctp_buffer: usize,
    media: Option<ChannelId>,
    control: Option<ChannelId>,
    stats: SenderStats,
    next_frame_at: Option<Instant>,
    next_stats_at: Instant,
    finished_at: Option<Instant>,
}

impl Session {
    fn new(rtc: Rtc, socket: UdpSocket, params: SessionParams) -> Result<Self> {
        let epoch = Instant::now();
        let local = socket.local_addr()?;
        let source = FrameSource::synthetic(params.config, params.config.dgram, epoch)?;
        Ok(Self {
            rtc,
            socket,
            local,
            epoch,
            source,
            sctp_buffer: params.sctp_buffer,
            media: None,
            control: None,
            stats: SenderStats::default(),
            next_frame_at: None,
            next_stats_at: epoch + STATS_INTERVAL,
            finished_at: None,
        })
    }

    fn run(mut self) -> Result<()> {
        let mut buf = vec![0u8; 2048];
        loop {
            let now = Instant::now();
            if let Some(done) = self.finished_at
                && now >= done + DRAIN
            {
                return Ok(());
            }
            self.maybe_send_frame(now);
            if now >= self.next_stats_at {
                self.next_stats_at = now + STATS_INTERVAL;
                let elapsed_ms = self.epoch.elapsed().as_millis() as u64;
                let transport = self.transport_stats();
                self.send_control(&ServerMsg::Stats {
                    elapsed_ms,
                    sender: self.stats,
                    transport,
                });
            }

            let Some(rtc_deadline) = self.drive()? else {
                return Ok(());
            };

            let mut deadline = rtc_deadline.min(self.next_stats_at);
            if let Some(at) = self.next_frame_at {
                deadline = deadline.min(at);
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                self.rtc.handle_input(Input::Timeout(Instant::now()))?;
                continue;
            }
            self.socket.set_read_timeout(Some(wait))?;
            match self.socket.recv_from(&mut buf) {
                Ok((n, source)) => {
                    self.receive(&buf[..n], source)?;
                    // Drain everything else already queued (SACK bursts) instead of
                    // one packet per wakeup. str0m's DTLS layer only queues ~30
                    // records, so output is driven after every packet.
                    self.socket.set_nonblocking(true)?;
                    let drained = loop {
                        if self.drive()?.is_none() {
                            break Ok(false);
                        }
                        if self.timer_due(Instant::now()) {
                            break Ok(true);
                        }
                        match self.socket.recv_from(&mut buf) {
                            Ok((n, source)) => self.receive(&buf[..n], source)?,
                            Err(err) if err.kind() == ErrorKind::WouldBlock => break Ok(true),
                            Err(err) => break Err(err),
                        }
                    };
                    self.socket.set_nonblocking(false)?;
                    if !drained? {
                        return Ok(());
                    }
                }
                Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                    self.rtc.handle_input(Input::Timeout(Instant::now()))?;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }

    /// Whether a frame or stats tick is due; draining input must not starve them.
    fn timer_due(&self, now: Instant) -> bool {
        now >= self.next_stats_at || self.next_frame_at.is_some_and(|at| now >= at)
    }

    /// Polls output until str0m wants to wait. Returns its next deadline, or
    /// `None` when the session should end.
    fn drive(&mut self) -> Result<Option<Instant>> {
        loop {
            match self.rtc.poll_output()? {
                Output::Timeout(at) => return Ok(Some(at)),
                Output::Transmit(t) => {
                    if let Err(err) = self.socket.send_to(&t.contents, t.destination)
                        && err.kind() != ErrorKind::WouldBlock
                    {
                        return Err(err.into());
                    }
                }
                Output::Event(event) => {
                    if !self.handle_event(event) {
                        return Ok(None);
                    }
                }
            }
        }
    }

    fn receive(&mut self, contents: &[u8], source: SocketAddr) -> Result<()> {
        let input = Input::Receive(
            Instant::now(),
            Receive {
                proto: Protocol::Udp,
                source,
                destination: self.local,
                contents: contents.try_into()?,
            },
        );
        self.rtc.handle_input(input)?;
        Ok(())
    }

    /// Returns false when the session should end.
    fn handle_event(&mut self, event: Event) -> bool {
        match event {
            Event::IceConnectionStateChange(IceConnectionState::Disconnected) => return false,
            Event::ChannelOpen(id, label) => match label.as_str() {
                "media" => self.media = Some(id),
                "control" => {
                    self.control = Some(id);
                    let config = self.source.config();
                    self.send_control(&ServerMsg::Hello {
                        config,
                        frame_bytes: config.frame_bytes(),
                        fragments_per_frame: self.source.fragments_per_frame(),
                        max_datagram: config.dgram,
                        total_frames: config.total_frames(),
                        stream: None,
                    });
                }
                other => warn!("ignoring unexpected channel {other:?}"),
            },
            Event::ChannelData(data) if Some(data.id) == self.control => {
                for line in String::from_utf8_lossy(&data.data).lines() {
                    match serde_json::from_str::<ClientMsg>(line) {
                        Ok(ClientMsg::Ping { c }) => {
                            let s_us = self.epoch.elapsed().as_micros() as u64;
                            self.send_control(&ServerMsg::Pong { c, s_us });
                        }
                        Err(err) => warn!("bad control message: {err}"),
                    }
                }
            }
            Event::ChannelClose(id) if Some(id) == self.control || Some(id) == self.media => {
                return false;
            }
            _ => {}
        }
        // Start the frame clock once both channels are up.
        if self.next_frame_at.is_none() && self.media.is_some() && self.control.is_some() {
            self.next_frame_at = Some(Instant::now());
        }
        true
    }

    fn maybe_send_frame(&mut self, now: Instant) {
        let (Some(at), Some(media)) = (self.next_frame_at, self.media) else {
            return;
        };
        if now < at || self.finished_at.is_some() {
            return;
        }
        let interval = self.source.config().frame_interval();
        // Skip missed ticks rather than bursting to catch up.
        let mut next = at + interval;
        while next <= now {
            next += interval;
        }
        self.next_frame_at = Some(next);
        self.stats.frames_generated += 1;

        let frame_bytes = self.source.frame_wire_bytes();
        let Some(mut channel) = self.rtc.channel(media) else {
            return;
        };
        if channel.buffered_amount() + frame_bytes + CONTROL_HEADROOM > self.sctp_buffer {
            self.stats.frames_dropped_backpressure += 1;
        } else {
            let stats = &mut self.stats;
            self.source
                .next_frame(now, |datagram| match channel.write(true, datagram) {
                    Ok(true) => {
                        stats.datagrams_sent += 1;
                        stats.bytes_sent += datagram.len() as u64;
                    }
                    _ => stats.datagrams_refused += 1,
                });
            self.stats.frames_sent += 1;
        }

        if self.stats.frames_generated >= self.source.config().total_frames() {
            info!(local = %self.local, stats = ?self.stats, "webrtc run complete");
            self.finished_at = Some(now);
            self.send_control(&ServerMsg::Done { sender: self.stats });
        }
    }

    fn transport_stats(&mut self) -> TransportStats {
        let buffered_amount = self
            .media
            .and_then(|id| self.rtc.channel(id))
            .map(|mut channel| channel.buffered_amount());
        TransportStats {
            buffered_amount,
            ..Default::default()
        }
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

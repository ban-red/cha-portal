//! Synthetic PyroWave-shaped traffic: fixed-size intra frames at a fixed rate,
//! burst out at each frame boundary like an encoder would.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use cha_proto::{DatagramHeader, Flags, Fragmenter, Kind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TrafficConfig {
    /// Target video bitrate in Mbit/s.
    pub mbps: f64,
    pub fps: u32,
    /// Run length in seconds.
    pub secs: u32,
    /// Largest datagram (header included) the sender may emit.
    pub dgram: usize,
}

impl TrafficConfig {
    /// Parses `mbps=600&fps=60&secs=20&dgram=1200` (path prefix and missing keys allowed).
    pub fn from_query(query: &str) -> Result<Self> {
        let query = query.split_once('?').map_or(query, |(_, q)| q);
        let mut cfg = TrafficConfig {
            mbps: 300.0,
            fps: 60,
            secs: 20,
            dgram: 1200,
        };
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "mbps" => cfg.mbps = value.parse()?,
                "fps" => cfg.fps = value.parse()?,
                "secs" => cfg.secs = value.parse()?,
                "dgram" => cfg.dgram = value.parse()?,
                _ => {}
            }
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if !(self.mbps > 0.0 && self.mbps <= 5_000.0) {
            bail!("mbps must be in (0, 5000]");
        }
        if !(1..=480).contains(&self.fps) {
            bail!("fps must be in 1..=480");
        }
        if !(1..=600).contains(&self.secs) {
            bail!("secs must be in 1..=600");
        }
        if !(cha_proto::HEADER_LEN + 64..=65_000).contains(&self.dgram) {
            bail!("dgram must be in {}..=65000", cha_proto::HEADER_LEN + 64);
        }
        if self
            .frame_bytes()
            .div_ceil(self.dgram - cha_proto::HEADER_LEN)
            > u16::MAX as usize
        {
            bail!(
                "frame needs more than {} fragments; raise fps or dgram",
                u16::MAX
            );
        }
        Ok(())
    }

    pub fn frame_bytes(&self) -> usize {
        (self.mbps * 1e6 / 8.0 / self.fps as f64) as usize
    }

    pub fn frame_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.fps as f64)
    }

    pub fn total_frames(&self) -> u64 {
        self.secs as u64 * self.fps as u64
    }
}

/// Sender-side counters, reported to the client every stats tick.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct SenderStats {
    pub frames_generated: u64,
    pub frames_sent: u64,
    /// Frames skipped because the transport's send buffer could not take a
    /// whole frame (backpressure applied before "encoding", never mid-frame).
    pub frames_dropped_backpressure: u64,
    /// Datagrams the transport refused after the frame was admitted.
    pub datagrams_refused: u64,
    pub datagrams_sent: u64,
    pub bytes_sent: u64,
}

/// One frame to send: its bytes and whether it decodes on its own.
pub struct Frame {
    pub data: Vec<u8>,
    pub key: bool,
}

/// Produces the datagrams of successive frames: either synthetic intra frames at
/// a fixed bitrate, or an encoded stream replayed in a loop.
pub struct FrameSource {
    config: TrafficConfig,
    fragmenter: Fragmenter,
    frames: Arc<[Frame]>,
    kind: Kind,
    epoch: Instant,
    next_frame_id: u32,
}

impl FrameSource {
    /// Fixed-size intra frames at `config.mbps`, like PyroWave at a byte cap.
    pub fn synthetic(config: TrafficConfig, max_datagram: usize, epoch: Instant) -> Result<Self> {
        let payload = (0..config.frame_bytes()).map(|i| (i % 251) as u8).collect();
        let frames: Arc<[Frame]> = Arc::from(vec![Frame {
            data: payload,
            key: true,
        }]);
        Self::new(config, frames, Kind::Probe, max_datagram, epoch)
    }

    /// Replays `frames` in a loop at `config.fps`.
    pub fn replay(
        config: TrafficConfig,
        frames: Arc<[Frame]>,
        max_datagram: usize,
        epoch: Instant,
    ) -> Result<Self> {
        Self::new(config, frames, Kind::Video, max_datagram, epoch)
    }

    fn new(
        config: TrafficConfig,
        frames: Arc<[Frame]>,
        kind: Kind,
        max_datagram: usize,
        epoch: Instant,
    ) -> Result<Self> {
        if max_datagram <= cha_proto::HEADER_LEN {
            bail!("transport datagram limit {max_datagram} leaves no room for payload");
        }
        if frames.is_empty() {
            bail!("no frames to send");
        }
        let fragmenter = Fragmenter::new(max_datagram);
        let most = frames
            .iter()
            .map(|f| fragmenter.fragment_count(f.data.len()))
            .max()
            .unwrap_or(1);
        if most > u16::MAX as usize {
            bail!(
                "a frame needs more than {} fragments at datagram size {max_datagram}",
                u16::MAX
            );
        }
        Ok(Self {
            config,
            fragmenter,
            frames,
            kind,
            epoch,
            next_frame_id: 0,
        })
    }

    pub fn config(&self) -> TrafficConfig {
        self.config
    }

    fn current(&self) -> &Frame {
        &self.frames[self.next_frame_id as usize % self.frames.len()]
    }

    /// Largest frame in bytes.
    pub fn max_frame_bytes(&self) -> usize {
        self.frames.iter().map(|f| f.data.len()).max().unwrap_or(0)
    }

    /// Fragments of the largest frame.
    pub fn fragments_per_frame(&self) -> usize {
        self.fragmenter.fragment_count(self.max_frame_bytes())
    }

    /// Fragments of the frame `next_frame` will send.
    pub fn next_fragments(&self) -> usize {
        self.fragmenter.fragment_count(self.current().data.len())
    }

    /// Bytes the next frame occupies in datagram payloads (headers included).
    pub fn frame_wire_bytes(&self) -> usize {
        self.current().data.len() + self.next_fragments() * cha_proto::HEADER_LEN
    }

    pub fn micros_since_epoch(&self, now: Instant) -> u64 {
        now.duration_since(self.epoch).as_micros() as u64
    }

    /// Emits every datagram of the next frame through `emit`. Frames skipped
    /// for backpressure simply never call this, so frame ids stay contiguous
    /// and the receiver only counts real network loss.
    pub fn next_frame(&mut self, now: Instant, emit: impl FnMut(&[u8])) {
        let index = self.next_frame_id as usize % self.frames.len();
        let frame = &self.frames[index];
        let header = DatagramHeader {
            kind: self.kind,
            flags: Flags(if frame.key { Flags::KEYFRAME } else { 0 }),
            stream: 0,
            fec: 0,
            frame_id: self.next_frame_id,
            frag_index: 0,
            frag_count: 0,
            send_ts_us: self.micros_since_epoch(now) as u32,
        };
        self.next_frame_id = self.next_frame_id.wrapping_add(1);
        self.fragmenter
            .fragment(header, &frame.data, emit)
            .expect("frame sizes validated against the u16 fragment limit");
    }
}

/// Value of `key` in a `a=1&b=2` query (path prefix allowed).
pub fn query_value<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    let query = query.split_once('?').map_or(query, |(_, q)| q);
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find_map(|(k, v)| (k == key).then_some(v))
}

/// Control messages, sent as JSON lines on the reliable channel.
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Clock-sync probe; `c` is the client's clock in ms.
    Ping { c: f64 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Hello {
        config: TrafficConfig,
        frame_bytes: usize,
        fragments_per_frame: usize,
        max_datagram: usize,
        total_frames: u64,
        /// Header of the replayed stream (codec, size, …); absent for synthetic runs.
        #[serde(skip_serializing_if = "Option::is_none")]
        stream: Option<serde_json::Value>,
    },
    /// Reply to a ping; `s_us` is the sender's session clock.
    Pong { c: f64, s_us: u64 },
    /// WebRTC media path: frame `id` went out with RTP timestamp `rtp` at `s_us`.
    Sent { id: u32, rtp: u32, s_us: u64 },
    Stats {
        elapsed_ms: u64,
        #[serde(flatten)]
        sender: SenderStats,
        #[serde(flatten)]
        transport: TransportStats,
    },
    Done {
        #[serde(flatten)]
        sender: SenderStats,
    },
}

/// Transport-specific numbers; fields that don't apply are left `None`.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct TransportStats {
    pub rtt_ms: Option<f64>,
    pub cwnd: Option<u64>,
    pub lost_packets: Option<u64>,
    pub mtu: Option<u16>,
    pub send_buffer_free: Option<usize>,
    pub buffered_amount: Option<usize>,
}

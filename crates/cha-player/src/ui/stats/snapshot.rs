//! One reading of the running stream, and how the panel's value keys are taken from it.

use cha_client::NodeStats;
use cha_ui_spec::panel::Values;

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

    /// The panel's value keys (`stats-panel.json`) from this reading. This is the only place the
    /// panel's names meet the player's snapshot; a key left out is one this transport can't give,
    /// and the panel reads it as "–" or hides its row.
    pub fn values(&self) -> Values {
        let mut v = Values::new();
        v.num("shown_fps", self.present_fps)
            .opt("target_fps", self.target_fps.filter(|t| *t > 0))
            .opt("mbps", self.mbps)
            .text("codec_tag", &self.codec_text())
            .opt("width", Some(self.width).filter(|w| *w > 0))
            .opt("height", Some(self.height).filter(|h| *h > 0))
            .opt("reconnects", Some(self.reconnects).filter(|r| *r > 0))
            .opt("latency_ms", self.latency_ms)
            .opt("decode_ms", self.decode_ms)
            // The panel's `audio_jitter_ms` is the sound waiting in the buffer, here and in the browser.
            .opt("audio_jitter_ms", self.audio_buffer_ms)
            .num("audio_underruns", self.audio_underruns as f64)
            .num("audio_dropped_ms", self.audio_dropped_ms as f64)
            .opt("rtt_ms", self.rtt_ms)
            .opt("lost", self.lost.map(|l| l as f64))
            .opt("recovered", self.recovered.map(|l| l as f64))
            .num("dropped", self.dropped as f64)
            .num("decode_errors", self.decode_errors as f64);
        // Only PyroWave shows frames from some of their packets; for the others there is nothing to count.
        if self.is_pyrowave() || self.partial > 0 {
            v.num("partial", self.partial as f64);
        }
        if let Some(n) = &self.node {
            node_values(&mut v, n);
        }
        v
    }
}

fn node_values(v: &mut Values, n: &NodeStats) {
    v.num("node_cpu", n.cpu)
        // Only a node that reports its cores has a load line, and only a card with a limit a limit.
        .opt("node_cores", Some(n.cores).filter(|c| *c > 0))
        .num("node_load1", n.load1)
        .num("node_mem_used", n.mem_used as f64)
        .num("node_mem_total", n.mem_total as f64)
        .opt("node_gpu", n.gpu)
        .opt("node_vram_used", n.vram_used.map(|b| b as f64))
        .opt("node_vram_total", n.vram_total.map(|b| b as f64))
        .opt("node_temp", n.temp)
        .opt("node_power", n.power)
        .opt("node_power_limit", n.power_limit.filter(|l| *l > 0.0))
        .opt("node_clock", n.clock)
        .opt("node_enc", n.enc)
        .opt("node_dec", n.dec)
        .num("node_streamer_cpu", n.streamer_cpu);
}

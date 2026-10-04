//! The control channel, the same on every transport (plan §3.1): JSON lines
//! carrying the page's pings, input, resize and keyframe requests in, and
//! pongs, acknowledgements and stats out. WebRTC carries it on the `control`
//! DataChannel, WebTransport on the session's first bidirectional stream.

use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;

use crate::codec::VideoCodec;
use crate::gamepad::{Gamepads, PadState};
use crate::input::BrowserInput;
use crate::media::Media;

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
    /// The output was resized to `w`×`h` at `s_us` (as asked, rounded to fit).
    Resized {
        w: u32,
        h: u32,
        s_us: u64,
    },
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
        match msg.get("t").and_then(|t| t.as_str()) {
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
            // The page lost a frame (WebTransport): start over from a keyframe.
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

pub fn percentile<T: Copy>(sorted: &[T], q: f64) -> Option<T> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((sorted.len() as f64 * q) as usize).min(sorted.len() - 1);
    Some(sorted[i])
}

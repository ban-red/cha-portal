//! The control channel, the same on every transport (plan §3.1): JSON lines
//! carrying the page's pings, input, resize, keyframe requests and clipboard
//! text in, and pongs, acknowledgements, stats, the apps' clipboard and their
//! setup status out.
//! Only the session with the floor (`viewers`) acts on the environment; the
//! others watch. WebRTC carries it on the `control`
//! DataChannel, WebTransport on the session's first bidirectional stream.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use base64::Engine;
use serde::Serialize;

use crate::codec::VideoCodec;
use crate::compositor::{ClipboardWatch, CursorShape, CursorWatch, PointerSpot, PointerWatch};
use crate::gamepad::{Gamepads, PadState};
use crate::input::BrowserInput;
use crate::media::Media;
use crate::status::{Status, StatusWatch};
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
        if matches!(kind, Some("input" | "resize" | "clipboard" | "cursor"))
            && !self.seat.has_control()
        {
            return;
        }
        match kind {
            Some("take_control") => {
                self.seat.take_control();
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

    fn line(msg: &ServerMsg) -> String {
        serde_json::to_string(msg).unwrap()
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
}

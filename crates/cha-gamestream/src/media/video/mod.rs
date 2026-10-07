//! The video stream: backend frames in, packets to the client out.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use crate::backend::{EncodedVideo, HdrMetadata, MediaControl};
use crate::handoff::SessionHandoff;
use crate::hdr;

use super::ping::is_client_ping;
use super::{Shared, stopped};

pub(crate) mod fec;
pub(crate) mod packetizer;
pub(crate) mod sender;

use packetizer::{Packetizer, PacketizerConfig};
use sender::VideoSocket;

/// How many recent frames the host can map a client frame number back to a
/// backend index for.
const FRAME_MAP_LEN: usize = 1024;

/// Wire frame number to backend frame index, for the frames just sent. A
/// client's "I lost frames 40 to 42" names wire numbers; the backend knows its
/// own.
#[derive(Default)]
pub(crate) struct FrameMap {
    recent: VecDeque<(u32, u64)>,
}

impl FrameMap {
    pub fn record(&mut self, frame_number: u32, index: u64) {
        if self.recent.len() == FRAME_MAP_LEN {
            self.recent.pop_front();
        }
        self.recent.push_back((frame_number, index));
    }

    /// The backend indexes of wire frames `first..=last`, from the lowest to
    /// the highest known. `None` when none of the range is remembered.
    pub fn backend_range(&self, first: u32, last: u32) -> Option<(u64, u64)> {
        let mut found = self
            .recent
            .iter()
            .filter(|(n, _)| (first..=last).contains(n))
            .map(|(_, i)| *i);
        let lo = found.next()?;
        let (min, max) = found.fold((lo, lo), |(lo, hi), i| (lo.min(i), hi.max(i)));
        Some((min, max))
    }
}

pub(crate) struct VideoTask {
    pub socket: VideoSocket,
    pub handoff: SessionHandoff,
    pub frames: mpsc::Receiver<EncodedVideo>,
    pub control: Arc<dyn MediaControl>,
    pub shared: Arc<Shared>,
    pub hdr: watch::Receiver<Option<HdrMetadata>>,
}

/// Tenths of a millisecond the frame spent between capture and now, as the
/// client's overlay reads it.
fn latency(captured: std::time::Instant) -> u16 {
    (captured.elapsed().as_micros() / 100).min(u128::from(u16::MAX)) as u16
}

pub(crate) async fn run(task: VideoTask) {
    let VideoTask {
        socket,
        handoff,
        mut frames,
        control,
        shared,
        hdr,
    } = task;
    let params = &handoff.params;
    let mut stop = shared.stop_rx();
    let mut packetizer = Packetizer::new(PacketizerConfig {
        packet_size: params.packet_size,
        fec_percent: params.fec_percent,
        min_fec_packets: params.min_fec_packets,
        key: handoff.encryption.video.then_some(handoff.keys.key),
    });
    let client_ip: IpAddr = params.client_ip;
    let mut client = None;
    // A client starts on a keyframe, and so does a client at a new address.
    let mut need_key = true;
    let mut frame_number = 0u32;
    let fps = u64::from(params.fps.max(1));
    let mut buf = [0u8; 64];

    loop {
        tokio::select! {
            () = stopped(&mut stop) => break,
            frame = frames.recv() => {
                let Some(frame) = frame else {
                    shared.end(super::EndReason::BackendEnded);
                    break;
                };
                if !shared.is_started() {
                    continue;
                }
                let Some(to) = client else {
                    shared.stats.video_dropped();
                    continue;
                };
                if need_key && !frame.key {
                    shared.stats.video_dropped();
                    continue;
                }
                need_key = false;
                let mut data = frame.data;
                if handoff.params.hdr
                    && frame.key
                    && let Some(meta) = *hdr.borrow()
                {
                    data = hdr::inject_hdr_metadata(&data, &meta, handoff.params.codec).into();
                }
                let next = frame_number + 1;
                let rtp = (u64::from(next - 1) * 90_000 / fps) as u32;
                match packetizer.packetize(&data, frame.key, next, rtp, latency(frame.captured)) {
                    Ok(batch) => {
                        frame_number = next;
                        shared.frames.lock().expect("frame map").record(next, frame.index);
                        let fallbacks = socket.send_batch(&batch, to).await;
                        if fallbacks > 0 {
                            tracing::debug!("{fallbacks} video chunk(s) fell back from GSO");
                        }
                        shared.stats.video_sent();
                    }
                    Err(e) => {
                        tracing::warn!("dropping a video frame: {e}");
                        shared.stats.video_dropped();
                        need_key = true;
                        control.request_keyframe();
                    }
                }
            }
            received = socket.recv_from(&mut buf) => {
                let Ok((n, from)) = received else { continue };
                if is_client_ping(&buf[..n], from, client_ip, &handoff.ping_payload) {
                    if client != Some(from) {
                        tracing::debug!("video client at {from}");
                        client = Some(from);
                        need_key = true;
                        control.request_keyframe();
                    }
                } else {
                    shared.stats.rejected_pings();
                }
            }
        }
    }
    tracing::debug!("video stream stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_map_names_the_backend_frames() {
        let mut m = FrameMap::default();
        for n in 1..=10u32 {
            m.record(n, 100 + u64::from(n) * 2);
        }
        assert_eq!(m.backend_range(3, 5), Some((106, 110)));
        assert_eq!(m.backend_range(9, 50), Some((118, 120)));
        assert_eq!(m.backend_range(11, 12), None);
        for n in 11..=2000u32 {
            m.record(n, u64::from(n));
        }
        assert_eq!(m.backend_range(1, 100), None, "old frames are forgotten");
        assert_eq!(m.backend_range(1990, 1990), Some((1990, 1990)));
    }
}

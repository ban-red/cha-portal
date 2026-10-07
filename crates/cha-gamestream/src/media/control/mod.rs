// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: the loop speaks only to the media backend's neutral handle; a malformed or unauthentic packet
// is dropped and counted instead of ending the session; every pending feedback message goes out each tick;
// only the session client's address, with the session's connect data, may connect; plain messages are refused.

//! The control stream: an ENet host the client connects to, carrying
//! encrypted control messages (pings, keyframe requests, input) in and
//! feedback (rumble, LEDs, HDR, termination) out.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_enet::{Event, Host, Packet, PacketMode, PeerId, PeerState};

use crate::backend::{Feedback, HdrMetadata, MediaControl};
use crate::handoff::SessionHandoff;
use crate::input;
use crate::net::same_ip;

use super::{EndReason, Shared, stopped};

pub(crate) mod crypto;
pub(crate) mod feedback;
pub(crate) mod messages;

use messages::{Inbound, Outer};

pub(crate) struct ControlTask {
    pub host: Host,
    pub handoff: SessionHandoff,
    pub feedback: mpsc::Receiver<Feedback>,
    pub control: Arc<dyn MediaControl>,
    pub shared: Arc<Shared>,
    pub stream_timeout: Duration,
}

/// The tick: how long one `service` waits for the network before the loop
/// looks at feedback, the stop signal and the deadline again.
const TICK: Duration = Duration::from_millis(5);
/// Events handled per tick before the loop gets back to feedback.
const EVENTS_PER_TICK: usize = 256;

struct Link {
    host: Host,
    peer: Option<PeerId>,
    key: [u8; 16],
    sequence: u32,
}

impl Link {
    /// Encrypts and queues a host-to-client message, if a client is there.
    fn send(&mut self, inner: &[u8], what: &str) {
        let Some(peer_id) = self.peer else { return };
        let Ok(wire) = crypto::seal(&self.key, self.sequence, inner) else {
            tracing::warn!("couldn't encrypt {what}");
            return;
        };
        self.sequence = self.sequence.wrapping_add(1);
        if let Some(peer) = self.host.peer_mut(peer_id)
            && peer.state() == PeerState::Connected
            && let Err(e) = peer.send(0, Packet::new(&wire, PacketMode::ReliableSequenced))
        {
            tracing::warn!("sending {what}: {e}");
        }
    }
}

pub(crate) async fn run(task: ControlTask) {
    let ControlTask {
        host,
        handoff,
        mut feedback,
        control,
        shared,
        stream_timeout,
    } = task;
    let mut stop = shared.stop_rx();
    let mut link = Link {
        host,
        peer: None,
        key: handoff.keys.key,
        sequence: 0,
    };
    let client_ip: IpAddr = handoff.params.client_ip;
    let hdr_stream = handoff.params.hdr;
    let mut hdr_state: Option<(bool, Option<HdrMetadata>)> = None;
    let mut deadline = Instant::now() + stream_timeout;

    'outer: loop {
        if *stop.borrow() {
            break;
        }
        if Instant::now() > deadline {
            tracing::info!("no control ping for {stream_timeout:?}: ending the session");
            shared.end(EndReason::TimedOut);
            break;
        }

        // Everything the backend has for the client, not one message a tick.
        while let Ok(fb) = feedback.try_recv() {
            match fb {
                Feedback::Hdr { enabled, metadata } => {
                    shared
                        .hdr
                        .send_replace(if enabled { metadata } else { None });
                    hdr_state = Some((enabled, metadata));
                    if shared.is_started() {
                        link.send(&feedback::hdr_mode(enabled, metadata.as_ref()), "HDR mode");
                    }
                }
                other => {
                    if let Some(inner) = feedback::encode(&other) {
                        link.send(&inner, "feedback");
                    }
                }
            }
        }

        let mut timeout = TICK;
        for _ in 0..EVENTS_PER_TICK {
            let event = tokio::select! {
                () = stopped(&mut stop) => break 'outer,
                event = link.host.service(timeout) => event,
            };
            timeout = Duration::ZERO;
            let event = match event {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(e) => {
                    tracing::error!("ENet: {e}");
                    shared.end(EndReason::ClientLeft);
                    break 'outer;
                }
            };
            match event {
                Event::Connect { peer_id, data } => {
                    let from = link.host.peer(peer_id).map(|p| p.address().ip());
                    let allowed = link.peer.is_none()
                        && data == handoff.control_connect_data
                        && from.is_some_and(|ip| same_ip(ip, client_ip));
                    if allowed {
                        tracing::debug!("control client connected");
                        link.peer = Some(peer_id);
                    } else {
                        tracing::warn!("refusing a control connection from {from:?}");
                        shared.stats.rejected_peers();
                        // A graceful disconnect: the refused peer is told, and frees its slot.
                        if let Some(peer) = link.host.peer_mut(peer_id) {
                            peer.disconnect(0);
                        }
                    }
                }
                Event::Disconnect { peer_id, .. } => {
                    if link.peer == Some(peer_id) {
                        tracing::debug!("control client left");
                        shared.end(EndReason::ClientLeft);
                        break 'outer;
                    }
                }
                Event::Receive {
                    peer_id, packet, ..
                } => {
                    if link.peer != Some(peer_id) {
                        shared.stats.rejected_peers();
                        continue;
                    }
                    let decrypted;
                    let inner: &[u8] = match messages::parse_outer(packet.data()) {
                        Err(e) => {
                            tracing::debug!("dropping a malformed control packet: {e}");
                            shared.stats.malformed_control();
                            continue;
                        }
                        // An unencrypted message could come from anyone who can reach the port.
                        Ok(Outer::Plain { ty }) => {
                            tracing::debug!("refusing an unencrypted control message ({ty:#06x})");
                            shared.stats.rejected_control();
                            continue;
                        }
                        Ok(Outer::Encrypted {
                            sequence,
                            tag,
                            ciphertext,
                        }) => match crypto::open(&link.key, sequence, &tag, ciphertext) {
                            Ok(plain) => {
                                decrypted = plain;
                                &decrypted
                            }
                            Err(_) => {
                                tracing::debug!(
                                    "refusing a control message that doesn't authenticate"
                                );
                                shared.stats.rejected_control();
                                continue;
                            }
                        },
                    };
                    let message = match messages::parse_inner(inner) {
                        Ok(m) => Some(m),
                        Err(e) => {
                            tracing::debug!("dropping a malformed control message: {e}");
                            shared.stats.malformed_control();
                            None
                        }
                    };
                    match message {
                        Some(Inbound::Ping) => deadline = Instant::now() + stream_timeout,
                        Some(Inbound::StartB) => {
                            shared.start();
                            control.request_keyframe();
                            if hdr_stream {
                                let (enabled, meta) = hdr_state.unwrap_or((true, None));
                                // With no metadata the client gets the fallback.
                                let shown = meta.or_else(|| enabled.then(HdrMetadata::fallback));
                                link.send(&feedback::hdr_mode(enabled, shown.as_ref()), "HDR mode");
                            }
                        }
                        Some(Inbound::RequestIdr) => control.request_keyframe(),
                        Some(Inbound::InvalidateRefs { first, last }) => {
                            let range = shared
                                .frames
                                .lock()
                                .expect("frame map")
                                .backend_range(first, last);
                            match range {
                                Some((lo, hi)) => control.invalidate(lo, hi),
                                // Frames we can't place: a keyframe is the safe answer.
                                None => control.request_keyframe(),
                            }
                        }
                        Some(Inbound::Input(packet)) => match input::parse(packet) {
                            Ok(Some(event)) => {
                                shared.stats.input_events();
                                control.input(event);
                            }
                            Ok(None) => {}
                            Err(e) => {
                                tracing::debug!("dropping an input packet: {e}");
                                shared.stats.malformed_control();
                            }
                        },
                        Some(Inbound::Ignored(_)) | None => {}
                    }
                }
            }
        }
    }

    // Tell the client why the stream ends, unless it is the one that left.
    if link.peer.is_some() && shared.end_reason() != Some(EndReason::ClientLeft) {
        link.send(
            &feedback::termination(feedback::TERMINATED_BY_SERVER),
            "termination",
        );
        let _ = link.host.flush().await;
    }
    tracing::debug!("control stream stopped");
}

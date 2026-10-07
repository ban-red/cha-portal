//! The audio stream: backend Opus packets in, RTP with FEC to the client out.

use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::backend::OpusPacket;
use crate::handoff::SessionHandoff;

use super::ping::is_client_ping;
use super::{Shared, stopped};

pub(crate) mod packetizer;

use packetizer::AudioPacketizer;

pub(crate) struct AudioTask {
    pub socket: UdpSocket,
    pub handoff: SessionHandoff,
    pub packets: mpsc::Receiver<OpusPacket>,
    pub shared: Arc<Shared>,
}

pub(crate) async fn run(task: AudioTask) {
    let AudioTask {
        socket,
        handoff,
        mut packets,
        shared,
    } = task;
    let mut stop = shared.stop_rx();
    let key = handoff
        .encryption
        .audio
        .then_some((handoff.keys.key, handoff.keys.key_id));
    let mut packetizer = AudioPacketizer::new(key);
    let mut client = None;
    let mut buf = [0u8; 64];

    loop {
        tokio::select! {
            () = stopped(&mut stop) => break,
            packet = packets.recv() => {
                let Some(packet) = packet else {
                    // A backend with no audio closes the channel at once; the stream goes on.
                    tracing::debug!("audio channel closed");
                    // Wait out the session instead of spinning on a closed channel.
                    stopped(&mut stop).await;
                    break;
                };
                if !shared.is_started() {
                    continue;
                }
                let Some(to) = client else { continue };
                for datagram in packetizer.push(&packet.data, packet.samples) {
                    if let Err(e) = socket.send_to(&datagram, to).await {
                        tracing::warn!("audio send: {e}");
                    }
                }
                shared.stats.audio_sent();
            }
            received = socket.recv_from(&mut buf) => {
                let Ok((n, from)) = received else { continue };
                if is_client_ping(&buf[..n], from, handoff.params.client_ip, &handoff.ping_payload) {
                    if client != Some(from) {
                        tracing::debug!("audio client at {from}");
                        client = Some(from);
                    }
                } else {
                    shared.stats.rejected_pings();
                }
            }
        }
    }
    tracing::debug!("audio stream stopped");
}

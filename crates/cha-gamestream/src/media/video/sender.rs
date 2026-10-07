// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: GSO is an internal detail with a per-packet fallback on every platform (quinn-udp reports one
// segment where the OS has no GSO), and the socket stays unconnected so PING sources can be checked.

//! The video UDP socket: sends a frame's shards in as few syscalls as the
//! platform allows, and receives the client's `PING`s.

use std::io;
use std::net::SocketAddr;

use quinn_udp::{Transmit, UdpSockRef, UdpSocketState};
use tokio::net::UdpSocket;

use super::packetizer::ShardBatch;

/// The largest payload of one UDP datagram over IPv4; a GSO send is one
/// datagram until the kernel splits it.
const MAX_UDP_PAYLOAD: usize = 65507;

/// Shards per GSO send: the kernel's segment cap or the datagram cap,
/// whichever binds first, never zero.
fn segments_per_send(max_gso_segments: usize, shard_size: usize) -> usize {
    max_gso_segments
        .min(MAX_UDP_PAYLOAD.checked_div(shard_size).unwrap_or(0))
        .max(1)
}

pub(crate) struct VideoSocket {
    socket: UdpSocket,
    state: UdpSocketState,
}

impl VideoSocket {
    pub fn new(socket: UdpSocket) -> io::Result<Self> {
        let state = UdpSocketState::new(UdpSockRef::from(&socket))?;
        Ok(Self { socket, state })
    }

    pub async fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        self.socket.recv_from(buf).await
    }

    /// Sends the batch to `to`. Returns how many chunks the kernel refused
    /// as GSO and that went out one shard at a time instead.
    pub async fn send_batch(&self, batch: &ShardBatch, to: SocketAddr) -> usize {
        let shard_size = batch.shard_size();
        if shard_size == 0 {
            return 0;
        }
        if self.state.max_gso_segments() <= 1 {
            self.send_each(batch.as_bytes(), shard_size, to).await;
            return 0;
        }
        let per_send = segments_per_send(self.state.max_gso_segments(), shard_size);
        let mut fallbacks = 0;
        for chunk in batch.as_bytes().chunks(per_send * shard_size) {
            let transmit = Transmit {
                destination: to,
                ecn: None,
                contents: chunk,
                segment_size: Some(shard_size),
                src_ip: None,
            };
            let result = loop {
                match self
                    .state
                    .try_send(UdpSockRef::from(&self.socket), &transmit)
                {
                    // A frame's burst can outrun the socket buffer: wait, resend the same chunk.
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        let _ = self.socket.writable().await;
                    }
                    other => break other,
                }
            };
            if result.is_err() {
                fallbacks += 1;
                self.send_each(chunk, shard_size, to).await;
            }
        }
        fallbacks
    }

    async fn send_each(&self, bytes: &[u8], shard_size: usize, to: SocketAddr) {
        for shard in bytes.chunks(shard_size) {
            if let Err(e) = self.socket.send_to(shard, to).await {
                tracing::warn!("video send: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gso_chunks_respect_both_caps() {
        assert_eq!(segments_per_send(8, 100), 8);
        assert_eq!(segments_per_send(64, 2000), 32);
        assert_eq!(segments_per_send(64, MAX_UDP_PAYLOAD + 1), 1);
        assert_eq!(segments_per_send(64, 0), 1);
    }

    #[tokio::test]
    async fn a_batch_arrives_shard_by_shard() {
        let tx = VideoSocket::new(UdpSocket::bind("127.0.0.1:0").await.unwrap()).unwrap();
        let rx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let batch = {
            let mut p = super::super::packetizer::Packetizer::new(
                super::super::packetizer::PacketizerConfig {
                    packet_size: 512,
                    fec_percent: 20,
                    min_fec_packets: 0,
                    key: None,
                },
            );
            p.packetize(&vec![7u8; 5000], true, 1, 0, 0).unwrap()
        };
        tx.send_batch(&batch, rx.local_addr().unwrap()).await;
        let mut buf = [0u8; 2048];
        for expected in batch.shards() {
            let (n, _) =
                tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv_from(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(&buf[..n], expected);
        }
    }
}

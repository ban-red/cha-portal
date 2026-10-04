//! WebTransport path: media on datagrams, JSON-line control on one
//! client-opened bidirectional stream.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::{MissedTickBehavior, interval, interval_at};
use tracing::{info, warn};
use wtransport::endpoint::IncomingSession;
use wtransport::endpoint::endpoint_side::Server;
use wtransport::{Connection, Endpoint, VarInt};

use crate::stream::StreamLibrary;
use crate::traffic::{
    ClientMsg, FrameSource, SenderStats, ServerMsg, TrafficConfig, TransportStats, query_value,
};

const STATS_INTERVAL: Duration = Duration::from_millis(500);
/// Bytes wtransport prepends to each datagram (session-id varint), rounded up.
const WT_DATAGRAM_OVERHEAD: usize = 8;

pub async fn serve(endpoint: Endpoint<Server>, streams: Arc<StreamLibrary>) {
    loop {
        let incoming = endpoint.accept().await;
        let streams = Arc::clone(&streams);
        tokio::spawn(async move {
            if let Err(err) = handle(incoming, &streams).await {
                warn!("webtransport session ended: {err:#}");
            }
        });
    }
}

async fn handle(incoming: IncomingSession, streams: &StreamLibrary) -> Result<()> {
    let request = incoming.await?;
    let path = request.path().to_owned();
    let config = TrafficConfig::from_query(&path)?;
    // `/stream?name=…` replays an encoded stream; anything else is synthetic.
    let stream = if path.starts_with("/stream") {
        let name = query_value(&path, "name").context("missing ?name=")?;
        Some(streams.load(name)?)
    } else {
        None
    };
    let remote = request.remote_address();
    let conn = request.accept().await?;
    info!(%remote, ?config, "webtransport session");

    let (mut ctl_send, ctl_recv) = conn
        .accept_bi()
        .await
        .context("waiting for the client's control stream")?;
    let epoch = Instant::now();
    let max_datagram = conn
        .max_datagram_size()
        .context("peer does not accept datagrams")?
        .min(config.dgram);
    let mut source = match &stream {
        Some(stream) => {
            FrameSource::replay(config, Arc::clone(&stream.frames), max_datagram, epoch)?
        }
        None => FrameSource::synthetic(config, max_datagram, epoch)?,
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut line = serde_json::to_vec(&msg)?;
            line.push(b'\n');
            ctl_send.write_all(&line).await?;
        }
        anyhow::Ok(())
    });
    let pong_tx = tx.clone();
    let reader = tokio::spawn(async move {
        let mut lines = BufReader::new(ctl_recv).lines();
        while let Some(line) = lines.next_line().await? {
            match serde_json::from_str::<ClientMsg>(&line) {
                Ok(ClientMsg::Ping { c }) => {
                    let s_us = epoch.elapsed().as_micros() as u64;
                    let _ = pong_tx.send(ServerMsg::Pong { c, s_us });
                }
                Err(err) => warn!("bad control message: {err}"),
            }
        }
        anyhow::Ok(())
    });

    tx.send(ServerMsg::Hello {
        config,
        frame_bytes: source.max_frame_bytes(),
        fragments_per_frame: source.fragments_per_frame(),
        max_datagram,
        total_frames: config.total_frames(),
        stream: stream.as_ref().map(|s| s.header.clone()),
    })?;

    let mut stats = SenderStats::default();
    let mut frames = interval(config.frame_interval());
    frames.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut report = interval_at(tokio::time::Instant::now() + STATS_INTERVAL, STATS_INTERVAL);

    while stats.frames_generated < config.total_frames() {
        tokio::select! {
            _ = frames.tick() => {
                stats.frames_generated += 1;
                // quinn's send_datagram silently evicts the oldest queued datagram
                // when full, so admit whole frames only while they fit.
                let frame_budget =
                    source.frame_wire_bytes() + source.next_fragments() * WT_DATAGRAM_OVERHEAD;
                if conn.quic_connection().datagram_send_buffer_space() < frame_budget {
                    stats.frames_dropped_backpressure += 1;
                    continue;
                }
                source.next_frame(Instant::now(), |datagram| match conn.send_datagram(datagram) {
                    Ok(()) => {
                        stats.datagrams_sent += 1;
                        stats.bytes_sent += datagram.len() as u64;
                    }
                    Err(_) => stats.datagrams_refused += 1,
                });
                stats.frames_sent += 1;
            }
            _ = report.tick() => {
                tx.send(ServerMsg::Stats {
                    elapsed_ms: epoch.elapsed().as_millis() as u64,
                    sender: stats,
                    transport: transport_stats(&conn),
                })?;
            }
            err = conn.closed() => {
                reader.abort();
                return Err(err).context("client closed the session early");
            }
        }
    }

    tx.send(ServerMsg::Done { sender: stats })?;
    info!(%remote, ?stats, "webtransport run complete");
    reader.abort();
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    // Give the client a moment to read the final message before closing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    conn.close(VarInt::from_u32(0), b"done");
    Ok(())
}

fn transport_stats(conn: &Connection) -> TransportStats {
    let quic = conn.quic_connection();
    let path = quic.stats().path;
    TransportStats {
        rtt_ms: Some(path.rtt.as_secs_f64() * 1e3),
        cwnd: Some(path.cwnd),
        lost_packets: Some(path.lost_packets),
        mtu: Some(path.current_mtu),
        send_buffer_free: Some(quic.datagram_send_buffer_space()),
        buffered_amount: None,
    }
}

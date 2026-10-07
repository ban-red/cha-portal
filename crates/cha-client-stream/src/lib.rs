//! The native client's `cha-stream/1` receiver (ADR 0013): connects to a Cha
//! streamer over WebTransport as the browser player does, and returns a
//! [`cha_client::Session`]. Platform-neutral: it builds on Linux too.
//!
//! ```ignore
//! let target = Target { urls, cert_hash, codec };       // from POST /connect
//! let session = cha_client_stream::connect(&target, 2560, 1440).await?;
//! ```
//!
//! What is here (the contract is `docs/plans/c2-transport.md`; the browser
//! side being ported is `web/packages/player/src/wt-worker.ts`, `fec.ts`, the
//! control, ping and report parts of `player.ts`, `input.ts` and
//! `controllers/manager.ts`):
//!
//! - [`net`]: the connection. TLS is pinned to the certificate hash the
//!   portal gives and nothing else is trusted; every address is tried at
//!   once and the first that is ready wins (2.5 s each); big receive buffers
//!   and a keep-alive.
//! - [`session`]: the task that owns a connection. One bidirectional stream
//!   carries JSON lines both ways (the first, a `ping`, is written at once);
//!   everything else is datagrams. Clock sync from ping/pong every second, a
//!   `report` every 100 ms, a 4 s silence watchdog that ends the session. The
//!   whole QUIC side runs on a thread of its own per session (see the
//!   module's notes for why).
//! - [`receiver`] and [`video`]: the Sans-IO receive logic with injected
//!   time: datagram demux, video reassembly with FEC recovery, in-order
//!   delivery gated on a keyframe (or a RECOVERY frame after loss), `rfi`
//!   then `keyframe` requests, the loss accounting behind the report, audio
//!   ordering. Unit-testable without sockets.
//! - [`control`]: the control stream's lines, parsed and built.
//! - [`input`]: [`cha_client::Input`] as the browser's `{"t":"input"}` lines,
//!   gated on holding the floor.
//!
//! Video leaves as [`cha_client::VideoFrame`]s: Annex-B access units for H.264
//! and HEVC, OBUs for AV1, exactly as the streamer sent them. Parameter sets
//! ride in every keyframe; a frame flagged RECOVERY (after the streamer
//! referred around a loss) is a non-key P-frame the decoder takes as it is.
//! PyroWave frames (flagged INTRA) leave as the concatenated wavelet packets
//! that arrived whole, [`cha_client::VideoFrame::partial`] when some did not.
//! Audio leaves as 10 ms Opus packets, 48 kHz stereo.
//!
//! `cargo run --release -p cha-client-stream --example bench` measures the
//! receive path against a PyroWave-shaped stream over loopback (frames per
//! second, delivery jitter, CPU); see the example's header.
//!
//! Not here: finding or launching the environment and the media token (the
//! portal transport, `cha-client-portal`), reconnecting, codec switching, the
//! clipboard, the streamer's cursor images.

pub mod control;
pub mod input;
pub mod net;
pub mod receiver;
mod session;
pub mod video;

pub use session::{Target, connect};

//! The client half (ADR 0011): what a Moonlight client does, for Cha Player's
//! GameStream transport, the gateway and the node's discovery and pairing.
//!
//! - [`front`]: the host's HTTP and HTTPS API (serverinfo, pairing, applist,
//!   launch, resume, cancel) with our client certificate, and RTSP. A launch
//!   ends in a [`StreamSetup`].
//! - [`media`]: one stream from a [`StreamSetup`]: the ENet control channel,
//!   video and audio, out as plain channels, and input and feedback.
//!
//! The halves meet only through [`StreamSetup`], as the host's do through
//! [`crate::handoff::SessionHandoff`], and share the host's types, so both
//! sides of the protocol use one vocabulary.

use std::net::IpAddr;
use std::time::Instant;

use bytes::Bytes;
use serde::{Deserialize, Serialize};

pub use crate::backend::Feedback;
pub use crate::handoff::{AudioParams, Chroma, Encryption, MediaPorts, SessionKeys, VideoCodec};
pub use crate::input::InputEvent;

pub mod front;
pub mod media;

/// What [`front`] negotiated (launch, then RTSP), and all [`media`] needs to
/// run the stream. It holds the session's key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamSetup {
    /// The host's address the streams come from and go to.
    pub host: IpAddr,
    /// From RTSP `SETUP`: where the host's video, control and audio are.
    pub ports: MediaPorts,
    /// The key and id we gave in `/launch` (`rikey`, `rikeyid`).
    pub keys: SessionKeys,
    /// What the host agreed to encrypt (RTSP `ANNOUNCE`).
    pub encryption: Encryption,
    /// From `SETUP`'s `X-SS-Connect-Data`: presented when connecting the
    /// control peer.
    pub control_connect_data: u32,
    /// From `SETUP`'s `X-SS-Ping-Payload`: carried in our `PING`s.
    pub ping_payload: [u8; 16],
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Video packet size we asked for (bytes of NV header and payload).
    pub packet_size: usize,
    pub hdr: bool,
    pub chroma: Chroma,
    pub audio: AudioParams,
}

/// One access unit as the host sent it: Annex-B for H.264/HEVC, OBUs for AV1.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub data: Bytes,
    pub key: bool,
    /// The host's frame number, increasing.
    pub number: u32,
    /// When its last packet arrived.
    pub received: Instant,
}

/// One Opus packet from the host.
#[derive(Clone, Debug)]
pub struct AudioPacket {
    pub data: Bytes,
    /// RTP timestamp, for ordering and gaps.
    pub timestamp: u32,
}

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
    /// Set when the stream is PyroWave (the Vibepollo contract, never chosen
    /// automatically). [`codec`](Self::codec) is then a placeholder (H.264)
    /// that nothing may consult; use [`is_pyrowave`](Self::is_pyrowave).
    #[serde(default)]
    pub pyrowave: Option<PyrowaveSetup>,
}

impl StreamSetup {
    pub fn is_pyrowave(&self) -> bool {
        self.pyrowave.is_some()
    }
}

/// What was negotiated for a PyroWave stream (8-bit only for now).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PyrowaveSetup {
    /// The bitstream id the host advertised (`x-ss-pyrowave.bitstream`), one
    /// of [`front::PYROWAVE_BITSTREAMS`].
    pub bitstream: String,
    /// Whether we asked for record framing (`pyrowaveFeatures` bit 0). The
    /// frame itself says which framing it is in, by the first word's bit 31.
    pub record_framing: bool,
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
    /// Set on PyroWave frames: [`data`](Self::data) is then the frame's
    /// sequence header followed by the whole block records that arrived (the
    /// packets a PyroWave decoder takes, concatenated), and this says where
    /// they are and what was lost. Always `key`: PyroWave frames stand alone.
    pub pyrowave: Option<PyrowaveFrame>,
}

/// How a PyroWave frame was framed on the wire (see `docs/plans/vibepollo-pyrowave.md` section 3.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PyrowaveFraming {
    /// 32-bit records: sequence header, block records, padding.
    Record,
    /// A packet count, then each packet behind its size. Any loss drops the frame.
    LengthPrefixed,
}

/// One unit of a PyroWave frame in [`VideoFrame::data`]: the sequence header,
/// or one whole block record (exactly an upstream PyroWave "packet" of one block).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PyrowaveRecord {
    pub offset: u32,
    pub len: u32,
    /// Part of the coarsest wavelet level (and the sequence header): what a
    /// decoder can't do without. Maps to `cha-stream/1`'s `CRITICAL`.
    pub critical: bool,
}

/// What a PyroWave frame carries and what the network cost it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PyrowaveFrame {
    pub framing: PyrowaveFraming,
    /// The sequence header first, then block records in the order they arrived.
    pub records: Vec<PyrowaveRecord>,
    pub width: u32,
    pub height: u32,
    pub chroma: Chroma,
    /// `total_blocks` of the sequence header.
    pub total_blocks: u32,
    /// Distinct blocks among the records kept.
    pub blocks_received: u32,
    /// Every block of the coarsest level is among them.
    pub coarse_complete: bool,
    /// Block records dropped for lost bytes, or lost with their header.
    pub records_skipped: u32,
    /// Video packets of the frame, and the ones that never came (zero-filled).
    pub packets: u32,
    pub packets_lost: u32,
}

impl PyrowaveFrame {
    /// The record's bytes in `data`.
    pub fn record<'a>(&self, data: &'a [u8], index: usize) -> Option<&'a [u8]> {
        let r = self.records.get(index)?;
        data.get(r.offset as usize..(r.offset + r.len) as usize)
    }
}

/// One Opus packet from the host.
#[derive(Clone, Debug)]
pub struct AudioPacket {
    pub data: Bytes,
    /// RTP timestamp, for ordering and gaps.
    pub timestamp: u32,
}

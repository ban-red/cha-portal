//! A GameStream (Moonlight protocol) host, in two halves that don't depend on each other.
//!
//! * [`front`]: nvhttp (server info, pairing, apps, launch, resume, cancel),
//!   RTSP and mDNS. Run by whoever faces the clients. It learns what a client
//!   wants and gives a [`SessionHandoff`](handoff::SessionHandoff) to a
//!   [`Directory`](directory::Directory).
//! * [`media`]: one session's control (ENet), video and audio streams on
//!   their own ports, fed by a [`MediaBackend`](backend::MediaBackend). Run
//!   wherever the encoder is.
//!
//! The crate knows the protocol and nothing of the engine behind it; see the
//! README for the boundary, what was ported from Moonshine and the security
//! fixes made on the way.

pub mod backend;
pub mod client;
pub mod config;
pub mod directory;
pub mod front;
pub mod handoff;
pub mod hdr;
pub mod input;
pub mod media;

mod crypto;
mod net;

pub use backend::{
    Capabilities, EncodedVideo, Feedback, HdrMetadata, MediaBackend, MediaControl, MediaStreams,
    OpusPacket,
};
pub use config::{HostConfig, Ports, generate_unique_id};
pub use directory::{
    App, Directory, DirectoryError, LaunchRequest, MemoryPairingStore, PairedClient,
    PairingAttempt, PairingStore, PinSender, PinWaiter, ResumeRequest, SessionTarget, StoreError,
    pin_channel,
};
pub use front::identity::Identity;
pub use front::{Host, HostBuilder, HostError, HostHandle, RunningHost};
pub use handoff::{
    AudioParams, ClientId, Encryption, MediaPorts, SessionHandoff, SessionKeys, StreamParams,
    VideoCodec,
};
pub use input::InputEvent;
pub use media::{EndReason, MediaConfig, MediaSession, MediaSockets, MediaStatsSnapshot};

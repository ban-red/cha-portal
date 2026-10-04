//! `cha-stream/1` wire framing.
//!
//! Sans-IO building blocks shared by the streamer, the native client and (via
//! wasm) the browser player. Nothing here touches sockets or clocks; callers
//! pass timestamps in and get bytes/events out.
//!
//! Every media datagram starts with a fixed 16-byte [`DatagramHeader`]. Frames
//! larger than one datagram are split by [`Fragmenter`] and rebuilt by
//! [`Reassembler`].

mod header;
mod reassembly;

pub use header::{DatagramHeader, DecodeError, Flags, HEADER_LEN, Kind, WIRE_VERSION};
pub use reassembly::{
    CompletedFrame, DropReason, DroppedFrame, Fragmenter, FrameTooLarge, ReassembleEvent,
    Reassembler, ReassemblerConfig,
};

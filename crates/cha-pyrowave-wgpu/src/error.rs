//! Errors from creating pipelines and decoders and from parsing packets.

/// Failures that are the caller's to handle.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The device was created without `wgpu::Features::SUBGROUP`. The dequant shader
    /// needs subgroup operations; there is no subgroup-free path.
    #[error("the wgpu device lacks Features::SUBGROUP, which PyroWave decoding needs")]
    SubgroupsUnsupported,
    /// The frame size is zero or beyond what the 14-bit sequence header can carry.
    #[error("frame size {width}x{height} is out of range (1..=16384)")]
    BadDimensions { width: u32, height: u32 },
    /// A shader failed to compile or a pipeline failed validation.
    #[error("{label}: {message}")]
    Pipeline {
        label: &'static str,
        message: String,
    },
    /// A packet is malformed or does not belong to this decoder.
    #[error("bad packet: {0}")]
    Packet(#[from] PacketError),
    /// A `.pyrowave` container is truncated or has the wrong magic.
    #[error("bad .pyrowave file: {0}")]
    File(&'static str),
}

/// Why a packet was rejected. The decoder stays usable; the frame in progress may
/// be incomplete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PacketError {
    #[error("chroma resolution mismatch")]
    ChromaMismatch,
    #[error("unrecognized sequence header mode {0}")]
    UnknownMode(u32),
    #[error("dimension mismatch: {width}x{height}")]
    DimensionMismatch { width: u32, height: u32 },
    #[error("packet claims {claimed} bytes, {left} left")]
    Truncated { claimed: usize, left: usize },
    #[error("block_index {0} out of bounds")]
    BlockOutOfBounds(u32),
    #[error("payload_words smaller than its header")]
    ShortPayload,
    #[error("did not consume packet completely")]
    Trailing,
}

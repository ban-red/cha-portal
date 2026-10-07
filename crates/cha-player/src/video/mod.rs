//! Video decode. The core hands over Annex-B; a [`VideoDecoder`] turns it into
//! a picture the presenter can draw without a copy.

pub mod annexb;
mod videotoolbox;

use std::time::Instant;

use anyhow::Result;
use cha_client::{Codec, VideoFrame};

pub use videotoolbox::PixelBuffer;

/// Which YCbCr → RGB matrix a picture wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// How a decoded picture's samples map to colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorSpec {
    /// Samples use the whole code range, not 16..235.
    pub full_range: bool,
    /// 10 bits per sample (in the top of 16) rather than 8.
    pub ten_bit: bool,
    pub matrix: Matrix,
}

/// A decoded picture, still in the decoder's own memory.
pub struct DecodedFrame {
    /// When the transport got the encoded frame.
    pub received: Instant,
    /// Wall time the decoder took, in milliseconds.
    pub decode_ms: f32,
    pub image: PixelBuffer,
}

impl DecodedFrame {
    pub fn width(&self) -> u32 {
        self.image.width()
    }

    pub fn height(&self) -> u32 {
        self.image.height()
    }
}

/// Decodes one stream. `Ok(None)` means the frame produced no picture (it
/// carried only parameter sets); an error means the picture is lost and the
/// caller should ask the host for a key frame.
pub trait VideoDecoder: Send {
    fn decode(&mut self, frame: VideoFrame) -> Result<Option<DecodedFrame>>;
}

/// The best decoder this platform has for `codec`.
pub fn new_decoder(codec: Codec) -> Result<Box<dyn VideoDecoder>> {
    Ok(Box::new(videotoolbox::VideoToolboxDecoder::new(codec)?))
}

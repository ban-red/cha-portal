//! Getting a decoded picture onto the GPU without copying it: the decoder's
//! IOSurface-backed buffer becomes two `wgpu` textures (luma and chroma) that
//! alias its memory.

mod metal;

use anyhow::Result;

use crate::video::{ColorSpec, DecodedFrame};

/// A decoded picture as two sampled planes: `luma` is R (8 or 16 bits),
/// `chroma` is RG at half size.
pub struct ImportedFrame {
    pub luma: wgpu::Texture,
    pub chroma: wgpu::Texture,
    pub width: u32,
    pub height: u32,
    pub color: ColorSpec,
    /// Whatever must outlive the GPU's use of the planes (the platform
    /// texture wrappers and the decoder buffer).
    _hold: Box<dyn std::any::Any>,
}

/// Makes [`ImportedFrame`]s for one `wgpu` device.
pub trait FrameImporter {
    fn import(&mut self, frame: &DecodedFrame) -> Result<ImportedFrame>;
}

pub fn new_importer(device: &wgpu::Device) -> Result<Box<dyn FrameImporter>> {
    Ok(Box::new(metal::MetalImporter::new(device)?))
}

//! Zero-copy import on Metal: `CVMetalTextureCache` wraps each plane of the
//! decoded `CVPixelBuffer` in an `MTLTexture` (same IOSurface, no copy), and
//! `wgpu`'s Metal HAL adopts that texture as a `wgpu::Texture`.

use std::ptr::{self, NonNull};

use anyhow::{Result, anyhow, bail};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CFRetained;
use objc2_core_video::{
    CVMetalTexture, CVMetalTextureCache, CVMetalTextureGetTexture, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPlaneCount, CVPixelBufferGetWidthOfPlane,
};
use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTexture, MTLTextureType};

use super::{FrameImporter, ImportedFrame};
use crate::video::DecodedFrame;

pub struct MetalImporter {
    device: wgpu::Device,
    cache: CFRetained<CVMetalTextureCache>,
    imported: u64,
}

impl MetalImporter {
    pub fn new(device: &wgpu::Device) -> Result<Self> {
        // SAFETY: only reads the Metal device wgpu already owns.
        let mtl_device: Retained<ProtocolObject<dyn MTLDevice>> = unsafe {
            device
                .as_hal::<wgpu::hal::api::Metal>()
                .ok_or_else(|| anyhow!("wgpu is not running on Metal"))?
                .raw_device()
                .clone()
        };
        let mut cache: *mut CVMetalTextureCache = ptr::null_mut();
        // SAFETY: valid device and out pointer.
        let status = unsafe {
            CVMetalTextureCache::create(None, None, &mtl_device, None, NonNull::from(&mut cache))
        };
        let cache = NonNull::new(cache)
            .filter(|_| status == 0)
            .ok_or_else(|| anyhow!("CVMetalTextureCache creation failed ({status})"))?;
        Ok(Self {
            device: device.clone(),
            // SAFETY: Create returned +1.
            cache: unsafe { CFRetained::from_raw(cache) },
            imported: 0,
        })
    }

    /// One plane of `frame` as a `wgpu::Texture`, and the CoreVideo wrapper
    /// that has to stay alive with it.
    fn plane(
        &self,
        frame: &DecodedFrame,
        index: usize,
        mtl_format: MTLPixelFormat,
        format: wgpu::TextureFormat,
    ) -> Result<(wgpu::Texture, CFRetained<CVMetalTexture>)> {
        let buffer = &frame.image.0;
        let width = CVPixelBufferGetWidthOfPlane(buffer, index);
        let height = CVPixelBufferGetHeightOfPlane(buffer, index);
        let mut cv_texture: *mut CVMetalTexture = ptr::null_mut();
        // SAFETY: valid cache and buffer; the out pointer is ours.
        let status = unsafe {
            CVMetalTextureCache::create_texture_from_image(
                None,
                &self.cache,
                buffer,
                None,
                mtl_format,
                width,
                height,
                index,
                NonNull::from(&mut cv_texture),
            )
        };
        let cv_texture = NonNull::new(cv_texture)
            .filter(|_| status == 0)
            .ok_or_else(|| anyhow!("no Metal texture for plane {index} ({status})"))?;
        // SAFETY: Create returned +1.
        let cv_texture = unsafe { CFRetained::from_raw(cv_texture) };
        let mtl_texture: Retained<ProtocolObject<dyn MTLTexture>> =
            CVMetalTextureGetTexture(&cv_texture)
                .ok_or_else(|| anyhow!("CVMetalTexture holds no MTLTexture"))?;

        // SAFETY: the texture is a live 2D, single-mip, shader-readable
        // Metal texture in `format`, on wgpu's own device.
        let hal = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                mtl_texture,
                format,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width: width as u32,
                    height: height as u32,
                    depth: 1,
                },
            )
        };
        let descriptor = wgpu::TextureDescriptor {
            label: Some("decoded plane"),
            size: wgpu::Extent3d {
                width: width as u32,
                height: height as u32,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        // SAFETY: `hal` was made for this descriptor on this device.
        let texture = unsafe {
            self.device
                .create_texture_from_hal::<wgpu::hal::api::Metal>(hal, &descriptor)
        };
        Ok((texture, cv_texture))
    }
}

impl FrameImporter for MetalImporter {
    fn import(&mut self, frame: &DecodedFrame) -> Result<ImportedFrame> {
        if CVPixelBufferGetPlaneCount(&frame.image.0) != 2 {
            bail!("decoded picture is not bi-planar YCbCr");
        }
        let ten_bit = frame.image.ten_bit();
        let (luma_formats, chroma_formats) = if ten_bit {
            (
                (MTLPixelFormat::R16Unorm, wgpu::TextureFormat::R16Unorm),
                (MTLPixelFormat::RG16Unorm, wgpu::TextureFormat::Rg16Unorm),
            )
        } else {
            (
                (MTLPixelFormat::R8Unorm, wgpu::TextureFormat::R8Unorm),
                (MTLPixelFormat::RG8Unorm, wgpu::TextureFormat::Rg8Unorm),
            )
        };
        let (luma, luma_cv) = self.plane(frame, 0, luma_formats.0, luma_formats.1)?;
        let (chroma, chroma_cv) = self.plane(frame, 1, chroma_formats.0, chroma_formats.1)?;

        self.imported += 1;
        if self.imported.is_multiple_of(120) {
            self.cache.flush(0);
        }
        Ok(ImportedFrame {
            luma,
            chroma,
            width: frame.width(),
            height: frame.height(),
            color: frame.image.color(),
            _hold: Box::new((luma_cv, chroma_cv, frame.image.0.clone())),
        })
    }
}

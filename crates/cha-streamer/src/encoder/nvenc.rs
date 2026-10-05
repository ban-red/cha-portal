//! NVENC behind [`VideoEncoder`]: `cha-nvenc`'s session, fed the output
//! buffer's CUDA view. Nothing here changes how it encodes.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, anyhow};
use cha_nvenc::{CudaContext, Encoder, EncoderConfig, InputFormat, Timings};

use super::{Params, VideoEncoder};
use crate::compositor::MAX_SIZE;
use crate::media::Frame;

pub struct Nvenc {
    inner: Encoder,
    params: Params,
    timings: Timings,
}

impl Nvenc {
    pub fn new(cuda: Arc<CudaContext>, params: Params) -> Result<Self> {
        let inner = Encoder::new(
            cuda,
            EncoderConfig {
                codec: params.codec,
                input: InputFormat::Argb,
                width: params.width,
                height: params.height,
                max_width: MAX_SIZE.0,
                max_height: MAX_SIZE.1,
                fps: params.fps,
                bitrate_bps: params.bitrate_bps,
            },
        )
        .map_err(|e| anyhow!("{e}"))?;
        Ok(Self {
            inner,
            params,
            timings: Timings::default(),
        })
    }
}

impl VideoEncoder for Nvenc {
    fn params(&self) -> &Params {
        &self.params
    }

    fn encode(&mut self, frame: &Frame, keyframe: bool, out: &mut Vec<u8>) -> Result<bool> {
        let surface_started = Instant::now();
        let image = frame
            .slot
            .cuda_image()
            .ok_or_else(|| anyhow!("NVENC takes frames the compositor shares through CUDA"))?;
        let (surface, _, _) = image.surface().map_err(|e| anyhow!("{e}"))?;
        let surface_time = surface_started.elapsed();
        let key = self
            .inner
            .encode(surface, keyframe, out)
            .map_err(|e| anyhow!("{e}"))?;
        // Getting the CUDA view counts with mapping the input.
        self.timings = self.inner.last_timings();
        self.timings.map += surface_time;
        Ok(key)
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        self.inner
            .resize(width, height)
            .map_err(|e| anyhow!("{e}"))?;
        self.params.width = width;
        self.params.height = height;
        Ok(())
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> Result<()> {
        self.inner
            .set_bitrate(bitrate_bps)
            .map_err(|e| anyhow!("{e}"))?;
        self.params.bitrate_bps = bitrate_bps;
        Ok(())
    }

    fn set_frame_rate(&mut self, fps: u32, bitrate_bps: u32) -> Result<()> {
        self.inner
            .set_frame_rate(fps, bitrate_bps)
            .map_err(|e| anyhow!("{e}"))?;
        self.params.fps = fps;
        self.params.bitrate_bps = bitrate_bps;
        Ok(())
    }

    fn next_index(&self) -> u64 {
        self.inner.next_index()
    }

    fn last_index(&self) -> u64 {
        self.inner.last_index()
    }

    fn invalidate_from(&mut self, from: u64) -> Result<bool> {
        self.inner.invalidate_from(from).map_err(|e| anyhow!("{e}"))
    }

    fn forget_surfaces(&mut self) {
        self.inner.forget_surfaces();
    }

    fn last_timings(&self) -> Timings {
        self.timings
    }
}

//! H.264 and HEVC decode on VideoToolbox's hardware decoders.
//!
//! Each access unit is split ([`super::annexb`]), the parameter sets build a
//! `CMVideoFormatDescription` (the session is recreated when they change), and
//! the picture NAL units go in as one AVCC sample. Decoding is synchronous and
//! real-time, with the sample marked display-immediately, so a frame comes out
//! as soon as it is decoded.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use cha_client::{Codec, VideoFrame};
use objc2_core_foundation::{
    CFArray, CFBoolean, CFDictionary, CFMutableDictionary, CFNumber, CFRetained, CFString, CFType,
};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
    kCMSampleAttachmentKey_DisplayImmediately,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetWidth, kCVImageBufferYCbCrMatrix_ITU_R_601_4,
    kCVImageBufferYCbCrMatrix_ITU_R_2020, kCVImageBufferYCbCrMatrixKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
    kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession, VTSessionSetProperty, kVTDecompressionPropertyKey_RealTime,
    kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
};

use super::annexb::{self, ParamSets};
use super::{ColorSpec, DecodedFrame, Matrix, VideoDecoder};

/// A decoded, IOSurface-backed `CVPixelBuffer`.
pub struct PixelBuffer(pub(crate) CFRetained<CVPixelBuffer>);

// SAFETY: CVPixelBuffer is an immutable-once-decoded, reference-counted
// object that CoreVideo documents as usable from any thread.
unsafe impl Send for PixelBuffer {}

impl PixelBuffer {
    pub fn width(&self) -> u32 {
        CVPixelBufferGetWidth(&self.0) as u32
    }

    pub fn height(&self) -> u32 {
        CVPixelBufferGetHeight(&self.0) as u32
    }

    pub fn pixel_format(&self) -> u32 {
        CVPixelBufferGetPixelFormatType(&self.0)
    }

    pub fn color(&self) -> ColorSpec {
        ColorSpec {
            full_range: self.full_range(),
            ten_bit: self.ten_bit(),
            matrix: self.matrix(),
        }
    }

    /// 10 bits per sample (P010-like) rather than 8.
    pub fn ten_bit(&self) -> bool {
        [
            kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
            kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
        ]
        .contains(&self.pixel_format())
    }

    /// Samples use the whole code range (0..255), not 16..235.
    pub fn full_range(&self) -> bool {
        [
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            kCVPixelFormatType_420YpCbCr10BiPlanarFullRange,
        ]
        .contains(&self.pixel_format())
    }

    /// From the buffer's attachment; BT.709 when the stream doesn't say.
    pub fn matrix(&self) -> Matrix {
        // SAFETY: a null mode pointer is allowed.
        let value = unsafe {
            self.0
                .attachment(kCVImageBufferYCbCrMatrixKey, ptr::null_mut())
        };
        let Some(value) = value.and_then(|v| v.downcast::<CFString>().ok()) else {
            return Matrix::Bt709;
        };
        // SAFETY: reading CoreVideo's constant strings.
        let (bt601, bt2020) = unsafe {
            (
                kCVImageBufferYCbCrMatrix_ITU_R_601_4,
                kCVImageBufferYCbCrMatrix_ITU_R_2020,
            )
        };
        if *value == *bt601 {
            Matrix::Bt601
        } else if *value == *bt2020 {
            Matrix::Bt2020
        } else {
            Matrix::Bt709
        }
    }
}

/// The decode callback's result, picked up right after the (synchronous)
/// decode call returns.
#[derive(Default)]
struct Slot(Mutex<Option<Result<CFRetained<CVImageBuffer>, i32>>>);

unsafe extern "C-unwind" fn on_frame(
    refcon: *mut c_void,
    _source: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _duration: CMTime,
) {
    // SAFETY: `refcon` is the `Slot` the decoder keeps alive for the session.
    let slot = unsafe { &*(refcon as *const Slot) };
    let result = match NonNull::new(image) {
        // SAFETY: VideoToolbox passes a valid image buffer, only for this call.
        Some(image) if status == 0 => Ok(unsafe { CFRetained::retain(image) }),
        _ => Err(if status == 0 { -1 } else { status }),
    };
    *slot.0.lock().unwrap() = Some(result);
}

struct Active {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    params: ParamSets,
}

impl Drop for Active {
    fn drop(&mut self) {
        // SAFETY: the session is ours; invalidate before the refcon goes.
        unsafe { self.session.invalidate() };
    }
}

pub struct VideoToolboxDecoder {
    codec: Codec,
    /// Boxed so the callback's refcon stays put.
    slot: Box<Slot>,
    active: Option<Active>,
}

impl VideoToolboxDecoder {
    pub fn new(codec: Codec) -> Result<Self> {
        match codec {
            Codec::H264 | Codec::Hevc => Ok(Self {
                codec,
                slot: Box::default(),
                active: None,
            }),
            Codec::Av1 => bail!("AV1 decode is not implemented yet"),
        }
    }

    fn format_description(&self, params: &ParamSets) -> Result<CFRetained<CMFormatDescription>> {
        let sets = params.ordered();
        let pointers: Vec<NonNull<u8>> = sets.iter().map(|s| NonNull::from(&s[0])).collect();
        let sizes: Vec<usize> = sets.iter().map(|s| s.len()).collect();
        let mut out: *const CMFormatDescription = ptr::null();
        // SAFETY: pointers and sizes describe `sets`, which outlive the call.
        let status = unsafe {
            match self.codec {
                Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    None,
                    sets.len(),
                    NonNull::new(pointers.as_ptr().cast_mut()).unwrap(),
                    NonNull::new(sizes.as_ptr().cast_mut()).unwrap(),
                    4,
                    NonNull::from(&mut out),
                ),
                _ => CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    None,
                    sets.len(),
                    NonNull::new(pointers.as_ptr().cast_mut()).unwrap(),
                    NonNull::new(sizes.as_ptr().cast_mut()).unwrap(),
                    4,
                    None,
                    NonNull::from(&mut out),
                ),
            }
        };
        match NonNull::new(out.cast_mut()) {
            // SAFETY: a Create function returned +1.
            Some(out) if status == 0 => Ok(unsafe { CFRetained::from_raw(out) }),
            _ => bail!("format description from the parameter sets failed ({status})"),
        }
    }

    fn open(&mut self, params: ParamSets) -> Result<()> {
        self.active = None;
        let format = self.format_description(&params)?;

        let key_format = unsafe { kCVPixelBufferPixelFormatTypeKey };
        // The decoder picks among the formats we can draw; it knows the
        // stream's range.
        let formats = [
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
        ]
        .map(|f| CFNumber::new_i32(f as i32));
        let format_refs: Vec<&CFType> = formats.iter().map(|n| n.as_ref()).collect();
        let formats = CFArray::from_objects(&format_refs);
        let empty = CFDictionary::<CFString, CFType>::empty();
        let yes = CFBoolean::new(true);
        let attrs = CFDictionary::<CFString, CFType>::from_slices(
            &[
                key_format,
                unsafe { kCVPixelBufferMetalCompatibilityKey },
                unsafe { kCVPixelBufferIOSurfacePropertiesKey },
            ],
            &[formats.as_ref(), yes.as_ref(), empty.as_ref()],
        );
        let spec = CFDictionary::<CFString, CFType>::from_slices(
            &[unsafe { kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder }],
            &[yes.as_ref()],
        );

        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(on_frame),
            decompressionOutputRefCon: &*self.slot as *const Slot as *mut c_void,
        };
        let mut session: *mut VTDecompressionSession = ptr::null_mut();
        // SAFETY: all arguments are valid for the call; the refcon outlives
        // the session (see `Active::drop`).
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                Some(spec.as_opaque()),
                Some(attrs.as_opaque()),
                &callback,
                NonNull::from(&mut session),
            )
        };
        let Some(session) = NonNull::new(session).filter(|_| status == 0) else {
            bail!(
                "VideoToolbox could not open a {:?} decoder ({status})",
                self.codec
            );
        };
        // SAFETY: Create returned +1.
        let session = unsafe { CFRetained::from_raw(session) };
        // SAFETY: valid session, key and value.
        unsafe {
            VTSessionSetProperty(
                &session,
                kVTDecompressionPropertyKey_RealTime,
                Some(yes.as_ref()),
            );
        }
        tracing::info!(codec = ?self.codec, "video decoder opened");
        self.active = Some(Active {
            session,
            format,
            params,
        });
        Ok(())
    }

    fn sample_buffer(&self, avcc: &[u8], number: u64) -> Result<CFRetained<CMSampleBuffer>> {
        let active = self.active.as_ref().expect("decoder is open");
        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        // SAFETY: a null block with AssureMemoryNow allocates `avcc.len()`
        // bytes, which `replace_data_bytes` then fills.
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                ptr::null_mut(),
                avcc.len(),
                None,
                ptr::null(),
                0,
                avcc.len(),
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut block),
            )
        };
        let block = NonNull::new(block)
            .filter(|_| status == 0)
            .ok_or_else(|| anyhow!("block buffer allocation failed ({status})"))?;
        // SAFETY: Create returned +1.
        let block = unsafe { CFRetained::from_raw(block) };
        // SAFETY: the destination has `avcc.len()` bytes.
        let status = unsafe {
            CMBlockBuffer::replace_data_bytes(
                NonNull::new(avcc.as_ptr().cast_mut().cast()).unwrap(),
                &block,
                0,
                avcc.len(),
            )
        };
        if status != 0 {
            bail!("block buffer fill failed ({status})");
        }

        let timing = CMSampleTimingInfo {
            duration: invalid_time(),
            presentationTimeStamp: CMTime {
                value: number as i64,
                timescale: 1000,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            },
            decodeTimeStamp: invalid_time(),
        };
        let size = avcc.len();
        let mut sample: *mut CMSampleBuffer = ptr::null_mut();
        // SAFETY: one sample, one timing, one size, all valid for the call.
        let status = unsafe {
            CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&active.format),
                1,
                1,
                &timing,
                1,
                &size,
                NonNull::from(&mut sample),
            )
        };
        let sample = NonNull::new(sample)
            .filter(|_| status == 0)
            .ok_or_else(|| anyhow!("sample buffer creation failed ({status})"))?;
        // SAFETY: Create returned +1.
        let sample = unsafe { CFRetained::from_raw(sample) };
        mark_display_immediately(&sample);
        Ok(sample)
    }
}

fn invalid_time() -> CMTime {
    CMTime {
        value: 0,
        timescale: 0,
        flags: CMTimeFlags::empty(),
        epoch: 0,
    }
}

/// Without this a decoder may hold frames back waiting to reorder them.
fn mark_display_immediately(sample: &CMSampleBuffer) {
    // SAFETY: the attachments array holds one mutable dictionary per sample.
    unsafe {
        let Some(array) = sample.sample_attachments_array(true) else {
            return;
        };
        let dict = array.value_at_index(0) as *const CFMutableDictionary;
        CFMutableDictionary::set_value(
            dict.as_ref(),
            kCMSampleAttachmentKey_DisplayImmediately as *const CFString as *const c_void,
            CFBoolean::new(true) as *const CFBoolean as *const c_void,
        );
    }
}

// SAFETY: VideoToolbox sessions can be used from any one thread at a time;
// the decoder is only ever used by one.
unsafe impl Send for VideoToolboxDecoder {}

impl VideoDecoder for VideoToolboxDecoder {
    fn decode(&mut self, frame: VideoFrame) -> Result<Option<DecodedFrame>> {
        let started = Instant::now();
        let sample = annexb::to_sample(frame.codec, &frame.data);
        if let Some(params) = sample.params.filter(|p| !p.is_empty()) {
            let changed = self.active.as_ref().is_none_or(|a| a.params != params);
            if changed {
                self.open(params)?;
            }
        }
        if sample.avcc.is_empty() {
            return Ok(None);
        }
        if self.active.is_none() {
            bail!("waiting for a key frame");
        }

        let buffer = self.sample_buffer(&sample.avcc, frame.number)?;
        let active = self.active.as_ref().expect("decoder is open");
        *self.slot.0.lock().unwrap() = None;
        let mut info = VTDecodeInfoFlags::empty();
        // SAFETY: valid session and sample; flags 0 decodes synchronously.
        let status = unsafe {
            active.session.decode_frame(
                &buffer,
                VTDecodeFrameFlags::empty(),
                ptr::null_mut(),
                &mut info,
            )
        };
        let result = self.slot.0.lock().unwrap().take();
        if status != 0 {
            // A session that errored (a format change it could not absorb,
            // or one the system invalidated) is rebuilt on the next key frame.
            self.active = None;
            bail!("VideoToolbox rejected a frame ({status})");
        }
        match result {
            Some(Ok(image)) => Ok(Some(DecodedFrame {
                received: frame.received,
                decode_ms: started.elapsed().as_secs_f32() * 1000.0,
                image: PixelBuffer(image),
            })),
            Some(Err(code)) => bail!("VideoToolbox could not decode a frame ({code})"),
            None => Ok(None),
        }
    }
}

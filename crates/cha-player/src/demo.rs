//! `--demo`: a fake host, so the whole decode → present → audio path runs
//! with no network.
//!
//! The host draws a moving test pattern, encodes it with VideoToolbox's H.264
//! encoder (real-time, no B-frames) in this process, and hands the Annex-B
//! frames over as a transport would; a sine tone goes out as Opus. The square
//! follows the mouse (absolute mode), which shows the input path end to end.

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use cha_client::{
    App, AudioPacket, BoxFuture, Codec, Ended, Host, Input, Pairing, Session, SessionControl,
    StreamConfig, Transport, VideoFrame,
};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMSampleBuffer, CMTime, CMTimeFlags, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelFormatType_32BGRA,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty,
    kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_ProfileLevel, kVTCompressionPropertyKey_RealTime,
    kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_Main_AutoLevel,
};
use tokio::sync::{mpsc as tmpsc, oneshot};

const HOST_ID: &str = "demo";

pub struct DemoTransport;

impl Transport for DemoTransport {
    fn name(&self) -> &str {
        "Demo"
    }

    fn hosts(&self) -> Vec<Host> {
        vec![Host {
            id: HOST_ID.into(),
            name: "Demo host (in this process)".into(),
            address: "local".into(),
            paired: true,
            running_app: None,
        }]
    }

    fn add_host(&self, _address: &str) -> BoxFuture<'_, Result<Host>> {
        Box::pin(async { bail!("the demo has one host") })
    }

    fn pair(&self, _host_id: &str) -> BoxFuture<'_, Result<Pairing>> {
        Box::pin(async { bail!("the demo host needs no pairing") })
    }

    fn apps(&self, _host_id: &str) -> BoxFuture<'_, Result<Vec<App>>> {
        Box::pin(async {
            Ok(vec![App {
                id: 1,
                name: "Test pattern".into(),
                hdr: false,
            }])
        })
    }

    fn launch(
        &self,
        _host_id: &str,
        _app_id: u32,
        config: StreamConfig,
    ) -> BoxFuture<'_, Result<Session>> {
        Box::pin(async move { start(config).await })
    }
}

/// Shared between the player's calls and the host's threads.
struct Control {
    stop: AtomicBool,
    keyframe: AtomicBool,
    pointer: Mutex<Option<(f32, f32)>>,
}

impl SessionControl for Control {
    fn input(&self, input: Input) {
        match input {
            Input::MouseMove { x, y } => *self.pointer.lock().unwrap() = Some((x, y)),
            other => tracing::trace!(?other, "demo input"),
        }
    }

    fn release_all(&self) {}

    fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }

    fn stop(&self, _quit_app: bool) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

async fn start(config: StreamConfig) -> Result<Session> {
    // Keep the demo's CPU-drawn pattern at a size one core fills at 60 fps.
    let (mut width, mut height) = (config.width, config.height);
    if width > 1920 {
        height = height * 1920 / width;
        width = 1920;
    }
    let (width, height) = (width.max(64) & !1, height.max(64) & !1);
    let fps = config.fps.clamp(1, 240);

    let control = Arc::new(Control {
        stop: AtomicBool::new(false),
        keyframe: AtomicBool::new(true),
        pointer: Mutex::new(None),
    });
    let (video_tx, video) = tmpsc::channel(4);
    let (audio_tx, audio) = tmpsc::channel(64);
    let (_feedback_tx, feedback) = tmpsc::channel(1);
    let (ended_tx, ended) = oneshot::channel();
    let (ready_tx, ready) = oneshot::channel();

    {
        let control = control.clone();
        let bitrate = config.bitrate_kbps.min(30_000) * 1000;
        std::thread::Builder::new()
            .name("demo-video".into())
            .spawn(move || {
                let mut encoder = match Encoder::new(width, height, fps, bitrate) {
                    Ok(e) => {
                        let _ = ready_tx.send(Ok(()));
                        e
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                let ended = run_video(&mut encoder, &control, &video_tx, width, height, fps);
                let _ = ended_tx.send(ended);
            })?;
    }
    ready
        .await
        .map_err(|_| anyhow!("demo encoder thread died"))??;

    {
        let control = control.clone();
        std::thread::Builder::new()
            .name("demo-audio".into())
            .spawn(move || {
                if let Err(e) = run_audio(&control, &audio_tx) {
                    tracing::warn!("demo audio: {e:#}");
                }
            })?;
    }

    Ok(Session {
        video,
        audio,
        feedback,
        ended,
        codec: Codec::H264,
        width,
        height,
        control: Box::new(ControlHandle(control)),
    })
}

/// `Session` owns its control as a box; the threads share the `Arc`.
struct ControlHandle(Arc<Control>);

impl SessionControl for ControlHandle {
    fn input(&self, input: Input) {
        self.0.input(input);
    }
    fn release_all(&self) {
        self.0.release_all();
    }
    fn request_keyframe(&self) {
        self.0.request_keyframe();
    }
    fn stop(&self, quit_app: bool) {
        self.0.stop(quit_app);
    }
}

impl Drop for ControlHandle {
    /// Dropping a session stops it.
    fn drop(&mut self) {
        self.0.stop.store(true, Ordering::Relaxed);
    }
}

fn run_video(
    encoder: &mut Encoder,
    control: &Control,
    tx: &tmpsc::Sender<VideoFrame>,
    width: u32,
    height: u32,
    fps: u32,
) -> Ended {
    let start = Instant::now();
    let period = Duration::from_secs_f64(1.0 / fps as f64);
    let mut number = 0u64;
    loop {
        if control.stop.load(Ordering::Relaxed) || tx.is_closed() {
            return Ended::Stopped;
        }
        let due = start + period * number as u32;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        let pointer = *control.pointer.lock().unwrap();
        let force_key = control.keyframe.swap(false, Ordering::Relaxed);
        let encoded = match encoder.encode(number, width, height, pointer, force_key) {
            Ok(e) => e,
            Err(e) => return Ended::Failed(format!("{e:#}")),
        };
        let frame = VideoFrame {
            codec: Codec::H264,
            data: Bytes::from(encoded.annexb),
            key: encoded.key,
            partial: false,
            number,
            received: Instant::now(),
        };
        // A slow player loses frames, as it would on the network.
        let _ = tx.try_send(frame);
        number += 1;
    }
}

fn run_audio(control: &Control, tx: &tmpsc::Sender<AudioPacket>) -> Result<()> {
    const RATE: u32 = 48_000;
    const FRAMES: usize = 960; // 20 ms
    let mut encoder = opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::LowDelay)
        .map_err(|e| anyhow!("opus encoder: {e}"))?;
    let start = Instant::now();
    let mut pcm = vec![0.0f32; FRAMES * 2];
    let mut out = vec![0u8; 4000];
    let mut phase = 0.0f32;
    let step = 2.0 * std::f32::consts::PI * 440.0 / RATE as f32;
    for n in 0u32.. {
        if control.stop.load(Ordering::Relaxed) || tx.is_closed() {
            return Ok(());
        }
        let due = start + Duration::from_millis(20) * n;
        if let Some(wait) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        // Quiet, with a slow swell so a gap or glitch is audible.
        let swell = 0.04 + 0.03 * (n as f32 * 0.02).sin();
        for frame in pcm.chunks_mut(2) {
            let v = phase.sin() * swell;
            frame.fill(v);
            phase = (phase + step) % (2.0 * std::f32::consts::PI);
        }
        let len = encoder
            .encode_float(&pcm, &mut out)
            .map_err(|e| anyhow!("opus encode: {e}"))?;
        let _ = tx.try_send(AudioPacket {
            data: Bytes::copy_from_slice(&out[..len]),
            channels: 2,
            sample_rate: RATE,
            samples: FRAMES as u32,
        });
    }
    Ok(())
}

struct Encoded {
    annexb: Vec<u8>,
    key: bool,
}

struct Encoder {
    session: CFRetained<VTCompressionSession>,
    output: Receiver<Result<Encoded>>,
    // The callback's refcon; boxed so its address is stable.
    _sink: Box<Sender<Result<Encoded>>>,
    fps: u32,
}

impl Encoder {
    fn new(width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Self> {
        let (tx, output) = mpsc::channel();
        let sink = Box::new(tx);
        let mut session: *mut VTCompressionSession = ptr::null_mut();
        // SAFETY: valid arguments; the refcon (the boxed sender) outlives the
        // session, which `Drop` invalidates first.
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width as i32,
                height as i32,
                0x6176_6331, // 'avc1'
                None,
                None,
                None,
                Some(on_encoded),
                &*sink as *const Sender<_> as *mut c_void,
                NonNull::from(&mut session),
            )
        };
        let session = NonNull::new(session)
            .filter(|_| status == 0)
            .ok_or_else(|| anyhow!("VideoToolbox H.264 encoder unavailable ({status})"))?;
        // SAFETY: Create returned +1.
        let session = unsafe { CFRetained::from_raw(session) };

        let yes = CFBoolean::new(true);
        let no = CFBoolean::new(false);
        let set = |key: &CFString, value: &CFType| {
            // SAFETY: valid session, key and value.
            unsafe { VTSessionSetProperty(&session, key, Some(value)) };
        };
        // SAFETY: reading VideoToolbox's constant keys.
        unsafe {
            set(kVTCompressionPropertyKey_RealTime, yes.as_ref());
            set(kVTCompressionPropertyKey_AllowFrameReordering, no.as_ref());
            set(
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_Main_AutoLevel.as_ref(),
            );
            set(
                kVTCompressionPropertyKey_AverageBitRate,
                CFNumber::new_i32(bitrate as i32).as_ref(),
            );
            set(
                kVTCompressionPropertyKey_ExpectedFrameRate,
                CFNumber::new_i32(fps as i32).as_ref(),
            );
            set(
                kVTCompressionPropertyKey_MaxKeyFrameInterval,
                CFNumber::new_i32(fps as i32 * 4).as_ref(),
            );
            session.prepare_to_encode_frames();
        }
        Ok(Self {
            session,
            output,
            _sink: sink,
            fps,
        })
    }

    fn encode(
        &mut self,
        number: u64,
        width: u32,
        height: u32,
        pointer: Option<(f32, f32)>,
        force_key: bool,
    ) -> Result<Encoded> {
        let pixels = pattern(number, width, height, pointer)?;
        let pts = CMTime {
            value: number as i64,
            timescale: self.fps as i32,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        };
        let duration = CMTime { value: 1, ..pts };
        let properties = force_key.then(|| {
            // SAFETY: reading a VideoToolbox constant key.
            let key = unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame };
            CFDictionary::<CFString, CFType>::from_slices(&[key], &[CFBoolean::new(true).as_ref()])
        });
        let mut info = VTEncodeInfoFlags::empty();
        // SAFETY: valid session and buffer; the properties dictionary is typed.
        let status = unsafe {
            self.session.encode_frame(
                &pixels,
                pts,
                duration,
                properties.as_deref().map(|p| p.as_opaque()),
                ptr::null_mut(),
                &mut info,
            )
        };
        if status != 0 {
            bail!("encoder rejected a frame ({status})");
        }
        self.output
            .recv_timeout(Duration::from_millis(500))
            .context("the encoder produced nothing")?
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the session is ours; invalidate before the refcon goes.
        unsafe { self.session.invalidate() };
    }
}

unsafe extern "C-unwind" fn on_encoded(
    refcon: *mut c_void,
    _source: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: the refcon is the boxed sender, alive for the session.
    let tx = unsafe { &*(refcon as *const Sender<Result<Encoded>>) };
    let result = match unsafe { sample.as_ref() } {
        Some(sample) if status == 0 => annexb_from(sample),
        _ => Err(anyhow!("encode failed ({status})")),
    };
    let _ = tx.send(result);
}

/// A sample's AVCC data as Annex-B, with SPS and PPS ahead of key frames.
fn annexb_from(sample: &CMSampleBuffer) -> Result<Encoded> {
    const START: [u8; 4] = [0, 0, 0, 1];
    // SAFETY: the sample is valid for this call; pointers come from CoreMedia.
    unsafe {
        let block = sample.data_buffer().context("no data in the sample")?;
        let (mut at, mut total) = (0usize, 0usize);
        let mut data: *mut std::ffi::c_char = ptr::null_mut();
        let status = block.data_pointer(0, &mut at, &mut total, &mut data);
        if status != 0 || data.is_null() || at != total {
            bail!("sample data is not contiguous ({status})");
        }
        let avcc = std::slice::from_raw_parts(data as *const u8, total);

        let mut annexb = Vec::with_capacity(total + 64);
        let mut key = false;
        let mut i = 0;
        while i + 4 <= avcc.len() {
            let len = u32::from_be_bytes(avcc[i..i + 4].try_into().unwrap()) as usize;
            i += 4;
            let nal = avcc.get(i..i + len).context("truncated NAL unit")?;
            key |= nal.first().is_some_and(|b| b & 0x1f == 5);
            annexb.extend_from_slice(&START);
            annexb.extend_from_slice(nal);
            i += len;
        }
        if key {
            let format = sample
                .format_description()
                .context("no format description")?;
            let mut headers = Vec::new();
            for index in 0..2 {
                let (mut p, mut size) = (ptr::null::<u8>(), 0usize);
                let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    &format,
                    index,
                    &mut p,
                    &mut size,
                    ptr::null_mut(),
                    ptr::null_mut(),
                );
                if status != 0 || p.is_null() {
                    bail!("no parameter set {index} ({status})");
                }
                headers.extend_from_slice(&START);
                headers.extend_from_slice(std::slice::from_raw_parts(p, size));
            }
            headers.extend_from_slice(&annexb);
            annexb = headers;
        }
        Ok(Encoded { annexb, key })
    }
}

/// One frame of the test pattern as a BGRA pixel buffer: a drifting gradient,
/// a square bouncing (or following the mouse), and a row of blocks that count
/// frames in binary, to read a latency or a dropped frame off a screenshot.
fn pattern(
    number: u64,
    width: u32,
    height: u32,
    pointer: Option<(f32, f32)>,
) -> Result<CFRetained<CVPixelBuffer>> {
    let mut buffer: *mut CVPixelBuffer = ptr::null_mut();
    let empty = CFDictionary::<CFString, CFType>::empty();
    // SAFETY: reading a CoreVideo constant key.
    let attrs = CFDictionary::<CFString, CFType>::from_slices(
        &[unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
        &[empty.as_ref()],
    );
    // SAFETY: valid arguments.
    let status = unsafe {
        CVPixelBufferCreate(
            None,
            width as usize,
            height as usize,
            kCVPixelFormatType_32BGRA,
            Some(attrs.as_opaque()),
            NonNull::from(&mut buffer),
        )
    };
    let buffer = NonNull::new(buffer)
        .filter(|_| status == 0)
        .ok_or_else(|| anyhow!("pixel buffer allocation failed ({status})"))?;
    // SAFETY: Create returned +1.
    let buffer = unsafe { CFRetained::from_raw(buffer) };

    // SAFETY: the buffer is locked while we write inside its bounds.
    unsafe {
        CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        let base = CVPixelBufferGetBaseAddress(&buffer) as *mut u8;
        let stride = CVPixelBufferGetBytesPerRow(&buffer);
        let pixels = std::slice::from_raw_parts_mut(base, stride * height as usize);
        draw(pixels, stride, number, width, height, pointer);
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
    }
    Ok(buffer)
}

fn draw(
    pixels: &mut [u8],
    stride: usize,
    number: u64,
    width: u32,
    height: u32,
    pointer: Option<(f32, f32)>,
) {
    let (w, h) = (width as usize, height as usize);
    let shift = (number * 3) as usize;
    for (y, row) in pixels.chunks_mut(stride).take(h).enumerate() {
        let g = (y * 255 / h) as u8;
        for (x, px) in row[..w * 4].chunks_exact_mut(4).enumerate() {
            let r = ((x + shift) * 255 / w) as u8;
            px.copy_from_slice(&[255 - g, g / 2 + 40, r / 2 + 30, 255]);
        }
    }

    // The square: bounces on its own, or sits under the mouse.
    let side = (h / 6).max(8);
    let (cx, cy) = match pointer {
        Some((px, py)) => ((px * w as f32) as usize, (py * h as f32) as usize),
        None => {
            let t = number as f32 / 60.0;
            let tri = |t: f32| (1.0 - ((t % 2.0) - 1.0).abs()).clamp(0.0, 1.0);
            (
                (tri(t * 0.7) * (w - side) as f32) as usize + side / 2,
                (tri(t * 0.45) * (h - side) as f32) as usize + side / 2,
            )
        }
    };
    let (x0, y0) = (cx.saturating_sub(side / 2), cy.saturating_sub(side / 2));
    for y in y0..(y0 + side).min(h) {
        let row = &mut pixels[y * stride..y * stride + w * 4];
        for x in x0..(x0 + side).min(w) {
            row[x * 4..x * 4 + 4].copy_from_slice(&[245, 245, 245, 255]);
        }
    }

    // The frame counter, 16 blocks wide, along the bottom.
    let block = (w / 32).max(4);
    let top = h.saturating_sub(block * 2);
    for bit in 0..16usize {
        let on = (number >> (15 - bit)) & 1 == 1;
        let shade = if on { 255 } else { 20 };
        for y in top..top + block {
            if y >= h {
                break;
            }
            let row = &mut pixels[y * stride..y * stride + w * 4];
            let start = block + bit * block * 3 / 2;
            for x in start..(start + block).min(w) {
                row[x * 4..x * 4 + 4].copy_from_slice(&[shade, shade, shade, 255]);
            }
        }
    }
}

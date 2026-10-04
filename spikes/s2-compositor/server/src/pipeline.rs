//! The media side: the compositor (`waylanddisplaysrc`, CUDA zero-copy) feeding
//! one always-on NVENC branch per codec, and input going back into the
//! compositor as its custom upstream events.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use tokio::sync::mpsc;
use tracing::{info, warn};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    H264,
    Hevc,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::Hevc => "hevc",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Codec::H264, Codec::Hevc]
            .into_iter()
            .find(|c| c.name() == name)
    }

    fn encoder(self) -> &'static str {
        match self {
            Codec::H264 => "nvh264enc",
            Codec::Hevc => "nvh265enc",
        }
    }

    /// Parser that repeats the parameter sets before every keyframe, so a new
    /// viewer (or one that asked for a keyframe) can start decoding.
    fn parser(self) -> &'static str {
        match self {
            Codec::H264 => {
                "h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au"
            }
            Codec::Hevc => {
                "h265parse config-interval=-1 ! video/x-h265,stream-format=byte-stream,alignment=au"
            }
        }
    }
}

/// One encoded access unit (Annex-B) and when it was made.
pub struct EncodedFrame {
    pub data: Bytes,
    pub key: bool,
    /// When the compositor's buffer for this frame left `waylanddisplaysrc`.
    pub composited: Option<Instant>,
    /// When the encoded frame reached the app sink.
    pub encoded: Instant,
}

pub struct PipelineConfig {
    pub render_node: String,
    pub width: u32,
    pub height: u32,
    /// Encoded (and sent) frame rate.
    pub fps: u32,
    /// The compositor's (and so the app's) frame rate, ≥ `fps`. Input latency
    /// follows this clock (~4.5 of its frames from click to composited); frames
    /// beyond `fps` are dropped before the encoders.
    pub compositor_fps: u32,
    pub bitrate_kbps: u32,
    pub codecs: Vec<Codec>,
}

/// Input for the compositor, in its own units.
#[derive(Debug)]
pub enum Input {
    /// Pointer position in output pixels.
    Move { x: f64, y: f64 },
    /// Linux `BTN_*` code.
    Button { code: u32, pressed: bool },
    /// Wheel, 120 per notch.
    Axis { x: f64, y: f64 },
    /// Linux evdev `KEY_*` code.
    Key { code: u32, pressed: bool },
}

type Subscriber = Arc<Mutex<Option<mpsc::Sender<EncodedFrame>>>>;

struct Branch {
    sink: gst_app::AppSink,
    subscriber: Subscriber,
}

/// When each source buffer (by PTS) left the compositor; bounded.
#[derive(Default)]
struct SourceTimes {
    first_pts: Option<u64>,
    recent: VecDeque<(u64, Instant)>,
    /// The next compositor PTS that may pass the decimator.
    next_kept_pts: u64,
}

pub struct Media {
    pipeline: gst::Pipeline,
    source_pad: gst::Pad,
    /// The compositor's output caps; changing them resizes the output (and the
    /// app's window) while running.
    source_caps: gst::Element,
    compositor_fps: u32,
    branches: HashMap<Codec, Branch>,
    size: Mutex<(u32, u32)>,
}

/// Output size limits; NVENC and the CUDA pool want sizes in multiples of 8.
const MIN_SIZE: (u32, u32) = (320, 240);
const MAX_SIZE: (u32, u32) = (3840, 2160);

fn source_caps(width: u32, height: u32, fps: u32) -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .features(["memory:CUDAMemory"])
        .field("width", width as i32)
        .field("height", height as i32)
        .field("framerate", gst::Fraction::new(fps as i32, 1))
        .build()
}

/// Low-latency NVENC settings (as applied in S1e on this GStreamer), set only
/// where the element has the property.
fn encoder_settings(codec: Codec, config: &PipelineConfig) -> Result<String> {
    let probe = gst::ElementFactory::make(codec.encoder())
        .build()
        .with_context(|| format!("{} (is the GPU visible to the container?)", codec.encoder()))?;
    let vbv = config.bitrate_kbps / config.fps;
    let wanted = [
        ("preset", "p1".to_string()),
        ("tune", "ultra-low-latency".into()),
        ("rc-mode", "cbr".into()),
        ("bitrate", config.bitrate_kbps.to_string()),
        ("max-bitrate", config.bitrate_kbps.to_string()),
        ("vbv-buffer-size", vbv.to_string()),
        ("bframes", "0".into()),
        ("gop-size", "-1".into()),
        ("rc-lookahead", "0".into()),
        ("zerolatency", "true".into()),
        ("aud", "false".into()),
    ];
    Ok(wanted
        .iter()
        .filter(|(name, _)| probe.find_property(name).is_some())
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join(" "))
}

impl Media {
    pub fn new(config: &PipelineConfig) -> Result<Self> {
        gst::init()?;
        let compositor_fps = config.compositor_fps.max(config.fps);
        // When the compositor runs faster, tell the encoders the rate they really
        // get, so rate control budgets bits per encoded frame.
        let retime = if compositor_fps == config.fps {
            String::new()
        } else {
            format!(
                " ! capssetter caps=\"video/x-raw,framerate={}/1\"",
                config.fps
            )
        };
        let mut desc = format!(
            "waylanddisplaysrc name=wl render-node={} cuda-device-id=0 do-timestamp=true \
             ! capsfilter name=srccaps caps=\"video/x-raw(memory:CUDAMemory),width={},height={},framerate={}/1\"{} \
             ! tee name=t",
            config.render_node, config.width, config.height, compositor_fps, retime
        );
        for &codec in &config.codecs {
            desc += &format!(
                " t. ! queue max-size-buffers=1 leaky=downstream ! {} {} ! {} \
                 ! appsink name=sink_{} sync=false max-buffers=2 drop=true",
                codec.encoder(),
                encoder_settings(codec, config)?,
                codec.parser(),
                codec.name()
            );
        }
        info!(pipeline = %desc, "building");
        let pipeline = gst::parse::launch(&desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let source = pipeline.by_name("wl").context("no waylanddisplaysrc")?;
        let source_pad = source.static_pad("src").context("source has no src pad")?;

        let times = Arc::new(Mutex::new(SourceTimes::default()));
        {
            let times = Arc::clone(&times);
            let period = 1_000_000_000 / u64::from(config.fps);
            let slack = 1_000_000_000 / u64::from(compositor_fps) / 2;
            let decimate = compositor_fps > config.fps;
            // Decimate on arrival (never wait for the next frame) and note when each
            // kept frame left the compositor.
            source_pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                let Some(pts) = info.buffer().and_then(|b| b.pts()).map(|p| p.nseconds()) else {
                    return gst::PadProbeReturn::Ok;
                };
                let mut t = times.lock().expect("source times lock");
                if decimate {
                    if pts < t.next_kept_pts {
                        return gst::PadProbeReturn::Drop;
                    }
                    t.next_kept_pts = pts + period - slack;
                }
                t.first_pts.get_or_insert(pts);
                t.recent.push_back((pts, Instant::now()));
                if t.recent.len() > 240 {
                    t.recent.pop_front();
                }
                gst::PadProbeReturn::Ok
            });
        }

        let mut branches = HashMap::new();
        for &codec in &config.codecs {
            let sink = pipeline
                .by_name(&format!("sink_{}", codec.name()))
                .context("missing app sink")?
                .downcast::<gst_app::AppSink>()
                .map_err(|_| anyhow!("not an appsink"))?;
            let subscriber: Subscriber = Arc::new(Mutex::new(None));
            let times = Arc::clone(&times);
            let sub = Arc::clone(&subscriber);
            // NVENC shifts output PTS by a constant (they start at 3600 s); learn it
            // from the first frame to find each frame's compositor time.
            let mut offset: Option<i128> = None;
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let encoded = Instant::now();
                        let Some(buffer) = sample.buffer() else {
                            return Ok(gst::FlowSuccess::Ok);
                        };
                        let composited = buffer.pts().and_then(|pts| {
                            let t = times.lock().expect("source times lock");
                            let out = pts.nseconds() as i128;
                            let off = *offset.get_or_insert(out - t.first_pts? as i128);
                            let src = (out - off) as u64;
                            t.recent
                                .iter()
                                .rev()
                                .find(|(p, _)| *p == src)
                                .map(|(_, at)| *at)
                        });
                        let mut slot = sub.lock().expect("subscriber lock");
                        let Some(tx) = slot.as_ref() else {
                            return Ok(gst::FlowSuccess::Ok);
                        };
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                        let frame = EncodedFrame {
                            data: Bytes::copy_from_slice(&map),
                            key: !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT),
                            composited,
                            encoded,
                        };
                        if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send(frame) {
                            *slot = None;
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
            branches.insert(codec, Branch { sink, subscriber });
        }

        pipeline.set_state(gst::State::Playing)?;
        let bus = pipeline.bus().context("pipeline has no bus")?;
        std::thread::Builder::new()
            .name("gst-bus".into())
            .spawn(move || {
                for msg in bus.iter_timed(gst::ClockTime::NONE) {
                    match msg.view() {
                        gst::MessageView::Error(e) => {
                            warn!(error = %e.error(), debug = ?e.debug(), "pipeline error");
                        }
                        gst::MessageView::Warning(w) => {
                            warn!(warning = %w.error(), "pipeline warning")
                        }
                        gst::MessageView::Eos(_) => {
                            warn!("pipeline ended");
                            break;
                        }
                        _ => {}
                    }
                }
            })?;
        let source_caps = pipeline
            .by_name("srccaps")
            .context("no source capsfilter")?;
        Ok(Self {
            pipeline,
            source_pad,
            source_caps,
            compositor_fps,
            branches,
            size: Mutex::new((config.width, config.height)),
        })
    }

    /// The compositor's current output size.
    pub fn size(&self) -> (u32, u32) {
        *self.size.lock().expect("size lock")
    }

    /// Resizes the output while running: the compositor renegotiates, resizes
    /// the app's window, and the encoders restart at the new size with a
    /// keyframe. Returns the size actually applied.
    pub fn resize(&self, width: u32, height: u32) -> (u32, u32) {
        let fit = |v: u32, lo: u32, hi: u32| v.clamp(lo, hi) & !7;
        let size = (
            fit(width, MIN_SIZE.0, MAX_SIZE.0),
            fit(height, MIN_SIZE.1, MAX_SIZE.1),
        );
        if size == self.size() {
            return size;
        }
        info!(width = size.0, height = size.1, "resizing the output");
        self.source_caps
            .set_property("caps", source_caps(size.0, size.1, self.compositor_fps));
        *self.size.lock().expect("size lock") = size;
        for codec in self.codecs() {
            self.request_keyframe(codec);
        }
        size
    }

    pub fn codecs(&self) -> Vec<Codec> {
        let mut codecs: Vec<Codec> = self.branches.keys().copied().collect();
        codecs.sort_by_key(|c| c.name());
        codecs
    }

    /// Frames of `codec` from now on, starting with a keyframe. Replaces any
    /// previous subscriber of that codec.
    pub fn subscribe(&self, codec: Codec) -> Result<mpsc::Receiver<EncodedFrame>> {
        let branch = self
            .branches
            .get(&codec)
            .ok_or_else(|| anyhow!("{} isn't encoded here", codec.name()))?;
        let (tx, rx) = mpsc::channel(8);
        *branch.subscriber.lock().expect("subscriber lock") = Some(tx);
        self.request_keyframe(codec);
        Ok(rx)
    }

    pub fn request_keyframe(&self, codec: Codec) {
        if let Some(branch) = self.branches.get(&codec) {
            let event = gst_video::UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            if !branch.sink.send_event(event) {
                warn!(codec = codec.name(), "keyframe request not handled");
            }
        }
    }

    /// Delivers input to the compositor as if it came from downstream.
    pub fn input(&self, input: Input) {
        let structure = match input {
            Input::Move { x, y } => gst::Structure::builder("MouseMoveAbsolute")
                .field("pointer_x", x)
                .field("pointer_y", y)
                .build(),
            Input::Button { code, pressed } => gst::Structure::builder("MouseButton")
                .field("button", code)
                .field("pressed", pressed)
                .build(),
            Input::Axis { x, y } => gst::Structure::builder("MouseAxis")
                .field("x", x)
                .field("y", y)
                .build(),
            Input::Key { code, pressed } => gst::Structure::builder("KeyboardKey")
                .field("key", code)
                .field("pressed", pressed)
                .build(),
        };
        if !self
            .source_pad
            .send_event(gst::event::CustomUpstream::new(structure))
        {
            warn!("compositor refused an input event");
        }
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

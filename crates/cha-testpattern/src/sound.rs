//! The tone: 60 ms of 1 kHz with every flash, silence otherwise, played
//! through the environment's sound server (the PulseAudio protocol, in the
//! `pulseaudio` crate's wire format), so a viewer can time sound against
//! picture. A 20 ms buffer, like a game's. Without a server, no sound.

use std::error::Error;
use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pulseaudio::protocol::stream::{BufferAttr, StreamFlags};
use pulseaudio::protocol::{
    self as pa, AuthParams, AuthReply, ChannelMap, ChannelVolume, Command,
    CreatePlaybackStreamReply, PlaybackStreamParams, Prop, Props, SampleFormat, SampleSpec,
    SetClientNameReply,
};

const RATE: u32 = 48_000;
const FRAME_BYTES: usize = 4; // s16 stereo
const TONE_HZ: f32 = 1000.0;
const TONE_FRAMES: usize = RATE as usize * 60 / 1000;
const FADE_FRAMES: usize = RATE as usize * 5 / 1000;
const TLENGTH_MS: u32 = 20;

/// Asks for a tone; the next block written starts with it.
#[derive(Clone, Default)]
pub struct Beeper(Arc<AtomicBool>);

impl Beeper {
    pub fn beep(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub fn start() -> Beeper {
    let beeper = Beeper::default();
    let playing = beeper.clone();
    std::thread::spawn(move || {
        if let Err(err) = play(&playing) {
            eprintln!("cha-testpattern: no sound: {err}");
        }
    });
    beeper
}

fn server() -> Option<PathBuf> {
    match std::env::var("PULSE_SERVER") {
        Ok(s) => s.strip_prefix("unix:").map(PathBuf::from),
        Err(_) => {
            std::env::var_os("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("pulse/native"))
        }
    }
}

fn play(beeper: &Beeper) -> Result<(), Box<dyn Error>> {
    let socket = UnixStream::connect(server().ok_or("no sound server configured")?)?;
    let mut w = socket.try_clone()?;
    let mut r = BufReader::new(socket);
    let auth = AuthParams {
        version: pa::MAX_VERSION,
        supports_shm: false,
        supports_memfd: false,
        cookie: vec![0; 256],
    };
    pa::write_command_message(&mut w, 0, &Command::Auth(auth), pa::MAX_VERSION)?;
    let (_, auth) = pa::read_reply_message::<AuthReply>(&mut r, pa::MAX_VERSION)?;
    let version = auth.version.min(pa::MAX_VERSION);

    let mut props = Props::new();
    props.set(Prop::ApplicationName, c"cha-testpattern");
    pa::write_command_message(&mut w, 1, &Command::SetClientName(props), version)?;
    pa::read_reply_message::<SetClientNameReply>(&mut r, version)?;

    let tlength = RATE * TLENGTH_MS / 1000 * FRAME_BYTES as u32;
    let mut props = Props::new();
    props.set(Prop::MediaName, c"Test tone");
    let params = PlaybackStreamParams {
        sample_spec: SampleSpec {
            format: SampleFormat::S16Le,
            channels: 2,
            sample_rate: RATE,
        },
        channel_map: ChannelMap::stereo(),
        buffer_attr: BufferAttr {
            max_length: u32::MAX,
            target_length: tlength,
            pre_buffering: u32::MAX,
            minimum_request_length: tlength / 4,
            fragment_size: u32::MAX,
        },
        cvolume: Some(ChannelVolume::norm(2)),
        props,
        flags: StreamFlags {
            adjust_latency: true,
            ..Default::default()
        },
        ..Default::default()
    };
    pa::write_command_message(&mut w, 2, &Command::CreatePlaybackStream(params), version)?;
    let (_, stream) = pa::read_reply_message::<CreatePlaybackStreamReply>(&mut r, version)?;

    let mut tone = Tone::default();
    let mut block = Vec::new();
    tone.fill(&mut block, stream.requested_bytes as usize, beeper);
    pa::write_memblock(&mut w, stream.channel, &block, 0)?;
    loop {
        if let (_, Command::Request(request)) = pa::read_command_message(&mut r, version)? {
            tone.fill(&mut block, request.length as usize, beeper);
            pa::write_memblock(&mut w, stream.channel, &block, 0)?;
        }
    }
}

#[derive(Default)]
struct Tone {
    /// Frames of tone left to play.
    left: usize,
    phase: f32,
}

impl Tone {
    /// `bytes` of s16 stereo: silence, or the tone from the block's start when
    /// a beep is asked for.
    fn fill(&mut self, block: &mut Vec<u8>, bytes: usize, beeper: &Beeper) {
        if beeper.0.swap(false, Ordering::Relaxed) {
            self.left = TONE_FRAMES;
            self.phase = 0.0;
        }
        block.clear();
        for _ in 0..bytes / FRAME_BYTES {
            let v = if self.left > 0 {
                // A sharp start (for onset timing), a 5 ms fade at the end.
                let fade = (self.left.min(FADE_FRAMES) as f32) / FADE_FRAMES as f32;
                self.left -= 1;
                let v = self.phase.sin() * 0.5 * fade;
                self.phase += std::f32::consts::TAU * TONE_HZ / RATE as f32;
                (v * 32767.0) as i16
            } else {
                0
            };
            block.extend_from_slice(&v.to_le_bytes());
            block.extend_from_slice(&v.to_le_bytes());
        }
    }
}

//! Our libopus binding: the encoder alone, CELT-only (restricted low delay).

use std::ffi::{CStr, c_char, c_int};

use anyhow::{Result, bail};

#[repr(C)]
struct OpusEncoder {
    _private: [u8; 0],
}

#[link(name = "opus")]
unsafe extern "C" {
    fn opus_encoder_create(
        fs: i32,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut OpusEncoder;
    fn opus_encode_float(
        st: *mut OpusEncoder,
        pcm: *const f32,
        frame_size: c_int,
        data: *mut u8,
        max_data_bytes: i32,
    ) -> i32;
    fn opus_encoder_ctl(st: *mut OpusEncoder, request: c_int, ...) -> c_int;
    fn opus_encoder_destroy(st: *mut OpusEncoder);
    fn opus_strerror(error: c_int) -> *const c_char;
}

/// CELT only: 2.5 ms less algorithmic delay than the default, no speech mode.
const APPLICATION_RESTRICTED_LOWDELAY: c_int = 2051;
const SET_BITRATE: c_int = 4002;
const SET_VBR: c_int = 4006;
const SET_COMPLEXITY: c_int = 4010;
const SET_SIGNAL: c_int = 4024;
const SIGNAL_MUSIC: c_int = 3002;
const RESET_STATE: c_int = 4028;

pub struct Encoder {
    st: *mut OpusEncoder,
    channels: usize,
}

// The encoder state is plain memory, used from one thread at a time.
unsafe impl Send for Encoder {}

impl Encoder {
    /// A 48 kHz encoder for interleaved float frames.
    pub fn new(channels: usize, bitrate_bps: i32) -> Result<Self> {
        let mut error = 0;
        // SAFETY: valid arguments; the result is checked.
        let st = unsafe {
            opus_encoder_create(
                48_000,
                channels as c_int,
                APPLICATION_RESTRICTED_LOWDELAY,
                &mut error,
            )
        };
        if st.is_null() || error != 0 {
            bail!("opus_encoder_create: {}", strerror(error));
        }
        let encoder = Self { st, channels };
        encoder.ctl(SET_BITRATE, bitrate_bps)?;
        // VBR: silence costs a few bytes a frame instead of the full rate.
        encoder.ctl(SET_VBR, 1)?;
        encoder.ctl(SET_COMPLEXITY, 10)?;
        encoder.ctl(SET_SIGNAL, SIGNAL_MUSIC)?;
        Ok(encoder)
    }

    fn ctl(&self, request: c_int, value: c_int) -> Result<()> {
        // SAFETY: every request used here takes one opus_int32.
        let error = unsafe { opus_encoder_ctl(self.st, request, value) };
        if error != 0 {
            bail!("opus_encoder_ctl({request}): {}", strerror(error));
        }
        Ok(())
    }

    /// Forgets earlier audio, e.g. before encoding again after a pause.
    pub fn reset(&self) {
        // SAFETY: OPUS_RESET_STATE takes no argument.
        unsafe { opus_encoder_ctl(self.st, RESET_STATE) };
    }

    /// Encodes one frame (2.5–60 ms of interleaved samples) into `out`,
    /// returning the packet's length.
    pub fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize> {
        let frames = pcm.len() / self.channels;
        // SAFETY: `pcm` holds `frames` interleaved frames, `out` is writable
        // for its length.
        let n = unsafe {
            opus_encode_float(
                self.st,
                pcm.as_ptr(),
                frames as c_int,
                out.as_mut_ptr(),
                out.len() as i32,
            )
        };
        if n < 0 {
            bail!("opus_encode_float: {}", strerror(n));
        }
        Ok(n as usize)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: created by opus_encoder_create, destroyed once.
        unsafe { opus_encoder_destroy(self.st) };
    }
}

fn strerror(error: c_int) -> String {
    // SAFETY: libopus returns a static string for any code.
    unsafe { CStr::from_ptr(opus_strerror(error)) }
        .to_string_lossy()
        .into_owned()
}

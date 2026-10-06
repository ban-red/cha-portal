//! `cha-streamer`: one environment's media engine.
//!
//! - Our headless Wayland compositor (Smithay) runs on the GPU (or, on the CPU
//!   device, in Mesa's software renderer); the app is its client.
//! - Composited frames go zero-copy into NVENC through our own binding
//!   (`cha-nvenc`), or are read back and encoded by x264 or SVT-AV1 on the CPU device;
//!   VA-API (Intel, AMD) is next (`device.rs`). There is no GStreamer or FFmpeg.
//! - Apps play sound into our PulseAudio-protocol server; it is mixed and
//!   Opus-encoded every 10 ms (libopus, our binding).
//! - Chromium can take the same media as `cha-stream/1` datagrams over
//!   WebTransport (wtransport/quinn), the fast path.
//! - A browser gets WebRTC video (str0m, playout-delay 0) and audio tracks and
//!   sends keyboard, mouse and gamepads back on the `control` DataChannel;
//!   gamepads become virtual controllers: Xbox 360 (uinput), or a DualSense or
//!   Steam Controller (uhid).
//!
//! Until the portal brokers sessions (P1.5), it speaks the S1/S2 signalling
//! API so the S1c/S1d page can measure it:
//! - `GET /info`, `GET /streams` (one per codec);
//! - `POST /webrtc/media?name=live-<codec>&secs=&host=&token=`: SDP answer.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod input;

#[cfg(target_os = "linux")]
mod audio;
#[cfg(target_os = "linux")]
mod codec;
#[cfg(target_os = "linux")]
mod compositor;
#[cfg(target_os = "linux")]
mod congestion;
#[cfg(target_os = "linux")]
mod control;
#[cfg(target_os = "linux")]
mod device;
#[cfg(target_os = "linux")]
mod dualsense;
#[cfg(target_os = "linux")]
mod encoder;
#[cfg(target_os = "linux")]
mod framerate;
#[cfg(target_os = "linux")]
mod gamepad;
#[cfg(target_os = "linux")]
mod media;
#[cfg(target_os = "linux")]
mod net;
#[cfg(target_os = "linux")]
mod pyro;
#[cfg(target_os = "linux")]
mod rate;
#[cfg(target_os = "linux")]
mod rtc_hub;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod signal;
#[cfg(target_os = "linux")]
mod status;
#[cfg(target_os = "linux")]
mod steam_controller;
#[cfg(target_os = "linux")]
mod system;
#[cfg(target_os = "linux")]
mod uhid;
#[cfg(target_os = "linux")]
mod viewers;
#[cfg(target_os = "linux")]
mod wt;
#[cfg(target_os = "linux")]
mod x11_clipboard;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cha-streamer runs on Linux nodes only (Wayland, GBM, NVENC).");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() {
    use std::io::Write;

    let code = match signal::main() {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("Error: {err:?}");
            1
        }
    };
    let _ = std::io::stdout().flush();
    // Leave without `exit`'s handlers. NVIDIA's libraries unload themselves
    // in them (`dlclose`), and PyroWave's device warms up on its own thread
    // at the start (it makes its Vulkan device on that driver): a start that
    // failed meanwhile (the driver out of memory for CUDA, say) had the
    // dynamic loader read a library just unmapped under that thread, and
    // ended in SIGSEGV (exit 139) or a hang instead of the error above.
    // SAFETY: `_exit` only ends the process.
    unsafe { libc::_exit(code) }
}

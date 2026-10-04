//! `cha-streamer`: one environment's media engine.
//!
//! - Our headless Wayland compositor (Smithay) runs on the GPU; the app is its
//!   client.
//! - Composited frames go zero-copy into NVENC through our own binding
//!   (`cha-nvenc`); there is no GStreamer or FFmpeg.
//! - Apps play sound into our PulseAudio-protocol server; it is mixed and
//!   Opus-encoded every 10 ms (libopus, our binding).
//! - A browser gets WebRTC video (str0m, playout-delay 0) and audio tracks and
//!   sends keyboard, mouse and gamepads back on the `control` DataChannel;
//!   gamepads become virtual Xbox 360 controllers (uinput).
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
mod compositor;
#[cfg(target_os = "linux")]
mod gamepad;
#[cfg(target_os = "linux")]
mod media;
#[cfg(target_os = "linux")]
mod net;
#[cfg(target_os = "linux")]
mod session;
#[cfg(target_os = "linux")]
mod signal;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cha-streamer runs on Linux nodes only (Wayland, GBM, NVENC).");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    signal::main()
}

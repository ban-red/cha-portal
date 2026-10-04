//! `cha-testpattern`: the test-pattern environment (plan §3.6), a Wayland
//! client that draws, every frame callback:
//! - a bar sweeping across the screen (motion, tearing);
//! - the frame counter, and the same number as a 32-cell binary strip, so a
//!   viewer can spot drops, repeats and reorders;
//! - a white square for 100 ms after any click or key press (input → screen),
//!   with a 60 ms tone through the sound server (input → speaker, A/V);
//! - the first gamepad's buttons, triggers, d-pad and sticks.
//!
//! Only what changed is repainted and reported as damage, in shared memory.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod draw;

#[cfg(target_os = "linux")]
mod client;
#[cfg(target_os = "linux")]
mod pads;
#[cfg(target_os = "linux")]
mod sound;

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("cha-testpattern runs on Linux only.");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() {
    if let Err(err) = client::run() {
        eprintln!("cha-testpattern: {err}");
        std::process::exit(1);
    }
}

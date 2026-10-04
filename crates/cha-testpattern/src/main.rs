//! `cha-testpattern`: the test-pattern environment (plan §3.6), a Wayland
//! client that draws, every frame callback:
//! - a bar sweeping across the screen (motion, tearing);
//! - the frame counter, and the same number as a 32-cell binary strip, so a
//!   viewer can spot drops, repeats and reorders;
//! - a white square for 100 ms after any click or key press (input → screen).
//!
//! Only what changed is repainted and reported as damage, in shared memory.

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod draw;

#[cfg(target_os = "linux")]
mod client;

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

//! QUIC's congestion window for our sessions (plan §3.1 rule 9).
//!
//! A video stream can't back off the way a download does. Halving the window
//! on every loss (Cubic, NewReno) starves it under random loss: at 3 % the
//! window collapsed, frames piled up behind it, and the stream fell to its
//! floor (spike S7). BBR's probing stalls it (S1). So the window here never
//! limits a session; the rate is our rate control's (`rate.rs`), from the
//! page's reports and our send queue (rule 1), and it takes the encoder down
//! before a queue builds. The window only has to let a frame or two be in
//! flight: 8 MB covers PyroWave 4:4:4 at 1440p (1.2 MB a frame) with room.

use std::any::Any;
use std::sync::Arc;
use std::time::Instant;

use wtransport::quinn::congestion::{Controller, ControllerFactory};

const WINDOW: u64 = 8 << 20;

#[derive(Clone, Debug)]
struct MediaWindow;

impl Controller for MediaWindow {
    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        _persistent: bool,
        _lost: u64,
    ) {
    }

    fn on_mtu_update(&mut self, _mtu: u16) {}

    fn window(&self) -> u64 {
        WINDOW
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(self.clone())
    }

    fn initial_window(&self) -> u64 {
        WINDOW
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

#[derive(Debug)]
pub struct MediaWindowFactory;

impl ControllerFactory for MediaWindowFactory {
    fn build(self: Arc<Self>, _now: Instant, _mtu: u16) -> Box<dyn Controller> {
        Box::new(MediaWindow)
    }
}

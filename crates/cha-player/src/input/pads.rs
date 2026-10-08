//! Gamepads through SDL3's gamepad subsystem (no SDL window): up to four
//! pads in the W3C standard mapping, hot-plug, and rumble back.
//!
//! SDL insists on being initialised from one thread for the life of the
//! process, so a single thread owns it from startup; sessions attach to it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use anyhow::{Context, Result};
use cha_client::{Feedback, Input, PadState, SessionControl};
use sdl3::event::Event;
use sdl3::gamepad::{Axis, Button, Gamepad};
use sdl3::joystick::JoystickId;

const MAX_PADS: usize = 4;
const POLL: Duration = Duration::from_millis(4);
/// "Until told otherwise" for a rumble: SDL ends effects at this many ms.
const RUMBLE_MS: u32 = 0xFFFF;

enum Command {
    Attach(Arc<dyn SessionControl>),
    Detach,
    Feedback(Feedback),
}

/// Handle to the gamepad thread.
pub struct PadService {
    commands: Sender<Command>,
}

impl PadService {
    /// Starts the thread; if SDL can't start, pads are unavailable and the
    /// handle does nothing.
    pub fn spawn() -> Self {
        let (commands, rx) = mpsc::channel();
        // A switch for diagnosing the window without SDL in the process.
        if std::env::var_os("CHA_PLAYER_NO_PADS").is_some() {
            tracing::info!("gamepads off (CHA_PLAYER_NO_PADS)");
            return Self { commands };
        }
        let spawned = std::thread::Builder::new()
            .name("pads".into())
            .spawn(move || {
                if let Err(e) = run(rx) {
                    tracing::warn!("gamepads unavailable: {e:#}");
                }
            });
        if let Err(e) = spawned {
            tracing::warn!("gamepad thread: {e}");
        }
        Self { commands }
    }

    /// Send pad state to this session (pads already connected are announced).
    pub fn attach(&self, control: Arc<dyn SessionControl>) {
        let _ = self.commands.send(Command::Attach(control));
    }

    pub fn detach(&self) {
        let _ = self.commands.send(Command::Detach);
    }

    pub fn feedback(&self, feedback: Feedback) {
        let _ = self.commands.send(Command::Feedback(feedback));
    }
}

/// Whether macOS lets this app read input devices (Privacy & Security, Input
/// Monitoring). Without it SDL's HIDAPI drivers open nothing, so a Steam
/// Controller, among others, is silently missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputAccess {
    Granted,
    Denied,
    /// Not asked yet: macOS asks when the first device is opened.
    Unknown,
}

/// Gamepads open now, for the launcher: a controller that works says more
/// than [`input_access`], which judges this exact build (an ad-hoc signed one
/// is new to macOS after every rebuild) or, started from a terminal, the
/// terminal.
static CONNECTED: AtomicUsize = AtomicUsize::new(0);

pub fn connected() -> usize {
    CONNECTED.load(Ordering::Relaxed)
}

pub fn input_access() -> InputAccess {
    // IOKit's IOHIDRequestType and IOHIDAccessType.
    const LISTEN_EVENT: u32 = 1;
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOHIDCheckAccess(request: u32) -> u32;
    }
    // SAFETY: takes and returns plain integers.
    match unsafe { IOHIDCheckAccess(LISTEN_EVENT) } {
        0 => InputAccess::Granted,
        1 => InputAccess::Denied,
        _ => InputAccess::Unknown,
    }
}

struct Slot {
    id: JoystickId,
    pad: Gamepad,
    /// A Steam Controller: its trackpad clicks sit where SDL puts them.
    steam: bool,
    last: Option<PadState>,
}

fn run(commands: Receiver<Command>) -> Result<()> {
    sdl3::hint::set("SDL_JOYSTICK_ALLOW_BACKGROUND_EVENTS", "1");
    // SDL's Apple GameController (MFi) backend expects the main thread, which
    // winit owns; started from this thread it left macOS never showing our
    // window as visible, so wgpu never got a drawable (a blank window).
    // Without it, pads come through IOKit and SDL's HIDAPI drivers (DualSense,
    // Xbox, Steam Controller, with rumble and LEDs).
    sdl3::hint::set("SDL_JOYSTICK_MFI", "0");
    let access = input_access();
    if access != InputAccess::Granted {
        tracing::warn!(
            ?access,
            "Input Monitoring not granted: some gamepads won't be seen"
        );
    }
    let sdl = sdl3::init().context("SDL init")?;
    let subsystem = sdl.gamepad().context("SDL gamepad subsystem")?;
    let mut events = sdl.event_pump().context("SDL event pump")?;

    let mut slots: [Option<Slot>; MAX_PADS] = Default::default();
    let mut control: Option<Arc<dyn SessionControl>> = None;
    for id in subsystem.gamepads().unwrap_or_default() {
        open(&subsystem, &mut slots, id);
    }

    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Attach(c)) => {
                    // A new session knows no pads: forget what we sent.
                    for slot in slots.iter_mut().flatten() {
                        slot.last = None;
                    }
                    control = Some(c);
                }
                Ok(Command::Detach) => control = None,
                Ok(Command::Feedback(f)) => feedback(&mut slots, f),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }

        for event in events.poll_iter() {
            match event {
                Event::GamepadAdded { which, .. } => open(&subsystem, &mut slots, which),
                Event::GamepadRemoved { which, .. } => {
                    for (index, slot) in slots.iter_mut().enumerate() {
                        if slot.as_ref().is_some_and(|s| s.id == which) {
                            tracing::info!(index, "gamepad removed");
                            *slot = None;
                            if let Some(c) = &control {
                                c.input(Input::PadGone { index: index as u8 });
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        CONNECTED.store(slots.iter().flatten().count(), Ordering::Relaxed);

        if let Some(c) = &control {
            for (index, slot) in slots.iter_mut().enumerate() {
                let Some(slot) = slot else { continue };
                let state =
                    standard_state(|b| slot.pad.button(b), |a| slot.pad.axis(a), slot.steam);
                if slot.last.as_ref() != Some(&state) {
                    tracing::debug!(
                        index,
                        pressed = ?state
                            .buttons
                            .iter()
                            .enumerate()
                            .filter(|(_, v)| **v > 0.5)
                            .map(|(i, _)| i)
                            .collect::<Vec<_>>(),
                        "pad state"
                    );
                    slot.last = Some(state.clone());
                    c.input(Input::Pad {
                        index: index as u8,
                        pad: state,
                    });
                }
            }
        }
        std::thread::sleep(POLL);
    }
}

fn open(subsystem: &sdl3::GamepadSubsystem, slots: &mut [Option<Slot>; MAX_PADS], id: JoystickId) {
    if slots.iter().flatten().any(|s| s.id == id) {
        return;
    }
    let Some(free) = slots.iter().position(Option::is_none) else {
        tracing::warn!("more than {MAX_PADS} gamepads; ignoring one");
        return;
    };
    match subsystem.open(id) {
        Ok(pad) => {
            let steam = pad.name().is_some_and(|n| n.contains("Steam"));
            tracing::info!(index = free, name = ?pad.name(), steam, "gamepad connected");
            slots[free] = Some(Slot {
                id,
                pad,
                steam,
                last: None,
            });
        }
        Err(e) => tracing::warn!("opening a gamepad: {e}"),
    }
}

fn feedback(slots: &mut [Option<Slot>; MAX_PADS], feedback: Feedback) {
    if let Feedback::Rumble { index, low, high } = feedback
        && let Some(Some(slot)) = slots.get_mut(index as usize)
    {
        let level = |v: f32| (v.clamp(0.0, 1.0) * 65535.0) as u16;
        let ms = if low == 0.0 && high == 0.0 {
            0
        } else {
            RUMBLE_MS
        };
        if let Err(e) = slot.pad.set_rumble(level(low), level(high), ms) {
            tracing::debug!("rumble: {e}");
        }
    }
}

/// A pad's whole state in the W3C standard mapping: 17 buttons (6 and 7 the
/// analog triggers) and 4 axes, Y down.
/// The browser player's 24 buttons: the W3C standard 17, then a trackpad or
/// touchpad click (17), a Steam Controller's left trackpad click (18), the back
/// paddles or grips L4, R4, L5, R5 (19-22), and a DualSense's mute or a Steam
/// Controller's quick-access button (23) (`web/packages/player`, `EXTRA`).
/// SDL names a Steam Controller's left trackpad click `touchpad` and its right
/// one `misc2`; a DualSense's touchpad click is `touchpad`.
pub fn standard_state(
    button: impl Fn(Button) -> bool,
    axis: impl Fn(Axis) -> i16,
    steam: bool,
) -> PadState {
    let digital = |b| if button(b) { 1.0 } else { 0.0 };
    let trigger = |a| (axis(a).max(0) as f32 / 32767.0).clamp(0.0, 1.0);
    let stick = |a| (axis(a) as f32 / 32767.0).clamp(-1.0, 1.0);
    PadState {
        buttons: vec![
            digital(Button::South),
            digital(Button::East),
            digital(Button::West),
            digital(Button::North),
            digital(Button::LeftShoulder),
            digital(Button::RightShoulder),
            trigger(Axis::TriggerLeft),
            trigger(Axis::TriggerRight),
            digital(Button::Back),
            digital(Button::Start),
            digital(Button::LeftStick),
            digital(Button::RightStick),
            digital(Button::DPadUp),
            digital(Button::DPadDown),
            digital(Button::DPadLeft),
            digital(Button::DPadRight),
            digital(Button::Guide),
            // Extras.
            digital(if steam {
                Button::Misc2
            } else {
                Button::Touchpad
            }),
            if steam {
                digital(Button::Touchpad)
            } else {
                0.0
            },
            digital(Button::LeftPaddle1),
            digital(Button::RightPaddle1),
            digital(Button::LeftPaddle2),
            digital(Button::RightPaddle2),
            digital(Button::Misc1),
        ],
        axes: vec![
            stick(Axis::LeftX),
            stick(Axis::LeftY),
            stick(Axis::RightX),
            stick(Axis::RightY),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_layout() {
        let state = standard_state(
            |b| matches!(b, Button::South | Button::DPadLeft | Button::Guide),
            |a| match a {
                Axis::TriggerRight => 32767,
                Axis::TriggerLeft => -5, // some drivers rest slightly negative
                Axis::LeftX => -32767,
                Axis::RightY => 16384,
                _ => 0,
            },
            false,
        );
        assert_eq!(state.buttons.len(), 24);
        assert_eq!(state.axes.len(), 4);
        assert_eq!(state.buttons[0], 1.0, "A / South");
        assert_eq!(state.buttons[1], 0.0);
        assert_eq!(state.buttons[6], 0.0, "left trigger");
        assert_eq!(state.buttons[7], 1.0, "right trigger");
        assert_eq!(state.buttons[14], 1.0, "dpad left");
        assert_eq!(state.buttons[16], 1.0, "guide");
        assert_eq!(state.axes[0], -1.0);
        assert!((state.axes[3] - 0.5).abs() < 0.001);
    }

    #[test]
    fn a_steam_controllers_extras_go_where_the_browser_puts_them() {
        let pressed = [
            Button::Misc1,
            Button::Misc2,
            Button::Touchpad,
            Button::LeftPaddle2,
        ];
        let steam = standard_state(|b| pressed.contains(&b), |_| 0, true);
        let on: Vec<usize> = (0..24).filter(|&i| steam.buttons[i] > 0.5).collect();
        // Right trackpad, left trackpad, L5, quick access.
        assert_eq!(on, [17, 18, 21, 23]);
        // A DualSense: touchpad click 17, mute 23, no 18.
        let ds = standard_state(|b| pressed.contains(&b), |_| 0, false);
        let on: Vec<usize> = (0..24).filter(|&i| ds.buttons[i] > 0.5).collect();
        assert_eq!(on, [17, 21, 23]);
    }
}

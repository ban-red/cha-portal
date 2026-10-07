//! Moonlight's input, in our units: Windows virtual keys to evdev, the
//! client's mouse space to output pixels, its pad bitmasks to the W3C
//! standard mapping the pads take.

use std::collections::HashSet;

use cha_gamestream::InputEvent;
use cha_gamestream::input::{GamepadKind, MouseButton};
use tracing::info;

use crate::gamepad::{MAX_PADS, PadKind, PadState};
use crate::input::Input;

/// What an event comes to.
#[derive(Debug)]
pub enum Action {
    Input(Input),
    Pad(PadState),
}

/// Moonlight's wheel notch is 120; ours is pixels, where a notch is about 100
/// (the compositor sends 1.2 v120 units per pixel).
const V120_PER_PIXEL: f64 = 1.2;

// Linux `BTN_*`.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
const BTN_SIDE: u32 = 0x113;
const BTN_EXTRA: u32 = 0x114;

/// W3C standard mapping buttons, by Moonlight's flag.
const BUTTONS: [(u32, usize); 14] = [
    (0x1000, 0),  // A
    (0x2000, 1),  // B
    (0x4000, 2),  // X
    (0x8000, 3),  // Y
    (0x0100, 4),  // left bumper
    (0x0200, 5),  // right bumper
    (0x0020, 8),  // back
    (0x0010, 9),  // start
    (0x0040, 10), // left stick
    (0x0080, 11), // right stick
    (0x0001, 12), // d-pad up
    (0x0002, 13), // down
    (0x0004, 14), // left
    (0x0008, 15), // right
];
const HOME: u32 = 0x0400;
const TOUCHPAD: u32 = 0x10_0000;
/// Buttons 0..=17 of the standard mapping: through the touchpad click.
const BUTTON_COUNT: usize = 18;

/// Turns the control stream's events into compositor input and pad states,
/// remembering which pads the client has connected.
#[derive(Default)]
pub struct Mapper {
    pads: u16,
    warned: HashSet<&'static str>,
}

impl Mapper {
    /// Maps `event` for an output of `size` pixels, passing what it comes to
    /// to `emit`.
    pub fn map(&mut self, event: InputEvent, size: (u32, u32), emit: &mut dyn FnMut(Action)) {
        match event {
            InputEvent::Key { down, vk, .. } => match vk_to_evdev(vk) {
                Some(code) => emit(Action::Input(Input::Key {
                    code,
                    pressed: down,
                })),
                None => self.once("an unmapped key", "key without an evdev code (ignored)"),
            },
            InputEvent::MouseMoveAbsolute {
                x,
                y,
                width,
                height,
            } => {
                if let Some((x, y)) = scale(x, y, width, height, size) {
                    emit(Action::Input(Input::Move { x, y }));
                }
            }
            InputEvent::MouseMoveRelative { dx, dy } => emit(Action::Input(Input::Relative {
                dx: f64::from(dx),
                dy: f64::from(dy),
            })),
            InputEvent::MouseButton { button, down } => emit(Action::Input(Input::Button {
                code: match button {
                    MouseButton::Left => BTN_LEFT,
                    MouseButton::Middle => BTN_MIDDLE,
                    MouseButton::Right => BTN_RIGHT,
                    MouseButton::Side => BTN_SIDE,
                    MouseButton::Extra => BTN_EXTRA,
                },
                pressed: down,
            })),
            // Moonlight scrolls up with positive numbers; the compositor's
            // vertical axis grows downwards, like a page's wheel.
            InputEvent::ScrollVertical { amount } => emit(Action::Input(Input::Axis {
                x: 0.0,
                y: -f64::from(amount) / V120_PER_PIXEL,
            })),
            InputEvent::ScrollHorizontal { amount } => emit(Action::Input(Input::Axis {
                x: f64::from(amount) / V120_PER_PIXEL,
                y: 0.0,
            })),
            InputEvent::GamepadArrival { pad, kind, .. } => {
                let i = usize::from(pad);
                if i < MAX_PADS {
                    self.pads |= 1 << i;
                    emit(Action::Pad(PadState {
                        kind: Some(pad_kind(kind)),
                        ..rest(i)
                    }));
                } else {
                    self.too_many_pads();
                }
            }
            InputEvent::GamepadState {
                pad,
                active_mask,
                buttons,
                left_trigger,
                right_trigger,
                left_stick,
                right_stick,
            } => {
                // A pad that was connected and no longer is: back to rest.
                let left = self.pads & !active_mask;
                for i in (0..MAX_PADS).filter(|i| left & (1 << i) != 0) {
                    emit(Action::Pad(PadState {
                        gone: true,
                        ..rest(i)
                    }));
                }
                self.pads = active_mask;
                let i = usize::from(pad);
                if i >= MAX_PADS {
                    self.too_many_pads();
                } else if active_mask & (1 << i) != 0 {
                    emit(Action::Pad(pad_state(
                        i,
                        buttons,
                        left_trigger,
                        right_trigger,
                        left_stick,
                        right_stick,
                    )));
                }
            }
            InputEvent::Touch(_) => self.once("touch", "touch input (ignored)"),
            InputEvent::Pen(_) => self.once("pen", "pen input (ignored)"),
            InputEvent::Text(_) => self.once("text", "typed text (ignored)"),
            InputEvent::GamepadTouch { .. } => self.once("pad touch", "pad touchpads (ignored)"),
            InputEvent::GamepadMotion { .. } => self.once("pad motion", "pad motion (ignored)"),
            InputEvent::GamepadBattery { .. } => self.once("pad battery", "pad battery (ignored)"),
        }
    }

    /// The pads' memory goes with the input's: after a release none are
    /// connected.
    pub fn reset(&mut self) {
        self.pads = 0;
    }

    fn too_many_pads(&mut self) {
        self.once("pad number", "a pad past the fourth (ignored)");
    }

    /// Logs the first time something is dropped, not every time.
    fn once(&mut self, what: &'static str, message: &'static str) {
        if self.warned.insert(what) {
            info!("GameStream client sent {message}");
        }
    }
}

fn pad_kind(kind: GamepadKind) -> PadKind {
    match kind {
        GamepadKind::Xbox => PadKind::Xbox,
        GamepadKind::PlayStation => PadKind::Playstation,
        GamepadKind::Nintendo => PadKind::Switch,
        GamepadKind::Unknown => PadKind::Generic,
    }
}

/// A pad at rest: sticks centered, buttons up.
fn rest(i: usize) -> PadState {
    PadState {
        i,
        b: vec![0.0; BUTTON_COUNT],
        a: vec![0.0; 4],
        ..PadState::default()
    }
}

/// Moonlight's state (XInput's flags, a byte per trigger, 16-bit sticks with Y
/// up) as the W3C standard mapping (sticks -1..1 with Y down, triggers 0..1).
fn pad_state(
    i: usize,
    buttons: u32,
    left_trigger: u8,
    right_trigger: u8,
    left: (i16, i16),
    right: (i16, i16),
) -> PadState {
    let mut state = rest(i);
    for (flag, index) in BUTTONS {
        if buttons & flag != 0 {
            state.b[index] = 1.0;
        }
    }
    state.b[6] = f32::from(left_trigger) / 255.0;
    state.b[7] = f32::from(right_trigger) / 255.0;
    if buttons & HOME != 0 {
        state.b[16] = 1.0;
    }
    if buttons & TOUCHPAD != 0 {
        state.b[17] = 1.0;
    }
    state.a = vec![
        stick(left.0),
        -stick(left.1),
        stick(right.0),
        -stick(right.1),
    ];
    state
}

fn stick(v: i16) -> f32 {
    (f32::from(v) / 32767.0).clamp(-1.0, 1.0)
}

/// The client's position (in its `width` by `height` view) in output pixels,
/// or `None` for a view of no size.
fn scale(x: i16, y: i16, width: i16, height: i16, size: (u32, u32)) -> Option<(f64, f64)> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let to = |v: i16, from: i16, out: u32| {
        (f64::from(v) / f64::from(from)).clamp(0.0, 1.0) * f64::from(out)
    };
    Some((to(x, width, size.0), to(y, height, size.1)))
}

/// Windows virtual-key code to Linux evdev `KEY_*`, for a US layout. Moonlight
/// sends the code of the key's position on the client's layout, so this is
/// what the key does on a US keyboard.
pub fn vk_to_evdev(vk: u8) -> Option<u32> {
    // KEY_A..KEY_Z by letter.
    const LETTERS: [u32; 26] = [
        30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17,
        45, 21, 44,
    ];
    Some(match vk {
        0x41..=0x5A => LETTERS[usize::from(vk - 0x41)],
        // 1..9 are KEY_1..KEY_9 (2..10), 0 is KEY_0 (11).
        0x31..=0x39 => u32::from(vk - 0x31) + 2,
        0x30 => 11,
        // F1..F10, F11, F12, then F13..F24.
        0x70..=0x79 => u32::from(vk - 0x70) + 59,
        0x7A => 87,
        0x7B => 88,
        0x7C..=0x87 => u32::from(vk - 0x7C) + 183,
        // The numpad.
        0x60 => 82,
        0x61..=0x63 => u32::from(vk - 0x61) + 79,
        0x64..=0x66 => u32::from(vk - 0x64) + 75,
        0x67..=0x69 => u32::from(vk - 0x67) + 71,
        0x6A => 55, // multiply
        0x6B => 78, // add
        0x6D => 74, // subtract
        0x6E => 83, // decimal
        0x6F => 98, // divide
        0x08 => 14, // backspace
        0x09 => 15, // tab
        0x0D => 28, // enter (the numpad's is the same code to a client)
        0x13 => 119,
        0x14 => 58,
        0x1B => 1,
        0x20 => 57,
        0x21 => 104, // page up
        0x22 => 109,
        0x23 => 107, // end
        0x24 => 102, // home
        0x25 => 105, // left
        0x26 => 103, // up
        0x27 => 106, // right
        0x28 => 108, // down
        0x2C => 99,  // print screen
        0x2D => 110, // insert
        0x2E => 111, // delete
        0x5B => 125, // left Windows
        0x5C => 126,
        0x5D => 127, // menu
        0x90 => 69,  // num lock
        0x91 => 70,  // scroll lock
        // Modifiers: the generic ones are the left ones.
        0x10 | 0xA0 => 42,
        0xA1 => 54,
        0x11 | 0xA2 => 29,
        0xA3 => 97,
        0x12 | 0xA4 => 56,
        0xA5 => 100,
        // US punctuation.
        0xBA => 39, // ;
        0xBB => 13, // =
        0xBC => 51, // ,
        0xBD => 12, // -
        0xBE => 52, // .
        0xBF => 53, // /
        0xC0 => 41, // `
        0xDB => 26, // [
        0xDC => 43, // \
        0xDD => 27, // ]
        0xDE => 40, // '
        0xE2 => 86, // the key left of Z on ISO keyboards
        // Media and browser keys.
        0xA6 => 158, // browser back
        0xA7 => 159,
        0xA8 => 173, // refresh
        0xA9 => 128, // stop
        0xAA => 217, // search
        0xAB => 156, // favorites
        0xAC => 172, // home page
        0xAD => 113, // mute
        0xAE => 114, // volume down
        0xAF => 115,
        0xB0 => 163, // next track
        0xB1 => 165, // previous
        0xB2 => 166, // stop
        0xB3 => 164, // play and pause
        0xB4 => 155, // mail
        0xB5 => 226, // media player
        0xB6 => 157, // my computer
        0xB7 => 140, // calculator
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(mapper: &mut Mapper, event: InputEvent) -> Vec<Action> {
        let mut out = Vec::new();
        mapper.map(event, (2560, 1440), &mut |a| out.push(a));
        out
    }

    /// The compositor inputs among `actions`; a pad among them is a mistake.
    fn inputs(actions: Vec<Action>) -> Vec<Input> {
        actions
            .into_iter()
            .map(|a| match a {
                Action::Input(input) => input,
                Action::Pad(pad) => panic!("a pad: {pad:?}"),
            })
            .collect()
    }

    fn state(
        pad: u16,
        mask: u16,
        buttons: u32,
        triggers: (u8, u8),
        sticks: ((i16, i16), (i16, i16)),
    ) -> InputEvent {
        InputEvent::GamepadState {
            pad,
            active_mask: mask,
            buttons,
            left_trigger: triggers.0,
            right_trigger: triggers.1,
            left_stick: sticks.0,
            right_stick: sticks.1,
        }
    }

    #[test]
    fn keys_map_to_evdev() {
        // Spot checks against <linux/input-event-codes.h>.
        for (vk, code) in [
            (0x41, 30),  // A
            (0x5A, 44),  // Z
            (0x51, 16),  // Q
            (0x4D, 50),  // M
            (0x31, 2),   // 1
            (0x39, 10),  // 9
            (0x30, 11),  // 0
            (0x70, 59),  // F1
            (0x79, 68),  // F10
            (0x7A, 87),  // F11
            (0x7B, 88),  // F12
            (0x7C, 183), // F13
            (0x87, 194), // F24
            (0x0D, 28),  // enter
            (0x1B, 1),   // escape
            (0x20, 57),  // space
            (0x08, 14),  // backspace
            (0x09, 15),  // tab
            (0x14, 58),  // caps lock
            (0x25, 105), // left
            (0x26, 103), // up
            (0x27, 106), // right
            (0x28, 108), // down
            (0x21, 104), // page up
            (0x22, 109), // page down
            (0x24, 102), // home
            (0x23, 107), // end
            (0x2D, 110), // insert
            (0x2E, 111), // delete
            (0xA0, 42),  // left shift
            (0xA1, 54),  // right shift
            (0xA2, 29),  // left ctrl
            (0xA3, 97),  // right ctrl
            (0xA4, 56),  // left alt
            (0xA5, 100), // right alt
            (0x5B, 125), // left Windows
            (0x5C, 126), // right Windows
            (0x10, 42),  // generic shift
            (0x60, 82),  // numpad 0
            (0x61, 79),  // numpad 1
            (0x65, 76),  // numpad 5
            (0x69, 73),  // numpad 9
            (0x6A, 55),  // numpad *
            (0x6B, 78),  // numpad +
            (0x6D, 74),  // numpad -
            (0x6E, 83),  // numpad .
            (0x6F, 98),  // numpad /
            (0x90, 69),  // num lock
            (0xBA, 39),  // ;
            (0xBB, 13),  // =
            (0xBC, 51),  // ,
            (0xBD, 12),  // -
            (0xBE, 52),  // .
            (0xBF, 53),  // /
            (0xC0, 41),  // `
            (0xDB, 26),  // [
            (0xDC, 43),  // \
            (0xDD, 27),  // ]
            (0xDE, 40),  // '
            (0xAD, 113), // mute
            (0xB3, 164), // play and pause
        ] {
            assert_eq!(vk_to_evdev(vk), Some(code), "vk {vk:#x}");
        }
        for vk in [0x00, 0x07, 0x0A, 0xFF, 0xE7] {
            assert_eq!(vk_to_evdev(vk), None, "vk {vk:#x}");
        }
    }

    #[test]
    fn no_two_keys_share_an_evdev_code_but_the_generic_modifiers() {
        let mut seen = std::collections::HashMap::new();
        for vk in 0..=255u8 {
            if let Some(code) = vk_to_evdev(vk)
                && let Some(other) = seen.insert(code, vk)
            {
                assert!(
                    matches!((other, vk), (0x10, 0xA0) | (0x11, 0xA2) | (0x12, 0xA4)),
                    "{other:#x} and {vk:#x} are both evdev {code}"
                );
            }
        }
    }

    #[test]
    fn the_browser_and_moonlight_agree_on_letters() {
        // The letters in the browser path's table (`crate::input`) and here.
        for (i, c) in ('A'..='Z').enumerate() {
            let browser = crate::input::BrowserInput::Key {
                code: format!("Key{c}"),
                down: true,
            }
            .to_compositor(1, 1);
            assert_eq!(
                browser,
                vk_to_evdev(0x41 + i as u8).map(|code| Input::Key {
                    code,
                    pressed: true
                }),
                "{c}"
            );
        }
    }

    #[test]
    fn a_key_event_goes_down_and_up() {
        let mut m = Mapper::default();
        let key = |down| InputEvent::Key {
            down,
            vk: 0x41,
            modifiers: 0,
        };
        assert_eq!(
            inputs(map(&mut m, key(true))),
            [Input::Key {
                code: 30,
                pressed: true
            }]
        );
        assert_eq!(
            inputs(map(&mut m, key(false))),
            [Input::Key {
                code: 30,
                pressed: false
            }]
        );
        assert!(
            map(
                &mut m,
                InputEvent::Key {
                    down: true,
                    vk: 0xFE,
                    modifiers: 0
                }
            )
            .is_empty()
        );
    }

    #[test]
    fn the_absolute_mouse_scales_from_the_clients_view_to_output_pixels() {
        let mut m = Mapper::default();
        let at = |x, y, width, height| InputEvent::MouseMoveAbsolute {
            x,
            y,
            width,
            height,
        };
        // 1920x1080 view on a 2560x1440 output.
        assert_eq!(
            inputs(map(&mut m, at(960, 540, 1920, 1080))),
            [Input::Move {
                x: 1280.0,
                y: 720.0
            }]
        );
        assert_eq!(
            inputs(map(&mut m, at(0, 0, 1920, 1080))),
            [Input::Move { x: 0.0, y: 0.0 }]
        );
        // Outside the view clamps to its edge.
        assert_eq!(
            inputs(map(&mut m, at(3000, -5, 1920, 1080))),
            [Input::Move { x: 2560.0, y: 0.0 }]
        );
        assert!(map(&mut m, at(1, 1, 0, 0)).is_empty(), "no view, no move");
    }

    #[test]
    fn relative_motion_and_buttons_pass_through() {
        let mut m = Mapper::default();
        assert_eq!(
            inputs(map(&mut m, InputEvent::MouseMoveRelative { dx: -3, dy: 7 })),
            [Input::Relative { dx: -3.0, dy: 7.0 }]
        );
        for (button, code) in [
            (MouseButton::Left, 0x110),
            (MouseButton::Right, 0x111),
            (MouseButton::Middle, 0x112),
            (MouseButton::Side, 0x113),
            (MouseButton::Extra, 0x114),
        ] {
            assert_eq!(
                inputs(map(&mut m, InputEvent::MouseButton { button, down: true })),
                [Input::Button {
                    code,
                    pressed: true
                }]
            );
        }
    }

    #[test]
    fn a_wheel_notch_is_a_hundred_pixels_and_up_is_negative() {
        let mut m = Mapper::default();
        let Action::Input(Input::Axis { x, y }) =
            map(&mut m, InputEvent::ScrollVertical { amount: 120 }).remove(0)
        else {
            panic!("an axis")
        };
        assert_eq!((x, y.round()), (0.0, -100.0));
        let Action::Input(Input::Axis { x, y }) =
            map(&mut m, InputEvent::ScrollVertical { amount: -240 }).remove(0)
        else {
            panic!("an axis")
        };
        assert_eq!((x, y.round()), (0.0, 200.0));
        let Action::Input(Input::Axis { x, y }) =
            map(&mut m, InputEvent::ScrollHorizontal { amount: 120 }).remove(0)
        else {
            panic!("an axis")
        };
        assert_eq!((x.round(), y), (100.0, 0.0));
    }

    #[test]
    fn a_pad_state_becomes_the_standard_mapping() {
        let mut m = Mapper::default();
        // A, left bumper, d-pad up and right, guide; triggers; sticks pushed
        // up (Moonlight: positive Y) and right.
        let buttons = 0x1000 | 0x0100 | 0x0001 | 0x0008 | 0x0400;
        let Action::Pad(pad) = map(
            &mut m,
            state(
                1,
                0b10,
                buttons,
                (255, 51),
                ((32767, 32767), (-32768, -16384)),
            ),
        )
        .remove(0) else {
            panic!("a pad")
        };
        assert_eq!(pad.i, 1);
        assert!(!pad.gone);
        let down: Vec<usize> = pad
            .b
            .iter()
            .enumerate()
            .filter(|(i, v)| **v == 1.0 && *i != 6 && *i != 7)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(down, [0, 4, 12, 15, 16]);
        assert_eq!(pad.b[6], 1.0);
        assert!((pad.b[7] - 0.2).abs() < 1e-6);
        assert_eq!(pad.a[0], 1.0);
        assert_eq!(pad.a[1], -1.0, "up is negative on the W3C's Y");
        assert_eq!(pad.a[2], -1.0, "i16::MIN is as far left as 32767 is right");
        assert!(
            (pad.a[3] - 16384.0 / 32767.0).abs() < 1e-6,
            "down is positive"
        );
    }

    #[test]
    fn the_other_buttons_land_on_their_indexes() {
        let mut m = Mapper::default();
        let buttons = [
            (0x2000, 1),
            (0x4000, 2),
            (0x8000, 3),
            (0x0200, 5),
            (0x0020, 8),
            (0x0010, 9),
            (0x0040, 10),
            (0x0080, 11),
            (0x0002, 13),
            (0x0004, 14),
            (0x10_0000, 17),
        ];
        for (flag, index) in buttons {
            let Action::Pad(pad) =
                map(&mut m, state(0, 1, flag, (0, 0), ((0, 0), (0, 0)))).remove(0)
            else {
                panic!("a pad")
            };
            let down: Vec<usize> = pad
                .b
                .iter()
                .enumerate()
                .filter(|(_, v)| **v == 1.0)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(down, [index], "flag {flag:#x}");
        }
    }

    #[test]
    fn a_pad_that_leaves_the_mask_goes_back_to_rest() {
        let mut m = Mapper::default();
        let first = map(&mut m, state(0, 0b11, 0, (0, 0), ((0, 0), (0, 0))));
        assert_eq!(first.len(), 1);
        // Pad 1's state with pad 0 missing from the mask: pad 0 is gone.
        let out = map(&mut m, state(1, 0b10, 0x1000, (0, 0), ((0, 0), (0, 0))));
        assert_eq!(out.len(), 2);
        let Action::Pad(gone) = &out[0] else {
            panic!("a pad")
        };
        assert!(gone.gone && gone.i == 0);
        let Action::Pad(live) = &out[1] else {
            panic!("a pad")
        };
        assert!(!live.gone && live.i == 1 && live.b[0] == 1.0);
        // A pad reporting while its own bit is clear is itself gone, once.
        let out = map(&mut m, state(1, 0, 0, (0, 0), ((0, 0), (0, 0))));
        assert_eq!(out.len(), 1);
        let Action::Pad(gone) = &out[0] else {
            panic!("a pad")
        };
        assert!(gone.gone && gone.i == 1);
        assert!(map(&mut m, state(1, 0, 0, (0, 0), ((0, 0), (0, 0)))).is_empty());
    }

    #[test]
    fn arrival_makes_the_pad_and_pads_past_the_fourth_are_ignored() {
        let mut m = Mapper::default();
        let arrival = |pad| InputEvent::GamepadArrival {
            pad,
            kind: GamepadKind::PlayStation,
            capabilities: 0,
            supported_buttons: 0,
        };
        let Action::Pad(pad) = map(&mut m, arrival(2)).remove(0) else {
            panic!("a pad")
        };
        assert_eq!((pad.i, pad.kind), (2, Some(PadKind::Playstation)));
        assert!(pad.b.iter().all(|v| *v == 0.0));
        assert!(map(&mut m, arrival(MAX_PADS as u8)).is_empty());
        assert!(
            map(
                &mut m,
                state(MAX_PADS as u16, 0xFFFF, 0x1000, (0, 0), ((0, 0), (0, 0)))
            )
            .iter()
            .all(|a| !matches!(a, Action::Pad(p) if p.i >= MAX_PADS))
        );
    }

    #[test]
    fn touch_pen_and_text_are_ignored() {
        use cha_gamestream::input::{PointerKind, Touch};
        let mut m = Mapper::default();
        assert!(map(&mut m, InputEvent::Text("hi".into())).is_empty());
        assert!(
            map(
                &mut m,
                InputEvent::Touch(Touch {
                    kind: PointerKind::Down,
                    pointer_id: 0,
                    x: 0.5,
                    y: 0.5,
                    pressure: 1.0,
                    rotation: 0,
                    contact_minor: 0.0,
                    contact_major: 0.0,
                })
            )
            .is_empty()
        );
        assert_eq!(m.warned.len(), 2, "each kind is noted once");
    }
}

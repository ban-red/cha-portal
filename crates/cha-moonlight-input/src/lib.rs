//! The browser input model → Moonlight input events.
//!
//! Input arrives as [`cha_client::Input`] (keys by W3C `KeyboardEvent.code`,
//! pointer in the DOM's units, gamepads in the Gamepad API's standard
//! mapping); the events are [`cha_gamestream::InputEvent`]s for a Moonlight
//! host's control stream. One [`InputState`] per viewing session: it knows what
//! that session holds down (keys, mouse buttons, pads), so it can let go of it
//! all when the session loses the controls or leaves. Otherwise a key held
//! when the tab closes would stay held on the host.
//!
//! `cha-gateway` (the browser's input, over its own JSON) and
//! `cha-client-gamestream` (the native player's) both map through here.

mod keys;

use std::collections::{BTreeSet, HashSet};

use cha_client::{Input, PadState};
use cha_gamestream::input::{GamepadKind, MouseButton, buttons, capabilities, modifiers};

pub use cha_gamestream::InputEvent;
pub use keys::{Key, Modifier, lookup as key_lookup, modifier as key_modifier};

/// Moonlight's wheel unit: 120 per notch (`WHEEL_DELTA`).
const WHEEL_NOTCH: f64 = 120.0;
/// What a notch of a mouse wheel is in the DOM's pixels (Chrome and Firefox
/// report 100 for one notch of a physical wheel; the player forwards
/// `deltaY * unit` and nothing else).
const PAGE_PIXELS_PER_NOTCH: f64 = 100.0;

/// More pads than this aren't sent (GameStream's own limit is 4; Sunshine
/// takes more, and the player has 4 slots).
const MAX_PADS: u8 = 16;

/// `PointerEvent.button` → Moonlight's button.
pub fn mouse_button(button: u8) -> Option<MouseButton> {
    Some(match button {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        3 => MouseButton::Side,  // back
        4 => MouseButton::Extra, // forward
        _ => return None,
    })
}

/// The standard mapping's buttons as Moonlight's bitmask ([`buttons`]). The
/// triggers (6 and 7) are analog and travel separately.
pub fn pad_buttons(b: &[f32]) -> u32 {
    const MAP: [(usize, u32); 15] = [
        (0, buttons::A),
        (1, buttons::B),
        (2, buttons::X),
        (3, buttons::Y),
        (4, buttons::LEFT_SHOULDER),
        (5, buttons::RIGHT_SHOULDER),
        (8, buttons::BACK),
        (9, buttons::PLAY),
        (10, buttons::LEFT_STICK),
        (11, buttons::RIGHT_STICK),
        (12, buttons::UP),
        (13, buttons::DOWN),
        (14, buttons::LEFT),
        (15, buttons::RIGHT),
        (16, buttons::SPECIAL), // the guide button
    ];
    let mut pressed = 0;
    for (index, flag) in MAP {
        if b.get(index).is_some_and(|v| *v > 0.5) {
            pressed |= flag;
        }
    }
    pressed
}

/// What an Xbox pad can press, for the host to size its virtual pad.
const SUPPORTED_BUTTONS: u32 = buttons::A
    | buttons::B
    | buttons::X
    | buttons::Y
    | buttons::UP
    | buttons::DOWN
    | buttons::LEFT
    | buttons::RIGHT
    | buttons::LEFT_SHOULDER
    | buttons::RIGHT_SHOULDER
    | buttons::PLAY
    | buttons::BACK
    | buttons::LEFT_STICK
    | buttons::RIGHT_STICK
    | buttons::SPECIAL;

/// A trigger's pull, 0..1, as the wire's byte.
fn trigger(b: &[f32], index: usize) -> u8 {
    let pull = b.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0);
    (pull * 255.0).round() as u8
}

/// A stick axis, -1..1, as the wire's 16-bit value.
fn axis(a: &[f32], index: usize) -> i16 {
    let v = a.get(index).copied().unwrap_or(0.0).clamp(-1.0, 1.0);
    (v * f32::from(i16::MAX)).round() as i16
}

/// One session's input to the host, and what it holds down.
#[derive(Default)]
pub struct InputState {
    keys: BTreeSet<u8>,
    buttons: HashSet<u8>,
    pads: BTreeSet<u8>,
    /// Pixels not yet sent as whole units (a trackpad's small deltas add up).
    rel: (f64, f64),
    wheel: (f64, f64),
}

impl InputState {
    /// The host's events for one input from the player; `width` and `height`
    /// are the stream's size, which absolute positions refer to.
    pub fn apply(&mut self, input: &Input, width: u32, height: u32) -> Vec<InputEvent> {
        match input {
            Input::MouseMove { x, y } => {
                let w = width.clamp(1, i16::MAX as u32);
                let h = height.clamp(1, i16::MAX as u32);
                let at = |v: f32, size: u32| {
                    (f64::from(v).clamp(0.0, 1.0) * f64::from(size - 1)).round() as i16
                };
                vec![InputEvent::MouseMoveAbsolute {
                    x: at(*x, w),
                    y: at(*y, h),
                    width: w as i16,
                    height: h as i16,
                }]
            }
            Input::MouseMotion { dx, dy } => {
                self.rel.0 += f64::from(*dx);
                self.rel.1 += f64::from(*dy);
                let (dx, rest_x) = whole(self.rel.0);
                let (dy, rest_y) = whole(self.rel.1);
                self.rel = (rest_x, rest_y);
                if dx == 0 && dy == 0 {
                    return Vec::new();
                }
                vec![InputEvent::MouseMoveRelative { dx, dy }]
            }
            Input::MouseButton { button: b, down } => {
                let Some(button) = mouse_button(*b) else {
                    return Vec::new();
                };
                let was_down = if *down {
                    !self.buttons.insert(*b)
                } else {
                    self.buttons.remove(b)
                };
                // A repeat of the state it's in, or a release of what it
                // never pressed, would only confuse the host.
                if was_down == *down {
                    return Vec::new();
                }
                vec![InputEvent::MouseButton {
                    button,
                    down: *down,
                }]
            }
            Input::Wheel { dx, dy } => {
                // The DOM scrolls down for positive; Moonlight scrolls up for
                // positive. Horizontal is right for positive in both.
                self.wheel.0 += f64::from(*dx) * WHEEL_NOTCH / PAGE_PIXELS_PER_NOTCH;
                self.wheel.1 += -f64::from(*dy) * WHEEL_NOTCH / PAGE_PIXELS_PER_NOTCH;
                let (amount_x, rest_x) = whole(self.wheel.0);
                let (amount_y, rest_y) = whole(self.wheel.1);
                self.wheel = (rest_x, rest_y);
                let mut events = Vec::new();
                if amount_y != 0 {
                    events.push(InputEvent::ScrollVertical { amount: amount_y });
                }
                if amount_x != 0 {
                    events.push(InputEvent::ScrollHorizontal { amount: amount_x });
                }
                events
            }
            Input::Key { code, down } => {
                let Some(key) = keys::lookup(code) else {
                    return Vec::new();
                };
                // Every code in the table is a one-byte virtual key.
                let Ok(vk) = u8::try_from(key.vk) else {
                    return Vec::new();
                };
                if *down {
                    self.keys.insert(vk);
                } else {
                    self.keys.remove(&vk);
                }
                vec![self.key_event(vk, key.extended, *down)]
            }
            Input::Pad { index, pad } => self.pad(*index, pad),
            Input::PadGone { index } => self.pad_gone(*index),
        }
    }

    /// The Moonlight keyboard event, carrying the modifiers held now.
    fn key_event(&self, vk: u8, extended: bool, down: bool) -> InputEvent {
        let mut held_modifiers = 0;
        for held in &self.keys {
            match keys::modifier(u16::from(*held)) {
                Some(Modifier::Shift) => held_modifiers |= modifiers::SHIFT,
                Some(Modifier::Ctrl) => held_modifiers |= modifiers::CTRL,
                Some(Modifier::Alt) => held_modifiers |= modifiers::ALT,
                Some(Modifier::Meta) => held_modifiers |= modifiers::META,
                None => {}
            }
        }
        if extended {
            held_modifiers |= modifiers::EXTENDED;
        }
        InputEvent::Key {
            down,
            vk,
            modifiers: held_modifiers,
        }
    }

    /// The pads connected, a bit each: what every pad state tells the host.
    fn pad_mask(&self) -> u16 {
        self.pads.iter().fold(0, |mask, pad| mask | 1 << pad)
    }

    /// A pad letting go of everything and leaving the mask: how a client
    /// tells the host a pad is gone.
    fn pad_leaves(&mut self, index: u8) -> Option<InputEvent> {
        if !self.pads.remove(&index) {
            return None;
        }
        Some(InputEvent::GamepadState {
            pad: u16::from(index),
            active_mask: self.pad_mask(),
            buttons: 0,
            left_trigger: 0,
            right_trigger: 0,
            left_stick: (0, 0),
            right_stick: (0, 0),
        })
    }

    fn pad_gone(&mut self, index: u8) -> Vec<InputEvent> {
        self.pad_leaves(index).into_iter().collect()
    }

    fn pad(&mut self, index: u8, pad: &PadState) -> Vec<InputEvent> {
        if index >= MAX_PADS {
            return Vec::new();
        }
        let mut events = Vec::new();
        if self.pads.insert(index) {
            events.push(InputEvent::GamepadArrival {
                pad: index,
                kind: GamepadKind::Xbox,
                capabilities: capabilities::ANALOG_TRIGGERS | capabilities::RUMBLE,
                supported_buttons: SUPPORTED_BUTTONS,
            });
        }
        events.push(InputEvent::GamepadState {
            pad: u16::from(index),
            active_mask: self.pad_mask(),
            buttons: pad_buttons(&pad.buttons),
            left_trigger: trigger(&pad.buttons, 6),
            right_trigger: trigger(&pad.buttons, 7),
            // The Gamepad API's Y is down for positive; Moonlight's is up.
            left_stick: (axis(&pad.axes, 0), axis(&pad.axes, 1).saturating_neg()),
            right_stick: (axis(&pad.axes, 2), axis(&pad.axes, 3).saturating_neg()),
        });
        events
    }

    /// Lets go of everything held: keys up, buttons up, pads disconnected.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let mut events = Vec::new();
        let held: Vec<u8> = self.keys.iter().copied().collect();
        // Modifiers last, so the others go up with them still described.
        for vk in held
            .iter()
            .filter(|vk| keys::modifier(u16::from(**vk)).is_none())
        {
            self.keys.remove(vk);
            events.push(self.key_event(*vk, false, false));
        }
        for vk in held
            .iter()
            .filter(|vk| keys::modifier(u16::from(**vk)).is_some())
        {
            self.keys.remove(vk);
            events.push(self.key_event(*vk, false, false));
        }
        let mut pressed: Vec<u8> = self.buttons.drain().collect();
        pressed.sort_unstable();
        for b in pressed {
            if let Some(button) = mouse_button(b) {
                events.push(InputEvent::MouseButton {
                    button,
                    down: false,
                });
            }
        }
        // One at a time, so each state's mask drops its own pad.
        let pads: Vec<u8> = self.pads.iter().copied().collect();
        for i in pads {
            events.extend(self.pad_leaves(i));
        }
        events
    }
}

/// The whole units of `v` (as Moonlight's i16) and what's left.
fn whole(v: f64) -> (i16, f64) {
    let units = v.trunc().clamp(f64::from(i16::MIN), f64::from(i16::MAX));
    (units as i16, v - units)
}

/// The motors of a host rumble packet as levels: the
/// strong (low-frequency) and weak (high-frequency) motors, 0..1.
pub fn rumble_levels(low_frequency: u16, high_frequency: u16) -> (f32, f32) {
    (
        f32::from(low_frequency) / f32::from(u16::MAX),
        f32::from(high_frequency) / f32::from(u16::MAX),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 2560;
    const H: u32 = 1440;

    fn key(code: &str, down: bool) -> Input {
        Input::Key {
            code: code.into(),
            down,
        }
    }

    fn button(button: u8, down: bool) -> Input {
        Input::MouseButton { button, down }
    }

    fn pad(index: u8, buttons: &[f32], axes: &[f32]) -> Input {
        Input::Pad {
            index,
            pad: PadState {
                buttons: buttons.to_vec(),
                axes: axes.to_vec(),
            },
        }
    }

    fn apply(state: &mut InputState, input: Input) -> Vec<InputEvent> {
        state.apply(&input, W, H)
    }

    #[test]
    fn mouse_buttons_follow_pointer_events() {
        assert_eq!(mouse_button(0), Some(MouseButton::Left));
        assert_eq!(mouse_button(1), Some(MouseButton::Middle));
        assert_eq!(mouse_button(2), Some(MouseButton::Right));
        assert_eq!(mouse_button(3), Some(MouseButton::Side));
        assert_eq!(mouse_button(4), Some(MouseButton::Extra));
        assert_eq!(mouse_button(5), None);
    }

    #[test]
    fn button_events_press_and_release_once() {
        let mut s = InputState::default();
        let down = apply(&mut s, button(2, true));
        assert!(matches!(
            down[..],
            [InputEvent::MouseButton {
                down: true,
                button: MouseButton::Right
            }]
        ));
        // A repeat press does nothing.
        assert!(apply(&mut s, button(2, true)).is_empty());
        let up = apply(&mut s, button(2, false));
        assert!(matches!(
            up[..],
            [InputEvent::MouseButton {
                down: false,
                button: MouseButton::Right
            }]
        ));
        // A release of what isn't held, and an unknown button, are dropped.
        assert!(apply(&mut s, button(2, false)).is_empty());
        assert!(apply(&mut s, button(9, true)).is_empty());
    }

    #[test]
    fn absolute_moves_scale_to_the_stream_size() {
        let mut s = InputState::default();
        let events = apply(&mut s, Input::MouseMove { x: 0.5, y: 1.0 });
        assert!(matches!(
            events[..],
            [InputEvent::MouseMoveAbsolute {
                x: 1280,
                y: 1439,
                width: 2560,
                height: 1440
            }]
        ));
        // Out of range clamps to the picture.
        let events = apply(&mut s, Input::MouseMove { x: -3.0, y: 9.0 });
        assert!(matches!(
            events[..],
            [InputEvent::MouseMoveAbsolute { x: 0, y: 1439, .. }]
        ));
    }

    #[test]
    fn relative_moves_keep_their_fractions() {
        let mut s = InputState::default();
        assert!(apply(&mut s, Input::MouseMotion { dx: 0.4, dy: 0.4 }).is_empty());
        let events = apply(&mut s, Input::MouseMotion { dx: 0.7, dy: -3.2 });
        assert!(matches!(
            events[..],
            [InputEvent::MouseMoveRelative { dx: 1, dy: -2 }]
        ));
    }

    #[test]
    fn a_wheel_notch_is_120_and_down_is_negative() {
        let mut s = InputState::default();
        let wheel = |dx, dy| Input::Wheel { dx, dy };
        // The player forwards 100 px per notch of a physical wheel; down is positive there.
        let down = apply(&mut s, wheel(0.0, 100.0));
        assert!(matches!(
            down[..],
            [InputEvent::ScrollVertical { amount: -120 }]
        ));
        let up = apply(&mut s, wheel(0.0, -100.0));
        assert!(matches!(
            up[..],
            [InputEvent::ScrollVertical { amount: 120 }]
        ));
        let right = apply(&mut s, wheel(50.0, 0.0));
        assert!(matches!(
            right[..],
            [InputEvent::ScrollHorizontal { amount: 60 }]
        ));
        // Trackpad crumbs add up instead of vanishing.
        for _ in 0..2 {
            assert!(apply(&mut s, wheel(0.0, -0.3)).is_empty());
        }
        let crumbs = apply(&mut s, wheel(0.0, -0.3));
        assert!(matches!(
            crumbs[..],
            [InputEvent::ScrollVertical { amount: 1 }]
        ));
    }

    #[test]
    fn keys_carry_modifier_flags() {
        let mut s = InputState::default();
        let shift = apply(&mut s, key("ShiftLeft", true));
        assert!(matches!(
            shift[..],
            [InputEvent::Key {
                down: true,
                vk: 0xA0,
                modifiers: modifiers::SHIFT
            }]
        ));
        let a = apply(&mut s, key("KeyA", true));
        assert!(matches!(
            a[..],
            [InputEvent::Key {
                down: true,
                vk: 0x41,
                modifiers: modifiers::SHIFT
            }]
        ));
        apply(&mut s, key("ControlRight", true));
        let up = apply(&mut s, key("KeyA", false));
        assert!(matches!(
            up[..],
            [InputEvent::Key {
                down: false,
                modifiers,
                ..
            }] if modifiers == modifiers::SHIFT | modifiers::CTRL
        ));
        // Releasing the modifier clears its flag on its own event.
        let up = apply(&mut s, key("ShiftLeft", false));
        assert!(matches!(
            up[..],
            [InputEvent::Key { modifiers, .. }] if modifiers == modifiers::CTRL
        ));
        // A key with no Windows code is dropped.
        assert!(apply(&mut s, key("Unidentified", true)).is_empty());
    }

    #[test]
    fn numpad_enter_is_extended() {
        let mut s = InputState::default();
        let events = apply(&mut s, key("NumpadEnter", true));
        assert!(matches!(
            events[..],
            [InputEvent::Key { vk: 0x0D, modifiers, .. }] if modifiers == modifiers::EXTENDED
        ));
        let events = apply(&mut s, key("Enter", true));
        assert!(matches!(
            events[..],
            [InputEvent::Key {
                vk: 0x0D,
                modifiers: 0,
                ..
            }]
        ));
    }

    #[test]
    fn pad_buttons_follow_the_standard_mapping() {
        let mut b = vec![0.0; 17];
        assert_eq!(pad_buttons(&b), 0);
        for (index, flag) in [
            (0, buttons::A),
            (1, buttons::B),
            (2, buttons::X),
            (3, buttons::Y),
            (4, buttons::LEFT_SHOULDER),
            (5, buttons::RIGHT_SHOULDER),
            (8, buttons::BACK),
            (9, buttons::PLAY),
            (10, buttons::LEFT_STICK),
            (11, buttons::RIGHT_STICK),
            (12, buttons::UP),
            (13, buttons::DOWN),
            (14, buttons::LEFT),
            (15, buttons::RIGHT),
            (16, buttons::SPECIAL),
        ] {
            b.fill(0.0);
            b[index] = 1.0;
            assert_eq!(pad_buttons(&b), flag, "button {index}");
        }
        b.fill(0.0);
        // The triggers are not buttons.
        b[6] = 1.0;
        b[7] = 1.0;
        assert_eq!(pad_buttons(&b), 0);
        // Several at once, and a short array.
        b[0] = 1.0;
        b[3] = 1.0;
        assert_eq!(pad_buttons(&b), buttons::A | buttons::Y);
        assert_eq!(pad_buttons(&[1.0]), buttons::A);
        // Half-pressed counts as pressed only past the middle.
        assert_eq!(pad_buttons(&[0.4]), 0);
    }

    #[test]
    fn pads_connect_once_scale_and_flip_y() {
        let mut s = InputState::default();
        let first = apply(
            &mut s,
            pad(
                1,
                &[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 1.0],
                &[0.25, -1.0, -0.5, 1.0],
            ),
        );
        assert_eq!(first.len(), 2);
        assert!(matches!(
            first[0],
            InputEvent::GamepadArrival {
                pad: 1,
                kind: GamepadKind::Xbox,
                ..
            }
        ));
        let InputEvent::GamepadState {
            pad: number,
            active_mask,
            buttons: pressed,
            left_trigger,
            right_trigger,
            left_stick,
            right_stick,
        } = first[1].clone()
        else {
            panic!("a state event follows the arrival");
        };
        assert_eq!(number, 1);
        assert_eq!(active_mask, 0b10);
        assert_eq!(pressed, buttons::A);
        assert_eq!(left_trigger, 128);
        assert_eq!(right_trigger, 255);
        assert_eq!(left_stick.0, 8192);
        // Up on the page is negative; Moonlight's up is positive.
        assert_eq!(left_stick.1, i16::MAX);
        assert_eq!(right_stick, (-16384, -i16::MAX));

        // The second report is a state alone.
        let second = apply(&mut s, pad(1, &[], &[]));
        assert!(matches!(second[..], [InputEvent::GamepadState { .. }]));
        // Out-of-range axes clamp.
        let wild = apply(&mut s, pad(1, &[], &[7.0, -7.0]));
        assert!(matches!(
            wild[..],
            [InputEvent::GamepadState {
                left_stick: (32767, 32767),
                ..
            }]
        ));
    }

    #[test]
    fn gone_pads_leave_the_mask_and_can_come_back() {
        let mut s = InputState::default();
        assert!(apply(&mut s, Input::PadGone { index: 0 }).is_empty());
        apply(&mut s, pad(0, &[], &[]));
        let second = apply(&mut s, pad(2, &[], &[]));
        assert!(matches!(
            second[1],
            InputEvent::GamepadState {
                pad: 2,
                active_mask: 0b101,
                ..
            }
        ));
        // Pad 0 goes: a state of nothing held, with its bit cleared.
        let gone = apply(&mut s, Input::PadGone { index: 0 });
        assert!(matches!(
            gone[..],
            [InputEvent::GamepadState {
                pad: 0,
                active_mask: 0b100,
                buttons: 0,
                left_trigger: 0,
                right_trigger: 0,
                left_stick: (0, 0),
                right_stick: (0, 0),
            }]
        ));
        let back = apply(&mut s, pad(0, &[], &[]));
        assert!(matches!(back[0], InputEvent::GamepadArrival { pad: 0, .. }));
        // A slot past the limit is ignored.
        assert!(apply(&mut s, pad(40, &[], &[])).is_empty());
    }

    #[test]
    fn release_all_lets_go_of_everything() {
        let mut s = InputState::default();
        apply(&mut s, key("ShiftLeft", true));
        apply(&mut s, key("KeyW", true));
        apply(&mut s, button(0, true));
        apply(&mut s, pad(2, &[], &[]));
        apply(&mut s, pad(3, &[], &[]));
        let events = s.release_all();
        assert_eq!(events.len(), 5);
        // The ordinary key goes up before the modifier it was pressed under.
        assert!(matches!(
            events[0],
            InputEvent::Key {
                down: false,
                vk: 0x57,
                ..
            }
        ));
        assert!(matches!(
            events[1],
            InputEvent::Key {
                down: false,
                vk: 0xA0,
                ..
            }
        ));
        assert!(matches!(
            events[2],
            InputEvent::MouseButton {
                button: MouseButton::Left,
                down: false
            }
        ));
        // Each pad leaves in turn, the mask shrinking as they go.
        assert!(matches!(
            events[3],
            InputEvent::GamepadState {
                pad: 2,
                active_mask: 0b1000,
                ..
            }
        ));
        assert!(matches!(
            events[4],
            InputEvent::GamepadState {
                pad: 3,
                active_mask: 0,
                ..
            }
        ));
        assert!(s.release_all().is_empty());
    }

    #[test]
    fn rumble_motors_scale_to_unit_range() {
        assert_eq!(rumble_levels(0, 0), (0.0, 0.0));
        assert_eq!(rumble_levels(u16::MAX, 0), (1.0, 0.0));
        let (lo, hi) = rumble_levels(0, 0x8000);
        assert_eq!(lo, 0.0);
        assert!((hi - 0.5).abs() < 0.001);
    }
}

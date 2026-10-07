//! The browser input model → Moonlight input events.
//!
//! Input arrives as [`cha_client::Input`] (keys by W3C `KeyboardEvent.code`,
//! pointer in the DOM's units, gamepads in the Gamepad API's standard
//! mapping); the events go to a Moonlight host's control stream. One
//! [`InputState`] per viewing session: it knows what that session holds down
//! (keys, mouse buttons, pads), so it can let go of it all when the session
//! loses the controls or leaves. Otherwise a key held when the tab closes would
//! stay held on the host.
//!
//! `cha-gateway` (the browser's input, over its own JSON) and
//! `cha-client-gamestream` (the native player's) both map through here.

mod keys;

use std::collections::{BTreeSet, HashSet};

use cha_client::{Input, PadState};
use moonlight_common::stream::control::{
    ControllerButtons, ControllerCapabilities, ControllerType, KeyAction, KeyCode, KeyFlags,
    KeyModifiers, MouseButton, MouseButtonAction,
};
pub use moonlight_common::stream::proto::control::input_batcher::ClientInputEvent;

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
        3 => MouseButton::X1, // back
        4 => MouseButton::X2, // forward
        _ => return None,
    })
}

/// The standard mapping's buttons as Moonlight's bitmask. The triggers
/// (6 and 7) are analog and travel separately.
pub fn pad_buttons(b: &[f32]) -> ControllerButtons {
    const MAP: [(usize, ControllerButtons); 15] = [
        (0, ControllerButtons::A),
        (1, ControllerButtons::B),
        (2, ControllerButtons::X),
        (3, ControllerButtons::Y),
        (4, ControllerButtons::LB),
        (5, ControllerButtons::RB),
        (8, ControllerButtons::BACK),
        (9, ControllerButtons::PLAY),
        (10, ControllerButtons::LS_CLK),
        (11, ControllerButtons::RS_CLK),
        (12, ControllerButtons::UP),
        (13, ControllerButtons::DOWN),
        (14, ControllerButtons::LEFT),
        (15, ControllerButtons::RIGHT),
        (16, ControllerButtons::SPECIAL), // the guide button
    ];
    let mut buttons = ControllerButtons::empty();
    for (index, flag) in MAP {
        if b.get(index).is_some_and(|v| *v > 0.5) {
            buttons |= flag;
        }
    }
    buttons
}

/// What an Xbox pad can press, for the host to size its virtual pad.
fn supported_buttons() -> ControllerButtons {
    ControllerButtons::A
        | ControllerButtons::B
        | ControllerButtons::X
        | ControllerButtons::Y
        | ControllerButtons::UP
        | ControllerButtons::DOWN
        | ControllerButtons::LEFT
        | ControllerButtons::RIGHT
        | ControllerButtons::LB
        | ControllerButtons::RB
        | ControllerButtons::PLAY
        | ControllerButtons::BACK
        | ControllerButtons::LS_CLK
        | ControllerButtons::RS_CLK
        | ControllerButtons::SPECIAL
}

/// A trigger's pull, 0..1.
fn trigger(b: &[f32], index: usize) -> f32 {
    b.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0)
}

/// A stick axis, -1..1.
fn axis(a: &[f32], index: usize) -> f32 {
    a.get(index).copied().unwrap_or(0.0).clamp(-1.0, 1.0)
}

/// One session's input to the host, and what it holds down.
#[derive(Default)]
pub struct InputState {
    keys: BTreeSet<u16>,
    buttons: HashSet<u8>,
    pads: BTreeSet<u8>,
    /// Pixels not yet sent as whole units (a trackpad's small deltas add up).
    rel: (f64, f64),
    wheel: (f64, f64),
}

impl InputState {
    /// The host's events for one input from the player; `width` and `height`
    /// are the stream's size, which absolute positions refer to.
    pub fn apply(&mut self, input: &Input, width: u32, height: u32) -> Vec<ClientInputEvent> {
        match input {
            Input::MouseMove { x, y } => {
                let w = width.clamp(1, i16::MAX as u32);
                let h = height.clamp(1, i16::MAX as u32);
                let at = |v: f32, size: u32| {
                    (f64::from(v).clamp(0.0, 1.0) * f64::from(size - 1)).round() as i16
                };
                vec![ClientInputEvent::MouseMoveAbsolute {
                    x: at(*x, w),
                    y: at(*y, h),
                    reference_width: w as i16,
                    reference_height: h as i16,
                }]
            }
            Input::MouseMotion { dx, dy } => {
                self.rel.0 += f64::from(*dx);
                self.rel.1 += f64::from(*dy);
                let (delta_x, rest_x) = whole(self.rel.0);
                let (delta_y, rest_y) = whole(self.rel.1);
                self.rel = (rest_x, rest_y);
                if delta_x == 0 && delta_y == 0 {
                    return Vec::new();
                }
                vec![ClientInputEvent::MouseMoveRelative { delta_x, delta_y }]
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
                vec![ClientInputEvent::MouseButton {
                    action: if *down {
                        MouseButtonAction::Press
                    } else {
                        MouseButtonAction::Release
                    },
                    button,
                }]
            }
            Input::Wheel { dx, dy } => {
                // The DOM scrolls down for positive; Moonlight scrolls up for
                // positive. Horizontal is right for positive in both.
                self.wheel.0 += f64::from(*dx) * WHEEL_NOTCH / PAGE_PIXELS_PER_NOTCH;
                self.wheel.1 += -f64::from(*dy) * WHEEL_NOTCH / PAGE_PIXELS_PER_NOTCH;
                let (scroll_x, rest_x) = whole(self.wheel.0);
                let (scroll_y, rest_y) = whole(self.wheel.1);
                self.wheel = (rest_x, rest_y);
                let mut events = Vec::new();
                if scroll_y != 0 {
                    events.push(ClientInputEvent::MouseScrollVertical { scroll_y });
                }
                if scroll_x != 0 {
                    events.push(ClientInputEvent::MouseScrollHorizontal { scroll_x });
                }
                events
            }
            Input::Key { code, down } => {
                let Some(key) = keys::lookup(code) else {
                    return Vec::new();
                };
                if *down {
                    self.keys.insert(key.vk);
                } else {
                    self.keys.remove(&key.vk);
                }
                vec![self.key_event(key.vk, key.extended, *down)]
            }
            Input::Pad { index, pad } => self.pad(*index, pad),
            Input::PadGone { index } => self.pad_gone(*index),
        }
    }

    /// The Moonlight keyboard event. The key code carries the 0x8000 marker
    /// every Moonlight client sets on its (normalized) virtual-key codes.
    fn key_event(&self, vk: u16, extended: bool, down: bool) -> ClientInputEvent {
        let mut modifiers = KeyModifiers::empty();
        for held in &self.keys {
            match keys::modifier(*held) {
                Some(Modifier::Shift) => modifiers |= KeyModifiers::SHIFT,
                Some(Modifier::Ctrl) => modifiers |= KeyModifiers::CTRL,
                Some(Modifier::Alt) => modifiers |= KeyModifiers::ALT,
                Some(Modifier::Meta) => modifiers |= KeyModifiers::META,
                None => {}
            }
        }
        if extended {
            modifiers |= KeyModifiers::EXTENDED;
        }
        ClientInputEvent::Keyboard {
            action: if down { KeyAction::Down } else { KeyAction::Up },
            flags: KeyFlags::empty(),
            key_code: KeyCode((0x8000 | vk) as i16),
            modifiers,
        }
    }

    fn pad_gone(&mut self, index: u8) -> Vec<ClientInputEvent> {
        if self.pads.remove(&index) {
            vec![ClientInputEvent::ControllerDisconnect {
                controller_number: index,
            }]
        } else {
            Vec::new()
        }
    }

    fn pad(&mut self, index: u8, pad: &PadState) -> Vec<ClientInputEvent> {
        if index >= MAX_PADS {
            return Vec::new();
        }
        let controller_number = index;
        let mut events = Vec::new();
        if self.pads.insert(index) {
            events.push(ClientInputEvent::ControllerConnect {
                controller_number,
                ty: ControllerType::Xbox,
                capabilities: ControllerCapabilities::ANALOG_TRIGGERS
                    | ControllerCapabilities::RUMBLE,
                supported_buttons: supported_buttons(),
            });
        }
        events.push(ClientInputEvent::ControllerState {
            controller_number,
            pressed_buttons: pad_buttons(&pad.buttons),
            left_trigger: trigger(&pad.buttons, 6),
            right_trigger: trigger(&pad.buttons, 7),
            left_stick_x: axis(&pad.axes, 0),
            // The Gamepad API's Y is down for positive; Moonlight's is up.
            left_stick_y: -axis(&pad.axes, 1),
            right_stick_x: axis(&pad.axes, 2),
            right_stick_y: -axis(&pad.axes, 3),
        });
        events
    }

    /// Lets go of everything held: keys up, buttons up, pads disconnected.
    pub fn release_all(&mut self) -> Vec<ClientInputEvent> {
        let mut events = Vec::new();
        let held: Vec<u16> = self.keys.iter().copied().collect();
        // Modifiers last, so the others go up with them still described.
        for vk in held.iter().filter(|vk| keys::modifier(**vk).is_none()) {
            self.keys.remove(vk);
            events.push(self.key_event(*vk, false, false));
        }
        for vk in held.iter().filter(|vk| keys::modifier(**vk).is_some()) {
            self.keys.remove(vk);
            events.push(self.key_event(*vk, false, false));
        }
        let mut buttons: Vec<u8> = self.buttons.drain().collect();
        buttons.sort_unstable();
        for b in buttons {
            if let Some(button) = mouse_button(b) {
                events.push(ClientInputEvent::MouseButton {
                    action: MouseButtonAction::Release,
                    button,
                });
            }
        }
        for i in std::mem::take(&mut self.pads) {
            events.push(ClientInputEvent::ControllerDisconnect {
                controller_number: i,
            });
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

    fn apply(state: &mut InputState, input: Input) -> Vec<ClientInputEvent> {
        state.apply(&input, W, H)
    }

    #[test]
    fn mouse_buttons_follow_pointer_events() {
        assert_eq!(mouse_button(0), Some(MouseButton::Left));
        assert_eq!(mouse_button(1), Some(MouseButton::Middle));
        assert_eq!(mouse_button(2), Some(MouseButton::Right));
        assert_eq!(mouse_button(3), Some(MouseButton::X1));
        assert_eq!(mouse_button(4), Some(MouseButton::X2));
        assert_eq!(mouse_button(5), None);
    }

    #[test]
    fn button_events_press_and_release_once() {
        let mut s = InputState::default();
        let down = apply(&mut s, button(2, true));
        assert!(matches!(
            down[..],
            [ClientInputEvent::MouseButton {
                action: MouseButtonAction::Press,
                button: MouseButton::Right
            }]
        ));
        // A repeat press does nothing.
        assert!(apply(&mut s, button(2, true)).is_empty());
        let up = apply(&mut s, button(2, false));
        assert!(matches!(
            up[..],
            [ClientInputEvent::MouseButton {
                action: MouseButtonAction::Release,
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
            [ClientInputEvent::MouseMoveAbsolute {
                x: 1280,
                y: 1439,
                reference_width: 2560,
                reference_height: 1440
            }]
        ));
        // Out of range clamps to the picture.
        let events = apply(&mut s, Input::MouseMove { x: -3.0, y: 9.0 });
        assert!(matches!(
            events[..],
            [ClientInputEvent::MouseMoveAbsolute { x: 0, y: 1439, .. }]
        ));
    }

    #[test]
    fn relative_moves_keep_their_fractions() {
        let mut s = InputState::default();
        assert!(apply(&mut s, Input::MouseMotion { dx: 0.4, dy: 0.4 }).is_empty());
        let events = apply(&mut s, Input::MouseMotion { dx: 0.7, dy: -3.2 });
        assert!(matches!(
            events[..],
            [ClientInputEvent::MouseMoveRelative {
                delta_x: 1,
                delta_y: -2
            }]
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
            [ClientInputEvent::MouseScrollVertical { scroll_y: -120 }]
        ));
        let up = apply(&mut s, wheel(0.0, -100.0));
        assert!(matches!(
            up[..],
            [ClientInputEvent::MouseScrollVertical { scroll_y: 120 }]
        ));
        let right = apply(&mut s, wheel(50.0, 0.0));
        assert!(matches!(
            right[..],
            [ClientInputEvent::MouseScrollHorizontal { scroll_x: 60 }]
        ));
        // Trackpad crumbs add up instead of vanishing.
        for _ in 0..2 {
            assert!(apply(&mut s, wheel(0.0, -0.3)).is_empty());
        }
        let crumbs = apply(&mut s, wheel(0.0, -0.3));
        assert!(matches!(
            crumbs[..],
            [ClientInputEvent::MouseScrollVertical { scroll_y: 1 }]
        ));
    }

    #[test]
    fn keys_carry_modifier_flags() {
        let mut s = InputState::default();
        let shift = apply(&mut s, key("ShiftLeft", true));
        assert!(matches!(
            shift[..],
            [ClientInputEvent::Keyboard {
                action: KeyAction::Down,
                key_code: KeyCode(k),
                modifiers,
                ..
            }] if k == 0x80A0_u16 as i16 && modifiers == KeyModifiers::SHIFT
        ));
        let a = apply(&mut s, key("KeyA", true));
        assert!(matches!(
            a[..],
            [ClientInputEvent::Keyboard {
                action: KeyAction::Down,
                key_code: KeyCode(k),
                modifiers,
                ..
            }] if k == 0x8041_u16 as i16 && modifiers == KeyModifiers::SHIFT
        ));
        apply(&mut s, key("ControlRight", true));
        let up = apply(&mut s, key("KeyA", false));
        assert!(matches!(
            up[..],
            [ClientInputEvent::Keyboard {
                action: KeyAction::Up,
                modifiers,
                ..
            }] if modifiers == KeyModifiers::SHIFT | KeyModifiers::CTRL
        ));
        // Releasing the modifier clears its flag on its own event.
        let up = apply(&mut s, key("ShiftLeft", false));
        assert!(matches!(
            up[..],
            [ClientInputEvent::Keyboard { modifiers, .. }] if modifiers == KeyModifiers::CTRL
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
            [ClientInputEvent::Keyboard { modifiers, .. }] if modifiers == KeyModifiers::EXTENDED
        ));
    }

    #[test]
    fn pad_buttons_follow_the_standard_mapping() {
        let mut b = vec![0.0; 17];
        assert_eq!(pad_buttons(&b), ControllerButtons::empty());
        for (index, flag) in [
            (0, ControllerButtons::A),
            (1, ControllerButtons::B),
            (2, ControllerButtons::X),
            (3, ControllerButtons::Y),
            (4, ControllerButtons::LB),
            (5, ControllerButtons::RB),
            (8, ControllerButtons::BACK),
            (9, ControllerButtons::PLAY),
            (10, ControllerButtons::LS_CLK),
            (11, ControllerButtons::RS_CLK),
            (12, ControllerButtons::UP),
            (13, ControllerButtons::DOWN),
            (14, ControllerButtons::LEFT),
            (15, ControllerButtons::RIGHT),
            (16, ControllerButtons::SPECIAL),
        ] {
            b.fill(0.0);
            b[index] = 1.0;
            assert_eq!(pad_buttons(&b), flag, "button {index}");
        }
        b.fill(0.0);
        // The triggers are not buttons.
        b[6] = 1.0;
        b[7] = 1.0;
        assert_eq!(pad_buttons(&b), ControllerButtons::empty());
        // Several at once, and a short array.
        b[0] = 1.0;
        b[3] = 1.0;
        assert_eq!(pad_buttons(&b), ControllerButtons::A | ControllerButtons::Y);
        assert_eq!(pad_buttons(&[1.0]), ControllerButtons::A);
        // Half-pressed counts as pressed only past the middle.
        assert_eq!(pad_buttons(&[0.4]), ControllerButtons::empty());
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
            ClientInputEvent::ControllerConnect {
                controller_number: 1,
                ty: ControllerType::Xbox,
                ..
            }
        ));
        let ClientInputEvent::ControllerState {
            controller_number,
            pressed_buttons,
            left_trigger,
            right_trigger,
            left_stick_x,
            left_stick_y,
            right_stick_x,
            right_stick_y,
        } = first[1].clone()
        else {
            panic!("a state event follows the connect");
        };
        assert_eq!(controller_number, 1);
        assert_eq!(pressed_buttons, ControllerButtons::A);
        assert_eq!(left_trigger, 0.5);
        assert_eq!(right_trigger, 1.0);
        assert_eq!(left_stick_x, 0.25);
        // Up on the page is negative; Moonlight's up is positive.
        assert_eq!(left_stick_y, 1.0);
        assert_eq!(right_stick_x, -0.5);
        assert_eq!(right_stick_y, -1.0);

        // The second report is a state alone.
        let second = apply(&mut s, pad(1, &[], &[]));
        assert!(matches!(
            second[..],
            [ClientInputEvent::ControllerState { .. }]
        ));
        // Out-of-range axes clamp.
        let wild = apply(&mut s, pad(1, &[], &[7.0, -7.0]));
        assert!(matches!(
            wild[..],
            [ClientInputEvent::ControllerState {
                left_stick_x: 1.0,
                left_stick_y: 1.0,
                ..
            }]
        ));
    }

    #[test]
    fn gone_pads_disconnect_and_can_come_back() {
        let mut s = InputState::default();
        assert!(apply(&mut s, Input::PadGone { index: 0 }).is_empty());
        apply(&mut s, pad(0, &[], &[]));
        let gone = apply(&mut s, Input::PadGone { index: 0 });
        assert!(matches!(
            gone[..],
            [ClientInputEvent::ControllerDisconnect {
                controller_number: 0
            }]
        ));
        let back = apply(&mut s, pad(0, &[], &[]));
        assert!(matches!(
            back[0],
            ClientInputEvent::ControllerConnect { .. }
        ));
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
        let events = s.release_all();
        assert_eq!(events.len(), 4);
        // The ordinary key goes up before the modifier it was pressed under.
        assert!(matches!(
            events[0],
            ClientInputEvent::Keyboard {
                action: KeyAction::Up,
                key_code: KeyCode(k),
                ..
            } if k == 0x8057_u16 as i16
        ));
        assert!(matches!(
            events[1],
            ClientInputEvent::Keyboard {
                action: KeyAction::Up,
                ..
            }
        ));
        assert!(matches!(
            events[2],
            ClientInputEvent::MouseButton {
                action: MouseButtonAction::Release,
                button: MouseButton::Left
            }
        ));
        assert!(matches!(
            events[3],
            ClientInputEvent::ControllerDisconnect {
                controller_number: 2
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

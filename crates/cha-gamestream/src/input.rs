// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: the parse halves of control/input/{mod,keyboard,mouse,touch,gamepad}.rs, with neutral
// output types (no evdev codes, no inputtino) and errors that say what was wrong.

//! Input events from the client, as the control stream carries them, parsed
//! into neutral types. Mapping them to a keyboard layout, a compositor or
//! uinput is the backend's business.
//!
//! A packet here is what follows the control message's length prefix: a
//! little-endian `u32` event type, then the body. Mouse fields are big-endian,
//! everything else little-endian, as Moonlight writes them.

/// Moonlight's modifier bits on keyboard events.
pub mod modifiers {
    pub const SHIFT: u8 = 0x01;
    pub const CTRL: u8 = 0x02;
    pub const ALT: u8 = 0x04;
    pub const META: u8 = 0x08;
    /// An extended key (the 0xE0 scancode prefix): tells Numpad Enter from
    /// Enter, which share a virtual-key code.
    pub const EXTENDED: u8 = 0x10;
}

/// Moonlight's gamepad button flags, for [`InputEvent::GamepadState::buttons`]
/// and the `supported_buttons` of an arrival.
pub mod buttons {
    pub const UP: u32 = 0x0001;
    pub const DOWN: u32 = 0x0002;
    pub const LEFT: u32 = 0x0004;
    pub const RIGHT: u32 = 0x0008;
    /// Start.
    pub const PLAY: u32 = 0x0010;
    /// Back, Select.
    pub const BACK: u32 = 0x0020;
    pub const LEFT_STICK: u32 = 0x0040;
    pub const RIGHT_STICK: u32 = 0x0080;
    pub const LEFT_SHOULDER: u32 = 0x0100;
    pub const RIGHT_SHOULDER: u32 = 0x0200;
    /// The guide button.
    pub const SPECIAL: u32 = 0x0400;
    pub const A: u32 = 0x1000;
    pub const B: u32 = 0x2000;
    pub const X: u32 = 0x4000;
    pub const Y: u32 = 0x8000;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    /// The back button.
    Side,
    /// The forward button.
    Extra,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Hover,
    Down,
    Up,
    Move,
    Cancel,
    ButtonOnly,
    HoverLeave,
    CancelAll,
}

impl PointerKind {
    fn from_wire(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Hover,
            1 => Self::Down,
            2 => Self::Up,
            3 => Self::Move,
            4 => Self::Cancel,
            5 => Self::ButtonOnly,
            6 => Self::HoverLeave,
            7 => Self::CancelAll,
            _ => return None,
        })
    }

    fn has_position(self) -> bool {
        matches!(self, Self::Hover | Self::Down | Self::Up | Self::Move)
    }
}

/// A finger on the video area; `x` and `y` run 0.0 to 1.0 from the top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Touch {
    pub kind: PointerKind,
    pub pointer_id: u32,
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    /// Degrees from vertical, 0..360; 0xFFFF when the client doesn't know.
    pub rotation: u16,
    pub contact_minor: f32,
    pub contact_major: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PenTool {
    Unknown,
    Pen,
    Eraser,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pen {
    pub kind: PointerKind,
    pub tool: PenTool,
    /// Bit 0 primary, bit 1 secondary, bit 2 tertiary.
    pub buttons: u8,
    pub x: f32,
    pub y: f32,
    pub pressure_or_distance: f32,
    pub rotation: u16,
    pub tilt: u8,
    pub contact_minor: f32,
    pub contact_major: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GamepadKind {
    Unknown,
    Xbox,
    PlayStation,
    Nintendo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionKind {
    Acceleration,
    Gyroscope,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatteryState {
    Unknown,
    NotPresent,
    Discharging,
    Charging,
    NotCharging,
    Full,
}

/// Capability bits of `GamepadArrival::capabilities`.
pub mod capabilities {
    pub const ANALOG_TRIGGERS: u16 = 0x01;
    pub const RUMBLE: u16 = 0x02;
    pub const TRIGGER_RUMBLE: u16 = 0x04;
    pub const TOUCHPAD: u16 = 0x08;
    pub const ACCELEROMETER: u16 = 0x10;
    pub const GYRO: u16 = 0x20;
    pub const BATTERY: u16 = 0x40;
    pub const RGB_LED: u16 = 0x80;
}

#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// `vk` is the Windows virtual-key code; `modifiers` are [`modifiers`] bits.
    Key {
        down: bool,
        vk: u8,
        modifiers: u8,
    },
    /// A position inside a `width` by `height` area (the client's view).
    MouseMoveAbsolute {
        x: i16,
        y: i16,
        width: i16,
        height: i16,
    },
    MouseMoveRelative {
        dx: i16,
        dy: i16,
    },
    MouseButton {
        button: MouseButton,
        down: bool,
    },
    /// 120 units is one wheel notch; positive is away from the user.
    ScrollVertical {
        amount: i16,
    },
    ScrollHorizontal {
        amount: i16,
    },
    Touch(Touch),
    Pen(Pen),
    GamepadArrival {
        pad: u8,
        kind: GamepadKind,
        capabilities: u16,
        supported_buttons: u32,
    },
    /// The state of pad `pad`; `active_mask` has a bit for every pad the
    /// client has connected (a cleared bit means that pad went away).
    GamepadState {
        pad: u16,
        active_mask: u16,
        /// Moonlight's button flags, both halves joined (bits 16 and up are
        /// the extra buttons: paddles, touchpad click, misc).
        buttons: u32,
        left_trigger: u8,
        right_trigger: u8,
        left_stick: (i16, i16),
        right_stick: (i16, i16),
    },
    GamepadTouch {
        pad: u8,
        kind: PointerKind,
        touchpad: u8,
        pointer_id: u32,
        x: f32,
        y: f32,
        pressure: f32,
    },
    GamepadMotion {
        pad: u8,
        kind: MotionKind,
        x: f32,
        y: f32,
        z: f32,
    },
    GamepadBattery {
        pad: u8,
        state: BatteryState,
        percent: u8,
    },
    /// Text the client typed or pasted, UTF-8.
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InputError {
    #[error("input packet of {0} bytes has no event type")]
    NoType(usize),
    #[error("event {event:#x} needs {need} bytes, got {got}")]
    Short { event: u32, need: usize, got: usize },
    #[error("unknown input event type {0:#x}")]
    UnknownType(u32),
    #[error("invalid {0}")]
    Invalid(&'static str),
}

pub(crate) const KEY_DOWN: u32 = 0x03;
pub(crate) const KEY_UP: u32 = 0x04;
pub(crate) const MOUSE_ABS: u32 = 0x05;
pub(crate) const MOUSE_REL: u32 = 0x07;
pub(crate) const MOUSE_BUTTON_DOWN: u32 = 0x08;
pub(crate) const MOUSE_BUTTON_UP: u32 = 0x09;
pub(crate) const SCROLL_V: u32 = 0x0A;
pub(crate) const GAMEPAD_STATE: u32 = 0x0C;
pub(crate) const GAMEPAD_HAPTICS: u32 = 0x0D;
pub(crate) const TEXT: u32 = 0x17;
pub(crate) const SCROLL_H: u32 = 0x5500_0001;
pub(crate) const TOUCH: u32 = 0x5500_0002;
pub(crate) const PEN: u32 = 0x5500_0003;
pub(crate) const GAMEPAD_ARRIVAL: u32 = 0x5500_0004;
pub(crate) const GAMEPAD_TOUCH: u32 = 0x5500_0005;
pub(crate) const GAMEPAD_MOTION: u32 = 0x5500_0006;
pub(crate) const GAMEPAD_BATTERY: u32 = 0x5500_0007;

fn need(event: u32, body: &[u8], n: usize) -> Result<(), InputError> {
    if body.len() < n {
        Err(InputError::Short {
            event,
            need: n,
            got: body.len(),
        })
    } else {
        Ok(())
    }
}

fn be16(b: &[u8], at: usize) -> i16 {
    i16::from_be_bytes([b[at], b[at + 1]])
}
fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn f32le(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Parses one input packet. `Ok(None)` is a packet that means nothing (a null
/// key some clients emit while handling touch, haptics enable); an error is a
/// packet that is malformed.
pub fn parse(packet: &[u8]) -> Result<Option<InputEvent>, InputError> {
    if packet.len() < 4 {
        return Err(InputError::NoType(packet.len()));
    }
    let event = le32(packet, 0);
    let b = &packet[4..];
    let parsed = match event {
        KEY_DOWN | KEY_UP => {
            // action(1) key(2) modifiers(1) pad(2)
            need(event, b, 6)?;
            let vk = (le16(b, 1) & 0x00FF) as u8;
            // Null or unmapped keys come with touch input; they aren't keys.
            if vk == 0x00 || vk == 0xFF {
                return Ok(None);
            }
            InputEvent::Key {
                down: event == KEY_DOWN,
                vk,
                modifiers: b[3],
            }
        }
        MOUSE_ABS => {
            need(event, b, 10)?;
            InputEvent::MouseMoveAbsolute {
                x: be16(b, 0),
                y: be16(b, 2),
                width: be16(b, 6),
                height: be16(b, 8),
            }
        }
        MOUSE_REL => {
            need(event, b, 4)?;
            InputEvent::MouseMoveRelative {
                dx: be16(b, 0),
                dy: be16(b, 2),
            }
        }
        MOUSE_BUTTON_DOWN | MOUSE_BUTTON_UP => {
            need(event, b, 1)?;
            let button = match b[0] {
                1 => MouseButton::Left,
                2 => MouseButton::Middle,
                3 => MouseButton::Right,
                4 => MouseButton::Side,
                5 => MouseButton::Extra,
                _ => return Err(InputError::Invalid("mouse button")),
            };
            InputEvent::MouseButton {
                button,
                down: event == MOUSE_BUTTON_DOWN,
            }
        }
        SCROLL_V => {
            need(event, b, 2)?;
            InputEvent::ScrollVertical { amount: be16(b, 0) }
        }
        SCROLL_H => {
            need(event, b, 2)?;
            InputEvent::ScrollHorizontal { amount: be16(b, 0) }
        }
        TOUCH => {
            need(event, b, 28)?;
            let kind =
                PointerKind::from_wire(b[0]).ok_or(InputError::Invalid("touch event kind"))?;
            let (x, y) = (f32le(b, 8), f32le(b, 12));
            if kind.has_position() && !(x.is_finite() && y.is_finite()) {
                return Err(InputError::Invalid("touch position"));
            }
            InputEvent::Touch(Touch {
                kind,
                pointer_id: le32(b, 4),
                x,
                y,
                pressure: f32le(b, 16),
                rotation: le16(b, 2),
                contact_major: f32le(b, 20),
                contact_minor: f32le(b, 24),
            })
        }
        PEN => {
            need(event, b, 28)?;
            let kind = PointerKind::from_wire(b[0]).ok_or(InputError::Invalid("pen event kind"))?;
            let tool = match b[1] {
                0 => PenTool::Unknown,
                1 => PenTool::Pen,
                2 => PenTool::Eraser,
                _ => return Err(InputError::Invalid("pen tool")),
            };
            let (x, y, p) = (f32le(b, 4), f32le(b, 8), f32le(b, 12));
            if kind.has_position() && !(x.is_finite() && y.is_finite() && p.is_finite()) {
                return Err(InputError::Invalid("pen position"));
            }
            InputEvent::Pen(Pen {
                kind,
                tool,
                buttons: b[2],
                x,
                y,
                pressure_or_distance: p,
                rotation: le16(b, 16),
                tilt: b[18],
                contact_major: f32le(b, 20),
                contact_minor: f32le(b, 24),
            })
        }
        GAMEPAD_ARRIVAL => {
            need(event, b, 8)?;
            let kind = match b[1] {
                0 => GamepadKind::Unknown,
                1 => GamepadKind::Xbox,
                2 => GamepadKind::PlayStation,
                3 => GamepadKind::Nintendo,
                _ => return Err(InputError::Invalid("gamepad kind")),
            };
            InputEvent::GamepadArrival {
                pad: b[0],
                kind,
                capabilities: le16(b, 2),
                supported_buttons: le32(b, 4),
            }
        }
        GAMEPAD_STATE => {
            // header(2) pad(2) active mask(2) mid(2) buttons(2) lt rt sticks(8) tail(2) buttons2(2) tail(2)
            need(event, b, 26)?;
            InputEvent::GamepadState {
                pad: le16(b, 2),
                active_mask: le16(b, 4),
                buttons: u32::from(le16(b, 8)) | (u32::from(le16(b, 22)) << 16),
                left_trigger: b[10],
                right_trigger: b[11],
                left_stick: (le16(b, 12) as i16, le16(b, 14) as i16),
                right_stick: (le16(b, 16) as i16, le16(b, 18) as i16),
            }
        }
        GAMEPAD_TOUCH => {
            need(event, b, 20)?;
            let kind =
                PointerKind::from_wire(b[1]).ok_or(InputError::Invalid("gamepad touch kind"))?;
            InputEvent::GamepadTouch {
                pad: b[0],
                kind,
                touchpad: b[3],
                pointer_id: le32(b, 4),
                x: f32le(b, 8).clamp(0.0, 1.0),
                y: f32le(b, 12).clamp(0.0, 1.0),
                pressure: f32le(b, 16).clamp(0.0, 1.0),
            }
        }
        GAMEPAD_MOTION => {
            need(event, b, 16)?;
            let kind = match b[1] {
                1 => MotionKind::Acceleration,
                2 => MotionKind::Gyroscope,
                _ => return Err(InputError::Invalid("motion kind")),
            };
            InputEvent::GamepadMotion {
                pad: b[0],
                kind,
                x: f32le(b, 4),
                y: f32le(b, 8),
                z: f32le(b, 12),
            }
        }
        GAMEPAD_BATTERY => {
            need(event, b, 4)?;
            let state = match b[1] {
                0x00 | 0xFF => BatteryState::Unknown,
                0x01 => BatteryState::NotPresent,
                0x02 => BatteryState::Discharging,
                0x03 => BatteryState::Charging,
                0x04 => BatteryState::NotCharging,
                0x05 => BatteryState::Full,
                _ => return Err(InputError::Invalid("battery state")),
            };
            InputEvent::GamepadBattery {
                pad: b[0],
                state,
                percent: b[2],
            }
        }
        GAMEPAD_HAPTICS => return Ok(None),
        TEXT => {
            let text = std::str::from_utf8(b).map_err(|_| InputError::Invalid("UTF-8 text"))?;
            if text.is_empty() {
                return Err(InputError::Invalid("empty text"));
            }
            InputEvent::Text(text.to_owned())
        }
        other => return Err(InputError::UnknownType(other)),
    };
    Ok(Some(parsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(event: u32, body: &[u8]) -> Vec<u8> {
        let mut p = event.to_le_bytes().to_vec();
        p.extend_from_slice(body);
        p
    }

    #[test]
    fn keys() {
        // down, VK 'A' with the "normalized" 0x8000 bit, shift held, padding.
        let p = packet(KEY_DOWN, &[0x00, 0x41, 0x80, modifiers::SHIFT, 0, 0]);
        assert_eq!(
            parse(&p).unwrap(),
            Some(InputEvent::Key {
                down: true,
                vk: 0x41,
                modifiers: 1
            })
        );
        let p = packet(KEY_UP, &[0x00, 0x41, 0x80, 0, 0, 0]);
        assert_eq!(
            parse(&p).unwrap(),
            Some(InputEvent::Key {
                down: false,
                vk: 0x41,
                modifiers: 0
            })
        );
        // Null and 0xFF keys are not keys.
        assert_eq!(
            parse(&packet(KEY_DOWN, &[0, 0, 0x80, 0, 0, 0])).unwrap(),
            None
        );
        assert_eq!(
            parse(&packet(KEY_DOWN, &[0, 0xFF, 0, 0, 0, 0])).unwrap(),
            None
        );
    }

    #[test]
    fn mouse() {
        let mut body = Vec::new();
        for v in [-5i16, 7, 0, 1920, 1080] {
            body.extend(v.to_be_bytes());
        }
        assert_eq!(
            parse(&packet(MOUSE_ABS, &body)).unwrap(),
            Some(InputEvent::MouseMoveAbsolute {
                x: -5,
                y: 7,
                width: 1920,
                height: 1080
            })
        );
        let mut body = (-3i16).to_be_bytes().to_vec();
        body.extend(9i16.to_be_bytes());
        assert_eq!(
            parse(&packet(MOUSE_REL, &body)).unwrap(),
            Some(InputEvent::MouseMoveRelative { dx: -3, dy: 9 })
        );
        assert_eq!(
            parse(&packet(MOUSE_BUTTON_DOWN, &[3])).unwrap(),
            Some(InputEvent::MouseButton {
                button: MouseButton::Right,
                down: true
            })
        );
        assert_eq!(
            parse(&packet(MOUSE_BUTTON_UP, &[1])).unwrap(),
            Some(InputEvent::MouseButton {
                button: MouseButton::Left,
                down: false
            })
        );
        assert!(parse(&packet(MOUSE_BUTTON_UP, &[9])).is_err());
        // Vertical scroll carries the amount twice, then zero.
        let mut body = 120i16.to_be_bytes().to_vec();
        body.extend(120i16.to_be_bytes());
        body.extend([0, 0]);
        assert_eq!(
            parse(&packet(SCROLL_V, &body)).unwrap(),
            Some(InputEvent::ScrollVertical { amount: 120 })
        );
        assert_eq!(
            parse(&packet(SCROLL_H, &(-240i16).to_be_bytes())).unwrap(),
            Some(InputEvent::ScrollHorizontal { amount: -240 })
        );
    }

    #[test]
    fn touch_and_pen() {
        let mut b = [0u8; 28];
        b[0] = 1;
        b[2..4].copy_from_slice(&270u16.to_le_bytes());
        b[4..8].copy_from_slice(&42u32.to_le_bytes());
        b[8..12].copy_from_slice(&0.25f32.to_le_bytes());
        b[12..16].copy_from_slice(&0.75f32.to_le_bytes());
        b[16..20].copy_from_slice(&0.5f32.to_le_bytes());
        let Some(InputEvent::Touch(t)) = parse(&packet(TOUCH, &b)).unwrap() else {
            panic!()
        };
        assert_eq!(
            (t.kind, t.pointer_id, t.x, t.y, t.pressure, t.rotation),
            (PointerKind::Down, 42, 0.25, 0.75, 0.5, 270)
        );
        b[8..12].copy_from_slice(&f32::NAN.to_le_bytes());
        assert!(parse(&packet(TOUCH, &b)).is_err());
        assert!(parse(&packet(TOUCH, &b[..27])).is_err());

        let mut b = [0u8; 28];
        b[0] = 3;
        b[1] = 1;
        b[2] = 2;
        b[4..8].copy_from_slice(&0.25f32.to_le_bytes());
        b[8..12].copy_from_slice(&0.75f32.to_le_bytes());
        b[12..16].copy_from_slice(&0.6f32.to_le_bytes());
        b[16..18].copy_from_slice(&270u16.to_le_bytes());
        b[18] = 35;
        let Some(InputEvent::Pen(p)) = parse(&packet(PEN, &b)).unwrap() else {
            panic!()
        };
        assert_eq!(
            (p.kind, p.tool, p.buttons, p.rotation, p.tilt),
            (PointerKind::Move, PenTool::Pen, 2, 270, 35)
        );
        assert_eq!((p.x, p.y, p.pressure_or_distance), (0.25, 0.75, 0.6));
        b[12..16].copy_from_slice(&f32::INFINITY.to_le_bytes());
        assert!(parse(&packet(PEN, &b)).is_err());
    }

    #[test]
    fn gamepads() {
        let mut b = vec![0u8, 2, 0xC3, 0x00];
        b.extend(0x0000_FFFFu32.to_le_bytes());
        assert_eq!(
            parse(&packet(GAMEPAD_ARRIVAL, &b)).unwrap(),
            Some(InputEvent::GamepadArrival {
                pad: 0,
                kind: GamepadKind::PlayStation,
                capabilities: 0xC3,
                supported_buttons: 0xFFFF
            })
        );

        let mut b = Vec::new();
        b.extend(0x1Au16.to_le_bytes()); // header
        b.extend(1u16.to_le_bytes()); // pad
        b.extend(0b11u16.to_le_bytes()); // active
        b.extend(0x14u16.to_le_bytes()); // mid
        b.extend(0x1001u16.to_le_bytes()); // buttons
        b.extend([200, 10]); // triggers
        for v in [-100i16, 200, 300, -400] {
            b.extend(v.to_le_bytes());
        }
        b.extend(0x9Cu16.to_le_bytes()); // tail a
        b.extend(0x0002u16.to_le_bytes()); // buttons 2
        b.extend(0x55u16.to_le_bytes()); // tail b
        assert_eq!(
            parse(&packet(GAMEPAD_STATE, &b)).unwrap(),
            Some(InputEvent::GamepadState {
                pad: 1,
                active_mask: 3,
                buttons: 0x0002_1001,
                left_trigger: 200,
                right_trigger: 10,
                left_stick: (-100, 200),
                right_stick: (300, -400)
            })
        );
        assert!(parse(&packet(GAMEPAD_STATE, &b[..25])).is_err());

        let mut b = vec![1u8, 2, 0, 0];
        b.extend(1.0f32.to_le_bytes());
        b.extend(2.0f32.to_le_bytes());
        b.extend(3.0f32.to_le_bytes());
        assert_eq!(
            parse(&packet(GAMEPAD_MOTION, &b)).unwrap(),
            Some(InputEvent::GamepadMotion {
                pad: 1,
                kind: MotionKind::Gyroscope,
                x: 1.0,
                y: 2.0,
                z: 3.0
            })
        );
        b[1] = 9;
        assert!(parse(&packet(GAMEPAD_MOTION, &b)).is_err());

        assert_eq!(
            parse(&packet(GAMEPAD_BATTERY, &[2, 3, 80, 0])).unwrap(),
            Some(InputEvent::GamepadBattery {
                pad: 2,
                state: BatteryState::Charging,
                percent: 80
            })
        );

        let mut b = vec![0u8, 1, 0, 1];
        b.extend(7u32.to_le_bytes());
        b.extend(2.0f32.to_le_bytes()); // clamped
        b.extend(0.5f32.to_le_bytes());
        b.extend(0.1f32.to_le_bytes());
        assert_eq!(
            parse(&packet(GAMEPAD_TOUCH, &b)).unwrap(),
            Some(InputEvent::GamepadTouch {
                pad: 0,
                kind: PointerKind::Down,
                touchpad: 1,
                pointer_id: 7,
                x: 1.0,
                y: 0.5,
                pressure: 0.1
            })
        );
        assert_eq!(parse(&packet(GAMEPAD_HAPTICS, &[])).unwrap(), None);
    }

    #[test]
    fn text() {
        assert_eq!(
            parse(&packet(TEXT, "héy".as_bytes())).unwrap(),
            Some(InputEvent::Text("héy".into()))
        );
        assert!(parse(&packet(TEXT, &[])).is_err());
        assert!(parse(&packet(TEXT, &[0xFF, 0xFE])).is_err());
    }

    #[test]
    fn unknown_and_short() {
        assert_eq!(parse(&[1, 2]), Err(InputError::NoType(2)));
        assert_eq!(
            parse(&packet(0xDEAD, &[])),
            Err(InputError::UnknownType(0xDEAD))
        );
        assert!(matches!(
            parse(&packet(MOUSE_ABS, &[0; 4])),
            Err(InputError::Short { .. })
        ));
    }

    /// Whatever bytes arrive, parsing returns; it never panics.
    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0x1234_5678_9ABC_DEF0u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let events = [
            KEY_DOWN,
            KEY_UP,
            MOUSE_ABS,
            MOUSE_REL,
            MOUSE_BUTTON_DOWN,
            SCROLL_V,
            GAMEPAD_STATE,
            TEXT,
            SCROLL_H,
            TOUCH,
            PEN,
            GAMEPAD_ARRIVAL,
            GAMEPAD_TOUCH,
            GAMEPAD_MOTION,
            GAMEPAD_BATTERY,
        ];
        for i in 0..20_000 {
            let len = (next() % 48) as usize;
            let mut p: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if i % 2 == 0 && p.len() >= 4 {
                p[..4].copy_from_slice(
                    &events[(next() % events.len() as u64) as usize].to_le_bytes(),
                );
            }
            let _ = parse(&p);
        }
    }
}

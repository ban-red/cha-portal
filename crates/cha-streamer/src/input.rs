//! Browser input → compositor input. The page sends JSON on the `control`
//! channel: positions normalized to the video (0..1), relative motion in
//! pixels (while the pointer is locked), keys as `KeyboardEvent.code`, buttons
//! as `PointerEvent.button`.

use serde::Deserialize;

/// Input for the compositor, in its own units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// Pointer position in output pixels.
    Move { x: f64, y: f64 },
    /// Relative pointer motion in pixels (pointer lock).
    Relative { dx: f64, dy: f64 },
    /// Linux `BTN_*` code.
    Button { code: u32, pressed: bool },
    /// Scroll in pixels (wheel `deltaMode` 0).
    Axis { x: f64, y: f64 },
    /// Linux evdev `KEY_*` code.
    Key { code: u32, pressed: bool },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum BrowserInput {
    Move { x: f64, y: f64 },
    Rel { dx: f64, dy: f64 },
    Button { b: u8, down: bool },
    Wheel { dx: f64, dy: f64 },
    Key { code: String, down: bool },
}

impl BrowserInput {
    /// The compositor event, or `None` for a key or button we don't map.
    pub fn to_compositor(&self, width: u32, height: u32) -> Option<Input> {
        Some(match self {
            BrowserInput::Move { x, y } => Input::Move {
                x: x.clamp(0.0, 1.0) * f64::from(width),
                y: y.clamp(0.0, 1.0) * f64::from(height),
            },
            BrowserInput::Rel { dx, dy } => Input::Relative { dx: *dx, dy: *dy },
            BrowserInput::Button { b, down } => Input::Button {
                code: button_code(*b)?,
                pressed: *down,
            },
            BrowserInput::Wheel { dx, dy } => Input::Axis { x: *dx, y: *dy },
            BrowserInput::Key { code, down } => Input::Key {
                code: key_code(code)?,
                pressed: *down,
            },
        })
    }
}

/// `PointerEvent.button` → Linux `BTN_*`.
fn button_code(button: u8) -> Option<u32> {
    Some(match button {
        0 => 0x110, // BTN_LEFT
        1 => 0x112, // BTN_MIDDLE
        2 => 0x111, // BTN_RIGHT
        3 => 0x113, // BTN_SIDE (back)
        4 => 0x114, // BTN_EXTRA (forward)
        _ => return None,
    })
}

/// `KeyboardEvent.code` (physical key) → Linux evdev `KEY_*`.
fn key_code(code: &str) -> Option<u32> {
    if let Some(letter) = code.strip_prefix("Key")
        && letter.len() == 1
    {
        const LETTERS: [u32; 26] = [
            30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47,
            17, 45, 21, 44,
        ];
        let i = letter.as_bytes()[0].checked_sub(b'A')? as usize;
        return LETTERS.get(i).copied();
    }
    if let Some(digit) = code.strip_prefix("Digit")
        && let Ok(d) = digit.parse::<u32>()
    {
        return Some(if d == 0 { 11 } else { d + 1 });
    }
    if let Some(n) = code.strip_prefix('F')
        && let Ok(n) = n.parse::<u32>()
    {
        return match n {
            1..=10 => Some(58 + n),
            11 => Some(87),
            12 => Some(88),
            _ => None,
        };
    }
    Some(match code {
        "Escape" => 1,
        "Minus" => 12,
        "Equal" => 13,
        "Backspace" => 14,
        "Tab" => 15,
        "BracketLeft" => 26,
        "BracketRight" => 27,
        "Enter" => 28,
        "ControlLeft" => 29,
        "Semicolon" => 39,
        "Quote" => 40,
        "Backquote" => 41,
        "ShiftLeft" => 42,
        "Backslash" => 43,
        "Comma" => 51,
        "Period" => 52,
        "Slash" => 53,
        "ShiftRight" => 54,
        "NumpadMultiply" => 55,
        "AltLeft" => 56,
        "Space" => 57,
        "CapsLock" => 58,
        "NumLock" => 69,
        "ScrollLock" => 70,
        "Numpad7" => 71,
        "Numpad8" => 72,
        "Numpad9" => 73,
        "NumpadSubtract" => 74,
        "Numpad4" => 75,
        "Numpad5" => 76,
        "Numpad6" => 77,
        "NumpadAdd" => 78,
        "Numpad1" => 79,
        "Numpad2" => 80,
        "Numpad3" => 81,
        "Numpad0" => 82,
        "NumpadDecimal" => 83,
        "IntlBackslash" => 86,
        "NumpadEnter" => 96,
        "ControlRight" => 97,
        "NumpadDivide" => 98,
        "PrintScreen" => 99,
        "AltRight" => 100,
        "Home" => 102,
        "ArrowUp" => 103,
        "PageUp" => 104,
        "ArrowLeft" => 105,
        "ArrowRight" => 106,
        "End" => 107,
        "ArrowDown" => 108,
        "PageDown" => 109,
        "Insert" => 110,
        "Delete" => 111,
        "Pause" => 119,
        "MetaLeft" => 125,
        "MetaRight" => 126,
        "ContextMenu" => 127,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys_to_evdev() {
        assert_eq!(key_code("KeyA"), Some(30));
        assert_eq!(key_code("KeyZ"), Some(44));
        assert_eq!(key_code("Digit1"), Some(2));
        assert_eq!(key_code("Digit0"), Some(11));
        assert_eq!(key_code("F1"), Some(59));
        assert_eq!(key_code("F10"), Some(68));
        assert_eq!(key_code("F12"), Some(88));
        assert_eq!(key_code("Enter"), Some(28));
        assert_eq!(key_code("Keyboard"), None);
        assert_eq!(key_code("Unidentified"), None);
    }

    #[test]
    fn scales_pointer_to_output_pixels() {
        let input = BrowserInput::Move { x: 0.5, y: 2.0 }.to_compositor(2560, 1440);
        assert_eq!(
            input,
            Some(Input::Move {
                x: 1280.0,
                y: 1440.0
            })
        );
    }

    #[test]
    fn parses_page_messages() {
        let msg: BrowserInput =
            serde_json::from_str(r#"{"t":"input","k":"rel","dx":3.5,"dy":-2}"#).unwrap();
        assert_eq!(
            msg.to_compositor(1, 1),
            Some(Input::Relative { dx: 3.5, dy: -2.0 })
        );
    }
}

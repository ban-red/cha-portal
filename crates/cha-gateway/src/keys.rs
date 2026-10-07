//! `KeyboardEvent.code` (the physical key) → Windows virtual-key code, which
//! is what Moonlight sends to the host. The layout is US: `Slash` is the key
//! right of the period, whatever it's labelled.

/// A key the host understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    /// The Windows virtual-key code (`VK_*`).
    pub vk: u16,
    /// Moonlight's "extended key" flag, set only where the VK alone is
    /// ambiguous: the numpad's Enter shares `VK_RETURN` with the main one.
    pub extended: bool,
}

const fn plain(vk: u16) -> Option<Key> {
    Some(Key {
        vk,
        extended: false,
    })
}

/// The virtual-key code for `code`, or `None` for a key Windows has no code for.
pub fn lookup(code: &str) -> Option<Key> {
    if let Some(letter) = code.strip_prefix("Key")
        && let [c @ b'A'..=b'Z'] = letter.as_bytes()
    {
        return plain(u16::from(*c)); // VK_A..VK_Z are the ASCII capitals
    }
    if let Some(digit) = code.strip_prefix("Digit")
        && let [c @ b'0'..=b'9'] = digit.as_bytes()
    {
        return plain(u16::from(*c)); // VK_0..VK_9 are ASCII too
    }
    if let Some(n) = code.strip_prefix("Numpad")
        && let [c @ b'0'..=b'9'] = n.as_bytes()
    {
        return plain(0x60 + u16::from(c - b'0')); // VK_NUMPAD0..9
    }
    if let Some(n) = code.strip_prefix('F')
        && let Ok(n @ 1..=24) = n.parse::<u16>()
        && !n.to_string().starts_with('0')
        && code.len() == 1 + n.to_string().len()
    {
        return plain(0x70 + n - 1); // VK_F1..VK_F24
    }
    let vk = match code {
        "Backspace" => 0x08,
        "Tab" => 0x09,
        "Enter" => 0x0D,
        "Pause" => 0x13,
        "CapsLock" => 0x14,
        "KanaMode" => 0x15,
        "Escape" => 0x1B,
        "Convert" => 0x1C,
        "NonConvert" => 0x1D,
        "Space" => 0x20,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "End" => 0x23,
        "Home" => 0x24,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "PrintScreen" => 0x2C,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Help" => 0x2F,
        "MetaLeft" | "OSLeft" => 0x5B,
        "MetaRight" | "OSRight" => 0x5C,
        "ContextMenu" => 0x5D, // VK_APPS
        "NumpadMultiply" => 0x6A,
        "NumpadAdd" => 0x6B,
        "NumpadComma" => 0x6C, // VK_SEPARATOR
        "NumpadSubtract" => 0x6D,
        "NumpadDecimal" => 0x6E,
        "NumpadDivide" => 0x6F,
        "NumLock" => 0x90,
        "ScrollLock" => 0x91,
        "ShiftLeft" => 0xA0,
        "ShiftRight" => 0xA1,
        "ControlLeft" => 0xA2,
        "ControlRight" => 0xA3,
        "AltLeft" => 0xA4, // VK_LMENU
        "AltRight" => 0xA5,
        "BrowserBack" => 0xA6,
        "BrowserForward" => 0xA7,
        "BrowserRefresh" => 0xA8,
        "BrowserStop" => 0xA9,
        "BrowserSearch" => 0xAA,
        "BrowserFavorites" => 0xAB,
        "BrowserHome" => 0xAC,
        // Firefox names these `Volume*`; Chrome `AudioVolume*`.
        "AudioVolumeMute" | "VolumeMute" => 0xAD,
        "AudioVolumeDown" | "VolumeDown" => 0xAE,
        "AudioVolumeUp" | "VolumeUp" => 0xAF,
        "MediaTrackNext" => 0xB0,
        "MediaTrackPrevious" => 0xB1,
        "MediaStop" => 0xB2,
        "MediaPlayPause" => 0xB3,
        "LaunchMail" => 0xB4,
        "MediaSelect" => 0xB5,
        "LaunchApp1" => 0xB6,
        "LaunchApp2" => 0xB7,
        "Semicolon" => 0xBA, // VK_OEM_1
        "Equal" => 0xBB,     // VK_OEM_PLUS
        "Comma" => 0xBC,
        "Minus" => 0xBD,
        "Period" => 0xBE,
        "Slash" => 0xBF,         // VK_OEM_2
        "Backquote" => 0xC0,     // VK_OEM_3
        "BracketLeft" => 0xDB,   // VK_OEM_4
        "Backslash" => 0xDC,     // VK_OEM_5
        "BracketRight" => 0xDD,  // VK_OEM_6
        "Quote" => 0xDE,         // VK_OEM_7
        "IntlBackslash" => 0xE2, // VK_OEM_102
        "NumpadEnter" => {
            return Some(Key {
                vk: 0x0D,
                extended: true,
            });
        }
        _ => return None,
    };
    plain(vk)
}

/// The modifier a virtual-key code is one of, as Moonlight's flag.
pub fn modifier(vk: u16) -> Option<Modifier> {
    Some(match vk {
        0xA0 | 0xA1 => Modifier::Shift,
        0xA2 | 0xA3 => Modifier::Ctrl,
        0xA4 | 0xA5 => Modifier::Alt,
        0x5B | 0x5C => Modifier::Meta,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modifier {
    Shift,
    Ctrl,
    Alt,
    Meta,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vk(code: &str) -> Option<u16> {
        lookup(code).map(|k| k.vk)
    }

    #[test]
    fn letters_and_digits_are_ascii() {
        assert_eq!(vk("KeyA"), Some(0x41));
        assert_eq!(vk("KeyW"), Some(0x57));
        assert_eq!(vk("KeyZ"), Some(0x5A));
        assert_eq!(vk("Digit0"), Some(0x30));
        assert_eq!(vk("Digit9"), Some(0x39));
        assert_eq!(vk("Key"), None);
        assert_eq!(vk("Keya"), None);
        assert_eq!(vk("KeyAB"), None);
    }

    #[test]
    fn function_keys_f1_to_f24() {
        assert_eq!(vk("F1"), Some(0x70));
        assert_eq!(vk("F5"), Some(0x74));
        assert_eq!(vk("F10"), Some(0x79));
        assert_eq!(vk("F12"), Some(0x7B));
        assert_eq!(vk("F13"), Some(0x7C));
        assert_eq!(vk("F24"), Some(0x87));
        assert_eq!(vk("F0"), None);
        assert_eq!(vk("F25"), None);
        assert_eq!(vk("F01"), None);
        assert_eq!(vk("F"), None);
        assert_eq!(vk("Fn"), None);
    }

    #[test]
    fn editing_and_navigation() {
        assert_eq!(vk("Escape"), Some(0x1B));
        assert_eq!(vk("Tab"), Some(0x09));
        assert_eq!(vk("Backspace"), Some(0x08));
        assert_eq!(vk("Enter"), Some(0x0D));
        assert_eq!(vk("Space"), Some(0x20));
        assert_eq!(vk("CapsLock"), Some(0x14));
        assert_eq!(vk("ArrowLeft"), Some(0x25));
        assert_eq!(vk("ArrowUp"), Some(0x26));
        assert_eq!(vk("ArrowRight"), Some(0x27));
        assert_eq!(vk("ArrowDown"), Some(0x28));
        assert_eq!(vk("PageUp"), Some(0x21));
        assert_eq!(vk("PageDown"), Some(0x22));
        assert_eq!(vk("End"), Some(0x23));
        assert_eq!(vk("Home"), Some(0x24));
        assert_eq!(vk("Insert"), Some(0x2D));
        assert_eq!(vk("Delete"), Some(0x2E));
        assert_eq!(vk("PrintScreen"), Some(0x2C));
        assert_eq!(vk("ScrollLock"), Some(0x91));
        assert_eq!(vk("Pause"), Some(0x13));
    }

    #[test]
    fn modifiers_left_and_right() {
        assert_eq!(vk("ShiftLeft"), Some(0xA0));
        assert_eq!(vk("ShiftRight"), Some(0xA1));
        assert_eq!(vk("ControlLeft"), Some(0xA2));
        assert_eq!(vk("ControlRight"), Some(0xA3));
        assert_eq!(vk("AltLeft"), Some(0xA4));
        assert_eq!(vk("AltRight"), Some(0xA5));
        assert_eq!(vk("MetaLeft"), Some(0x5B));
        assert_eq!(vk("MetaRight"), Some(0x5C));
        assert_eq!(vk("ContextMenu"), Some(0x5D));
        assert_eq!(modifier(0xA1), Some(Modifier::Shift));
        assert_eq!(modifier(0xA3), Some(Modifier::Ctrl));
        assert_eq!(modifier(0xA4), Some(Modifier::Alt));
        assert_eq!(modifier(0x5C), Some(Modifier::Meta));
        assert_eq!(modifier(0x41), None);
    }

    #[test]
    fn numpad() {
        assert_eq!(vk("Numpad0"), Some(0x60));
        assert_eq!(vk("Numpad9"), Some(0x69));
        assert_eq!(vk("NumpadMultiply"), Some(0x6A));
        assert_eq!(vk("NumpadAdd"), Some(0x6B));
        assert_eq!(vk("NumpadSubtract"), Some(0x6D));
        assert_eq!(vk("NumpadDecimal"), Some(0x6E));
        assert_eq!(vk("NumpadDivide"), Some(0x6F));
        assert_eq!(vk("NumLock"), Some(0x90));
        // Enter and Numpad Enter share a VK; only the numpad's is "extended".
        assert!(!lookup("Enter").unwrap().extended);
        assert_eq!(
            lookup("NumpadEnter"),
            Some(Key {
                vk: 0x0D,
                extended: true
            })
        );
    }

    #[test]
    fn us_punctuation() {
        assert_eq!(vk("Semicolon"), Some(0xBA));
        assert_eq!(vk("Equal"), Some(0xBB));
        assert_eq!(vk("Comma"), Some(0xBC));
        assert_eq!(vk("Minus"), Some(0xBD));
        assert_eq!(vk("Period"), Some(0xBE));
        assert_eq!(vk("Slash"), Some(0xBF));
        assert_eq!(vk("Backquote"), Some(0xC0));
        assert_eq!(vk("BracketLeft"), Some(0xDB));
        assert_eq!(vk("Backslash"), Some(0xDC));
        assert_eq!(vk("BracketRight"), Some(0xDD));
        assert_eq!(vk("Quote"), Some(0xDE));
        assert_eq!(vk("IntlBackslash"), Some(0xE2));
    }

    #[test]
    fn media_and_browser_keys() {
        assert_eq!(vk("AudioVolumeMute"), Some(0xAD));
        assert_eq!(vk("VolumeMute"), Some(0xAD));
        assert_eq!(vk("AudioVolumeDown"), Some(0xAE));
        assert_eq!(vk("AudioVolumeUp"), Some(0xAF));
        assert_eq!(vk("MediaTrackNext"), Some(0xB0));
        assert_eq!(vk("MediaTrackPrevious"), Some(0xB1));
        assert_eq!(vk("MediaStop"), Some(0xB2));
        assert_eq!(vk("MediaPlayPause"), Some(0xB3));
        assert_eq!(vk("BrowserBack"), Some(0xA6));
        assert_eq!(vk("BrowserHome"), Some(0xAC));
        assert_eq!(vk("LaunchMail"), Some(0xB4));
    }

    #[test]
    fn unknown_codes_are_none() {
        assert_eq!(vk(""), None);
        assert_eq!(vk("Fn"), None);
        assert_eq!(vk("Unidentified"), None);
        assert_eq!(vk("KeyboardLayoutSelect"), None);
    }
}

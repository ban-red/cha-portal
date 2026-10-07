//! winit physical keys → W3C `KeyboardEvent.code` strings, the key model the
//! core and every transport share.

use winit::keyboard::KeyCode;

pub fn w3c_code(key: KeyCode) -> Option<&'static str> {
    use KeyCode::*;
    Some(match key {
        Backquote => "Backquote",
        Backslash => "Backslash",
        BracketLeft => "BracketLeft",
        BracketRight => "BracketRight",
        Comma => "Comma",
        Digit0 => "Digit0",
        Digit1 => "Digit1",
        Digit2 => "Digit2",
        Digit3 => "Digit3",
        Digit4 => "Digit4",
        Digit5 => "Digit5",
        Digit6 => "Digit6",
        Digit7 => "Digit7",
        Digit8 => "Digit8",
        Digit9 => "Digit9",
        Equal => "Equal",
        IntlBackslash => "IntlBackslash",
        IntlRo => "IntlRo",
        IntlYen => "IntlYen",
        KeyA => "KeyA",
        KeyB => "KeyB",
        KeyC => "KeyC",
        KeyD => "KeyD",
        KeyE => "KeyE",
        KeyF => "KeyF",
        KeyG => "KeyG",
        KeyH => "KeyH",
        KeyI => "KeyI",
        KeyJ => "KeyJ",
        KeyK => "KeyK",
        KeyL => "KeyL",
        KeyM => "KeyM",
        KeyN => "KeyN",
        KeyO => "KeyO",
        KeyP => "KeyP",
        KeyQ => "KeyQ",
        KeyR => "KeyR",
        KeyS => "KeyS",
        KeyT => "KeyT",
        KeyU => "KeyU",
        KeyV => "KeyV",
        KeyW => "KeyW",
        KeyX => "KeyX",
        KeyY => "KeyY",
        KeyZ => "KeyZ",
        Minus => "Minus",
        Period => "Period",
        Quote => "Quote",
        Semicolon => "Semicolon",
        Slash => "Slash",

        AltLeft => "AltLeft",
        AltRight => "AltRight",
        Backspace => "Backspace",
        CapsLock => "CapsLock",
        ContextMenu => "ContextMenu",
        ControlLeft => "ControlLeft",
        ControlRight => "ControlRight",
        Enter => "Enter",
        // The Command key is the browser's "Meta".
        SuperLeft => "MetaLeft",
        SuperRight => "MetaRight",
        ShiftLeft => "ShiftLeft",
        ShiftRight => "ShiftRight",
        Space => "Space",
        Tab => "Tab",

        Convert => "Convert",
        KanaMode => "KanaMode",
        Lang1 => "Lang1",
        Lang2 => "Lang2",
        Lang3 => "Lang3",
        Lang4 => "Lang4",
        Lang5 => "Lang5",
        NonConvert => "NonConvert",

        Delete => "Delete",
        End => "End",
        Help => "Help",
        Home => "Home",
        Insert => "Insert",
        PageDown => "PageDown",
        PageUp => "PageUp",
        ArrowDown => "ArrowDown",
        ArrowLeft => "ArrowLeft",
        ArrowRight => "ArrowRight",
        ArrowUp => "ArrowUp",

        NumLock => "NumLock",
        Numpad0 => "Numpad0",
        Numpad1 => "Numpad1",
        Numpad2 => "Numpad2",
        Numpad3 => "Numpad3",
        Numpad4 => "Numpad4",
        Numpad5 => "Numpad5",
        Numpad6 => "Numpad6",
        Numpad7 => "Numpad7",
        Numpad8 => "Numpad8",
        Numpad9 => "Numpad9",
        NumpadAdd => "NumpadAdd",
        NumpadComma => "NumpadComma",
        NumpadDecimal => "NumpadDecimal",
        NumpadDivide => "NumpadDivide",
        NumpadEnter => "NumpadEnter",
        NumpadEqual => "NumpadEqual",
        NumpadMultiply => "NumpadMultiply",
        NumpadSubtract => "NumpadSubtract",

        Escape => "Escape",
        PrintScreen => "PrintScreen",
        ScrollLock => "ScrollLock",
        Pause => "Pause",

        F1 => "F1",
        F2 => "F2",
        F3 => "F3",
        F4 => "F4",
        F5 => "F5",
        F6 => "F6",
        F7 => "F7",
        F8 => "F8",
        F9 => "F9",
        F10 => "F10",
        F11 => "F11",
        F12 => "F12",
        F13 => "F13",
        F14 => "F14",
        F15 => "F15",
        F16 => "F16",
        F17 => "F17",
        F18 => "F18",
        F19 => "F19",
        F20 => "F20",
        F21 => "F21",
        F22 => "F22",
        F23 => "F23",
        F24 => "F24",

        AudioVolumeDown => "AudioVolumeDown",
        AudioVolumeMute => "AudioVolumeMute",
        AudioVolumeUp => "AudioVolumeUp",
        MediaPlayPause => "MediaPlayPause",
        MediaStop => "MediaStop",
        MediaTrackNext => "MediaTrackNext",
        MediaTrackPrevious => "MediaTrackPrevious",
        _ => return None,
    })
}

/// What a key press becomes on the wire, as the browser player does it: with
/// `command_as_control`, Cmd is sent as Ctrl (so Cmd+C copies in a Linux app),
/// and keys pressed while Cmd is held are released when Cmd comes up, since
/// macOS often never reports their own release.
#[derive(Default)]
pub struct KeyTranslator {
    pub command_as_control: bool,
    command_down: bool,
    /// Keys pressed during a Cmd chord and not yet released.
    chorded: Vec<&'static str>,
}

impl KeyTranslator {
    pub fn new(command_as_control: bool) -> Self {
        Self {
            command_as_control,
            ..Self::default()
        }
    }

    /// The `(code, down)` events to send for one physical key change.
    pub fn translate(&mut self, key: KeyCode, down: bool) -> Vec<(&'static str, bool)> {
        let Some(code) = w3c_code(key) else {
            return Vec::new();
        };
        let command = matches!(key, KeyCode::SuperLeft | KeyCode::SuperRight);
        if command {
            self.command_down = down;
            let sent = match (self.command_as_control, key) {
                (true, KeyCode::SuperLeft) => "ControlLeft",
                (true, _) => "ControlRight",
                (false, _) => code,
            };
            let mut out = Vec::new();
            if !down {
                out.extend(self.chorded.drain(..).map(|c| (c, false)));
            }
            out.push((sent, down));
            return out;
        }
        if down && self.command_down && !self.chorded.contains(&code) {
            self.chorded.push(code);
        } else if !down {
            self.chorded.retain(|c| *c != code);
        }
        vec![(code, down)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_is_sent_as_control_and_releases_its_chord() {
        let mut t = KeyTranslator::new(true);
        assert_eq!(
            t.translate(KeyCode::SuperLeft, true),
            [("ControlLeft", true)]
        );
        assert_eq!(t.translate(KeyCode::KeyC, true), [("KeyC", true)]);
        // macOS never reports C's release: Cmd's release lets go of it.
        assert_eq!(
            t.translate(KeyCode::SuperLeft, false),
            [("KeyC", false), ("ControlLeft", false)]
        );
        // A key released in the chord isn't released twice.
        t.translate(KeyCode::SuperRight, true);
        t.translate(KeyCode::KeyV, true);
        assert_eq!(t.translate(KeyCode::KeyV, false), [("KeyV", false)]);
        assert_eq!(
            t.translate(KeyCode::SuperRight, false),
            [("ControlRight", false)]
        );
    }

    #[test]
    fn command_stays_meta_when_asked() {
        let mut t = KeyTranslator::new(false);
        assert_eq!(t.translate(KeyCode::SuperLeft, true), [("MetaLeft", true)]);
        assert_eq!(t.translate(KeyCode::KeyA, true), [("KeyA", true)]);
        assert!(t.translate(KeyCode::Fn, true).is_empty());
    }

    #[test]
    fn letters_digits_and_arrows_keep_their_w3c_names() {
        assert_eq!(w3c_code(KeyCode::KeyA), Some("KeyA"));
        assert_eq!(w3c_code(KeyCode::Digit7), Some("Digit7"));
        assert_eq!(w3c_code(KeyCode::ArrowLeft), Some("ArrowLeft"));
        assert_eq!(w3c_code(KeyCode::F12), Some("F12"));
        assert_eq!(w3c_code(KeyCode::NumpadEnter), Some("NumpadEnter"));
    }

    #[test]
    fn command_is_meta() {
        assert_eq!(w3c_code(KeyCode::SuperLeft), Some("MetaLeft"));
        assert_eq!(w3c_code(KeyCode::SuperRight), Some("MetaRight"));
    }

    #[test]
    fn keys_a_host_has_no_name_for_are_dropped() {
        assert_eq!(w3c_code(KeyCode::Fn), None);
    }
}

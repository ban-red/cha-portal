//! Input capture: what the window's keyboard, mouse and gamepads become in the
//! core's input model, and the player's own hotkeys.

pub mod keymap;
pub mod pads;
pub mod pointer;

use winit::keyboard::{KeyCode, ModifiersState};

/// Player commands typed on the keyboard while streaming. They use
/// Ctrl+Alt+Shift so no game binding is in the way, and are never sent on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hotkey {
    /// Leave the stream and return to the launcher.
    Leave,
    /// Leave, and quit the app on the host.
    QuitApp,
    /// Show or hide the stats overlay.
    Stats,
    /// Toggle full screen (Cmd+Ctrl+F).
    FullScreen,
}

pub fn hotkey(key: KeyCode, modifiers: ModifiersState) -> Option<Hotkey> {
    let chord = ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SHIFT;
    if modifiers.contains(chord) {
        match key {
            KeyCode::KeyQ => return Some(Hotkey::Leave),
            KeyCode::KeyX => return Some(Hotkey::QuitApp),
            KeyCode::KeyS => return Some(Hotkey::Stats),
            _ => {}
        }
    }
    if key == KeyCode::KeyF && modifiers.contains(ModifiersState::SUPER | ModifiersState::CONTROL) {
        return Some(Hotkey::FullScreen);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leave_needs_the_whole_chord() {
        let chord = ModifiersState::CONTROL | ModifiersState::ALT | ModifiersState::SHIFT;
        assert_eq!(hotkey(KeyCode::KeyQ, chord), Some(Hotkey::Leave));
        assert_eq!(hotkey(KeyCode::KeyQ, ModifiersState::CONTROL), None);
        assert_eq!(hotkey(KeyCode::KeyS, chord), Some(Hotkey::Stats));
        assert_eq!(hotkey(KeyCode::KeyX, chord), Some(Hotkey::QuitApp));
    }

    #[test]
    fn full_screen_is_cmd_ctrl_f() {
        let m = ModifiersState::SUPER | ModifiersState::CONTROL;
        assert_eq!(hotkey(KeyCode::KeyF, m), Some(Hotkey::FullScreen));
        assert_eq!(hotkey(KeyCode::KeyF, ModifiersState::SUPER), None);
    }
}

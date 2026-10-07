//! The page's `input` messages, as the player's input model.
//!
//! The mapping from there to Moonlight's events (and what a session holds
//! down) is `cha-moonlight-input`, shared with the native client; what stays
//! here is the page's own JSON.

use cha_client::{Input, PadState};
use serde::Deserialize;

/// The page's `{"t":"input", "k": ...}` body.
#[derive(Debug, Deserialize)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum BrowserInput {
    /// Absolute pointer position, 0..1 across the video.
    Move { x: f64, y: f64 },
    /// Relative motion in pixels (pointer lock).
    Rel { dx: f64, dy: f64 },
    /// `PointerEvent.button`.
    Button { b: u8, down: bool },
    /// Wheel deltas in pixels, positive down and right (the DOM's).
    Wheel { dx: f64, dy: f64 },
    /// `KeyboardEvent.code`.
    Key { code: String, down: bool },
    /// `navigator.getGamepads()[i]`, standard mapping.
    Pad(BrowserPad),
}

/// One pad's state: buttons 0..1 and axes -1..1 in the Gamepad API's standard
/// order. The richer fields the page may add (gyro, touch, ...) are ignored.
#[derive(Debug, Default, Deserialize)]
pub struct BrowserPad {
    pub i: usize,
    #[serde(default)]
    pub b: Vec<f32>,
    #[serde(default)]
    pub a: Vec<f32>,
    /// The browser lost it: back to rest.
    #[serde(default)]
    pub gone: bool,
}

impl BrowserInput {
    /// The same input in the player model; `None` for a pad slot that no
    /// pad number can name.
    pub fn into_input(self) -> Option<Input> {
        Some(match self {
            BrowserInput::Move { x, y } => Input::MouseMove {
                x: x as f32,
                y: y as f32,
            },
            BrowserInput::Rel { dx, dy } => Input::MouseMotion {
                dx: dx as f32,
                dy: dy as f32,
            },
            BrowserInput::Button { b, down } => Input::MouseButton { button: b, down },
            BrowserInput::Wheel { dx, dy } => Input::Wheel {
                dx: dx as f32,
                dy: dy as f32,
            },
            BrowserInput::Key { code, down } => Input::Key { code, down },
            BrowserInput::Pad(pad) => {
                let index = u8::try_from(pad.i).ok()?;
                if pad.gone {
                    Input::PadGone { index }
                } else {
                    Input::Pad {
                        index,
                        pad: PadState {
                            buttons: pad.b,
                            axes: pad.a,
                        },
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use cha_moonlight_input::{InputEvent, InputState};

    use super::*;

    fn parse(json: &str) -> BrowserInput {
        serde_json::from_str(json).unwrap()
    }

    fn input(json: &str) -> Input {
        parse(json).into_input().expect("a mappable input")
    }

    #[test]
    fn the_pages_messages_become_player_inputs() {
        assert_eq!(
            input(r#"{"k":"move","x":0.5,"y":1.0}"#),
            Input::MouseMove { x: 0.5, y: 1.0 }
        );
        assert_eq!(
            input(r#"{"k":"rel","dx":-3,"dy":2.5}"#),
            Input::MouseMotion { dx: -3.0, dy: 2.5 }
        );
        assert_eq!(
            input(r#"{"k":"button","b":2,"down":true}"#),
            Input::MouseButton {
                button: 2,
                down: true
            }
        );
        assert_eq!(
            input(r#"{"k":"wheel","dx":0,"dy":100}"#),
            Input::Wheel { dx: 0.0, dy: 100.0 }
        );
        assert_eq!(
            input(r#"{"k":"key","code":"KeyA","down":false}"#),
            Input::Key {
                code: "KeyA".into(),
                down: false
            }
        );
    }

    #[test]
    fn pads_keep_their_slot_and_say_when_they_are_gone() {
        assert_eq!(
            input(r#"{"k":"pad","i":1,"b":[1,0],"a":[0.25,-1],"gyro":[0,0,0]}"#),
            Input::Pad {
                index: 1,
                pad: PadState {
                    buttons: vec![1.0, 0.0],
                    axes: vec![0.25, -1.0]
                }
            }
        );
        assert_eq!(
            input(r#"{"k":"pad","i":3,"gone":true}"#),
            Input::PadGone { index: 3 }
        );
        // No pad number is that large.
        assert_eq!(
            parse(r#"{"k":"pad","i":300,"b":[],"a":[]}"#).into_input(),
            None
        );
    }

    #[test]
    fn a_page_click_reaches_the_host_as_a_moonlight_event() {
        let mut state = InputState::default();
        let events = state.apply(&input(r#"{"k":"button","b":0,"down":true}"#), 2560, 1440);
        assert!(matches!(events[..], [InputEvent::MouseButton { .. }]));
    }
}

//! `cha_client::Input` as the browser's `{"t":"input","k":...}` control lines
//! (`docs/plans/c2-transport.md` §6; the browser's `input.ts` and
//! `controllers/manager.ts`).
//!
//! - Only the session holding the floor is heard, so nothing is produced
//!   without it. A pad's last state is still remembered, and sent when the
//!   floor comes ([`InputMapper::set_floor`]), as the browser does.
//! - Key auto-repeat is suppressed (the app repeats a held key itself): a
//!   second `down` of a held key says nothing.
//! - What is held (keys, buttons, pads) is remembered, so
//!   [`InputMapper::release_all`] can let go of it.
//! - A pad is sent as a snapshot, `b` (24 buttons, the standard mapping's 17
//!   first, 0..1) and `a` (4 axes), rounded to 3 decimals, and only when it
//!   changed. `PadGone` says `gone:true`.

use std::collections::BTreeSet;

use cha_client::{Input, PadState};
use serde_json::{Value, json};

/// Gamepad slots the streamer has.
pub const PADS: usize = 4;
const BUTTONS: usize = 24;
const AXES: usize = 4;

/// A pad as it goes on the wire.
#[derive(Clone, Debug, PartialEq)]
struct PadSnapshot {
    b: [f64; BUTTONS],
    a: [f64; AXES],
}

fn round3(v: f32) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    (f64::from(v) * 1000.0).round() / 1000.0
}

fn round(v: f32, places: i32) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    let k = 10f64.powi(places);
    (f64::from(v) * k).round() / k
}

impl PadSnapshot {
    fn of(pad: &PadState) -> Self {
        let mut b = [0.0; BUTTONS];
        for (slot, v) in b.iter_mut().zip(&pad.buttons) {
            *slot = round3(v.clamp(0.0, 1.0));
        }
        let mut a = [0.0; AXES];
        for (slot, v) in a.iter_mut().zip(&pad.axes) {
            *slot = round3(v.clamp(-1.0, 1.0));
        }
        Self { b, a }
    }

    fn line(&self, index: usize) -> String {
        json!({"t": "input", "k": "pad", "i": index, "b": self.b, "a": self.a}).to_string()
    }
}

fn pad_gone(index: usize) -> String {
    json!({"t": "input", "k": "pad", "i": index, "gone": true}).to_string()
}

fn key(code: &str, down: bool) -> String {
    json!({"t": "input", "k": "key", "code": code, "down": down}).to_string()
}

fn button(b: u8, down: bool) -> String {
    json!({"t": "input", "k": "button", "b": b, "down": down}).to_string()
}

#[derive(Debug, Default)]
pub struct InputMapper {
    floor: bool,
    keys: BTreeSet<String>,
    buttons: BTreeSet<u8>,
    /// The newest state of each pad the player reported.
    latest: [Option<PadSnapshot>; PADS],
    /// What the streamer was last told of each.
    sent: [Option<PadSnapshot>; PADS],
}

impl InputMapper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn has_floor(&self) -> bool {
        self.floor
    }

    /// The floor changed hands. On gaining it, the pads' states are sent
    /// again (the streamer knows none of them); on losing it, what was held
    /// is forgotten (the new controller owns the app's input now).
    pub fn set_floor(&mut self, floor: bool) -> Vec<String> {
        if floor == self.floor {
            return Vec::new();
        }
        self.floor = floor;
        if !floor {
            self.keys.clear();
            self.buttons.clear();
            self.sent = Default::default();
            return Vec::new();
        }
        let mut lines = Vec::new();
        for (i, pad) in self.latest.iter().enumerate() {
            if let Some(pad) = pad {
                lines.push(pad.line(i));
                self.sent[i] = Some(pad.clone());
            }
        }
        lines
    }

    /// The connection dropped: lets go of the keys and mouse buttons held
    /// (the lines are for the old connection, if it can still hear them) and
    /// gives up the floor. Unlike [`release_all`](Self::release_all) the
    /// pads' newest states stay, to be sent to the next session when it gives
    /// us the floor again.
    pub fn drop_floor(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.floor {
            lines.extend(self.keys.iter().map(|k| key(k, false)));
            lines.extend(self.buttons.iter().map(|b| button(*b, false)));
        }
        self.set_floor(false);
        lines
    }

    /// The lines for one input; none when we don't hold the floor (a pad's
    /// state is kept for when we do).
    pub fn map(&mut self, input: &Input) -> Vec<String> {
        match input {
            Input::Pad { index, pad } => {
                let i = usize::from(*index);
                if i >= PADS {
                    return Vec::new();
                }
                let snap = PadSnapshot::of(pad);
                self.latest[i] = Some(snap.clone());
                if !self.floor || self.sent[i].as_ref() == Some(&snap) {
                    return Vec::new();
                }
                let line = snap.line(i);
                self.sent[i] = Some(snap);
                vec![line]
            }
            Input::PadGone { index } => {
                let i = usize::from(*index);
                if i >= PADS {
                    return Vec::new();
                }
                self.latest[i] = None;
                if self.floor && self.sent[i].take().is_some() {
                    vec![pad_gone(i)]
                } else {
                    self.sent[i] = None;
                    Vec::new()
                }
            }
            _ if !self.floor => Vec::new(),
            Input::Key { code, down } => {
                if *down {
                    if !self.keys.insert(code.clone()) {
                        return Vec::new(); // auto-repeat
                    }
                } else if !self.keys.remove(code) {
                    // A key we never saw go down (or already released).
                    return Vec::new();
                }
                vec![key(code, *down)]
            }
            Input::MouseMove { x, y } => vec![
                json!({"t": "input", "k": "move", "x": round(x.clamp(0.0, 1.0), 5), "y": round(y.clamp(0.0, 1.0), 5)})
                    .to_string(),
            ],
            Input::MouseMotion { dx, dy } => {
                if *dx == 0.0 && *dy == 0.0 {
                    return Vec::new();
                }
                vec![
                    json!({"t": "input", "k": "rel", "dx": round(*dx, 2), "dy": round(*dy, 2)})
                        .to_string(),
                ]
            }
            Input::MouseButton { button: b, down } => {
                if *down {
                    self.buttons.insert(*b);
                } else {
                    self.buttons.remove(b);
                }
                vec![button(*b, *down)]
            }
            Input::Wheel { dx, dy } => {
                if *dx == 0.0 && *dy == 0.0 {
                    return Vec::new();
                }
                vec![
                    json!({"t": "input", "k": "wheel", "dx": round(*dx, 2), "dy": round(*dy, 2)})
                        .to_string(),
                ]
            }
        }
    }

    /// Lets go of everything held: keys up, buttons up, pads gone. The pads'
    /// remembered states go too: a stale button is not pressed again later.
    pub fn release_all(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.floor {
            lines.extend(std::mem::take(&mut self.keys).iter().map(|k| key(k, false)));
            lines.extend(
                std::mem::take(&mut self.buttons)
                    .iter()
                    .map(|b| button(*b, false)),
            );
            for (i, sent) in self.sent.iter_mut().enumerate() {
                if sent.take().is_some() {
                    lines.push(pad_gone(i));
                }
            }
        } else {
            self.keys.clear();
            self.buttons.clear();
            self.sent = Default::default();
        }
        self.latest = Default::default();
        lines
    }
}

/// Whether a line is an input line (for tests and logging).
pub fn is_input(line: &str) -> bool {
    serde_json::from_str::<Value>(line).is_ok_and(|v| v["t"] == "input")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    fn controller() -> InputMapper {
        let mut m = InputMapper::new();
        m.set_floor(true);
        m
    }

    #[test]
    fn nothing_is_sent_without_the_floor() {
        let mut m = InputMapper::new();
        assert!(
            m.map(&Input::Key {
                code: "KeyA".into(),
                down: true
            })
            .is_empty()
        );
        assert!(m.map(&Input::MouseMove { x: 0.5, y: 0.5 }).is_empty());
        assert!(
            m.map(&Input::MouseButton {
                button: 0,
                down: true
            })
            .is_empty()
        );
        assert!(m.release_all().is_empty());
    }

    #[test]
    fn keys_buttons_move_wheel_as_the_browser_sends_them() {
        let mut m = controller();
        let l = m.map(&Input::Key {
            code: "KeyW".into(),
            down: true,
        });
        assert_eq!(
            v(&l[0]),
            json!({"t":"input","k":"key","code":"KeyW","down":true})
        );
        let l = m.map(&Input::MouseMove { x: 0.25, y: 0.75 });
        assert_eq!(v(&l[0]), json!({"t":"input","k":"move","x":0.25,"y":0.75}));
        let l = m.map(&Input::MouseMotion { dx: 3.0, dy: -2.5 });
        assert_eq!(v(&l[0]), json!({"t":"input","k":"rel","dx":3.0,"dy":-2.5}));
        assert!(m.map(&Input::MouseMotion { dx: 0.0, dy: 0.0 }).is_empty());
        let l = m.map(&Input::MouseButton {
            button: 2,
            down: true,
        });
        assert_eq!(
            v(&l[0]),
            json!({"t":"input","k":"button","b":2,"down":true})
        );
        let l = m.map(&Input::Wheel { dx: 0.0, dy: 100.0 });
        assert_eq!(
            v(&l[0]),
            json!({"t":"input","k":"wheel","dx":0.0,"dy":100.0})
        );
    }

    #[test]
    fn moves_are_clamped_and_rounded() {
        let mut m = controller();
        let l = m.map(&Input::MouseMove {
            x: 1.7,
            y: 0.123_456_79,
        });
        assert_eq!(v(&l[0])["x"], 1.0);
        assert_eq!(v(&l[0])["y"], 0.12346);
    }

    #[test]
    fn key_repeat_is_swallowed_and_a_stray_up_is_too() {
        let mut m = controller();
        let down = Input::Key {
            code: "KeyA".into(),
            down: true,
        };
        assert_eq!(m.map(&down).len(), 1);
        assert!(m.map(&down).is_empty(), "auto-repeat");
        let up = Input::Key {
            code: "KeyA".into(),
            down: false,
        };
        assert_eq!(m.map(&up).len(), 1);
        assert!(m.map(&up).is_empty());
    }

    #[test]
    fn release_all_lets_go_of_what_is_held() {
        let mut m = controller();
        for code in ["ShiftLeft", "KeyW"] {
            m.map(&Input::Key {
                code: code.into(),
                down: true,
            });
        }
        m.map(&Input::MouseButton {
            button: 0,
            down: true,
        });
        m.map(&Input::Pad {
            index: 1,
            pad: PadState {
                buttons: vec![1.0],
                axes: vec![],
            },
        });
        let lines: Vec<Value> = m.release_all().iter().map(|l| v(l)).collect();
        assert_eq!(lines.len(), 4);
        assert!(lines.contains(&json!({"t":"input","k":"key","code":"KeyW","down":false})));
        assert!(lines.contains(&json!({"t":"input","k":"key","code":"ShiftLeft","down":false})));
        assert!(lines.contains(&json!({"t":"input","k":"button","b":0,"down":false})));
        assert!(lines.contains(&json!({"t":"input","k":"pad","i":1,"gone":true})));
        assert!(m.release_all().is_empty());
    }

    #[test]
    fn a_pad_is_a_snapshot_sent_only_on_change() {
        let mut m = controller();
        let mut pad = PadState {
            buttons: vec![0.0; 17],
            axes: vec![0.0; 4],
        };
        pad.buttons[0] = 1.0;
        pad.buttons[6] = 0.123_456;
        pad.axes[0] = -0.5;
        pad.axes[3] = 0.999_9;
        let l = m.map(&Input::Pad {
            index: 0,
            pad: pad.clone(),
        });
        assert_eq!(l.len(), 1);
        let line = v(&l[0]);
        assert_eq!(
            (line["t"].as_str(), line["k"].as_str(), line["i"].as_u64()),
            (Some("input"), Some("pad"), Some(0))
        );
        let b = line["b"].as_array().unwrap();
        assert_eq!(b.len(), 24);
        assert_eq!(b[0], 1.0);
        assert_eq!(b[6], 0.123);
        assert_eq!(b[23], 0.0);
        assert_eq!(line["a"], json!([-0.5, 0.0, 0.0, 1.0]));
        assert!(line.get("ty").is_none());
        // The same state again: nothing.
        assert!(
            m.map(&Input::Pad {
                index: 0,
                pad: pad.clone()
            })
            .is_empty()
        );
        // A change below the rounding: nothing. A real one: a line.
        pad.buttons[6] = 0.123_4;
        assert!(
            m.map(&Input::Pad {
                index: 0,
                pad: pad.clone()
            })
            .is_empty()
        );
        pad.buttons[1] = 1.0;
        assert_eq!(
            m.map(&Input::Pad {
                index: 0,
                pad: pad.clone()
            })
            .len(),
            1
        );
        // Gone, then back: the full state again.
        let l = m.map(&Input::PadGone { index: 0 });
        assert_eq!(v(&l[0]), json!({"t":"input","k":"pad","i":0,"gone":true}));
        assert!(m.map(&Input::PadGone { index: 0 }).is_empty());
        assert_eq!(m.map(&Input::Pad { index: 0, pad }).len(), 1);
        // Out of range slots are ignored.
        assert!(
            m.map(&Input::Pad {
                index: 4,
                pad: PadState::default()
            })
            .is_empty()
        );
    }

    #[test]
    fn a_pad_seen_without_the_floor_is_sent_when_it_comes() {
        let mut m = InputMapper::new();
        let pad = PadState {
            buttons: vec![0.0, 1.0],
            axes: vec![0.25],
        };
        assert!(m.map(&Input::Pad { index: 2, pad }).is_empty());
        let lines = m.set_floor(true);
        assert_eq!(lines.len(), 1);
        assert_eq!(v(&lines[0])["i"], 2);
        assert_eq!(v(&lines[0])["b"][1], 1.0);
        // Losing it forgets what was held; getting it again re-sends the pad.
        m.set_floor(false);
        assert_eq!(m.set_floor(true).len(), 1);
        assert!(m.set_floor(true).is_empty());
    }

    #[test]
    fn losing_the_floor_forgets_held_keys() {
        let mut m = controller();
        m.map(&Input::Key {
            code: "KeyA".into(),
            down: true,
        });
        m.set_floor(false);
        m.set_floor(true);
        // A stale "up" for a key not held now says nothing; a new down works.
        assert!(
            m.map(&Input::Key {
                code: "KeyA".into(),
                down: false
            })
            .is_empty()
        );
        assert_eq!(
            m.map(&Input::Key {
                code: "KeyA".into(),
                down: true
            })
            .len(),
            1
        );
    }

    #[test]
    fn a_drop_lets_go_of_keys_and_keeps_the_pads_for_the_next_session() {
        let mut m = controller();
        m.map(&Input::Key {
            code: "KeyA".into(),
            down: true,
        });
        m.map(&Input::MouseButton {
            button: 0,
            down: true,
        });
        m.map(&Input::Pad {
            index: 0,
            pad: PadState {
                buttons: vec![1.0; 2],
                axes: vec![0.0; 4],
            },
        });
        let lines = m.drop_floor();
        assert_eq!(lines.len(), 2);
        assert_eq!(v(&lines[0])["code"], "KeyA");
        assert_eq!(v(&lines[0])["down"], false);
        assert_eq!(v(&lines[1])["k"], "button");
        assert!(!m.has_floor());
        // Nothing is said without the floor, and a second drop says nothing.
        assert!(m.drop_floor().is_empty());
        // The next session's floor brings the pad back as it is held now.
        let again = m.set_floor(true);
        assert_eq!(again.len(), 1);
        assert_eq!(v(&again[0])["k"], "pad");
        assert_eq!(v(&again[0])["b"][0], 1.0);
    }
}

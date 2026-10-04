//! Browser input, delivered straight to the seat: no uinput, no libinput.

use smithay::backend::input::{Axis, AxisSource, ButtonState, InputTime, KeyState};
use smithay::input::keyboard::FilterResult;
use smithay::input::pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent};
use smithay::reexports::rustix::time::{ClockId, clock_gettime};
use smithay::utils::{Logical, Point, SERIAL_COUNTER};
use smithay::wayland::pointer_constraints::{PointerConstraint, with_pointer_constraint};

use super::State;
use crate::input::Input;

/// xkb keycodes are evdev codes + 8.
const XKB_OFFSET: u32 = 8;
/// Wayland's high-resolution wheel unit: 120 per notch, which browsers report
/// as roughly 100 px.
const V120_PER_PIXEL: f64 = 1.2;

/// Now on the monotonic clock, in microseconds (what clients get as event times).
fn input_time() -> InputTime {
    let now = clock_gettime(ClockId::Monotonic);
    InputTime::from_micros(now.tv_sec as u64 * 1_000_000 + now.tv_nsec as u64 / 1000)
}

impl State {
    pub(super) fn input(&mut self, input: Input) {
        let serial = SERIAL_COUNTER.next_serial();
        let time = input_time();
        let pointer = self.seat.get_pointer().expect("the seat has a pointer");
        match input {
            Input::Move { x, y } => {
                if self.pointer_locked() {
                    return;
                }
                let location = self.clamp_to_output((x, y).into());
                self.move_pointer(location, serial, time);
            }
            Input::Relative { dx, dy } => {
                let under = self.surface_under(self.pointer_location);
                pointer.relative_motion(
                    self,
                    under,
                    &RelativeMotionEvent {
                        delta: (dx, dy).into(),
                        delta_unaccel: (dx, dy).into(),
                        time,
                    },
                );
                if !self.pointer_locked() {
                    let location =
                        self.clamp_to_output(self.pointer_location + Point::from((dx, dy)));
                    self.move_pointer(location, serial, time);
                } else {
                    pointer.frame(self);
                }
            }
            Input::Button { code, pressed } => {
                if pressed {
                    self.focus_under_pointer(serial);
                }
                pointer.button(
                    self,
                    &ButtonEvent {
                        serial,
                        time,
                        button: code,
                        state: if pressed {
                            ButtonState::Pressed
                        } else {
                            ButtonState::Released
                        },
                    },
                );
                pointer.frame(self);
            }
            Input::Axis { x, y } => {
                let mut frame = AxisFrame::new(time).source(AxisSource::Wheel);
                if y != 0.0 {
                    frame = frame
                        .value(Axis::Vertical, y)
                        .v120(Axis::Vertical, (y * V120_PER_PIXEL).round() as i32);
                }
                if x != 0.0 {
                    frame = frame
                        .value(Axis::Horizontal, x)
                        .v120(Axis::Horizontal, (x * V120_PER_PIXEL).round() as i32);
                }
                pointer.axis(self, frame);
                pointer.frame(self);
            }
            Input::Key { code, pressed } => {
                // Apps repeat held keys themselves; ignore the browser's repeats.
                let held = self.keys_down.contains(&code);
                if pressed == held {
                    return;
                }
                if pressed {
                    self.keys_down.push(code);
                } else {
                    self.keys_down.retain(|&k| k != code);
                }
                let keyboard = self.seat.get_keyboard().expect("the seat has a keyboard");
                keyboard.input::<(), _>(
                    self,
                    (code + XKB_OFFSET).into(),
                    if pressed {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    },
                    serial,
                    time,
                    |_, _, _| FilterResult::Forward,
                );
            }
        }
    }

    fn move_pointer(
        &mut self,
        location: Point<f64, Logical>,
        serial: smithay::utils::Serial,
        time: InputTime,
    ) {
        self.pointer_location = location;
        let pointer = self.seat.get_pointer().expect("the seat has a pointer");
        let under = self.surface_under(location);
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location,
                serial,
                time,
            },
        );
        pointer.frame(self);
        self.maybe_activate_constraint();
        // The cursor is part of the picture, unless the page draws it.
        if !self.client_cursor {
            self.dirty = true;
        }
    }

    fn clamp_to_output(&self, location: Point<f64, Logical>) -> Point<f64, Logical> {
        let (w, h) = self.output_size();
        (
            location.x.clamp(0.0, f64::from(w.max(1) - 1)),
            location.y.clamp(0.0, f64::from(h.max(1) - 1)),
        )
            .into()
    }

    /// Clicking a window raises it and gives it the keyboard.
    fn focus_under_pointer(&mut self, serial: smithay::utils::Serial) {
        let keyboard = self.seat.get_keyboard().expect("the seat has a keyboard");
        if keyboard.is_grabbed() {
            return;
        }
        let Some((window, _)) = self
            .space
            .element_under(self.pointer_location)
            .map(|(w, p)| (w.clone(), p))
        else {
            return;
        };
        self.space.raise_element(&window, true);
        for other in self.space.elements() {
            other.set_activated(other == &window);
            if let Some(toplevel) = other.toplevel() {
                toplevel.send_pending_configure();
            }
        }
        if let Some(toplevel) = window.toplevel() {
            keyboard.set_focus(self, Some(toplevel.wl_surface().clone()), serial);
        }
    }

    /// Whether the focused surface holds an active pointer lock.
    fn pointer_locked(&self) -> bool {
        let pointer = self.seat.get_pointer().expect("the seat has a pointer");
        let Some(surface) = pointer.current_focus() else {
            return false;
        };
        with_pointer_constraint(&surface, &pointer, |constraint| {
            constraint.is_some_and(|c| c.is_active() && matches!(*c, PointerConstraint::Locked(_)))
        })
    }

    /// Activates a lock or confinement the focused surface asked for.
    pub(super) fn maybe_activate_constraint(&mut self) {
        let pointer = self.seat.get_pointer().expect("the seat has a pointer");
        let Some(surface) = pointer.current_focus() else {
            return;
        };
        with_pointer_constraint(&surface, &pointer, |constraint| {
            if let Some(constraint) = constraint
                && !constraint.is_active()
            {
                constraint.activate();
            }
        });
    }
}

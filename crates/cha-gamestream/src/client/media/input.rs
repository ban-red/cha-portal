//! Batching (a pad's batch ends when its buttons change, accumulated relative motion is
//! split into i16-sized packets, up to 16 pads, hover and move events unreliable) follows
//! Moonlight clients' input stream (moonlight-common-c's InputStream.c).
//!
//! Input events to the packets Moonlight clients send (layouts per
//! moonlight-common-c's `Input.h`), and a queue that batches them: relative mouse motion adds up, an absolute
//! position, a pad's axes and a motion sensor keep only the latest, and
//! everything else goes out in order.
//!
//! [`encode`] is the inverse of the host's `input::parse`: what it returns is
//! what follows the control message's length prefix, a little-endian `u32`
//! event type and the body (mouse fields big-endian, the rest little-endian).

use std::collections::VecDeque;

use crate::input::{
    self as wire, BatteryState, GamepadKind, InputEvent, MotionKind, MouseButton, Pen, PenTool,
    PointerKind, Touch,
};

/// ENet channels (numbers per moonlight-common-c's `Limelight-internal.h`).
pub(crate) mod channel {
    pub const KEYBOARD: u8 = 0x02;
    pub const MOUSE: u8 = 0x03;
    pub const PEN: u8 = 0x04;
    pub const TOUCH: u8 = 0x05;
    pub const UTF8: u8 = 0x06;
    pub const GAMEPAD_BASE: u8 = 0x10;
    pub const SENSOR_BASE: u8 = 0x20;
}

/// At most 16 pads, the width of the active-pad mask.
const MAX_PADS: u8 = 16;
/// `UTF8_TEXT_EVENT_MAX_COUNT`: a text packet holds at most this many bytes.
const MAX_TEXT_BYTES: usize = 32;
/// More than this many unsent packets and new ones are dropped.
const MAX_QUEUED: usize = 1024;

/// One input packet and how to send it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Packet {
    pub channel: u8,
    /// Reliable and sequenced, or unreliable (hover and move events, motion).
    pub reliable: bool,
    /// Event type and body.
    pub data: Vec<u8>,
}

/// A packet being built: the event type, then fields.
struct Builder(Vec<u8>);

impl Builder {
    fn new(event: u32) -> Self {
        Self(event.to_le_bytes().to_vec())
    }
    fn u8(mut self, v: u8) -> Self {
        self.0.push(v);
        self
    }
    fn le16(mut self, v: u16) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn be16(mut self, v: i16) -> Self {
        self.0.extend(v.to_be_bytes());
        self
    }
    fn le32(mut self, v: u32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn f32(mut self, v: f32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    fn packet(self, channel: u8, reliable: bool) -> Packet {
        Packet {
            channel,
            reliable,
            data: self.0,
        }
    }
}

// The numeric values of pointer kinds, battery states, motion kinds and pad kinds below
// are per Moonlight's Limelight.h enums (LI_TOUCH_EVENT_*, LI_BATTERY_STATE_*, LI_MOTION_TYPE_*).
fn pointer_kind(k: PointerKind) -> u8 {
    match k {
        PointerKind::Hover => 0,
        PointerKind::Down => 1,
        PointerKind::Up => 2,
        PointerKind::Move => 3,
        PointerKind::Cancel => 4,
        PointerKind::ButtonOnly => 5,
        PointerKind::HoverLeave => 6,
        PointerKind::CancelAll => 7,
    }
}

/// Hover and move events can be lost: the next one says where things are.
fn batchable(k: PointerKind) -> bool {
    matches!(k, PointerKind::Hover | PointerKind::Move)
}

fn pad_index(pad: u16) -> u8 {
    (pad % u16::from(MAX_PADS)) as u8
}

fn mouse_button(b: MouseButton) -> u8 {
    match b {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 3,
        MouseButton::Side => 4,
        MouseButton::Extra => 5,
    }
}

/// Splits `text` into pieces of at most `max` bytes, on character boundaries.
fn utf8_chunks(text: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if i + c.len_utf8() - start > max {
            out.push(&text[start..i]);
            start = i;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn touch(t: &Touch) -> Packet {
    // Major then minor contact area (layout per `SS_TOUCH_PACKET` in Input.h).
    Builder::new(wire::TOUCH)
        .u8(pointer_kind(t.kind))
        .u8(0)
        .le16(t.rotation)
        .le32(t.pointer_id)
        .f32(t.x)
        .f32(t.y)
        .f32(t.pressure)
        .f32(t.contact_major)
        .f32(t.contact_minor)
        .packet(channel::TOUCH, !batchable(t.kind))
}

fn pen(p: &Pen) -> Packet {
    Builder::new(wire::PEN)
        .u8(pointer_kind(p.kind))
        .u8(match p.tool {
            PenTool::Unknown => 0,
            PenTool::Pen => 1,
            PenTool::Eraser => 2,
        })
        .u8(p.buttons)
        .u8(0)
        .f32(p.x)
        .f32(p.y)
        .f32(p.pressure_or_distance)
        .le16(p.rotation)
        .u8(p.tilt)
        .u8(0)
        .f32(p.contact_major)
        .f32(p.contact_minor)
        .packet(channel::PEN, true)
}

/// The packets for one event. Most events are one packet; long text is
/// several, and a relative mouse move of zero is none.
pub(crate) fn encode(event: &InputEvent) -> Vec<Packet> {
    use InputEvent as E;
    let one = match event {
        E::Key {
            down,
            vk,
            modifiers,
        } => Builder::new(if *down { wire::KEY_DOWN } else { wire::KEY_UP })
            // Flags (a Sunshine extension), the key code with bit 15 set (the 0x8000 bit Moonlight
            // clients set on key codes), modifiers, padding.
            .u8(0)
            .le16(0x8000 | u16::from(*vk))
            .u8(*modifiers)
            .le16(0)
            .packet(channel::KEYBOARD, true),
        E::MouseMoveAbsolute {
            x,
            y,
            width,
            height,
        } => Builder::new(wire::MOUSE_ABS)
            .be16(*x)
            .be16(*y)
            .be16(0)
            .be16(*width)
            .be16(*height)
            .packet(channel::MOUSE, true),
        E::MouseMoveRelative { dx, dy } => {
            if *dx == 0 && *dy == 0 {
                return Vec::new();
            }
            Builder::new(wire::MOUSE_REL)
                .be16(*dx)
                .be16(*dy)
                .packet(channel::MOUSE, true)
        }
        E::MouseButton { button, down } => Builder::new(if *down {
            wire::MOUSE_BUTTON_DOWN
        } else {
            wire::MOUSE_BUTTON_UP
        })
        .u8(mouse_button(*button))
        .packet(channel::MOUSE, true),
        // The amount twice, then zero, as Moonlight sends it.
        E::ScrollVertical { amount } => Builder::new(wire::SCROLL_V)
            .be16(*amount)
            .be16(*amount)
            .be16(0)
            .packet(channel::MOUSE, true),
        E::ScrollHorizontal { amount } => Builder::new(wire::SCROLL_H)
            .be16(*amount)
            .packet(channel::MOUSE, true),
        E::Touch(t) => touch(t),
        E::Pen(p) => pen(p),
        E::GamepadArrival {
            pad,
            kind,
            capabilities,
            supported_buttons,
        } => Builder::new(wire::GAMEPAD_ARRIVAL)
            .u8(pad % MAX_PADS)
            .u8(match kind {
                GamepadKind::Unknown => 0,
                GamepadKind::Xbox => 1,
                GamepadKind::PlayStation => 2,
                GamepadKind::Nintendo => 3,
            })
            .le16(*capabilities)
            .le32(*supported_buttons)
            .packet(channel::GAMEPAD_BASE + pad % MAX_PADS, true),
        E::GamepadState {
            pad,
            active_mask,
            buttons,
            left_trigger,
            right_trigger,
            left_stick,
            right_stick,
        } => Builder::new(wire::GAMEPAD_STATE)
            // Header words per Moonlight's NV_MULTI_CONTROLLER_PACKET: 0x1A, 0x14, 0x9C, 0x55.
            .le16(0x1A)
            .le16(u16::from(pad_index(*pad)))
            .le16(*active_mask)
            .le16(0x14)
            .le16(*buttons as u16)
            .u8(*left_trigger)
            .u8(*right_trigger)
            .le16(left_stick.0 as u16)
            .le16(left_stick.1 as u16)
            .le16(right_stick.0 as u16)
            .le16(right_stick.1 as u16)
            .le16(0x9C)
            .le16((*buttons >> 16) as u16)
            .le16(0x55)
            .packet(channel::GAMEPAD_BASE + pad_index(*pad), true),
        E::GamepadTouch {
            pad,
            kind,
            touchpad,
            pointer_id,
            x,
            y,
            pressure,
        } => Builder::new(wire::GAMEPAD_TOUCH)
            .u8(pad % MAX_PADS)
            .u8(pointer_kind(*kind))
            .u8(0)
            .u8(*touchpad)
            .le32(*pointer_id)
            .f32(*x)
            .f32(*y)
            .f32(*pressure)
            .packet(channel::GAMEPAD_BASE + pad % MAX_PADS, !batchable(*kind)),
        E::GamepadMotion { pad, kind, x, y, z } => Builder::new(wire::GAMEPAD_MOTION)
            .u8(pad % MAX_PADS)
            .u8(match kind {
                MotionKind::Acceleration => 1,
                MotionKind::Gyroscope => 2,
            })
            .u8(0)
            .u8(0)
            .f32(*x)
            .f32(*y)
            .f32(*z)
            .packet(channel::SENSOR_BASE + pad % MAX_PADS, false),
        E::GamepadBattery {
            pad,
            state,
            percent,
        } => Builder::new(wire::GAMEPAD_BATTERY)
            .u8(pad % MAX_PADS)
            .u8(match state {
                BatteryState::Unknown => 0,
                BatteryState::NotPresent => 1,
                BatteryState::Discharging => 2,
                BatteryState::Charging => 3,
                BatteryState::NotCharging => 4,
                BatteryState::Full => 5,
            })
            .u8(*percent)
            .u8(0)
            .packet(channel::GAMEPAD_BASE + pad % MAX_PADS, true),
        E::Text(text) => {
            return utf8_chunks(text, MAX_TEXT_BYTES)
                .into_iter()
                .map(|piece| {
                    let mut b = Builder::new(wire::TEXT);
                    b.0.extend_from_slice(piece.as_bytes());
                    b.packet(channel::UTF8, true)
                })
                .collect();
        }
    };
    vec![one]
}

/// What waits to be sent.
enum Item {
    Packets(Vec<Packet>),
    /// Mouse motion since the last send, still adding up.
    Relative {
        dx: i32,
        dy: i32,
    },
    /// The latest absolute position.
    Absolute(InputEvent),
    /// The latest state of one pad.
    Pad(InputEvent),
    /// The latest reading of one motion sensor.
    Motion(InputEvent),
}

/// The events waiting to be sent, batched.
#[derive(Default)]
pub(crate) struct InputQueue {
    items: VecDeque<Item>,
    dropped: u64,
}

impl InputQueue {
    /// Queues an event. Returns false when it was dropped because the queue
    /// is full.
    pub fn push(&mut self, event: InputEvent) -> bool {
        // Merging stops at a mouse button or wheel event, so a click lands where
        // the pointer was when it was pressed.
        let barrier = |i: &Item| matches!(i, Item::Packets(p) if p.first().is_some_and(|p| p.channel == channel::MOUSE));
        match &event {
            InputEvent::MouseMoveRelative { dx, dy } => {
                if *dx == 0 && *dy == 0 {
                    return true;
                }
                for item in self.items.iter_mut().rev() {
                    if barrier(item) {
                        break;
                    }
                    if let Item::Relative { dx: x, dy: y } = item {
                        *x += i32::from(*dx);
                        *y += i32::from(*dy);
                        return true;
                    }
                }
                self.push_item(Item::Relative {
                    dx: i32::from(*dx),
                    dy: i32::from(*dy),
                })
            }
            InputEvent::MouseMoveAbsolute { .. } => {
                for item in self.items.iter_mut().rev() {
                    if barrier(item) {
                        break;
                    }
                    if let Item::Absolute(old) = item {
                        *old = event;
                        return true;
                    }
                }
                self.push_item(Item::Absolute(event))
            }
            InputEvent::GamepadState {
                pad,
                active_mask,
                buttons,
                ..
            } => {
                // A change in the buttons ends the batch, so the host sees the exact
                // axes at the time of the press.
                for item in self.items.iter_mut().rev() {
                    if let Item::Pad(InputEvent::GamepadState {
                        pad: p,
                        active_mask: m,
                        buttons: b,
                        ..
                    }) = item
                        && pad_index(*p) == pad_index(*pad)
                        && m == active_mask
                        && b == buttons
                    {
                        *item = Item::Pad(event);
                        return true;
                    }
                }
                self.push_item(Item::Pad(event))
            }
            InputEvent::GamepadMotion { pad, kind, .. } => {
                for item in self.items.iter_mut().rev() {
                    if let Item::Motion(InputEvent::GamepadMotion {
                        pad: p, kind: k, ..
                    }) = item
                        && *p % MAX_PADS == pad % MAX_PADS
                        && k == kind
                    {
                        *item = Item::Motion(event);
                        return true;
                    }
                }
                self.push_item(Item::Motion(event))
            }
            other => {
                let packets = encode(other);
                if packets.is_empty() {
                    return true;
                }
                self.push_item(Item::Packets(packets))
            }
        }
    }

    fn push_item(&mut self, item: Item) -> bool {
        if self.items.len() >= MAX_QUEUED {
            self.dropped += 1;
            return false;
        }
        self.items.push_back(item);
        true
    }

    /// Events dropped for a full queue so far.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether the next drain carries mouse motion (which is sent at most
    /// once a millisecond).
    pub fn has_mouse_motion(&self) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i, Item::Relative { .. } | Item::Absolute(_)))
    }

    /// Everything queued as packets, in order, and the queue empty.
    pub fn drain(&mut self) -> Vec<Packet> {
        let mut out = Vec::new();
        for item in self.items.drain(..) {
            match item {
                Item::Packets(p) => out.extend(p),
                Item::Relative { mut dx, mut dy } => {
                    // Each packet carries an i16 per axis; a longer sweep goes out as several.
                    while dx != 0 || dy != 0 {
                        let (cx, cy) = (
                            dx.clamp(i32::from(i16::MIN), i32::from(i16::MAX)),
                            dy.clamp(i32::from(i16::MIN), i32::from(i16::MAX)),
                        );
                        out.extend(encode(&InputEvent::MouseMoveRelative {
                            dx: cx as i16,
                            dy: cy as i16,
                        }));
                        dx -= cx;
                        dy -= cy;
                    }
                }
                Item::Absolute(e) | Item::Pad(e) | Item::Motion(e) => out.extend(encode(&e)),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{capabilities, modifiers, parse};

    fn round_trip(event: InputEvent) {
        let packets = encode(&event);
        assert_eq!(packets.len(), 1, "{event:?}");
        assert_eq!(parse(&packets[0].data), Ok(Some(event)));
    }

    #[test]
    fn every_event_parses_back_on_the_host() {
        round_trip(InputEvent::Key {
            down: true,
            vk: 0x41,
            modifiers: modifiers::SHIFT | modifiers::CTRL,
        });
        round_trip(InputEvent::Key {
            down: false,
            vk: 0x70,
            modifiers: 0,
        });
        round_trip(InputEvent::MouseMoveAbsolute {
            x: -5,
            y: 700,
            width: 1920,
            height: 1080,
        });
        round_trip(InputEvent::MouseMoveRelative { dx: -3, dy: 9 });
        for button in [
            MouseButton::Left,
            MouseButton::Middle,
            MouseButton::Right,
            MouseButton::Side,
            MouseButton::Extra,
        ] {
            for down in [true, false] {
                round_trip(InputEvent::MouseButton { button, down });
            }
        }
        round_trip(InputEvent::ScrollVertical { amount: 120 });
        round_trip(InputEvent::ScrollVertical { amount: -240 });
        round_trip(InputEvent::ScrollHorizontal { amount: -120 });
        round_trip(InputEvent::GamepadArrival {
            pad: 1,
            kind: GamepadKind::PlayStation,
            capabilities: capabilities::RUMBLE | capabilities::TOUCHPAD | capabilities::GYRO,
            supported_buttons: 0x0003_FFFF,
        });
        round_trip(InputEvent::GamepadState {
            pad: 3,
            active_mask: 0b1011,
            buttons: 0x0002_1001,
            left_trigger: 200,
            right_trigger: 10,
            left_stick: (-100, 200),
            right_stick: (i16::MAX, i16::MIN),
        });
        round_trip(InputEvent::GamepadTouch {
            pad: 0,
            kind: PointerKind::Move,
            touchpad: 1,
            pointer_id: 7,
            x: 0.25,
            y: 0.75,
            pressure: 0.5,
        });
        for kind in [MotionKind::Acceleration, MotionKind::Gyroscope] {
            round_trip(InputEvent::GamepadMotion {
                pad: 2,
                kind,
                x: 1.5,
                y: -2.5,
                z: 9.81,
            });
        }
        for state in [
            BatteryState::Unknown,
            BatteryState::NotPresent,
            BatteryState::Discharging,
            BatteryState::Charging,
            BatteryState::NotCharging,
            BatteryState::Full,
        ] {
            round_trip(InputEvent::GamepadBattery {
                pad: 1,
                state,
                percent: 80,
            });
        }
        round_trip(InputEvent::Text("héy 🎮".into()));
    }

    /// Touch and pen packets put the contact area's major axis first (body
    /// offset 20), then the minor (24), and round-trip through the host's parser.
    #[test]
    fn touch_and_pen_follow_moonlights_layout() {
        let touch_event = Touch {
            kind: PointerKind::Down,
            pointer_id: 42,
            x: 0.25,
            y: 0.75,
            pressure: 0.5,
            rotation: 270,
            contact_minor: 0.0625,
            contact_major: 0.125,
        };
        let p = encode(&InputEvent::Touch(touch_event));
        assert_eq!(p[0].channel, channel::TOUCH);
        assert!(p[0].reliable, "a touch-down is reliable");
        // Contact major is the field at body offset 20.
        assert_eq!(&p[0].data[4 + 20..4 + 24], &0.125f32.to_le_bytes());
        assert_eq!(&p[0].data[4 + 24..4 + 28], &0.0625f32.to_le_bytes());
        let Ok(Some(InputEvent::Touch(back))) = parse(&p[0].data) else {
            panic!()
        };
        assert_eq!(
            (back.kind, back.pointer_id, back.x, back.y, back.rotation),
            (PointerKind::Down, 42, 0.25, 0.75, 270)
        );
        assert_eq!(
            (back.contact_major, back.contact_minor),
            (touch_event.contact_major, touch_event.contact_minor)
        );
        let moved = encode(&InputEvent::Touch(Touch {
            kind: PointerKind::Move,
            ..touch_event
        }));
        assert!(!moved[0].reliable, "a move may be lost");

        let pen_event = Pen {
            kind: PointerKind::Move,
            tool: PenTool::Eraser,
            buttons: 3,
            x: 0.25,
            y: 0.75,
            pressure_or_distance: 0.6,
            rotation: 270,
            tilt: 35,
            contact_minor: 0.0625,
            contact_major: 0.125,
        };
        let p = encode(&InputEvent::Pen(pen_event));
        assert_eq!(p[0].channel, channel::PEN);
        let Ok(Some(InputEvent::Pen(back))) = parse(&p[0].data) else {
            panic!()
        };
        assert_eq!(
            (back.kind, back.tool, back.buttons, back.rotation, back.tilt),
            (PointerKind::Move, PenTool::Eraser, 3, 270, 35)
        );
        assert_eq!(
            (back.x, back.y, back.pressure_or_distance),
            (0.25, 0.75, 0.6)
        );
        assert_eq!(
            (back.contact_major, back.contact_minor),
            (pen_event.contact_major, pen_event.contact_minor)
        );
    }

    #[test]
    fn channels_follow_moonlight() {
        let ch = |e: InputEvent| encode(&e)[0].channel;
        assert_eq!(
            ch(InputEvent::Key {
                down: true,
                vk: 1,
                modifiers: 0
            }),
            channel::KEYBOARD
        );
        assert_eq!(
            ch(InputEvent::MouseMoveRelative { dx: 1, dy: 1 }),
            channel::MOUSE
        );
        assert_eq!(ch(InputEvent::Text("a".into())), channel::UTF8);
        assert_eq!(
            ch(InputEvent::GamepadBattery {
                pad: 5,
                state: BatteryState::Full,
                percent: 1
            }),
            channel::GAMEPAD_BASE + 5
        );
        assert_eq!(
            ch(InputEvent::GamepadMotion {
                pad: 4,
                kind: MotionKind::Gyroscope,
                x: 0.0,
                y: 0.0,
                z: 0.0
            }),
            channel::SENSOR_BASE + 4
        );
        // Pad 17 wraps to 1.
        assert_eq!(
            ch(InputEvent::GamepadBattery {
                pad: 17,
                state: BatteryState::Full,
                percent: 1
            }),
            channel::GAMEPAD_BASE + 1
        );
    }

    #[test]
    fn long_text_is_split_on_character_boundaries() {
        let text = "é".repeat(40); // 80 bytes
        let packets = encode(&InputEvent::Text(text.clone()));
        assert_eq!(packets.len(), 3);
        let mut joined = String::new();
        for p in &packets {
            assert!(p.data.len() - 4 <= MAX_TEXT_BYTES);
            let Ok(Some(InputEvent::Text(t))) = parse(&p.data) else {
                panic!()
            };
            joined.push_str(&t);
        }
        assert_eq!(joined, text);
        assert!(encode(&InputEvent::Text(String::new())).is_empty());
    }

    fn rel(dx: i16, dy: i16) -> InputEvent {
        InputEvent::MouseMoveRelative { dx, dy }
    }

    fn drained(q: &mut InputQueue) -> Vec<InputEvent> {
        q.drain()
            .iter()
            .map(|p| parse(&p.data).unwrap().unwrap())
            .collect()
    }

    #[test]
    fn mouse_motion_adds_up_and_keeps_its_place() {
        let mut q = InputQueue::default();
        q.push(rel(1, 2));
        q.push(InputEvent::Key {
            down: true,
            vk: 0x41,
            modifiers: 0,
        });
        q.push(rel(10, -5));
        q.push(rel(0, 0));
        assert!(q.has_mouse_motion());
        assert_eq!(
            drained(&mut q),
            vec![
                rel(11, -3),
                InputEvent::Key {
                    down: true,
                    vk: 0x41,
                    modifiers: 0
                }
            ]
        );
        assert!(q.is_empty() && !q.has_mouse_motion());
    }

    #[test]
    fn a_click_is_a_barrier_for_motion() {
        let mut q = InputQueue::default();
        q.push(rel(5, 5));
        q.push(InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
        });
        q.push(rel(1, 1));
        assert_eq!(
            drained(&mut q),
            vec![
                rel(5, 5),
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    down: true
                },
                rel(1, 1)
            ]
        );
    }

    #[test]
    fn a_long_sweep_is_several_packets() {
        let mut q = InputQueue::default();
        for _ in 0..4 {
            q.push(rel(i16::MAX, -i16::MAX));
        }
        let events = drained(&mut q);
        assert_eq!(
            events.len(),
            4,
            "4 * 32767 needs four i16 packets: {events:?}"
        );
        let (mut sx, mut sy) = (0i32, 0i32);
        for e in events {
            let InputEvent::MouseMoveRelative { dx, dy } = e else {
                panic!()
            };
            sx += i32::from(dx);
            sy += i32::from(dy);
        }
        assert_eq!((sx, sy), (4 * 32767, -4 * 32767));
    }

    #[test]
    fn absolute_position_keeps_the_latest() {
        let mut q = InputQueue::default();
        for x in 0..5 {
            q.push(InputEvent::MouseMoveAbsolute {
                x,
                y: 0,
                width: 100,
                height: 100,
            });
        }
        assert_eq!(
            drained(&mut q),
            vec![InputEvent::MouseMoveAbsolute {
                x: 4,
                y: 0,
                width: 100,
                height: 100
            }]
        );
    }

    fn pad_state(pad: u16, buttons: u32, lt: u8) -> InputEvent {
        InputEvent::GamepadState {
            pad,
            active_mask: 1,
            buttons,
            left_trigger: lt,
            right_trigger: 0,
            left_stick: (0, 0),
            right_stick: (0, 0),
        }
    }

    #[test]
    fn a_pads_axes_batch_until_its_buttons_change() {
        let mut q = InputQueue::default();
        q.push(pad_state(0, 0, 1));
        q.push(pad_state(0, 0, 2));
        q.push(pad_state(1, 0, 9));
        q.push(pad_state(0, 0, 3));
        q.push(pad_state(0, 1, 4)); // a press ends the batch
        q.push(pad_state(0, 1, 5));
        assert_eq!(
            drained(&mut q),
            vec![pad_state(0, 0, 3), pad_state(1, 0, 9), pad_state(0, 1, 5)]
        );
    }

    #[test]
    fn motion_keeps_the_latest_per_sensor() {
        let mut q = InputQueue::default();
        let m = |kind, x| InputEvent::GamepadMotion {
            pad: 0,
            kind,
            x,
            y: 0.0,
            z: 0.0,
        };
        q.push(m(MotionKind::Gyroscope, 1.0));
        q.push(m(MotionKind::Acceleration, 2.0));
        q.push(m(MotionKind::Gyroscope, 3.0));
        assert_eq!(
            drained(&mut q),
            vec![
                m(MotionKind::Gyroscope, 3.0),
                m(MotionKind::Acceleration, 2.0)
            ]
        );
    }

    #[test]
    fn a_full_queue_drops_new_events_and_counts_them() {
        let mut q = InputQueue::default();
        let key = |vk| InputEvent::Key {
            down: true,
            vk,
            modifiers: 0,
        };
        for _ in 0..MAX_QUEUED {
            assert!(q.push(key(0x41)));
        }
        assert!(!q.push(key(0x42)));
        assert_eq!(q.dropped(), 1);
        // Motion that merges still gets in.
        assert!(!q.push(rel(1, 1)), "a new item doesn't");
        assert_eq!(q.drain().len(), MAX_QUEUED);
        assert!(q.push(rel(1, 1)));
    }
}

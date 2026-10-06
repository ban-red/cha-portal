//! The wire between Steam's `/dev/uinput` shim and the streamer's broker
//! ([`crate::uinput_broker`]). The shim (`images/steam/uinput-shim/`, C, built
//! for i386 and amd64) keeps the same bytes in `proto.h`: change both together.
//!
//! One `SOCK_SEQPACKET` connection to `<runtime dir>/uinput.sock` is one
//! virtual device. Every packet is an op byte and a body; integers are
//! little-endian; nothing depends on the client's ABI (the shim turns its own
//! `input_event`, `uinput_setup`, `uinput_user_dev` and `ff_effect` layouts
//! into these).
//!
//! Shim to broker (each gets exactly one [`Message::Reply`], except `FF_DONE`):
//!
//! | op | name | body |
//! |---|---|---|
//! | 1 | `SET_BIT` | kind u8 ([`BitKind`]), code u16 |
//! | 2 | `ABS_SETUP` | code u16, min i32, max i32, fuzz i32, flat i32, res i32 |
//! | 3 | `DEV_SETUP` | bustype u16, vendor u16, product u16, version u16, ff_effects_max u32, name 80 bytes |
//! | 4 | `CREATE` | |
//! | 5 | `DESTROY` | |
//! | 6 | `GET_SYSNAME` | |
//! | 7 | `EVENTS` | n × event: type u16, code u16, value i32 |
//! | 8 | `FF_DONE` | request id u32, retval i32 (the client's `UI_END_FF_*`) |
//!
//! Broker to shim:
//!
//! | op | name | body |
//! |---|---|---|
//! | 0x80 | `REPLY` | errno i32 (0 for success), then a payload (`GET_SYSNAME`: the name, no NUL) |
//! | 0x81 | `FF_UPLOAD` | request id u32, effect 40 bytes, old effect 40 bytes |
//! | 0x82 | `FF_ERASE` | request id u32, effect id u32 |
//! | 0x83 | `EVENT` | type u16, code u16, value i32 (an `EV_FF` play, stop or gain) |
//!
//! An effect is the kernel's `struct ff_effect` made ABI-neutral: type u16,
//! id i16, direction u16, trigger button u16, trigger interval u16, replay
//! length u16, replay delay u16, 2 zero bytes, then the first 24 bytes of its
//! union (rumble, periodic up to `custom_len`, condition); the custom data
//! pointer isn't carried. The shim lays it out for its own bitness.

use std::fmt;

pub const SOCKET_NAME: &str = "uinput.sock";
/// The biggest packet either side sends or accepts.
pub const MAX_PACKET: usize = 4096;
/// `uinput_user_dev`'s and `uinput_setup`'s name field.
pub const NAME_LEN: usize = 80;
pub const EFFECT_LEN: usize = 40;
pub const EVENT_LEN: usize = 8;

pub const OP_SET_BIT: u8 = 1;
pub const OP_ABS_SETUP: u8 = 2;
pub const OP_DEV_SETUP: u8 = 3;
pub const OP_CREATE: u8 = 4;
pub const OP_DESTROY: u8 = 5;
pub const OP_GET_SYSNAME: u8 = 6;
pub const OP_EVENTS: u8 = 7;
pub const OP_FF_DONE: u8 = 8;
pub const OP_REPLY: u8 = 0x80;
pub const OP_FF_UPLOAD: u8 = 0x81;
pub const OP_FF_ERASE: u8 = 0x82;
pub const OP_EVENT: u8 = 0x83;

/// Which `UI_SET_*BIT` a [`Request::SetBit`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitKind {
    Ev = 0,
    Key = 1,
    Abs = 2,
    Ff = 3,
    Msc = 4,
    Rel = 5,
    Led = 6,
    Snd = 7,
    Sw = 8,
    Prop = 9,
}

impl BitKind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::Ev,
            1 => Self::Key,
            2 => Self::Abs,
            3 => Self::Ff,
            4 => Self::Msc,
            5 => Self::Rel,
            6 => Self::Led,
            7 => Self::Snd,
            8 => Self::Sw,
            9 => Self::Prop,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Ev => "EV",
            Self::Key => "KEY",
            Self::Abs => "ABS",
            Self::Ff => "FF",
            Self::Msc => "MSC",
            Self::Rel => "REL",
            Self::Led => "LED",
            Self::Snd => "SND",
            Self::Sw => "SW",
            Self::Prop => "PROP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub kind: u16,
    pub code: u16,
    pub value: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsSetup {
    pub code: u16,
    pub min: i32,
    pub max: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub res: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevSetup {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub ff_effects_max: u32,
    pub name: [u8; NAME_LEN],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    SetBit { kind: BitKind, code: u16 },
    AbsSetup(AbsSetup),
    DevSetup(DevSetup),
    Create,
    Destroy,
    GetSysname,
    Events(Vec<Event>),
    FfDone { request_id: u32, retval: i32 },
}

/// A packet that isn't one of ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    UnknownOp(u8),
    /// The body isn't the size the op takes.
    BadLength(u8),
    UnknownBitKind(u8),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty packet"),
            Self::UnknownOp(op) => write!(f, "unknown op {op:#x}"),
            Self::BadLength(op) => write!(f, "op {op:#x} with a body of the wrong size"),
            Self::UnknownBitKind(k) => write!(f, "unknown bit kind {k}"),
        }
    }
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn i32_at(b: &[u8], at: usize) -> i32 {
    u32_at(b, at) as i32
}

impl Request {
    pub fn decode(packet: &[u8]) -> Result<Self, DecodeError> {
        let (&op, body) = packet.split_first().ok_or(DecodeError::Empty)?;
        let want = |len: usize| {
            if body.len() == len {
                Ok(())
            } else {
                Err(DecodeError::BadLength(op))
            }
        };
        Ok(match op {
            OP_SET_BIT => {
                want(3)?;
                let kind = BitKind::from_u8(body[0]).ok_or(DecodeError::UnknownBitKind(body[0]))?;
                Self::SetBit {
                    kind,
                    code: u16_at(body, 1),
                }
            }
            OP_ABS_SETUP => {
                want(22)?;
                Self::AbsSetup(AbsSetup {
                    code: u16_at(body, 0),
                    min: i32_at(body, 2),
                    max: i32_at(body, 6),
                    fuzz: i32_at(body, 10),
                    flat: i32_at(body, 14),
                    res: i32_at(body, 18),
                })
            }
            OP_DEV_SETUP => {
                want(12 + NAME_LEN)?;
                let mut name = [0u8; NAME_LEN];
                name.copy_from_slice(&body[12..]);
                Self::DevSetup(DevSetup {
                    bustype: u16_at(body, 0),
                    vendor: u16_at(body, 2),
                    product: u16_at(body, 4),
                    version: u16_at(body, 6),
                    ff_effects_max: u32_at(body, 8),
                    name,
                })
            }
            OP_CREATE => {
                want(0)?;
                Self::Create
            }
            OP_DESTROY => {
                want(0)?;
                Self::Destroy
            }
            OP_GET_SYSNAME => {
                want(0)?;
                Self::GetSysname
            }
            OP_EVENTS => {
                if body.len() % EVENT_LEN != 0 {
                    return Err(DecodeError::BadLength(op));
                }
                Self::Events(body.chunks_exact(EVENT_LEN).map(decode_event).collect())
            }
            OP_FF_DONE => {
                want(8)?;
                Self::FfDone {
                    request_id: u32_at(body, 0),
                    retval: i32_at(body, 4),
                }
            }
            other => return Err(DecodeError::UnknownOp(other)),
        })
    }

    /// What the shim sends (the broker only decodes; tests and a fake client
    /// encode).
    #[cfg(test)]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::SetBit { kind, code } => {
                out.extend([OP_SET_BIT, *kind as u8]);
                out.extend(code.to_le_bytes());
            }
            Self::AbsSetup(a) => {
                out.push(OP_ABS_SETUP);
                out.extend(a.code.to_le_bytes());
                for v in [a.min, a.max, a.fuzz, a.flat, a.res] {
                    out.extend(v.to_le_bytes());
                }
            }
            Self::DevSetup(d) => {
                out.push(OP_DEV_SETUP);
                for v in [d.bustype, d.vendor, d.product, d.version] {
                    out.extend(v.to_le_bytes());
                }
                out.extend(d.ff_effects_max.to_le_bytes());
                out.extend(d.name);
            }
            Self::Create => out.push(OP_CREATE),
            Self::Destroy => out.push(OP_DESTROY),
            Self::GetSysname => out.push(OP_GET_SYSNAME),
            Self::Events(events) => {
                out.push(OP_EVENTS);
                for e in events {
                    out.extend(encode_event(e));
                }
            }
            Self::FfDone { request_id, retval } => {
                out.push(OP_FF_DONE);
                out.extend(request_id.to_le_bytes());
                out.extend(retval.to_le_bytes());
            }
        }
        out
    }
}

fn decode_event(b: &[u8]) -> Event {
    Event {
        kind: u16_at(b, 0),
        code: u16_at(b, 2),
        value: i32_at(b, 4),
    }
}

fn encode_event(e: &Event) -> [u8; EVENT_LEN] {
    let mut out = [0u8; EVENT_LEN];
    out[0..2].copy_from_slice(&e.kind.to_le_bytes());
    out[2..4].copy_from_slice(&e.code.to_le_bytes());
    out[4..8].copy_from_slice(&e.value.to_le_bytes());
    out
}

/// `struct ff_effect` without the ABI: see the module's notes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WireEffect {
    pub kind: u16,
    pub id: i16,
    pub direction: u16,
    pub trigger_button: u16,
    pub trigger_interval: u16,
    pub replay_length: u16,
    pub replay_delay: u16,
    pub union: [u8; 24],
}

impl WireEffect {
    pub fn encode(&self) -> [u8; EFFECT_LEN] {
        let mut out = [0u8; EFFECT_LEN];
        out[0..2].copy_from_slice(&self.kind.to_le_bytes());
        out[2..4].copy_from_slice(&self.id.to_le_bytes());
        out[4..6].copy_from_slice(&self.direction.to_le_bytes());
        out[6..8].copy_from_slice(&self.trigger_button.to_le_bytes());
        out[8..10].copy_from_slice(&self.trigger_interval.to_le_bytes());
        out[10..12].copy_from_slice(&self.replay_length.to_le_bytes());
        out[12..14].copy_from_slice(&self.replay_delay.to_le_bytes());
        out[16..].copy_from_slice(&self.union);
        out
    }

    #[cfg(test)]
    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != EFFECT_LEN {
            return None;
        }
        let mut union = [0u8; 24];
        union.copy_from_slice(&b[16..]);
        Some(Self {
            kind: u16_at(b, 0),
            id: u16_at(b, 2) as i16,
            direction: u16_at(b, 4),
            trigger_button: u16_at(b, 6),
            trigger_interval: u16_at(b, 8),
            replay_length: u16_at(b, 10),
            replay_delay: u16_at(b, 12),
            union,
        })
    }
}

/// What the broker sends the shim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Reply {
        errno: i32,
        payload: Vec<u8>,
    },
    FfUpload {
        request_id: u32,
        effect: WireEffect,
        old: WireEffect,
    },
    FfErase {
        request_id: u32,
        effect_id: u32,
    },
    Event(Event),
}

impl Message {
    pub fn ok() -> Self {
        Self::Reply {
            errno: 0,
            payload: Vec::new(),
        }
    }

    pub fn err(errno: i32) -> Self {
        Self::Reply {
            errno,
            payload: Vec::new(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Reply { errno, payload } => {
                out.push(OP_REPLY);
                out.extend(errno.to_le_bytes());
                out.extend(payload);
            }
            Self::FfUpload {
                request_id,
                effect,
                old,
            } => {
                out.push(OP_FF_UPLOAD);
                out.extend(request_id.to_le_bytes());
                out.extend(effect.encode());
                out.extend(old.encode());
            }
            Self::FfErase {
                request_id,
                effect_id,
            } => {
                out.push(OP_FF_ERASE);
                out.extend(request_id.to_le_bytes());
                out.extend(effect_id.to_le_bytes());
            }
            Self::Event(e) => {
                out.push(OP_EVENT);
                out.extend(encode_event(e));
            }
        }
        out
    }

    /// What the shim does with the broker's packets (tests, and a fake client).
    #[cfg(test)]
    pub fn decode(packet: &[u8]) -> Result<Self, DecodeError> {
        let (&op, body) = packet.split_first().ok_or(DecodeError::Empty)?;
        let want = |len: usize| {
            if body.len() == len {
                Ok(())
            } else {
                Err(DecodeError::BadLength(op))
            }
        };
        Ok(match op {
            OP_REPLY => {
                if body.len() < 4 {
                    return Err(DecodeError::BadLength(op));
                }
                Self::Reply {
                    errno: i32_at(body, 0),
                    payload: body[4..].to_vec(),
                }
            }
            OP_FF_UPLOAD => {
                want(4 + 2 * EFFECT_LEN)?;
                Self::FfUpload {
                    request_id: u32_at(body, 0),
                    effect: WireEffect::decode(&body[4..4 + EFFECT_LEN]).expect("sized"),
                    old: WireEffect::decode(&body[4 + EFFECT_LEN..]).expect("sized"),
                }
            }
            OP_FF_ERASE => {
                want(8)?;
                Self::FfErase {
                    request_id: u32_at(body, 0),
                    effect_id: u32_at(body, 4),
                }
            }
            OP_EVENT => {
                want(EVENT_LEN)?;
                Self::Event(decode_event(body))
            }
            other => return Err(DecodeError::UnknownOp(other)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> [u8; NAME_LEN] {
        let mut n = [0u8; NAME_LEN];
        n[..s.len()].copy_from_slice(s.as_bytes());
        n
    }

    #[test]
    fn requests_round_trip() {
        let requests = [
            Request::SetBit {
                kind: BitKind::Key,
                code: 0x130,
            },
            Request::AbsSetup(AbsSetup {
                code: 3,
                min: -32768,
                max: 32767,
                fuzz: 16,
                flat: 128,
                res: 0,
            }),
            Request::DevSetup(DevSetup {
                bustype: 3,
                vendor: 0x28de,
                product: 0x11ff,
                version: 0x0100,
                ff_effects_max: 16,
                name: name("Steam Virtual Gamepad"),
            }),
            Request::Create,
            Request::Destroy,
            Request::GetSysname,
            Request::Events(vec![
                Event {
                    kind: 1,
                    code: 0x130,
                    value: 1,
                },
                Event {
                    kind: 3,
                    code: 0,
                    value: -5,
                },
                Event {
                    kind: 0,
                    code: 0,
                    value: 0,
                },
            ]),
            Request::FfDone {
                request_id: 7,
                retval: -22,
            },
        ];
        for request in requests {
            assert_eq!(Request::decode(&request.encode()), Ok(request));
        }
    }

    #[test]
    fn exact_bytes() {
        // The shim's proto.h must produce these.
        assert_eq!(
            Request::SetBit {
                kind: BitKind::Abs,
                code: 0x11
            }
            .encode(),
            [1, 2, 0x11, 0]
        );
        assert_eq!(
            Request::FfDone {
                request_id: 0x0102_0304,
                retval: -1
            }
            .encode(),
            [8, 4, 3, 2, 1, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            Request::Events(vec![Event {
                kind: 3,
                code: 1,
                value: -2
            }])
            .encode(),
            [7, 3, 0, 1, 0, 0xfe, 0xff, 0xff, 0xff]
        );
        assert_eq!(Message::err(22).encode(), [0x80, 22, 0, 0, 0]);
        let mut sys = Message::ok().encode();
        sys.extend(b"input42");
        assert_eq!(&sys[..5], &[0x80, 0, 0, 0, 0]);
        let effect = WireEffect {
            kind: 0x50,
            id: -1,
            replay_length: 300,
            ..WireEffect::default()
        }
        .encode();
        assert_eq!(effect.len(), 40);
        assert_eq!(&effect[..4], &[0x50, 0, 0xff, 0xff]);
        assert_eq!(&effect[10..12], &300u16.to_le_bytes());
    }

    #[test]
    fn requests_with_the_wrong_size_are_refused() {
        assert_eq!(Request::decode(&[]), Err(DecodeError::Empty));
        assert_eq!(Request::decode(&[99]), Err(DecodeError::UnknownOp(99)));
        assert_eq!(
            Request::decode(&[OP_SET_BIT, 1]),
            Err(DecodeError::BadLength(OP_SET_BIT))
        );
        assert_eq!(
            Request::decode(&[OP_SET_BIT, 42, 0, 0]),
            Err(DecodeError::UnknownBitKind(42))
        );
        assert_eq!(
            Request::decode(&[OP_CREATE, 0]),
            Err(DecodeError::BadLength(OP_CREATE))
        );
        assert_eq!(
            Request::decode(&[OP_EVENTS, 1, 2, 3]),
            Err(DecodeError::BadLength(OP_EVENTS))
        );
        assert_eq!(
            Request::decode(&[OP_DEV_SETUP; 20]),
            Err(DecodeError::BadLength(OP_DEV_SETUP))
        );
    }

    #[test]
    fn messages_round_trip() {
        let effect = WireEffect {
            kind: 0x51,
            id: 3,
            direction: 0x4000,
            trigger_button: 1,
            trigger_interval: 2,
            replay_length: 500,
            replay_delay: 10,
            union: std::array::from_fn(|i| i as u8),
        };
        let messages = [
            Message::ok(),
            Message::err(libc::EINVAL),
            Message::Reply {
                errno: 0,
                payload: b"input17".to_vec(),
            },
            Message::FfUpload {
                request_id: 9,
                effect,
                old: WireEffect::default(),
            },
            Message::FfErase {
                request_id: 10,
                effect_id: 3,
            },
            Message::Event(Event {
                kind: 0x15,
                code: 0x60,
                value: 0xffff,
            }),
        ];
        for message in messages {
            assert_eq!(Message::decode(&message.encode()), Ok(message));
        }
    }
}

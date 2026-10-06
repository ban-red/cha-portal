//! What the uinput broker lets a client make: the validator that is the whole
//! trust boundary (the socket is reachable by every process of the app, games
//! included). Pure, so it is tested without a kernel.

use std::time::Duration;

use crate::uinput_proto::{AbsSetup, BitKind, DevSetup, Event, NAME_LEN};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const EV_FF: u16 = 0x15;

pub const BTN_SOUTH: u16 = 0x130;
pub const BTN_THUMBR: u16 = 0x13e;
pub const BTN_DPAD_UP: u16 = 0x220;
pub const BTN_DPAD_RIGHT: u16 = 0x223;
pub const ABS_X: u16 = 0x00;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;

pub const FF_RUMBLE: u16 = 0x50;
pub const FF_PERIODIC: u16 = 0x51;
/// The periodic waveforms: square, triangle, sine, saw up, saw down.
const FF_WAVEFORMS: std::ops::RangeInclusive<u16> = 0x58..=0x5c;
pub const FF_GAIN: u16 = 0x60;

/// An effect id the kernel hands out is below `ff_effects_max`.
pub const MAX_FF_EFFECTS: u32 = 16;
/// Axis ranges, either side of 0.
pub const AXIS_LIMIT: i32 = 65535;
pub const MAX_EVENTS_PER_WRITE: usize = 64;
/// Events per second a device may take, and the burst on top.
pub const EVENT_RATE: u32 = 20_000;
pub const EVENT_BURST: u32 = 2048;
/// Devices one environment's clients may have at once (the pad limit).
pub const MAX_DEVICES: usize = crate::gamepad::MAX_PADS;
pub const DEFAULT_NAME: &str = "Steam Virtual Gamepad";
pub const VENDOR: u16 = 0x28de;
pub const PRODUCT: u16 = 0x11ff;
pub const BUS_USB: u16 = 0x03;

/// Why a request was refused; the broker logs it once per `what`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// A short, stable label: what kind of thing, for the log-once.
    pub what: &'static str,
    pub detail: String,
}

fn refuse<T>(what: &'static str, detail: String) -> Result<T, Refusal> {
    Err(Refusal { what, detail })
}

/// A `UI_SET_*BIT` the client may make.
pub fn check_bit(kind: BitKind, code: u16) -> Result<(), Refusal> {
    let ok = match kind {
        BitKind::Ev => matches!(code, EV_SYN | EV_KEY | EV_ABS | EV_FF),
        BitKind::Key => {
            (BTN_SOUTH..=BTN_THUMBR).contains(&code)
                || (BTN_DPAD_UP..=BTN_DPAD_RIGHT).contains(&code)
        }
        BitKind::Abs => (ABS_X..=ABS_RZ).contains(&code) || matches!(code, ABS_HAT0X | ABS_HAT0Y),
        BitKind::Ff => {
            matches!(code, FF_RUMBLE | FF_PERIODIC | FF_GAIN) || FF_WAVEFORMS.contains(&code)
        }
        BitKind::Msc | BitKind::Rel | BitKind::Led | BitKind::Snd | BitKind::Sw | BitKind::Prop => {
            false
        }
    };
    if ok {
        Ok(())
    } else {
        refuse(
            "bit",
            format!("{} bit {code:#x} isn't one a gamepad has", kind.name()),
        )
    }
}

pub fn check_abs(setup: &AbsSetup) -> Result<(), Refusal> {
    check_bit(BitKind::Abs, setup.code)?;
    let range = -AXIS_LIMIT..=AXIS_LIMIT;
    if !range.contains(&setup.min) || !range.contains(&setup.max) || setup.min > setup.max {
        return refuse(
            "abs range",
            format!(
                "axis {:#x} range {}..{} is out of bounds",
                setup.code, setup.min, setup.max
            ),
        );
    }
    // Fuzz and flat are in the axis's units, so no bigger than its span.
    let span = i64::from(setup.max) - i64::from(setup.min);
    if setup.fuzz < 0
        || setup.flat < 0
        || setup.res < 0
        || i64::from(setup.fuzz) > span.max(1)
        || i64::from(setup.flat) > span.max(1)
        || setup.res > AXIS_LIMIT
    {
        return refuse(
            "abs range",
            format!(
                "axis {:#x} fuzz {} flat {} res {} don't fit its range",
                setup.code, setup.fuzz, setup.flat, setup.res
            ),
        );
    }
    Ok(())
}

pub fn check_dev_setup(setup: &DevSetup) -> Result<(), Refusal> {
    if setup.ff_effects_max > MAX_FF_EFFECTS {
        return refuse(
            "ff_effects_max",
            format!(
                "{} force feedback effects (at most {MAX_FF_EFFECTS})",
                setup.ff_effects_max
            ),
        );
    }
    Ok(())
}

/// The name the device gets: the client's, as printable ASCII and at most 79
/// characters, or the default.
pub fn sanitize_name(raw: &[u8; NAME_LEN]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
    let name: String = raw[..end]
        .iter()
        .map(|&b| {
            if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '?'
            }
        })
        .take(NAME_LEN - 1)
        .collect();
    let name = name.trim();
    if name.is_empty() {
        DEFAULT_NAME.to_string()
    } else {
        name.to_string()
    }
}

/// One write's events.
pub fn check_events(events: &[Event]) -> Result<(), Refusal> {
    if events.len() > MAX_EVENTS_PER_WRITE {
        return refuse(
            "too many events",
            format!(
                "{} events in one write (at most {MAX_EVENTS_PER_WRITE})",
                events.len()
            ),
        );
    }
    for e in events {
        let ok = match e.kind {
            EV_SYN => true,
            EV_KEY => {
                (BTN_SOUTH..=BTN_THUMBR).contains(&e.code)
                    || (BTN_DPAD_UP..=BTN_DPAD_RIGHT).contains(&e.code)
            }
            EV_ABS => (ABS_X..=ABS_RZ).contains(&e.code) || matches!(e.code, ABS_HAT0X | ABS_HAT0Y),
            // An effect's id, or the gain.
            EV_FF => u32::from(e.code) < MAX_FF_EFFECTS || e.code == FF_GAIN,
            _ => false,
        };
        if !ok {
            return refuse(
                "event",
                format!("event type {:#x} code {:#x}", e.kind, e.code),
            );
        }
    }
    Ok(())
}

/// Whether the kernel's effect, handed on to the client, is one we advertise.
pub fn effect_kind_ok(kind: u16) -> bool {
    matches!(kind, FF_RUMBLE | FF_PERIODIC)
}

/// A token bucket for events, with the clock passed in.
pub struct RateLimit {
    tokens: f64,
    last: Duration,
}

impl RateLimit {
    pub fn new() -> Self {
        Self {
            tokens: f64::from(EVENT_BURST),
            last: Duration::ZERO,
        }
    }

    /// Takes `n` events at time `now` (since any fixed start); false if the
    /// device is over its rate.
    pub fn take(&mut self, n: usize, now: Duration) -> bool {
        let gained = now.saturating_sub(self.last).as_secs_f64() * f64::from(EVENT_RATE);
        self.last = self.last.max(now);
        self.tokens = (self.tokens + gained).min(f64::from(EVENT_BURST));
        if self.tokens >= n as f64 {
            self.tokens -= n as f64;
            true
        } else {
            false
        }
    }
}

/// Which of the environment's [`MAX_DEVICES`] slots the clients' devices hold.
/// A slot is also the device's number in its `phys`.
#[derive(Default)]
pub struct Slots(std::sync::Mutex<[bool; MAX_DEVICES]>);

impl Slots {
    /// The lowest free slot, held until [`Slots::release`].
    pub fn take(&self) -> Option<usize> {
        let mut used = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let slot = used.iter().position(|u| !u)?;
        used[slot] = true;
        Some(slot)
    }

    pub fn release(&self, slot: usize) {
        if let Some(u) = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(slot)
        {
            *u = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: u16, code: u16, value: i32) -> Event {
        Event { kind, code, value }
    }

    fn abs(code: u16, min: i32, max: i32) -> AbsSetup {
        AbsSetup {
            code,
            min,
            max,
            fuzz: 0,
            flat: 0,
            res: 0,
        }
    }

    #[test]
    fn bits_a_gamepad_has() {
        for (kind, code) in [
            (BitKind::Ev, 0),
            (BitKind::Ev, 1),
            (BitKind::Ev, 3),
            (BitKind::Ev, 0x15),
            (BitKind::Key, 0x130),
            (BitKind::Key, 0x13e),
            (BitKind::Key, 0x220),
            (BitKind::Key, 0x223),
            (BitKind::Abs, 0),
            (BitKind::Abs, 5),
            (BitKind::Abs, 0x10),
            (BitKind::Abs, 0x11),
            (BitKind::Ff, 0x50),
            (BitKind::Ff, 0x51),
            (BitKind::Ff, 0x58),
            (BitKind::Ff, 0x5a),
            (BitKind::Ff, 0x60),
        ] {
            assert_eq!(check_bit(kind, code), Ok(()), "{kind:?} {code:#x}");
        }
    }

    #[test]
    fn bits_a_gamepad_hasnt() {
        for (kind, code) in [
            (BitKind::Ev, 2),      // EV_REL
            (BitKind::Ev, 4),      // EV_MSC
            (BitKind::Ev, 0x17),   // EV_FF_STATUS
            (BitKind::Key, 30),    // KEY_A
            (BitKind::Key, 0x110), // BTN_LEFT
            (BitKind::Key, 0x13f),
            (BitKind::Key, 0x224),
            (BitKind::Abs, 6),
            (BitKind::Abs, 0x28), // ABS_MISC
            (BitKind::Ff, 0x52),  // FF_CONSTANT
            (BitKind::Ff, 0x5d),  // FF_CUSTOM
            (BitKind::Msc, 4),
            (BitKind::Rel, 0),
            (BitKind::Led, 0),
            (BitKind::Snd, 0),
            (BitKind::Sw, 0),
            (BitKind::Prop, 0),
        ] {
            let err = check_bit(kind, code).unwrap_err();
            assert_eq!(err.what, "bit", "{kind:?} {code:#x}");
        }
    }

    #[test]
    fn axis_ranges() {
        assert_eq!(check_abs(&abs(0, -32768, 32767)), Ok(()));
        assert_eq!(check_abs(&abs(0x10, -1, 1)), Ok(()));
        assert_eq!(check_abs(&abs(2, 0, 255)), Ok(()));
        assert_eq!(check_abs(&abs(0, -65535, 65535)), Ok(()));
        assert!(check_abs(&abs(0, -65536, 0)).is_err());
        assert!(check_abs(&abs(0, 0, 65536)).is_err());
        assert!(check_abs(&abs(0, i32::MIN, i32::MAX)).is_err());
        assert!(check_abs(&abs(0, 5, -5)).is_err());
        // An axis a gamepad hasn't.
        assert!(check_abs(&abs(0x28, 0, 1)).is_err());
        let mut wide = abs(0, -100, 100);
        wide.flat = 10_000;
        assert!(check_abs(&wide).is_err());
        wide.flat = 4096;
        wide.fuzz = -1;
        assert!(check_abs(&wide).is_err());
    }

    fn setup(ff: u32) -> DevSetup {
        DevSetup {
            bustype: 5,
            vendor: 1,
            product: 2,
            version: 3,
            ff_effects_max: ff,
            name: [0; NAME_LEN],
        }
    }

    #[test]
    fn effect_count() {
        assert_eq!(check_dev_setup(&setup(0)), Ok(()));
        assert_eq!(check_dev_setup(&setup(16)), Ok(()));
        assert_eq!(
            check_dev_setup(&setup(17)).unwrap_err().what,
            "ff_effects_max"
        );
        assert!(check_dev_setup(&setup(u32::MAX)).is_err());
    }

    #[test]
    fn names_are_sanitised() {
        let name = |s: &[u8]| {
            let mut n = [0u8; NAME_LEN];
            n[..s.len()].copy_from_slice(s);
            sanitize_name(&n)
        };
        assert_eq!(name(b"Steam Virtual Gamepad"), "Steam Virtual Gamepad");
        assert_eq!(name(b""), DEFAULT_NAME);
        assert_eq!(name(b"   "), DEFAULT_NAME);
        assert_eq!(name(b"Pad\n\x1b[31m\xff"), "Pad??[31m?");
        let full = sanitize_name(&[b'x'; NAME_LEN]);
        assert_eq!(full.len(), 79);
        // Past a NUL nothing counts.
        assert_eq!(name(b"A\0B"), "A");
    }

    #[test]
    fn events_allowed_and_refused() {
        let ok = [
            ev(0, 0, 0),
            ev(1, 0x130, 1),
            ev(1, 0x221, 0),
            ev(3, 0, -32768),
            ev(3, 0x11, 1),
            ev(0x15, 3, 1),
            ev(0x15, 0x60, 0xffff),
        ];
        assert_eq!(check_events(&ok), Ok(()));
        assert_eq!(check_events(&[]), Ok(()));
        for bad in [
            ev(1, 30, 1),     // a key
            ev(2, 0, 1),      // relative motion
            ev(4, 4, 1),      // MSC_SCAN
            ev(3, 0x28, 1),   // another axis
            ev(0x15, 16, 1),  // an effect past the limit
            ev(0x0101, 1, 1), // EV_UINPUT: the kernel's, not the client's
            ev(0x14, 0, 0),   // EV_REP
        ] {
            assert_eq!(
                check_events(&[ok[0], bad]).unwrap_err().what,
                "event",
                "{bad:?}"
            );
        }
    }

    #[test]
    fn events_per_write() {
        let batch = vec![ev(0, 0, 0); MAX_EVENTS_PER_WRITE];
        assert_eq!(check_events(&batch), Ok(()));
        let batch = vec![ev(0, 0, 0); MAX_EVENTS_PER_WRITE + 1];
        assert_eq!(check_events(&batch).unwrap_err().what, "too many events");
    }

    #[test]
    fn effects_the_client_is_asked_for() {
        assert!(effect_kind_ok(FF_RUMBLE));
        assert!(effect_kind_ok(FF_PERIODIC));
        assert!(!effect_kind_ok(0x52));
        assert!(!effect_kind_ok(0x5d));
    }

    #[test]
    fn rate_cap() {
        let mut rate = RateLimit::new();
        let t0 = Duration::from_secs(10);
        // The burst, then nothing until time passes.
        for _ in 0..(EVENT_BURST / 64) {
            assert!(rate.take(64, t0));
        }
        assert!(!rate.take(1, t0));
        // 10 ms at 20 000/s is 200 events.
        let t1 = t0 + Duration::from_millis(10);
        assert!(rate.take(64, t1));
        assert!(rate.take(64, t1));
        assert!(rate.take(64, t1));
        assert!(!rate.take(64, t1));
        // Never past the burst, however long it was idle.
        let t2 = t1 + Duration::from_secs(3600);
        assert!(rate.take(EVENT_BURST as usize, t2));
        assert!(!rate.take(1, t2));
        // A clock that steps back doesn't mint tokens.
        assert!(!rate.take(1, t0));
    }

    #[test]
    fn device_slots() {
        let slots = Slots::default();
        let held: Vec<_> = (0..MAX_DEVICES).map(|_| slots.take().unwrap()).collect();
        assert_eq!(held, (0..MAX_DEVICES).collect::<Vec<_>>());
        assert_eq!(slots.take(), None);
        slots.release(2);
        assert_eq!(slots.take(), Some(2));
        assert_eq!(slots.take(), None);
        slots.release(99);
    }
}

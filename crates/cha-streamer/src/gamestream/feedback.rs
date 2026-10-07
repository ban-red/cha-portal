//! What the environment's apps do to the pads (rumble, lightbar, adaptive
//! triggers), as Moonlight takes it.
//!
//! The pads publish every change on a broadcast; this keeps the stream of
//! them to about 60 a second per pad and kind, and ends the rumble that the
//! app gave a length, since Moonlight's rumble runs until it is told to stop.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use cha_gamestream::Feedback;
use tokio::sync::{broadcast, mpsc};

use crate::gamepad::{ENDLESS_MS, PadEvent, Side};

/// Feedback of one kind to one pad goes out at most this often (the browser
/// path's `PAD_EVENT_INTERVAL`).
const INTERVAL: Duration = Duration::from_millis(16);

/// SDL's flags for which trigger an effect is for (the DualSense output
/// report's valid-flag bits).
const RIGHT_TRIGGER: u8 = 0x04;
const LEFT_TRIGGER: u8 = 0x08;

/// Feedback that supersedes its earlier kin.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Class {
    Rumble,
    Led,
    TriggerLeft,
    TriggerRight,
}

/// What Moonlight is told for `event`; nothing for what it has no message for
/// (Steam Controller haptics, the player LEDs).
fn feedback(event: &PadEvent) -> Option<(u16, Class, Feedback)> {
    let motor = |v: f32| (v.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16;
    Some(match *event {
        PadEvent::Rumble(r) => {
            let (low, high) = if r.ms == 0 {
                (0, 0)
            } else {
                (motor(r.lo), motor(r.hi))
            };
            let pad = r.slot as u16;
            (pad, Class::Rumble, Feedback::Rumble { pad, low, high })
        }
        PadEvent::Led(l) => {
            let pad = l.slot as u16;
            (
                pad,
                Class::Led,
                Feedback::Led {
                    pad,
                    rgb: (l.r, l.g, l.b),
                },
            )
        }
        PadEvent::Trigger(t) => {
            let pad = t.slot as u16;
            // The effect is its type, then ten parameters.
            let kind = t.effect[0];
            let mut params = [0u8; 10];
            params.copy_from_slice(&t.effect[1..]);
            let (class, flags, type_left, type_right, left, right) = match t.side {
                Side::Left => (Class::TriggerLeft, LEFT_TRIGGER, kind, 0, params, [0; 10]),
                Side::Right => (Class::TriggerRight, RIGHT_TRIGGER, 0, kind, [0; 10], params),
            };
            (
                pad,
                class,
                Feedback::TriggerEffect {
                    pad,
                    event_flags: flags,
                    type_left,
                    type_right,
                    left,
                    right,
                },
            )
        }
        PadEvent::Haptic(_) | PadEvent::Players(_) => return None,
    })
}

#[derive(Default)]
struct Slot {
    sent: Option<Instant>,
    /// The newest that came in before its turn.
    waiting: Option<Feedback>,
    /// A rumble with a length: when to stop it.
    stop_at: Option<Instant>,
}

/// The rate limit and the rumble timers.
#[derive(Default)]
pub struct Limiter {
    slots: HashMap<(u16, Class), Slot>,
}

impl Limiter {
    /// The messages to send now for `event`, at `now`.
    pub fn push(&mut self, event: &PadEvent, now: Instant) -> Vec<Feedback> {
        let Some((pad, class, message)) = feedback(event) else {
            return Vec::new();
        };
        let slot = self.slots.entry((pad, class)).or_default();
        if event.is_stop() {
            // A stop never waits behind anything, and ends what was waiting.
            *slot = Slot::default();
            return vec![message];
        }
        // An effect that was given a length ends by itself.
        slot.stop_at = match event {
            PadEvent::Rumble(r) if r.ms < ENDLESS_MS => {
                Some(now + Duration::from_millis(r.ms.into()))
            }
            _ => None,
        };
        if slot.sent.is_none_or(|at| now >= at + INTERVAL) {
            slot.sent = Some(now);
            slot.waiting = None;
            vec![message]
        } else {
            slot.waiting = Some(message);
            Vec::new()
        }
    }

    /// When `poll` next has something.
    pub fn next_due(&self) -> Option<Instant> {
        self.slots
            .values()
            .flat_map(|slot| {
                let turn = slot.waiting.as_ref().and(slot.sent).map(|at| at + INTERVAL);
                turn.into_iter().chain(slot.stop_at)
            })
            .min()
    }

    /// The messages whose time has come: a waiting one that has had its turn,
    /// a rumble whose length is up.
    pub fn poll(&mut self, now: Instant) -> Vec<Feedback> {
        let mut out = Vec::new();
        for ((pad, class), slot) in &mut self.slots {
            if slot.stop_at.is_some_and(|at| now >= at) {
                *slot = Slot::default();
                if *class == Class::Rumble {
                    out.push(Feedback::Rumble {
                        pad: *pad,
                        low: 0,
                        high: 0,
                    });
                }
                continue;
            }
            if slot.sent.is_some_and(|at| now >= at + INTERVAL)
                && let Some(message) = slot.waiting.take()
            {
                slot.sent = Some(now);
                out.push(message);
            }
        }
        out
    }
}

/// Sends what the pads' apps do to `tx` until it closes. While `has_control`
/// says no, nothing but stops goes out: only the controlling client feels
/// the apps' rumble. `replay` is what the apps set before (the lightbar,
/// trigger effects).
pub async fn run(
    mut events: broadcast::Receiver<PadEvent>,
    tx: mpsc::Sender<Feedback>,
    has_control: impl Fn() -> bool,
    replay: Vec<PadEvent>,
) {
    let mut limiter = Limiter::default();
    for event in &replay {
        for message in limiter.push(event, Instant::now()) {
            if tx.send(message).await.is_err() {
                return;
            }
        }
    }
    loop {
        let due = limiter.next_due();
        let out = tokio::select! {
            got = events.recv() => match got {
                Ok(event) if has_control() || event.is_stop() => limiter.push(&event, Instant::now()),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => Vec::new(),
                Err(broadcast::error::RecvError::Closed) => return,
            },
            () = tokio::time::sleep_until(due.unwrap_or_else(Instant::now).into()), if due.is_some() => {
                limiter.poll(Instant::now())
            }
            () = tx.closed() => return,
        };
        for message in out {
            if tx.send(message).await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gamepad::{Haptic, Led, Players, Rumble, Trigger};

    fn rumble(slot: usize, lo: f32, hi: f32, ms: u32) -> PadEvent {
        PadEvent::Rumble(Rumble { slot, lo, hi, ms })
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn rumble_maps_motors_to_16_bits() {
        let mut limiter = Limiter::default();
        let t = Instant::now();
        assert_eq!(
            limiter.push(&rumble(1, 1.0, 0.5, ENDLESS_MS), t),
            [Feedback::Rumble {
                pad: 1,
                low: 65535,
                high: 32768
            }]
        );
        // The strong motor is Moonlight's low-frequency one.
        assert_eq!(
            limiter.push(&rumble(0, 0.0, 1.0, ENDLESS_MS), t),
            [Feedback::Rumble {
                pad: 0,
                low: 0,
                high: 65535
            }]
        );
    }

    #[test]
    fn a_stop_goes_at_once_and_clears_what_waited() {
        let mut limiter = Limiter::default();
        let t = Instant::now();
        assert_eq!(limiter.push(&rumble(0, 1.0, 1.0, ENDLESS_MS), t).len(), 1);
        assert!(
            limiter
                .push(&rumble(0, 0.5, 0.5, ENDLESS_MS), t + ms(1))
                .is_empty()
        );
        assert_eq!(
            limiter.push(&rumble(0, 0.7, 0.7, 0), t + ms(2)),
            [Feedback::Rumble {
                pad: 0,
                low: 0,
                high: 0
            }]
        );
        assert_eq!(limiter.next_due(), None, "nothing waits any more");
        assert!(limiter.poll(t + ms(100)).is_empty());
    }

    #[test]
    fn a_burst_sends_the_first_and_the_newest() {
        let mut limiter = Limiter::default();
        let t = Instant::now();
        let mut sent = limiter.push(&rumble(0, 0.1, 0.1, ENDLESS_MS), t);
        for i in 1..10 {
            sent.extend(limiter.push(
                &rumble(0, 0.1 + i as f32 / 20.0, 0.0, ENDLESS_MS),
                t + ms(i),
            ));
        }
        assert_eq!(sent.len(), 1, "the rest wait for their turn");
        assert_eq!(limiter.next_due(), Some(t + INTERVAL));
        assert!(limiter.poll(t + ms(10)).is_empty(), "not yet");
        let late = limiter.poll(t + INTERVAL);
        assert_eq!(
            late,
            [Feedback::Rumble {
                pad: 0,
                low: (0.55f32 * 65535.0).round() as u16,
                high: 0
            }],
            "the newest only"
        );
        assert_eq!(limiter.next_due(), None);
        // Another pad is limited on its own.
        assert_eq!(
            limiter
                .push(&rumble(1, 1.0, 1.0, ENDLESS_MS), t + ms(5))
                .len(),
            1
        );
        // A different kind on the same pad too.
        let led = PadEvent::Led(Led {
            slot: 0,
            r: 1,
            g: 2,
            b: 3,
        });
        assert_eq!(
            limiter.push(&led, t + ms(5)),
            [Feedback::Led {
                pad: 0,
                rgb: (1, 2, 3)
            }]
        );
    }

    #[test]
    fn a_rumble_with_a_length_stops_by_itself() {
        let mut limiter = Limiter::default();
        let t = Instant::now();
        assert_eq!(limiter.push(&rumble(2, 1.0, 1.0, 200), t).len(), 1);
        assert_eq!(limiter.next_due(), Some(t + ms(200)));
        assert!(limiter.poll(t + ms(199)).is_empty());
        assert_eq!(
            limiter.poll(t + ms(200)),
            [Feedback::Rumble {
                pad: 2,
                low: 0,
                high: 0
            }]
        );
        assert_eq!(limiter.next_due(), None);
        // A new rumble replaces the old one's timer.
        limiter.push(&rumble(2, 1.0, 1.0, 100), t + ms(300));
        limiter.push(&rumble(2, 1.0, 1.0, ENDLESS_MS), t + ms(320));
        assert_eq!(limiter.next_due(), None, "endless has no end of its own");
    }

    #[test]
    fn triggers_name_their_side() {
        let mut limiter = Limiter::default();
        let t = Instant::now();
        let mut effect = [0u8; 11];
        effect[0] = 0x21;
        effect[1] = 7;
        effect[10] = 9;
        let left = PadEvent::Trigger(Trigger {
            slot: 0,
            side: Side::Left,
            effect,
        });
        let right = PadEvent::Trigger(Trigger {
            slot: 0,
            side: Side::Right,
            effect,
        });
        let mut params = [0u8; 10];
        params[0] = 7;
        params[9] = 9;
        assert_eq!(
            limiter.push(&left, t),
            [Feedback::TriggerEffect {
                pad: 0,
                event_flags: LEFT_TRIGGER,
                type_left: 0x21,
                type_right: 0,
                left: params,
                right: [0; 10]
            }]
        );
        assert_eq!(
            limiter.push(&right, t),
            [Feedback::TriggerEffect {
                pad: 0,
                event_flags: RIGHT_TRIGGER,
                type_left: 0,
                type_right: 0x21,
                left: [0; 10],
                right: params
            }],
            "its own limit"
        );
    }

    #[test]
    fn what_moonlight_has_no_message_for_is_dropped() {
        let mut limiter = Limiter::default();
        let t = Instant::now();
        let players = PadEvent::Players(Players { slot: 0, mask: 1 });
        let haptic = PadEvent::Haptic(Haptic {
            slot: 0,
            side: Side::Left,
            amp: 1.0,
            on_us: 1,
            off_us: 1,
            count: 1,
        });
        assert!(limiter.push(&players, t).is_empty());
        assert!(limiter.push(&haptic, t).is_empty());
    }

    #[tokio::test]
    async fn the_task_sends_while_the_client_has_control_and_stops_regardless() {
        let (events, rx) = broadcast::channel(16);
        let (tx, mut out) = mpsc::channel(16);
        let control = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = control.clone();
        let task = tokio::spawn(run(
            rx,
            tx,
            move || gate.load(std::sync::atomic::Ordering::Relaxed),
            vec![PadEvent::Led(Led {
                slot: 0,
                r: 9,
                g: 8,
                b: 7,
            })],
        ));
        assert_eq!(
            out.recv().await,
            Some(Feedback::Led {
                pad: 0,
                rgb: (9, 8, 7)
            }),
            "what the app set before"
        );
        events.send(rumble(0, 1.0, 1.0, ENDLESS_MS)).unwrap();
        assert!(matches!(
            out.recv().await,
            Some(Feedback::Rumble { low: 65535, .. })
        ));
        control.store(false, std::sync::atomic::Ordering::Relaxed);
        events.send(rumble(1, 1.0, 1.0, ENDLESS_MS)).unwrap();
        events.send(rumble(0, 0.0, 0.0, 0)).unwrap();
        assert_eq!(
            out.recv().await,
            Some(Feedback::Rumble {
                pad: 0,
                low: 0,
                high: 0
            }),
            "the other client's rumble never came, the stop did"
        );
        drop(out);
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("the task ends with its receiver")
            .unwrap();
    }
}

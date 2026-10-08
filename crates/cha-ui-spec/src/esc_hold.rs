//! Hold Esc to let go of exclusive input, as pure state with the time passed in.
//!
//! While the pointer is captured, Esc goes to the host at once, as every key does. Held for
//! `hold_ms` (`toolbar.json`'s `timing.release_hold_ms`) it also releases the pointer: Esc's
//! key-up goes to the host then, and the user's real key-up is swallowed. After `hint_ms` of the
//! hold a progress bar tells them to keep holding.
//!
//! The same rules run in `web/packages/player/src/escHold.ts`; both pass
//! `web/packages/ui-spec/capture-cases.json`.
//!
//! ```text
//! idle --down--> holding --tick >= hold_ms--> released (key-up swallowed) --down--> holding
//!                   |
//!                   +--up / blur / uncapture--> idle
//! ```

/// An Esc key event for the host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKey {
    Down,
    Up,
}

/// What an event did: the Esc key event to send to the host, and whether to release the pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Step {
    pub host: Option<HostKey>,
    /// Let go of the pointer and the capture, as the toolbar's exclusive-input toggle does.
    pub release: bool,
}

const NONE: Step = Step {
    host: None,
    release: false,
};

fn host(key: HostKey) -> Step {
    Step {
        host: Some(key),
        release: false,
    }
}

#[derive(Clone, Debug)]
pub struct EscHold {
    hold_ms: u64,
    hint_ms: u64,
    down_at: Option<u64>,
    /// The next key-up is ours: it ends a hold that released, or a press a blur cancelled.
    swallow_up: bool,
}

impl EscHold {
    pub fn new(hold_ms: u64, hint_ms: u64) -> Self {
        Self {
            hold_ms,
            hint_ms,
            down_at: None,
            swallow_up: false,
        }
    }

    /// Built from the spec's timing.
    pub fn from_spec() -> Self {
        let t = &crate::toolbar::spec().timing;
        Self::new(t.release_hold_ms, t.release_hint_ms)
    }

    /// Esc is down and nothing has released yet.
    pub fn holding(&self) -> bool {
        self.down_at.is_some()
    }

    /// An Esc key-down at `now` (ms). `captured`: the pointer is captured, so a hold counts.
    /// Repeats of a held key are dropped (the host repeats keys itself).
    pub fn key_down(&mut self, now: u64, repeat: bool, captured: bool) -> Step {
        if repeat {
            return NONE;
        }
        self.swallow_up = false;
        if !captured {
            self.down_at = None;
            return host(HostKey::Down);
        }
        if self.down_at.is_some() {
            return NONE;
        }
        self.down_at = Some(now);
        host(HostKey::Down)
    }

    pub fn key_up(&mut self, now: u64) -> Step {
        // The timer may have been late: a hold that lasted long enough releases.
        let fired = self.tick(now);
        if fired.release {
            return fired;
        }
        if self.down_at.take().is_some() {
            return host(HostKey::Up);
        }
        if std::mem::take(&mut self.swallow_up) {
            return NONE;
        }
        host(HostKey::Up)
    }

    /// Time passed (the timer, or a redraw).
    pub fn tick(&mut self, now: u64) -> Step {
        match self.down_at {
            Some(at) if now.saturating_sub(at) >= self.hold_ms => {
                self.down_at = None;
                self.swallow_up = true;
                Step {
                    host: Some(HostKey::Up),
                    release: true,
                }
            }
            _ => NONE,
        }
    }

    /// The window lost focus. The host gets Esc's key-up now; the real one is dropped.
    pub fn blur(&mut self) -> Step {
        if self.down_at.take().is_none() {
            return NONE;
        }
        self.swallow_up = true;
        host(HostKey::Up)
    }

    /// The pointer was released some other way: the hold is moot, and the key-up goes on normally.
    pub fn uncapture(&mut self) {
        self.down_at = None;
        self.swallow_up = false;
    }

    /// The hold's progress, 0..1, once the hint is due; `None` while it is hidden.
    pub fn hint(&self, now: u64) -> Option<f32> {
        let at = self.down_at?;
        let held = now.saturating_sub(at);
        (held >= self.hint_ms).then(|| (held as f32 / self.hold_ms as f32).min(1.0))
    }

    /// When the hold fires, in the caller's clock; `None` when not holding.
    pub fn due(&self) -> Option<u64> {
        self.down_at.map(|at| at + self.hold_ms)
    }

    /// When the hint is due; `None` when not holding.
    pub fn hint_due(&self) -> Option<u64> {
        self.down_at.map(|at| at + self.hint_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_when_the_hint_and_the_release_are_due() {
        let t = &crate::toolbar::spec().timing;
        let mut esc = EscHold::from_spec();
        assert_eq!((esc.hint_due(), esc.due()), (None, None));
        esc.key_down(5000, false, true);
        assert!(esc.holding());
        assert_eq!(esc.hint_due(), Some(5000 + t.release_hint_ms));
        assert_eq!(esc.due(), Some(5000 + t.release_hold_ms));
        esc.key_up(5100);
        assert!(!esc.holding());
        assert_eq!(esc.due(), None);
    }

    #[test]
    fn the_hints_progress_never_goes_past_one() {
        let mut esc = EscHold::from_spec();
        esc.key_down(0, false, true);
        assert_eq!(esc.hint(10_000), Some(1.0));
    }
}

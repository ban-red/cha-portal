//! When the compositor composites (docs/plans/hdr-and-vrr.md, step 3.1).
//!
//! - [`Pacing::Fixed`]: on the encode tick, every `period`, if anything
//!   changed. What every session did before, and what desktops keep: Chrome
//!   paces itself from the presentation feedback of the 240 Hz tick.
//! - [`Pacing::Commit`]: when the surface that drives the picture (gamescope's
//!   window, in a Steam session) commits a change, at most once a `period`.
//!   A game at or below the cap is composited the moment its frame lands, so
//!   it no longer waits for a clock of its own rate (research 12: about a frame
//!   on average with two such clocks in a row).
//!
//! Commit pacing follows Sunshine's capture budget: after a composite, the
//! next may come at `max(next, now - period / 4) + period`, so a frame that
//! was late by up to a quarter of a period doesn't push every later one back.
//! Pure, with the clock passed in, so every rule is a unit test.

use std::time::{Duration, Instant};

/// How the compositor decides when to composite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pacing {
    Fixed,
    Commit,
}

/// What a wake-up should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Nothing to composite yet.
    Idle,
    /// Composite now (in `Fixed`: an encode tick, where the compositor's own
    /// dirty and forced flags decide).
    Composite,
    /// Nothing changed for the keep-alive's interval: send the last picture
    /// again (for clients that expect a steady stream).
    KeepAlive,
}

#[derive(Debug)]
pub struct Schedule {
    pacing: Pacing,
    period: Duration,
    /// `Fixed`: the next encode tick. `Commit`: the earliest the next
    /// composite may come.
    next: Instant,
    /// `Commit`: a change committed and not yet composited.
    pending: bool,
    /// `Commit`: when the last composite (or keep-alive) went out.
    last: Option<Instant>,
    /// `Commit`: send the last picture again after this many periods with
    /// nothing new.
    keepalive: Option<u32>,
}

impl Schedule {
    pub fn new(pacing: Pacing, period: Duration, now: Instant) -> Self {
        Self {
            pacing,
            period,
            next: now,
            pending: false,
            last: None,
            keepalive: None,
        }
    }

    pub fn pacing(&self) -> Pacing {
        self.pacing
    }

    /// Switches pacing (a gamescope window mapped or went away). Nothing
    /// waiting is lost: a pending change composites at the next chance.
    pub fn set_pacing(&mut self, pacing: Pacing, now: Instant) {
        if pacing != self.pacing {
            self.pacing = pacing;
            self.next = now;
            self.pending = true;
        }
    }

    /// A new frame interval (the stream's rate changed), from now on.
    pub fn set_period(&mut self, period: Duration, now: Instant) {
        self.period = period;
        self.next = now;
    }

    pub fn period(&self) -> Duration {
        self.period
    }

    /// `Commit`: send the last picture again after `periods` frame intervals
    /// with nothing new (2: half the stream's rate, as Sunshine does).
    pub fn set_keepalive(&mut self, periods: Option<u32>) {
        self.keepalive = periods.filter(|n| *n > 0);
    }

    fn keepalive(&self) -> Option<Duration> {
        self.keepalive.map(|n| self.period * n)
    }

    /// `Commit`: the driving surface committed. A commit with no damage (a
    /// repeat of the same picture) isn't new content.
    pub fn on_commit(&mut self, damaged: bool) {
        if damaged {
            self.pending = true;
        }
    }

    /// `Commit`: when to wake up next, if at all (`None`: not until a commit).
    pub fn due(&self, now: Instant) -> Option<Instant> {
        if self.pacing == Pacing::Fixed {
            return Some(self.next);
        }
        if self.pending {
            return Some(self.next.max(now));
        }
        let ka = self.keepalive()?;
        Some(self.last.map_or(now, |last| last + ka).max(self.next))
    }

    /// A wake-up at `now`: whether to composite, and why. In `Commit`, a
    /// composite is taken as done (call [`Schedule::on_commit`] again if it
    /// couldn't happen).
    pub fn poll(&mut self, now: Instant) -> Wake {
        match self.pacing {
            Pacing::Fixed => {
                // The compositor's rule before this module: an encode tick
                // when the clock is within an eighth of a period of it.
                if now + self.period / 8 < self.next {
                    return Wake::Idle;
                }
                self.next += self.period;
                if self.next < now {
                    // Fell behind (a stall): realign instead of bursting.
                    self.next = now + self.period;
                }
                Wake::Composite
            }
            Pacing::Commit => {
                // A timerfd wakes within microseconds; allow a sixteenth.
                let ready = now + self.period / 16 >= self.next;
                if self.pending && ready {
                    self.pending = false;
                    self.sent(now);
                    return Wake::Composite;
                }
                if let (Some(ka), false) = (self.keepalive(), self.pending)
                    && ready
                    && self.last.is_none_or(|last| now >= last + ka)
                {
                    self.sent(now);
                    return Wake::KeepAlive;
                }
                Wake::Idle
            }
        }
    }

    fn sent(&mut self, now: Instant) {
        let floor = now.checked_sub(self.period / 4).unwrap_or(now);
        self.next = self.next.max(floor) + self.period;
        self.last = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn period(fps: u32) -> Duration {
        Duration::from_secs(1) / fps
    }

    /// Runs a commit-paced schedule against a game that commits at `game`
    /// fps, waking at every commit and at every due time, for `secs`. Returns
    /// each composite's time and how long its frame had waited.
    fn run(cap: u32, game_interval: Duration, secs: u64) -> Vec<(Duration, Duration)> {
        let t0 = Instant::now();
        let mut s = Schedule::new(Pacing::Commit, period(cap), t0);
        let end = t0 + Duration::from_secs(secs);
        let mut next_commit = t0 + game_interval;
        let mut committed_at: Option<Instant> = None;
        let mut out = Vec::new();
        let mut now = t0;
        while now < end {
            let due = s.due(now).filter(|d| *d > now);
            // The next event: the game's commit or the schedule's wake.
            let wake = due.map_or(next_commit, |d| d.min(next_commit));
            now = wake;
            if now >= next_commit {
                s.on_commit(true);
                committed_at.get_or_insert(now);
                next_commit += game_interval;
            }
            if s.poll(now) == Wake::Composite {
                let waited = now - committed_at.take().unwrap();
                out.push((now - t0, waited));
            }
        }
        out
    }

    #[test]
    fn a_game_at_or_below_the_cap_is_composited_the_moment_it_commits() {
        for game_fps in [60.0, 45.0, 37.0] {
            let interval = Duration::from_secs_f64(1.0 / game_fps);
            let composites = run(60, interval, 2);
            // One composite per game frame, none waiting.
            assert!(
                (composites.len() as f64 - 2.0 * game_fps).abs() <= 1.0,
                "{game_fps} fps: {} composites",
                composites.len()
            );
            assert!(
                composites.iter().all(|(_, waited)| waited.is_zero()),
                "{game_fps} fps waited"
            );
        }
    }

    #[test]
    fn a_game_above_the_cap_is_held_to_it() {
        // 240 fps into a 60 fps cap.
        let composites = run(60, period(240), 2);
        assert!(
            (118..=121).contains(&composites.len()),
            "{}",
            composites.len()
        );
        // Never two composites closer than three quarters of the period
        // (the late-frame allowance), and the newest frame waits at most one.
        for pair in composites.windows(2) {
            assert!(pair[1].0 - pair[0].0 >= period(60) * 3 / 4 - MS);
        }
        assert!(composites.iter().all(|(_, w)| *w <= period(60)));
    }

    #[test]
    fn a_late_commit_composites_at_once_and_the_next_one_period_later() {
        let t0 = Instant::now();
        let p = period(60);
        let mut s = Schedule::new(Pacing::Commit, p, t0);
        s.on_commit(true);
        assert_eq!(s.poll(t0), Wake::Composite);
        // The next frame is late by 10 ms: it goes out as it lands.
        let late = t0 + p + 10 * MS;
        assert_eq!(s.due(late), None, "nothing waits before it lands");
        s.on_commit(true);
        assert_eq!(s.due(late), Some(late));
        assert_eq!(s.poll(late), Wake::Composite);
        // The one after may come a period after that, less the quarter
        // period of allowance, not sooner.
        s.on_commit(true);
        let next = s.due(late).unwrap();
        assert!(
            next >= late + p * 3 / 4 && next <= late + p,
            "{:?}",
            next - late
        );
        assert_eq!(s.poll(late + p / 2), Wake::Idle);
    }

    #[test]
    fn an_idle_session_sends_nothing_unless_kept_alive() {
        let t0 = Instant::now();
        let mut s = Schedule::new(Pacing::Commit, period(60), t0);
        assert_eq!(s.due(t0), None);
        assert_eq!(s.poll(t0 + Duration::from_secs(5)), Wake::Idle);
        // A commit with no damage is a repeat, not content.
        s.on_commit(false);
        assert_eq!(s.due(t0), None);

        // With a keep-alive (the GameStream host's clients), the last
        // picture goes out again every two periods while nothing changes.
        s.set_keepalive(Some(2));
        let mut sent = 0;
        let mut now = t0;
        for _ in 0..30 {
            now = s.due(now).unwrap().max(now);
            if s.poll(now) == Wake::KeepAlive {
                sent += 1;
            }
            now += MS;
        }
        assert!((29..=30).contains(&sent), "{sent}");
        // Content takes over from the keep-alive at once.
        s.on_commit(true);
        let due = s.due(now).unwrap();
        assert_eq!(s.poll(due.max(now)), Wake::Composite);
    }

    #[test]
    fn fixed_pacing_ticks_as_the_compositor_did() {
        // The 240 Hz clock (270 for 90 fps), and an encode tick every
        // `tick / fps` ticks: 60, 90 and 120 a second.
        for (fps, tick_hz) in [(60, 240), (90, 270), (120, 240)] {
            let t0 = Instant::now();
            let mut s = Schedule::new(Pacing::Fixed, period(fps), t0);
            let tick = period(tick_hz);
            let encodes: Vec<u32> = (0..tick_hz)
                .filter(|n| s.poll(t0 + tick * *n) == Wake::Composite)
                .collect();
            assert_eq!(encodes.len() as u32, fps, "{fps} fps");
            let every = tick_hz / fps;
            assert!(
                encodes.iter().all(|n| n % every == 0),
                "{fps} fps on {tick_hz} Hz: {encodes:?}"
            );
        }
    }

    #[test]
    fn fixed_pacing_realigns_after_a_stall() {
        let t0 = Instant::now();
        let p = period(60);
        let mut s = Schedule::new(Pacing::Fixed, p, t0);
        assert_eq!(s.poll(t0), Wake::Composite);
        // A 200 ms stall: one encode tick, then the next a period later,
        // not a burst of the twelve missed.
        let after = t0 + 200 * MS;
        assert_eq!(s.poll(after), Wake::Composite);
        assert_eq!(s.poll(after + MS), Wake::Idle);
        assert_eq!(s.poll(after + p), Wake::Composite);
    }

    #[test]
    fn switching_pacing_keeps_what_was_waiting() {
        let t0 = Instant::now();
        let mut s = Schedule::new(Pacing::Fixed, period(60), t0);
        s.set_pacing(Pacing::Commit, t0);
        assert_eq!(s.pacing(), Pacing::Commit);
        assert_eq!(s.poll(t0), Wake::Composite);
        s.set_period(period(120), t0);
        assert_eq!(s.period(), period(120));
    }
}

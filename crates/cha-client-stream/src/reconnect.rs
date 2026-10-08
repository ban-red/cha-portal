//! Reconnecting (`docs/plans/c2-transport.md` section 9): when the connection
//! ends for a reason other than the player stopping it or the streamer
//! closing on purpose, the session task connects again inside the same
//! [`cha_client::Session`], as the browser does.
//!
//! - [`Backoff`]: the browser's schedule (`web/apps/portal/src/reconnect.ts`),
//!   1, 2, 4, 8 and then 15 s for as long as it takes.
//! - [`Refresh`]: the caller's answer when asked for a fresh [`Target`]. Every
//!   connect needs a new media token from the portal's `/connect` (valid 60 s,
//!   checked once, at session start), so the crate can't reuse the first one;
//!   [`Refresh::from_status`] sorts the portal's answers into "try again
//!   later" and "give up" as `classifyConnectError` does.
//! - [`reacquire`]: the wait-refresh-connect loop, generic over the connect
//!   step and driven by tokio's clock, so tests run it with paused time.

use std::fmt;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use cha_client::BoxFuture;
use tokio::sync::mpsc;
use tracing::info;

use crate::session::{Command, Target};

/// Waits before each automatic reconnect: 1, 2, 4, 8, then 15 s.
pub const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(15),
];

/// The wait before attempt `attempt` (1 is the first retry after a drop).
pub fn backoff_delay(attempt: u32) -> Duration {
    let i = attempt.max(1) as usize - 1;
    BACKOFF[i.min(BACKOFF.len() - 1)]
}

/// Why no fresh [`Target`] came.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refresh {
    /// Transient (network, 5xx, 408/429, an environment still coming back):
    /// wait and try again, for as long as it takes.
    Retry(String),
    /// Retrying won't help (signed out, the environment is gone, any other
    /// refusal): the session ends with this reason.
    GiveUp(String),
}

impl Refresh {
    /// Sorts an HTTP answer from the portal's `/connect` as the browser does
    /// (`classifyConnectError`): 401 and every other 4xx give up, except 408
    /// and 429; 409 `not_running` waits for the environment to return; 5xx
    /// retries. `message` is what the portal said, empty if nothing.
    pub fn from_status(status: u16, code: &str, message: &str) -> Self {
        let said = if message.is_empty() {
            format!("the portal answered {status}")
        } else {
            message.to_string()
        };
        match status {
            401 => Self::GiveUp("signed out".into()),
            409 if code == "not_running" => Self::Retry(said),
            408 | 429 => Self::Retry(said),
            s if s >= 500 => Self::Retry(said),
            s if s >= 400 => Self::GiveUp(said),
            _ => Self::Retry(said),
        }
    }
}

impl fmt::Display for Refresh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Retry(m) | Self::GiveUp(m) => f.write_str(m),
        }
    }
}

/// Gets a fresh [`Target`] (a new media token) for a reconnect.
pub type RefreshFn = Arc<dyn Fn() -> BoxFuture<'static, Result<Target, Refresh>> + Send + Sync>;

/// How [`reacquire`] ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Reacquired<T> {
    Up(T),
    /// The player stopped the session while it waited.
    Stopped,
    /// `refresh` said not to try again.
    GiveUp(String),
}

/// Waits for a stop command (or the player going away).
async fn stopped(commands: &mut mpsc::UnboundedReceiver<Command>) {
    match commands.recv().await {
        Some(Command::Stop) | None => {}
    }
}

/// Connects again: waits [`backoff_delay`], asks `refresh` for a target and
/// runs `connect` on it, until one succeeds, `refresh` gives up or the
/// player stops the session. `attempts` counts attempts since the stream was
/// last up (the caller resets it when it is); `announce` hears each attempt
/// before its wait, with the attempt number, the delay and the reason the
/// last try failed.
pub(crate) async fn reacquire<T, C, Fut>(
    refresh: &RefreshFn,
    mut connect: C,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    reason: String,
    attempts: &mut u32,
    mut announce: impl FnMut(u32, Duration, &str),
) -> Reacquired<T>
where
    C: FnMut(Target) -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut reason = reason;
    loop {
        *attempts += 1;
        let delay = backoff_delay(*attempts);
        announce(*attempts, delay, &reason);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = stopped(commands) => return Reacquired::Stopped,
        }
        let target = tokio::select! {
            r = refresh() => r,
            _ = stopped(commands) => return Reacquired::Stopped,
        };
        let target = match target {
            Ok(t) => t,
            Err(Refresh::GiveUp(why)) => return Reacquired::GiveUp(why),
            Err(Refresh::Retry(why)) => {
                info!(attempt = *attempts, %why, "no fresh target yet");
                reason = why;
                continue;
            }
        };
        tokio::select! {
            r = connect(target) => match r {
                Ok(up) => return Reacquired::Up(up),
                Err(e) => {
                    reason = format!("{e:#}");
                    info!(attempt = *attempts, %reason, "reconnect attempt failed");
                }
            },
            _ = stopped(commands) => return Reacquired::Stopped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use tokio::time::Instant;

    fn target() -> Target {
        Target {
            urls: vec![],
            cert_hash: String::new(),
            codec: cha_client::Codec::Hevc,
        }
    }

    #[test]
    fn the_schedule_is_the_browsers() {
        let secs: Vec<u64> = (1..=8).map(|n| backoff_delay(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 15, 15, 15, 15]);
        // Attempt 0 is not a thing; it gets the first wait.
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(u32::MAX), Duration::from_secs(15));
    }

    #[test]
    fn portal_answers_are_sorted_as_the_browser_does() {
        use Refresh::*;
        let c = Refresh::from_status;
        assert_eq!(c(401, "", ""), GiveUp("signed out".into()));
        assert!(matches!(c(404, "not_found", "gone"), GiveUp(m) if m == "gone"));
        assert!(matches!(c(410, "", ""), GiveUp(_)));
        assert!(matches!(c(400, "bad_codec", "no"), GiveUp(_)));
        assert!(matches!(c(403, "", ""), GiveUp(_)));
        assert!(matches!(c(409, "no_webtransport", "x"), GiveUp(_)));
        assert!(matches!(c(409, "no_node", "x"), GiveUp(_)));
        assert!(matches!(c(409, "not_running", "starting"), Retry(m) if m == "starting"));
        assert!(matches!(c(408, "", ""), Retry(_)));
        assert!(matches!(c(429, "", ""), Retry(_)));
        assert!(matches!(c(500, "", ""), Retry(_)));
        assert!(matches!(c(503, "", ""), Retry(m) if m.contains("503")));
    }

    fn refresh_with(
        answers: Vec<Result<Target, Refresh>>,
        asked: Arc<Mutex<Vec<Instant>>>,
    ) -> RefreshFn {
        let answers = Arc::new(Mutex::new(answers.into_iter()));
        Arc::new(move || {
            asked.lock().unwrap().push(Instant::now());
            let next = answers
                .lock()
                .unwrap()
                .next()
                .unwrap_or_else(|| Ok(target()));
            Box::pin(async move { next })
        })
    }

    #[tokio::test(start_paused = true)]
    async fn waits_follow_the_schedule_until_a_connect_works() {
        let begin = Instant::now();
        let asked = Arc::new(Mutex::new(Vec::new()));
        // Two refreshes that fail, two connects that fail, then it works.
        let refresh = refresh_with(
            vec![
                Err(Refresh::Retry("portal down".into())),
                Err(Refresh::Retry("not_running".into())),
            ],
            asked.clone(),
        );
        let (_tx, mut commands) = mpsc::unbounded_channel();
        let tries = Arc::new(Mutex::new(0));
        let connect = {
            let tries = tries.clone();
            move |_t: Target| {
                let n = {
                    let mut t = tries.lock().unwrap();
                    *t += 1;
                    *t
                };
                async move {
                    if n < 3 {
                        anyhow::bail!("refused {n}")
                    }
                    Ok(n)
                }
            }
        };
        let mut attempts = 0;
        let mut heard = Vec::new();
        let r = reacquire(
            &refresh,
            connect,
            &mut commands,
            "dropped".into(),
            &mut attempts,
            |n, d, why| heard.push((n, d.as_secs(), why.to_string())),
        )
        .await;
        assert_eq!(r, Reacquired::Up(3));
        assert_eq!(attempts, 5);
        // Each wait precedes its refresh: 1, 2, 4, 8, 15 s after the last.
        let at: Vec<u64> = asked
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.duration_since(begin).as_secs())
            .collect();
        assert_eq!(at, [1, 3, 7, 15, 30]);
        assert_eq!(
            heard,
            [
                (1, 1, "dropped".to_string()),
                (2, 2, "portal down".to_string()),
                (3, 4, "not_running".to_string()),
                (4, 8, "refused 1".to_string()),
                (5, 15, "refused 2".to_string()),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_refusal_ends_it_and_a_stop_ends_the_wait() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let refresh = refresh_with(vec![Err(Refresh::GiveUp("signed out".into()))], asked);
        let (tx, mut commands) = mpsc::unbounded_channel();
        let mut attempts = 0;
        let r: Reacquired<()> = reacquire(
            &refresh,
            |_t| async { Ok(()) },
            &mut commands,
            "x".into(),
            &mut attempts,
            |_, _, _| {},
        )
        .await;
        assert_eq!(r, Reacquired::GiveUp("signed out".into()));

        // A stop in the middle of the 1 s wait returns at once.
        let before = Instant::now();
        tx.send(Command::Stop).unwrap();
        let refresh = refresh_with(vec![], Arc::new(Mutex::new(Vec::new())));
        let r: Reacquired<()> = reacquire(
            &refresh,
            |_t| async { Ok(()) },
            &mut commands,
            "x".into(),
            &mut attempts,
            |_, _, _| {},
        )
        .await;
        assert_eq!(r, Reacquired::Stopped);
        assert!(before.elapsed() < Duration::from_millis(10));
    }
}

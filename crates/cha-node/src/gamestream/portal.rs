//! The host's questions to the portal (a launch, a stop) and the answers.
//!
//! A request goes out as [`ToPortal::Request`] on the host's outgoing channel,
//! which only a connection to a portal that reads the `GameStream*` messages
//! listens to; with none connected there is nobody to ask, and the ask says
//! so at once. The connection hands the portal's [`ToNode::Response`] back
//! through [`PortalLink::answer`].

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use cha_wire::{PortalRequest, PortalResponse, ToPortal};
use tokio::sync::{broadcast, oneshot};

type Answer = Result<PortalResponse, String>;

/// Why a request got no answer, or what the portal said instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    NotConnected,
    NoAnswer,
    /// The portal refused or failed, in words for the user.
    Said(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConnected => f.write_str(
                "this node isn't connected to the portal (or the portal is too old for this)",
            ),
            Self::NoAnswer => f.write_str("the portal didn't answer in time"),
            Self::Said(said) => f.write_str(said),
        }
    }
}

pub struct PortalLink {
    out: broadcast::Sender<ToPortal>,
    next: AtomicU64,
    /// The portal answers launches and stops. Set by each welcome.
    launches: AtomicBool,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Answer>>>,
}

impl PortalLink {
    pub fn new(out: broadcast::Sender<ToPortal>) -> Self {
        Self {
            out,
            next: AtomicU64::new(1),
            launches: AtomicBool::new(false),
            waiting: Mutex::default(),
        }
    }

    pub fn set_launches(&self, on: bool) {
        self.launches.store(on, Ordering::Relaxed);
    }

    /// Whether the portal takes launches and stops; a portal from before
    /// them drops the connection over the request.
    pub fn launches(&self) -> bool {
        self.launches.load(Ordering::Relaxed)
    }

    /// Asks the portal and waits up to `wait` for its answer.
    pub async fn ask(
        &self,
        request: PortalRequest,
        wait: Duration,
    ) -> Result<PortalResponse, Failure> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().expect("waiting lock").insert(id, tx);
        // Dropped with the guard, so a request nobody waits for any more
        // (its caller gave up, or timed out) leaves nothing behind.
        let _forget = Forget(self, id);
        if self.out.send(ToPortal::Request { id, request }).is_err() {
            return Err(Failure::NotConnected);
        }
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(Ok(response))) => Ok(response),
            Ok(Ok(Err(said))) => Err(Failure::Said(said)),
            // The answer's sender went (the connection was reset).
            Ok(Err(_)) => Err(Failure::NotConnected),
            Err(_) => Err(Failure::NoAnswer),
        }
    }

    /// The portal answered request `id`. One nobody waits for is ignored.
    pub fn answer(&self, id: u64, result: Answer) {
        if let Some(tx) = self.waiting.lock().expect("waiting lock").remove(&id) {
            let _ = tx.send(result);
        }
    }
}

struct Forget<'a>(&'a PortalLink, u64);

impl Drop for Forget<'_> {
    fn drop(&mut self) {
        self.0.waiting.lock().expect("waiting lock").remove(&self.1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listening() -> (PortalLink, broadcast::Receiver<ToPortal>) {
        let (out, rx) = broadcast::channel(8);
        (PortalLink::new(out), rx)
    }

    fn launch() -> PortalRequest {
        PortalRequest::GameStreamLaunch {
            user_id: "bob".into(),
            template_id: "chrome".into(),
        }
    }

    #[tokio::test]
    async fn an_answer_reaches_the_request_it_belongs_to() {
        let (link, mut rx) = listening();
        let link = std::sync::Arc::new(link);
        let portal = link.clone();
        tokio::spawn(async move {
            while let Ok(ToPortal::Request { id, .. }) = rx.recv().await {
                // Answers out of order don't matter: ids pair them.
                portal.answer(id + 100, Ok(PortalResponse::GameStreamStopped));
                portal.answer(id, Ok(PortalResponse::GameStreamStopped));
            }
        });
        for _ in 0..2 {
            let answer = link.ask(launch(), Duration::from_secs(5)).await;
            assert!(matches!(answer, Ok(PortalResponse::GameStreamStopped)));
        }
    }

    #[tokio::test]
    async fn the_portals_refusal_is_what_the_caller_hears() {
        let (link, mut rx) = listening();
        let link = std::sync::Arc::new(link);
        let portal = link.clone();
        tokio::spawn(async move {
            if let Ok(ToPortal::Request { id, .. }) = rx.recv().await {
                portal.answer(id, Err("already running on box".into()));
            }
        });
        assert_eq!(
            link.ask(launch(), Duration::from_secs(5))
                .await
                .unwrap_err(),
            Failure::Said("already running on box".into())
        );
    }

    #[tokio::test]
    async fn with_nobody_connected_or_no_answer_it_fails_and_forgets() {
        let (link, rx) = listening();
        drop(rx);
        assert_eq!(
            link.ask(launch(), Duration::from_secs(5))
                .await
                .unwrap_err(),
            Failure::NotConnected
        );
        let (link, _rx) = listening();
        assert_eq!(
            link.ask(launch(), Duration::from_millis(30))
                .await
                .unwrap_err(),
            Failure::NoAnswer
        );
        assert!(link.waiting.lock().unwrap().is_empty());
    }
}

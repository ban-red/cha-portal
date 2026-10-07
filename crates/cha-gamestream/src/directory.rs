//! The front's boundary with the world around it: who is paired, what a
//! paired client may launch, and where a launched session's media runs.
//! Everything an embedding application decides is a method here; the front
//! decides nothing about users, apps or processes.

use std::collections::BTreeMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_util::future::BoxFuture;
use tokio::sync::oneshot;

use crate::handoff::{ClientId, MediaPorts, SessionHandoff};

/// One thing a client can launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    /// What the client says in `/launch`; 0 means "nothing" to Moonlight, so
    /// use 1 and up.
    pub id: u32,
    pub title: String,
    pub hdr: bool,
}

/// What `/launch` carries, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchRequest {
    pub app_id: u32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub hdr: bool,
    /// Moonlight's `surroundAudioInfo`: channel mask in the high 16 bits,
    /// channel count in the low.
    pub surround_audio_info: u32,
    /// Play the audio on the host too (`localAudioPlayMode`).
    pub local_audio: bool,
    /// Let the host change its resolution to the client's (`sops`).
    pub optimize_game_settings: bool,
    /// Pads the client has (`gcmap`, a bit per pad).
    pub gamepad_mask: u32,
}

/// What `/resume` carries, parsed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeRequest {
    /// The app the session runs.
    pub app_id: u32,
    pub surround_audio_info: u32,
}

/// Where a session's media will run. The ports are the ones the client is
/// told in RTSP `SETUP`, which comes before the stream is negotiated, so
/// they are bound (reserved) now and [`Directory::start_media`] fills them
/// later.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionTarget {
    pub media_ports: MediaPorts,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    #[error("no such app")]
    NoSuchApp,
    #[error("refused: {0}")]
    Refused(String),
    #[error("failed: {0}")]
    Failed(String),
}

/// Who is pairing, from where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingAttempt {
    /// The `uniqueid` the client states; a label, not an identity.
    pub client_unique_id: String,
    pub client_name: Option<String>,
    pub peer: IpAddr,
    /// The client's certificate, which is its identity once paired.
    pub client: ClientId,
}

/// Resolves to the PIN once someone with the right to pair this client has
/// typed in what the client shows, or to `None` when pairing is abandoned.
#[must_use]
pub struct PinWaiter(oneshot::Receiver<String>);

/// The other end of a [`PinWaiter`]: keep it where a signed-in user can reach
/// it, and [`send`](Self::send) the PIN they type. Dropping it abandons the
/// pairing.
pub struct PinSender(oneshot::Sender<String>);

pub fn pin_channel() -> (PinSender, PinWaiter) {
    let (tx, rx) = oneshot::channel();
    (PinSender(tx), PinWaiter(rx))
}

impl PinSender {
    /// Gives the PIN. `false` when nobody is waiting any more.
    pub fn send(self, pin: String) -> bool {
        self.0.send(pin).is_ok()
    }
}

impl PinWaiter {
    /// A waiter that never gets a PIN: for a directory that is not pairing now.
    pub fn refused() -> Self {
        pin_channel().1
    }
}

impl Future for PinWaiter {
    type Output = Option<String>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx).map(Result::ok)
    }
}

/// The apps of paired clients, and launching them.
pub trait Directory: Send + Sync + 'static {
    /// The apps `client` sees.
    fn apps(&self, client: &ClientId) -> BoxFuture<'_, Vec<App>>;

    /// The box art of an app, PNG or JPEG, if it has any.
    fn app_image(&self, client: &ClientId, app_id: u32) -> BoxFuture<'_, Option<Bytes>>;

    /// A client launches an app. Start whatever runs it and reserve the ports
    /// its media will be served on. The host has already checked that the
    /// client is paired and that no other session is running.
    fn launch(
        &self,
        client: &ClientId,
        request: LaunchRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>>;

    /// The client that launched a session comes back to it. The media of the
    /// previous connection has been stopped; answer with the (possibly new)
    /// ports for the next one.
    fn resume(
        &self,
        client: &ClientId,
        request: ResumeRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>>;

    /// The client negotiated its stream (RTSP `PLAY`): run the media half for
    /// `handoff` on the ports from `launch` or `resume`. The handoff holds the
    /// session key; pass it only to the media half.
    fn start_media(&self, handoff: SessionHandoff) -> BoxFuture<'_, Result<(), DirectoryError>>;

    /// Stop the media of `session_id`: the client cancelled, or another
    /// connection replaces it. Not an error if it is already gone.
    fn stop_media(&self, session_id: u64) -> BoxFuture<'_, ()>;

    /// The client cancelled: quit the app.
    fn cancel(&self, client: &ClientId) -> BoxFuture<'_, ()>;

    /// A client wants to pair. Return where the PIN it shows will come from;
    /// the host holds the client's request open until it does (or the PIN
    /// timeout passes). Every attempt asks anew, so a wrong PIN costs the
    /// attacker a fresh prompt for the user.
    fn pin_for(&self, attempt: PairingAttempt) -> PinWaiter;
}

/// A paired client, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairedClient {
    pub client: ClientId,
    /// The `uniqueid` it stated when pairing.
    pub unique_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("pairing store: {0}")]
pub struct StoreError(pub String);

/// The paired clients, kept wherever the embedder keeps things.
pub trait PairingStore: Send + Sync + 'static {
    fn is_paired(&self, client: &ClientId) -> BoxFuture<'_, Result<bool, StoreError>>;
    fn add(&self, client: PairedClient) -> BoxFuture<'_, Result<(), StoreError>>;
    /// Whether it was paired.
    fn remove(&self, client: &ClientId) -> BoxFuture<'_, Result<bool, StoreError>>;
    fn list(&self) -> BoxFuture<'_, Result<Vec<PairedClient>, StoreError>>;
}

/// A [`PairingStore`] in memory, for tests and for hosts that don't persist.
#[derive(Default)]
pub struct MemoryPairingStore {
    clients: Mutex<BTreeMap<ClientId, PairedClient>>,
}

impl PairingStore for MemoryPairingStore {
    fn is_paired(&self, client: &ClientId) -> BoxFuture<'_, Result<bool, StoreError>> {
        let paired = self.clients.lock().expect("store").contains_key(client);
        Box::pin(async move { Ok(paired) })
    }
    fn add(&self, client: PairedClient) -> BoxFuture<'_, Result<(), StoreError>> {
        self.clients
            .lock()
            .expect("store")
            .insert(client.client.clone(), client);
        Box::pin(async { Ok(()) })
    }
    fn remove(&self, client: &ClientId) -> BoxFuture<'_, Result<bool, StoreError>> {
        let was = self.clients.lock().expect("store").remove(client).is_some();
        Box::pin(async move { Ok(was) })
    }
    fn list(&self) -> BoxFuture<'_, Result<Vec<PairedClient>, StoreError>> {
        let all = self
            .clients
            .lock()
            .expect("store")
            .values()
            .cloned()
            .collect();
        Box::pin(async move { Ok(all) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_pin_arrives_or_the_waiter_gives_up() {
        let (tx, rx) = pin_channel();
        assert!(tx.send("1234".into()));
        assert_eq!(rx.await.as_deref(), Some("1234"));
        let (tx, rx) = pin_channel();
        drop(tx);
        assert_eq!(rx.await, None);
        assert_eq!(PinWaiter::refused().await, None);
    }

    #[tokio::test]
    async fn the_memory_store_adds_lists_and_removes() {
        let store = MemoryPairingStore::default();
        let id = ClientId("ab".into());
        assert!(!store.is_paired(&id).await.unwrap());
        store
            .add(PairedClient {
                client: id.clone(),
                unique_id: "u".into(),
                name: "n".into(),
            })
            .await
            .unwrap();
        assert!(store.is_paired(&id).await.unwrap());
        assert_eq!(store.list().await.unwrap().len(), 1);
        assert!(store.remove(&id).await.unwrap());
        assert!(!store.remove(&id).await.unwrap());
    }
}

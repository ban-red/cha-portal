//! Who is paired with this node's host, and the pairings in progress.
//!
//! The portal owns the list ([`Pairings::set_devices`] replaces it; it is
//! sent on every connect and after every change) and this is the node's
//! copy, which is all the host trusts. Until the first list arrives nobody is
//! paired. A pairing starts when a client asks for a PIN
//! ([`Pairings::begin`]), is answered when a signed-in user types the PIN in
//! the portal ([`Pairings::submit_pin`]) and ends when the host stores the
//! client ([`PairingStore::add`]) or, with a wrong PIN, never does: the
//! client aborts without telling the host, so the attempt is reported failed
//! when no pairing follows its PIN within [`VERDICT_WAIT`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cha_gamestream::{
    ClientId, PairedClient, PairingAttempt, PairingStore, PinSender, PinWaiter, StoreError,
    pin_channel,
};
use cha_wire::{GameStreamDevice, ToPortal};
use futures_util::future::BoxFuture;
use tokio::sync::broadcast;
use tokio::time::Instant;
use tracing::{debug, info, warn};

/// How long a client's pairing attempt waits for its PIN to be typed.
pub const ATTEMPT_TTL: Duration = Duration::from_secs(120);
/// After the PIN went to the client's handshake, how long to wait for the
/// host to store the client before calling the attempt failed. The handshake
/// takes a moment; the portal's own wait is longer than this.
pub const VERDICT_WAIT: Duration = Duration::from_secs(8);
/// Attempts open at once; a client that keeps asking can't fill memory.
const MAX_PENDING: usize = 32;
/// The last list the portal sent, kept only so `--doctor` can count it. It
/// is never read back to decide who is paired.
const SNAPSHOT: &str = "devices.json";

struct Pending {
    client: ClientId,
    peer: IpAddr,
    expires: Instant,
    /// Set when the portal gave a PIN: whose device this becomes.
    user_id: Option<String>,
    /// Where the PIN goes, until it has.
    pin: Option<PinSender>,
}

/// The devices cache and the pending attempts; the host's [`PairingStore`].
pub struct Pairings {
    devices: Mutex<Vec<GameStreamDevice>>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    /// Messages for the portal. Dropped while no connection listens.
    out: broadcast::Sender<ToPortal>,
    snapshot: Option<PathBuf>,
}

impl Pairings {
    /// `dir` is where the doctor's snapshot of the list is kept, if anywhere.
    pub fn new(out: broadcast::Sender<ToPortal>, dir: Option<&Path>) -> Arc<Self> {
        Arc::new(Self {
            devices: Mutex::default(),
            pending: Arc::default(),
            out,
            snapshot: dir.map(|d| d.join(SNAPSHOT)),
        })
    }

    fn say(&self, msg: ToPortal) {
        // Nobody listening is normal: the portal isn't connected.
        let _ = self.out.send(msg);
    }

    /// The portal's list replaces the cache.
    pub fn set_devices(&self, devices: Vec<GameStreamDevice>) {
        debug!(count = devices.len(), "the portal sent the paired devices");
        if let Some(path) = &self.snapshot
            && let Err(err) = write_snapshot(path, &devices)
        {
            debug!("keeping the device count for the doctor: {err}");
        }
        *self.devices.lock().expect("devices lock") = devices;
    }

    pub fn devices(&self) -> Vec<GameStreamDevice> {
        self.devices.lock().expect("devices lock").clone()
    }

    /// The portal user a paired client belongs to.
    pub fn user_of(&self, client: &ClientId) -> Option<String> {
        self.devices
            .lock()
            .expect("devices lock")
            .iter()
            .find(|d| d.fingerprint == client.fingerprint())
            .map(|d| d.user_id.clone())
    }

    /// A client wants to pair: tell the portal, and give back where its PIN
    /// will come from. A client that asks again replaces its earlier attempt.
    pub fn begin(self: &Arc<Self>, attempt: PairingAttempt) -> PinWaiter {
        let (sender, waiter) = pin_channel();
        let id = random_id();
        let now = Instant::now();
        {
            let mut pending = self.pending.lock().expect("pending lock");
            pending.retain(|_, p| p.expires > now && p.client != attempt.client);
            if pending.len() >= MAX_PENDING {
                warn!("too many GameStream pairings open; refusing another");
                return PinWaiter::refused();
            }
            pending.insert(
                id.clone(),
                Pending {
                    client: attempt.client.clone(),
                    peer: attempt.peer,
                    expires: now + ATTEMPT_TTL,
                    user_id: None,
                    pin: Some(sender),
                },
            );
        }
        let device_name = attempt
            .client_name
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| attempt.client_unique_id.clone());
        info!(%device_name, peer = %attempt.peer, "a Moonlight device wants to pair");
        self.say(ToPortal::GameStreamPairRequest {
            attempt_id: id.clone(),
            device_name,
            address: attempt.peer.to_string(),
            expires_in_secs: ATTEMPT_TTL.as_secs(),
        });
        // The attempt goes with its time; its PIN sender drops, which ends the
        // client's wait.
        let pending = Arc::downgrade(&self.pending);
        tokio::spawn(async move {
            tokio::time::sleep(ATTEMPT_TTL).await;
            if let Some(pending) = pending.upgrade() {
                // One that was given a PIN has its own verdict to wait for.
                let mut pending = pending.lock().expect("pending lock");
                if pending.get(&id).is_some_and(|p| p.user_id.is_none()) {
                    pending.remove(&id);
                }
            }
        });
        waiter
    }

    /// The PIN a user typed in the portal, for the attempt it shows.
    pub fn submit_pin(
        self: &Arc<Self>,
        attempt_id: &str,
        pin: &str,
        user_id: &str,
    ) -> Result<(), String> {
        if pin.len() != 4 || !pin.bytes().all(|b| b.is_ascii_digit()) {
            return Err("a PIN is four digits".into());
        }
        let sender = {
            let mut pending = self.pending.lock().expect("pending lock");
            let attempt = pending
                .get_mut(attempt_id)
                .filter(|p| p.expires > Instant::now())
                .ok_or("no such pairing: it expired, or the device started another")?;
            let sender = attempt
                .pin
                .take()
                .ok_or("a PIN was already entered for this pairing")?;
            attempt.user_id = Some(user_id.to_string());
            sender
        };
        if !sender.send(pin.to_string()) {
            // The client stopped waiting.
            self.pending
                .lock()
                .expect("pending lock")
                .remove(attempt_id);
            return Err("the device stopped waiting; start pairing again on it".into());
        }
        // A pairing that doesn't follow was a wrong PIN, or the device gave up.
        let this = Arc::downgrade(self);
        let id = attempt_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(VERDICT_WAIT).await;
            let Some(this) = this.upgrade() else { return };
            let open = this.pending.lock().expect("pending lock").remove(&id);
            if open.is_some() {
                info!("a Moonlight pairing didn't complete after its PIN");
                this.say(ToPortal::GameStreamPairFailed {
                    attempt_id: id,
                    reason: "the PIN was wrong, or the device gave up".into(),
                });
            }
        });
        Ok(())
    }
}

impl PairingStore for Pairings {
    fn is_paired(&self, client: &ClientId) -> BoxFuture<'_, Result<bool, StoreError>> {
        let paired = self
            .devices
            .lock()
            .expect("devices lock")
            .iter()
            .any(|d| d.fingerprint == client.fingerprint());
        Box::pin(async move { Ok(paired) })
    }

    /// The host finished a pairing: only one whose PIN came through the
    /// portal is stored, for the user who typed it.
    fn add(&self, client: PairedClient) -> BoxFuture<'_, Result<(), StoreError>> {
        let attempt = {
            let mut pending = self.pending.lock().expect("pending lock");
            let id = pending
                .iter()
                .find(|(_, p)| p.client == client.client && p.user_id.is_some())
                .map(|(id, _)| id.clone());
            id.and_then(|id| pending.remove(&id).map(|p| (id, p)))
        };
        let result = match attempt {
            Some((attempt_id, attempt)) => {
                let user_id = attempt.user_id.unwrap_or_default();
                let fingerprint = client.client.fingerprint().to_string();
                debug!(peer = %attempt.peer, "a Moonlight device paired");
                {
                    let mut devices = self.devices.lock().expect("devices lock");
                    devices.retain(|d| d.fingerprint != fingerprint);
                    devices.push(GameStreamDevice {
                        fingerprint: fingerprint.clone(),
                        unique_id: Some(client.unique_id.clone()),
                        name: client.name.clone(),
                        user_id,
                    });
                }
                self.say(ToPortal::GameStreamPaired {
                    attempt_id,
                    fingerprint,
                    unique_id: client.unique_id,
                    name: client.name,
                });
                Ok(())
            }
            None => Err(StoreError(
                "no PIN was entered in the portal for this device".into(),
            )),
        };
        Box::pin(async move { result })
    }

    /// A client unpaired itself (HTTPS, its own certificate): the portal is
    /// told and forgets it.
    fn remove(&self, client: &ClientId) -> BoxFuture<'_, Result<bool, StoreError>> {
        let was = {
            let mut devices = self.devices.lock().expect("devices lock");
            let before = devices.len();
            devices.retain(|d| d.fingerprint != client.fingerprint());
            devices.len() != before
        };
        if was {
            self.say(ToPortal::GameStreamUnpaired {
                fingerprint: client.fingerprint().to_string(),
            });
        }
        Box::pin(async move { Ok(was) })
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<PairedClient>, StoreError>> {
        let all = self
            .devices
            .lock()
            .expect("devices lock")
            .iter()
            .map(|d| PairedClient {
                client: ClientId(d.fingerprint.clone()),
                unique_id: d.unique_id.clone().unwrap_or_default(),
                name: d.name.clone(),
            })
            .collect();
        Box::pin(async move { Ok(all) })
    }
}

fn random_id() -> String {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("the system has randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn write_snapshot(path: &Path, devices: &[GameStreamDevice]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp)?;
    file.write_all(&serde_json::to_vec(devices).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}

/// How many devices the portal last said are paired, as the node saw it; for
/// `--doctor`, which runs apart from the agent. `None` if it never heard.
pub fn snapshot_count(dir: &Path) -> Option<usize> {
    let text = std::fs::read(dir.join(SNAPSHOT)).ok()?;
    Some(
        serde_json::from_slice::<Vec<GameStreamDevice>>(&text)
            .ok()?
            .len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attempt(unique: &str, who: &str) -> PairingAttempt {
        PairingAttempt {
            client_unique_id: unique.into(),
            client_name: Some("Steam Deck".into()),
            peer: "192.168.1.9".parse().unwrap(),
            client: ClientId(who.repeat(64)),
        }
    }

    fn device(who: &str, user: &str) -> GameStreamDevice {
        GameStreamDevice {
            fingerprint: who.repeat(64),
            unique_id: None,
            name: "Phone".into(),
            user_id: user.into(),
        }
    }

    fn rig() -> (Arc<Pairings>, broadcast::Receiver<ToPortal>) {
        let (out, rx) = broadcast::channel(16);
        (Pairings::new(out, None), rx)
    }

    fn asked(rx: &mut broadcast::Receiver<ToPortal>) -> (String, String, String) {
        match rx.try_recv().expect("a message for the portal") {
            ToPortal::GameStreamPairRequest {
                attempt_id,
                device_name,
                address,
                ..
            } => (attempt_id, device_name, address),
            other => panic!("expected a pairing request, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn nobody_is_paired_until_the_portal_says_so() {
        let (pairings, _rx) = rig();
        let client = ClientId("a".repeat(64));
        assert!(!pairings.is_paired(&client).await.unwrap());
        assert!(pairings.list().await.unwrap().is_empty());
        pairings.set_devices(vec![device("a", "u1")]);
        assert!(pairings.is_paired(&client).await.unwrap());
        assert_eq!(pairings.user_of(&client).as_deref(), Some("u1"));
        assert!(!pairings.is_paired(&ClientId("b".repeat(64))).await.unwrap());
        // A new list replaces, not adds to, the old one.
        pairings.set_devices(vec![device("b", "u2")]);
        assert!(!pairings.is_paired(&client).await.unwrap());
        assert_eq!(pairings.user_of(&client), None);
        let listed = pairings.list().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Phone");
        pairings.set_devices(Vec::new());
        assert!(pairings.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_attempt_goes_to_the_portal_and_its_pin_to_the_right_waiter() {
        let (pairings, mut rx) = rig();
        let first = pairings.begin(attempt("u-one", "a"));
        let second = pairings.begin(attempt("u-two", "b"));
        let (one, name, address) = asked(&mut rx);
        let (two, _, _) = asked(&mut rx);
        assert_eq!(
            (name.as_str(), address.as_str()),
            ("Steam Deck", "192.168.1.9")
        );
        assert_ne!(one, two);

        // A PIN for the second reaches only the second.
        pairings.submit_pin(&two, "4321", "bob").unwrap();
        assert_eq!(second.await.as_deref(), Some("4321"));
        pairings.submit_pin(&one, "1111", "alice").unwrap();
        assert_eq!(first.await.as_deref(), Some("1111"));

        // The attempt is used, not reusable; bad PINs and unknown ids are refused.
        assert!(pairings.submit_pin(&one, "2222", "alice").is_err());
        assert!(pairings.submit_pin("nope", "1234", "alice").is_err());
        let third = pairings.begin(attempt("u-three", "c"));
        let (three, _, _) = asked(&mut rx);
        assert!(pairings.submit_pin(&three, "12", "alice").is_err());
        assert!(pairings.submit_pin(&three, "12a4", "alice").is_err());
        drop(third);
        let err = pairings.submit_pin(&three, "1234", "alice").unwrap_err();
        assert!(err.contains("stopped waiting"), "{err}");
    }

    #[tokio::test]
    async fn the_user_who_typed_the_pin_owns_the_device_the_portal_hears_of() {
        let (pairings, mut rx) = rig();
        let _waiter = pairings.begin(attempt("u-one", "a"));
        let (id, _, _) = asked(&mut rx);
        pairings.submit_pin(&id, "1234", "bob").unwrap();
        let client = ClientId("a".repeat(64));
        pairings
            .add(PairedClient {
                client: client.clone(),
                unique_id: "u-one".into(),
                name: "Steam Deck".into(),
            })
            .await
            .unwrap();
        // The host sees it paired at once (its own challenge follows over HTTPS).
        assert!(pairings.is_paired(&client).await.unwrap());
        assert_eq!(pairings.user_of(&client).as_deref(), Some("bob"));
        match rx.try_recv().unwrap() {
            ToPortal::GameStreamPaired {
                attempt_id,
                fingerprint,
                unique_id,
                name,
            } => {
                assert_eq!(attempt_id, id);
                assert_eq!(fingerprint, client.fingerprint());
                assert_eq!((unique_id.as_str(), name.as_str()), ("u-one", "Steam Deck"));
            }
            other => panic!("expected the pairing, got {other:?}"),
        }
        // The attempt is over: a late wrong-PIN report must not follow.
        assert!(pairings.submit_pin(&id, "1234", "bob").is_err());
    }

    #[tokio::test]
    async fn a_pairing_nobody_typed_a_pin_for_is_refused() {
        let (pairings, mut rx) = rig();
        let _waiter = pairings.begin(attempt("u-one", "a"));
        asked(&mut rx);
        let client = ClientId("a".repeat(64));
        let refused = pairings
            .add(PairedClient {
                client: client.clone(),
                unique_id: "u-one".into(),
                name: "Deck".into(),
            })
            .await;
        assert!(refused.is_err());
        assert!(!pairings.is_paired(&client).await.unwrap());
        // Nor one for a client with no attempt at all.
        let stranger = pairings
            .add(PairedClient {
                client: ClientId("f".repeat(64)),
                unique_id: "x".into(),
                name: "x".into(),
            })
            .await;
        assert!(stranger.is_err());
    }

    #[tokio::test]
    async fn a_client_unpairing_itself_tells_the_portal_once() {
        let (pairings, mut rx) = rig();
        pairings.set_devices(vec![device("a", "u1")]);
        let client = ClientId("a".repeat(64));
        assert!(pairings.remove(&client).await.unwrap());
        assert!(!pairings.remove(&client).await.unwrap());
        assert!(!pairings.is_paired(&client).await.unwrap());
        match rx.try_recv().unwrap() {
            ToPortal::GameStreamUnpaired { fingerprint } => {
                assert_eq!(fingerprint, client.fingerprint());
            }
            other => panic!("expected an unpairing, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "told once");
    }

    #[tokio::test(start_paused = true)]
    async fn attempts_expire_and_a_pin_without_a_pairing_is_reported_failed() {
        let (pairings, mut rx) = rig();
        let waiter = pairings.begin(attempt("u-one", "a"));
        let (expiring, _, _) = asked(&mut rx);
        let typed = pairings.begin(attempt("u-two", "b"));
        let (answered, _, _) = asked(&mut rx);

        // The second gets a PIN and no pairing follows: failed, after the wait.
        pairings.submit_pin(&answered, "1234", "bob").unwrap();
        assert_eq!(typed.await.as_deref(), Some("1234"));
        tokio::time::sleep(VERDICT_WAIT + Duration::from_millis(10)).await;
        match rx.try_recv().unwrap() {
            ToPortal::GameStreamPairFailed { attempt_id, .. } => assert_eq!(attempt_id, answered),
            other => panic!("expected a failure, got {other:?}"),
        }

        // The first never gets one: it is dropped at its time, its waiter ends.
        tokio::time::sleep(ATTEMPT_TTL).await;
        assert_eq!(waiter.await, None);
        assert!(pairings.submit_pin(&expiring, "1234", "bob").is_err());
        assert!(
            rx.try_recv().is_err(),
            "an unanswered attempt isn't a failure"
        );
    }

    #[tokio::test]
    async fn a_client_that_asks_again_replaces_its_attempt() {
        let (pairings, mut rx) = rig();
        let old = pairings.begin(attempt("u-one", "a"));
        let (first, _, _) = asked(&mut rx);
        let _new = pairings.begin(attempt("u-one", "a"));
        let (second, _, _) = asked(&mut rx);
        assert_eq!(old.await, None, "the first attempt was abandoned");
        assert!(pairings.submit_pin(&first, "1234", "bob").is_err());
        assert!(pairings.submit_pin(&second, "1234", "bob").is_ok());
    }

    #[tokio::test]
    async fn the_doctor_can_count_the_last_list() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _rx) = broadcast::channel(4);
        let pairings = Pairings::new(out, Some(dir.path()));
        assert_eq!(snapshot_count(dir.path()), None);
        pairings.set_devices(vec![device("a", "u1"), device("b", "u1")]);
        assert_eq!(snapshot_count(dir.path()), Some(2));
        // Another process's snapshot decides nothing: a new agent trusts no one yet.
        let (out, _rx) = broadcast::channel(4);
        let fresh = Pairings::new(out, Some(dir.path()));
        assert!(fresh.devices().is_empty());
    }
}

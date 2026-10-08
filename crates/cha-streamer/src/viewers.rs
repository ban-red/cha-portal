//! Who's watching an environment and who has the controls (plan §3.4).
//!
//! Sessions coexist, up to `MAX_SESSIONS`, over either transport (WebRTC
//! sessions share the environment's one UDP port, `rtc_hub`). One session has the floor: its
//! keyboard, mouse, gamepads, resizes and clipboard reach the environment,
//! and only it gets the apps' clipboard. The newest owner session takes it
//! on joining; an admin's only when no owner is watching. When the
//! controller leaves, the newest session that may control gets it. Owners
//! and admins can take it back (`{"t":"take_control"}`).
//!
//! A share link's `player` role (ADR 0014) is one gamepad slot and nothing
//! else: such a session never holds or takes the floor, and `control` routes
//! its pad to the slot and the slot's feedback back to it.

use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::watch;
use tracing::info;

use crate::session::Running;

pub const MAX_SESSIONS: usize = 4;

/// Who a media token (or the shared token) let in.
#[derive(Clone, Debug)]
pub struct Viewer {
    pub user: String,
    pub role: Role,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// A share link's gamepad, on pad index 1 to 3 (player 2 to 4). Ranks
    /// below every role, so the derived order never lets it near the floor.
    Player(u8),
    Viewer,
    Admin,
    Owner,
}

impl Role {
    /// The role a media token's claims name; anything unknown is a viewer.
    pub fn from_claims(role: &str, slot: Option<u8>) -> Self {
        match (role, slot) {
            ("owner", _) => Role::Owner,
            ("admin", _) => Role::Admin,
            ("player", Some(slot @ 1..=3)) => Role::Player(slot),
            _ => Role::Viewer,
        }
    }

    fn may_control(self) -> bool {
        matches!(self, Role::Admin | Role::Owner)
    }
}

/// The share id in a share token's `sub` (`share:<id>`), for logs.
fn share_id(user: &str) -> &str {
    user.strip_prefix("share:").unwrap_or(user)
}

struct Entry {
    id: u64,
    viewer: Viewer,
    /// Set once the session runs.
    running: Option<Running>,
    /// Someone is using this session: native clients always are; a browser says
    /// so with `{"t":"presence","active"}` (a hidden, silent tab isn't).
    active: bool,
}

struct Inner {
    next_id: u64,
    entries: Vec<Entry>,
    /// Since when no seat has been active: `None` while any is, and from the
    /// start while nobody has come.
    idle_since: Option<Instant>,
}

impl Inner {
    /// Starts or stops the idle clock after the seats changed.
    fn settle(&mut self) {
        if self.entries.iter().any(|e| e.active) {
            self.idle_since = None;
        } else if self.idle_since.is_none() {
            self.idle_since = Some(Instant::now());
        }
    }
}

/// Called when the floor moves (the streamer puts the cursor back in the
/// picture until the new controller's page says it draws it).
pub type OnFloorMove = Box<dyn Fn() + Send + Sync>;

pub struct Viewers {
    inner: Mutex<Inner>,
    /// The session with the floor.
    floor: watch::Sender<Option<u64>>,
    on_floor_move: OnFloorMove,
}

impl Viewers {
    pub fn new(on_floor_move: OnFloorMove) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                next_id: 0,
                entries: Vec::new(),
                idle_since: Some(Instant::now()),
            }),
            floor: watch::Sender::new(None),
            on_floor_move,
        })
    }

    /// A seat for a session about to start; an error if the environment is
    /// full.
    pub async fn join(self: &Arc<Self>, viewer: Viewer, webrtc: bool) -> Result<Seat, String> {
        let mut inner = self.lock();
        if inner.entries.len() >= MAX_SESSIONS {
            return Err(format!(
                "{MAX_SESSIONS} sessions are watching this environment already"
            ));
        }
        inner.next_id += 1;
        let id = inner.next_id;
        let role = viewer.role;
        info!(id, user = %viewer.user, ?role, webrtc, "viewer joined");
        if let Role::Player(slot) = role {
            info!(id, slot, share = share_id(&viewer.user), "player joined");
        }
        inner.entries.push(Entry {
            id,
            viewer,
            running: None,
            active: true,
        });
        inner.settle();
        // The newest owner takes the floor; anyone who may control takes an
        // empty one, or one held by a lower role.
        let holder = self
            .controller()
            .and_then(|c| inner.entries.iter().find(|e| e.id == c))
            .map(|e| e.viewer.role);
        if role.may_control() && holder.is_none_or(|h| role == Role::Owner || role > h) {
            self.give_floor(Some(id));
        } else {
            // The count changed: let the sessions say so.
            self.floor.send_modify(|_| ());
        }
        Ok(Seat {
            viewers: Arc::clone(self),
            id,
            role,
            floor: self.floor.subscribe(),
        })
    }

    /// Records how to stop a started session.
    pub fn attach(&self, id: u64, running: Running) {
        if let Some(entry) = self.lock().entries.iter_mut().find(|e| e.id == id) {
            entry.running = Some(running);
        }
    }

    pub fn count(&self) -> usize {
        self.lock().entries.len()
    }

    /// How many sessions are in use (see `Seat::set_active`).
    pub fn active_count(&self) -> usize {
        self.lock().entries.iter().filter(|e| e.active).count()
    }

    /// Seconds since no session has been in use; 0 while any is.
    pub fn idle_secs(&self) -> u64 {
        self.lock()
            .idle_since
            .map_or(0, |since| since.elapsed().as_secs())
    }

    fn set_active(&self, id: u64, active: bool) {
        let mut inner = self.lock();
        if let Some(entry) = inner.entries.iter_mut().find(|e| e.id == id) {
            entry.active = active;
        }
        inner.settle();
    }

    /// The pad indices live player sessions hold, one bit each (bit 1 is
    /// player 2's); the floor's pads on those are dropped.
    fn player_pads(&self) -> u8 {
        self.lock()
            .entries
            .iter()
            .filter_map(|e| match e.viewer.role {
                Role::Player(slot) => Some(1u8 << slot),
                _ => None,
            })
            .fold(0, |mask, bit| mask | bit)
    }

    fn controller(&self) -> Option<u64> {
        *self.floor.borrow()
    }

    fn leave(&self, id: u64) {
        let mut inner = self.lock();
        if let Some(at) = inner.entries.iter().position(|e| e.id == id) {
            let gone = inner.entries.remove(at).viewer;
            if let Role::Player(slot) = gone.role {
                info!(id, slot, share = share_id(&gone.user), "player left");
            }
        }
        inner.settle();
        if self.controller() == Some(id) {
            // The newest of the highest role that may control.
            let next = inner
                .entries
                .iter()
                .filter(|e| e.viewer.role.may_control())
                .max_by_key(|e| (e.viewer.role, e.id))
                .map(|e| e.id);
            self.give_floor(next);
        } else {
            // The count changed: let the sessions say so.
            self.floor.send_modify(|_| ());
        }
        info!(id, left = inner.entries.len(), "viewer left");
    }

    fn take(&self, id: u64, role: Role) -> bool {
        if !role.may_control() {
            return false;
        }
        let _inner = self.lock();
        if self.controller() != Some(id) {
            self.give_floor(Some(id));
        }
        true
    }

    fn give_floor(&self, id: Option<u64>) {
        info!(controller = ?id, "the floor moves");
        (self.on_floor_move)();
        self.floor.send_replace(id);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A session's place among the viewers; leaving when dropped.
pub struct Seat {
    viewers: Arc<Viewers>,
    pub id: u64,
    pub role: Role,
    floor: watch::Receiver<Option<u64>>,
}

impl Seat {
    pub fn has_control(&self) -> bool {
        *self.floor.borrow() == Some(self.id)
    }

    /// Takes the floor, if this role may; true if it has it now.
    pub fn take_control(&self) -> bool {
        self.viewers.take(self.id, self.role)
    }

    /// The pad slots live player sessions hold, one bit per pad index.
    pub fn player_pads(&self) -> u8 {
        self.viewers.player_pads()
    }

    /// How many sessions watch, this one included.
    pub fn viewers(&self) -> usize {
        self.viewers.count()
    }

    /// A browser's `presence`: whether anyone is using this session. Sessions
    /// start active; this moves neither the floor nor the count.
    pub fn set_active(&self, active: bool) {
        self.viewers.set_active(self.id, active);
    }

    /// Resolves when the floor moves or someone joins or leaves.
    pub async fn changed(&mut self) {
        if self.floor.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

impl Drop for Seat {
    fn drop(&mut self) {
        self.viewers.leave(self.id);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn viewer(user: &str, role: Role) -> Viewer {
        Viewer {
            user: user.into(),
            role,
        }
    }

    #[tokio::test]
    async fn the_floor_follows_the_rules() {
        let viewers = Viewers::new(Box::new(|| ()));
        let admin = viewers.join(viewer("a", Role::Admin), false).await.unwrap();
        assert!(admin.has_control(), "an admin takes an empty floor");
        let owner = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        assert!(
            owner.has_control() && !admin.has_control(),
            "the owner takes it from an admin"
        );
        let admin2 = viewers.join(viewer("b", Role::Admin), false).await.unwrap();
        assert!(
            !admin2.has_control(),
            "an admin doesn't take it from the owner"
        );
        let guest = viewers
            .join(viewer("g", Role::Viewer), false)
            .await
            .unwrap();
        assert!(
            !guest.take_control() && owner.has_control(),
            "a viewer can't take it"
        );
        assert!(
            admin.take_control() && admin.has_control(),
            "an admin can take it"
        );
        assert_eq!(admin.viewers(), 4);
        assert!(
            viewers.join(viewer("x", Role::Owner), false).await.is_err(),
            "full at {MAX_SESSIONS}"
        );
        drop(admin);
        assert!(
            owner.has_control(),
            "the controller left: the owner, first by role"
        );
        drop(owner);
        assert!(admin2.has_control(), "then the newest admin");
        drop(admin2);
        assert!(!guest.has_control(), "never a viewer");
        assert_eq!(guest.viewers(), 1);
    }

    #[tokio::test]
    async fn the_idle_clock_follows_who_is_active() {
        let viewers = Viewers::new(Box::new(|| ()));
        assert!(viewers.lock().idle_since.is_some(), "idle from the start");
        assert_eq!((viewers.active_count(), viewers.count()), (0, 0));

        let a = viewers.join(viewer("a", Role::Owner), false).await.unwrap();
        assert!(viewers.lock().idle_since.is_none(), "a seat starts active");
        assert_eq!(viewers.idle_secs(), 0);
        assert_eq!(viewers.active_count(), 1);

        a.set_active(false);
        assert!(
            viewers.lock().idle_since.is_some(),
            "the only seat went quiet"
        );
        assert_eq!((viewers.active_count(), viewers.count()), (0, 1));
        tokio::time::sleep(Duration::from_millis(5)).await;
        let t = viewers.lock().idle_since.unwrap();
        assert!(t.elapsed() >= Duration::from_millis(5));

        // Setting the same value again doesn't restart the clock.
        a.set_active(false);
        assert_eq!(viewers.lock().idle_since, Some(t));

        a.set_active(true);
        assert!(viewers.lock().idle_since.is_none(), "presence flipped back");
        assert_eq!(viewers.idle_secs(), 0);

        // A quiet seat and an active one: not idle.
        let b = viewers
            .join(viewer("b", Role::Viewer), false)
            .await
            .unwrap();
        a.set_active(false);
        assert_eq!(viewers.active_count(), 1);
        assert!(viewers.lock().idle_since.is_none());

        // The last active seat leaves: only the quiet one remains.
        drop(b);
        assert_eq!((viewers.active_count(), viewers.count()), (0, 1));
        assert!(viewers.lock().idle_since.is_some());

        drop(a);
        assert!(viewers.lock().idle_since.is_some(), "empty is idle");
        assert_eq!(viewers.count(), 0);
    }

    #[tokio::test]
    async fn a_second_owner_device_takes_over_the_controls() {
        let viewers = Viewers::new(Box::new(|| ()));
        let mac = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        let mut tv = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        assert!(tv.has_control() && !mac.has_control());
        // The floor moved, and the count changed, while tv watched: no news.
        drop(mac);
        tokio::time::timeout(Duration::from_millis(100), tv.changed())
            .await
            .expect("tv hears that the mac left");
        assert!(tv.has_control());
    }

    #[test]
    fn claims_make_roles() {
        assert_eq!(Role::from_claims("owner", None), Role::Owner);
        assert_eq!(Role::from_claims("admin", None), Role::Admin);
        assert_eq!(Role::from_claims("player", Some(2)), Role::Player(2));
        assert_eq!(Role::from_claims("player", Some(1)), Role::Player(1));
        assert_eq!(Role::from_claims("player", Some(3)), Role::Player(3));
        for slot in [None, Some(0), Some(4), Some(255)] {
            assert_eq!(Role::from_claims("player", slot), Role::Viewer, "{slot:?}");
        }
        assert_eq!(Role::from_claims("viewer", None), Role::Viewer);
        assert_eq!(Role::from_claims("wat", Some(1)), Role::Viewer);
    }

    #[test]
    fn a_player_ranks_below_every_role_and_may_not_control() {
        for slot in 1..=3 {
            let p = Role::Player(slot);
            assert!(p < Role::Viewer && p < Role::Admin && p < Role::Owner);
            assert!(!p.may_control());
        }
        assert!(!Role::Viewer.may_control());
        assert!(Role::Admin.may_control() && Role::Owner.may_control());
    }

    #[tokio::test]
    async fn a_player_never_gets_or_takes_the_floor() {
        let viewers = Viewers::new(Box::new(|| ()));
        let p1 = viewers
            .join(viewer("share:a", Role::Player(1)), false)
            .await
            .unwrap();
        assert!(!p1.has_control(), "not even alone");
        assert!(!p1.take_control() && !p1.has_control());
        let owner = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        assert!(owner.has_control());
        let p2 = viewers
            .join(viewer("share:b", Role::Player(2)), false)
            .await
            .unwrap();
        assert!(owner.has_control() && !p2.has_control());
        assert!(!p2.take_control() && owner.has_control());
        assert_eq!(viewers.player_pads(), 0b0110);
        assert_eq!(p1.player_pads(), 0b0110);
        // The floor passes over players: the controller leaves, nobody else may.
        drop(owner);
        assert!(!p1.has_control() && !p2.has_control());
        let admin = viewers.join(viewer("a", Role::Admin), false).await.unwrap();
        assert!(admin.has_control(), "an admin takes the empty floor");
        let viewer_seat = viewers
            .join(viewer("v", Role::Viewer), false)
            .await
            .unwrap();
        assert!(viewer_seat.viewers() == 4);
        drop(admin);
        assert!(
            !p1.has_control() && !p2.has_control() && !viewer_seat.has_control(),
            "players and viewers are passed over"
        );
        drop(p1);
        assert_eq!(
            viewers.player_pads(),
            0b0100,
            "a leaving player frees its slot"
        );
    }
}

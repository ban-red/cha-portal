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
//! Share links and their roles (viewer, controller, player-N) are Phase 3.

use std::sync::{Arc, Mutex};
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
    Viewer,
    Admin,
    Owner,
}

impl Role {
    pub fn from_claim(role: &str) -> Self {
        match role {
            "owner" => Role::Owner,
            "admin" => Role::Admin,
            _ => Role::Viewer,
        }
    }

    fn may_control(self) -> bool {
        self >= Role::Admin
    }
}

struct Entry {
    id: u64,
    viewer: Viewer,
    /// Set once the session runs.
    running: Option<Running>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    entries: Vec<Entry>,
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
            inner: Mutex::default(),
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
        inner.entries.push(Entry {
            id,
            viewer,
            running: None,
        });
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

    fn controller(&self) -> Option<u64> {
        *self.floor.borrow()
    }

    fn leave(&self, id: u64) {
        let mut inner = self.lock();
        inner.entries.retain(|e| e.id != id);
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

    /// How many sessions watch, this one included.
    pub fn viewers(&self) -> usize {
        self.viewers.count()
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
}

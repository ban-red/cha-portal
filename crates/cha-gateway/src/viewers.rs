//! Who's watching and who has the controls.
//!
//! Up to `MAX_VIEWERS` sessions watch the one host stream. One has the floor:
//! its keyboard, mouse and pads reach the host. The newest session whose
//! media token allows control (owner, admin) takes it on joining; when the
//! holder leaves, the newest of the others that may control gets it. The
//! streamer's rule (plan §3.4), without its take-over request: the gateway
//! ignores `take_control`.

use std::sync::{Arc, Mutex};

use tokio::sync::watch;

pub const MAX_VIEWERS: usize = 4;

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

/// Who holds the floor now, and how many sessions watch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Floor {
    pub holder: Option<u64>,
    pub viewers: usize,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    /// Oldest first.
    sessions: Vec<(u64, Role)>,
}

pub struct Viewers {
    inner: Mutex<Inner>,
    floor: watch::Sender<Floor>,
}

impl Viewers {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::default(),
            floor: watch::Sender::new(Floor::default()),
        })
    }

    /// A seat for a new viewer, or why there is none.
    pub fn join(self: &Arc<Self>, role: Role) -> Result<Seat, String> {
        let mut inner = self.inner.lock().expect("viewers lock");
        if inner.sessions.len() >= MAX_VIEWERS {
            return Err(format!("{MAX_VIEWERS} viewers are watching already"));
        }
        let id = inner.next_id;
        inner.next_id += 1;
        inner.sessions.push((id, role));
        self.publish(&inner, true);
        Ok(Seat {
            id,
            viewers: Arc::clone(self),
            floor: self.floor.subscribe(),
        })
    }

    fn leave(&self, id: u64) {
        let mut inner = self.inner.lock().expect("viewers lock");
        inner.sessions.retain(|(other, _)| *other != id);
        self.publish(&inner, false);
    }

    /// Works out the floor and tells the sessions if it changed. `joined`: the
    /// newest session may take it; otherwise it stays with its holder while
    /// that one is still here.
    fn publish(&self, inner: &Inner, joined: bool) {
        let current = *self.floor.borrow();
        let newest = inner
            .sessions
            .iter()
            .rev()
            .find(|(_, role)| role.may_control())
            .map(|(id, _)| *id);
        let holder = if joined {
            newest
        } else {
            current
                .holder
                .filter(|h| inner.sessions.iter().any(|(id, _)| id == h))
                .or(newest)
        };
        let floor = Floor {
            holder,
            viewers: inner.sessions.len(),
        };
        self.floor.send_replace(floor);
    }
}

/// A viewer's place; it leaves when dropped.
pub struct Seat {
    pub id: u64,
    viewers: Arc<Viewers>,
    floor: watch::Receiver<Floor>,
}

impl Seat {
    pub fn has_control(&self) -> bool {
        self.floor.borrow().holder == Some(self.id)
    }

    pub fn viewers(&self) -> usize {
        self.floor.borrow().viewers
    }

    /// Resolves when the floor or the number of viewers changes.
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
    use super::*;

    #[test]
    fn the_newest_controller_takes_the_floor_and_hands_it_on() {
        let viewers = Viewers::new();
        let a = viewers.join(Role::Owner).unwrap();
        assert!(a.has_control());
        let b = viewers.join(Role::Owner).unwrap();
        assert!(!a.has_control());
        assert!(b.has_control());
        assert_eq!((a.viewers(), b.viewers()), (2, 2));
        drop(b);
        assert!(a.has_control());
        assert_eq!(a.viewers(), 1);
    }

    #[test]
    fn viewers_never_control() {
        let viewers = Viewers::new();
        let watcher = viewers.join(Role::Viewer).unwrap();
        assert!(!watcher.has_control());
        let owner = viewers.join(Role::Owner).unwrap();
        assert!(owner.has_control() && !watcher.has_control());
        drop(owner);
        assert!(!watcher.has_control());
    }

    #[test]
    fn the_holder_keeps_the_floor_when_another_leaves() {
        let viewers = Viewers::new();
        let a = viewers.join(Role::Owner).unwrap();
        let b = viewers.join(Role::Admin).unwrap();
        let c = viewers.join(Role::Viewer).unwrap();
        assert!(b.has_control());
        drop(c);
        drop(a);
        assert!(b.has_control());
    }

    #[test]
    fn there_is_room_for_four() {
        let viewers = Viewers::new();
        let seats: Vec<_> = (0..MAX_VIEWERS)
            .map(|_| viewers.join(Role::Owner).unwrap())
            .collect();
        assert!(viewers.join(Role::Owner).is_err());
        drop(seats);
        assert!(viewers.join(Role::Owner).is_ok());
    }
}

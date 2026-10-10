//! Who's watching an environment and who has the controls (plan §3.4).
//!
//! Sessions coexist, up to `MAX_SESSIONS`, over either transport (WebRTC
//! sessions share the environment's one UDP port, `rtc_hub`). One session has the floor: its
//! keyboard, mouse, gamepads, resizes and clipboard reach the environment,
//! and only it gets the apps' clipboard. A session joining an environment
//! someone else is using starts as a viewer: it takes the floor on joining
//! only when nobody active holds it, or the holder is the same user (another
//! of their devices). Otherwise it watches, and `can_take` says whether its
//! page may offer "Take control". When the controller leaves, the newest
//! owner or admin gets it. Owners and admins can take it back
//! (`{"t":"take_control"}`).
//!
//! Another signed-in user joins as a `guest` (the environment is public) or a
//! `member` (free). A guest watches until the holder hands it the floor
//! (`give_control`), or takes it when nobody holds it; it can ask with
//! `request_control`, or for a gamepad with `request_pad`, and an owner or
//! admin holding the floor sees the ask in the `viewers` list. A member takes
//! the floor from anyone, freely. A host grants a pad slot to any of them
//! (`give_pad`), which makes the session a player until `revoke_pad`.
//!
//! A share link's `controller` role (ADR 0015) is a guest who may hold the
//! floor, one at a time: it takes it when nobody holds it, or from another
//! controller, never from an owner or admin. It joins without taking an
//! owner's or admin's floor, and the floor never goes to it on a leave; an
//! owner or admin hands it over (`give`). A `viewer` never has the floor.
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
    /// The user's display name, if the token carried one.
    pub name: Option<String>,
    pub role: Role,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// A share link's gamepad, on pad index 1 to 3 (player 2 to 4). Ranks
    /// below every role, so the derived order never lets it near the floor.
    Player(u8),
    Viewer,
    /// A signed-in user in a public environment: watches, may hold the floor
    /// when nobody does or when handed it, and can ask for it or a pad.
    Guest,
    /// A share link's guest who may hold the floor when it is free or a
    /// guest's, or when an owner or admin hands it over.
    Controller,
    /// A signed-in user in a free environment: takes the floor from anyone.
    Member,
    Admin,
    Owner,
}

impl Role {
    /// The role a media token's claims name; anything unknown is a viewer.
    pub fn from_claims(role: &str, slot: Option<u8>) -> Self {
        match (role, slot) {
            ("owner", _) => Role::Owner,
            ("admin", _) => Role::Admin,
            ("controller", _) => Role::Controller,
            ("guest", _) => Role::Guest,
            ("member", _) => Role::Member,
            ("player", Some(slot @ 1..=3)) => Role::Player(slot),
            _ => Role::Viewer,
        }
    }

    /// May hold the floor at all.
    fn may_control(self) -> bool {
        matches!(
            self,
            Role::Guest | Role::Controller | Role::Member | Role::Admin | Role::Owner
        )
    }

    /// An owner or admin: takes the floor back any time, gets it on a leave
    /// and hands it to a guest controller.
    fn is_host(self) -> bool {
        matches!(self, Role::Admin | Role::Owner)
    }

    /// The role's name on the wire (`viewers` messages).
    pub fn name(self) -> &'static str {
        match self {
            Role::Player(_) => "player",
            Role::Viewer => "viewer",
            Role::Guest => "guest",
            Role::Controller => "controller",
            Role::Member => "member",
            Role::Admin => "admin",
            Role::Owner => "owner",
        }
    }

    /// Whether `self` may take the floor now from whoever holds it (`holder`:
    /// their role, `None` for nobody).
    fn can_take_from(self, holder: Option<Role>) -> bool {
        match self {
            Role::Owner | Role::Admin | Role::Member => true,
            Role::Controller => holder.is_none_or(|h| !h.is_host()),
            Role::Guest => holder.is_none(),
            Role::Player(_) | Role::Viewer => false,
        }
    }
}

/// The share id in a share token's `sub` (`share:<id>`), for logs.
fn share_id(user: &str) -> &str {
    user.strip_prefix("share:").unwrap_or(user)
}

/// One session, as an owner's page lists who is watching.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Watcher {
    pub id: u64,
    pub role: &'static str,
    /// A player's pad index (1 to 3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<u8>,
    /// What it asks the holder for: `control` or `pad`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<&'static str>,
    /// A pad slot a host granted (not one its link gave): can be revoked.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub granted: bool,
    /// The user's display name, when its token carried one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// What a session has asked the floor holder for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Control,
    Pad,
}

impl Request {
    pub fn name(self) -> &'static str {
        match self {
            Request::Control => "control",
            Request::Pad => "pad",
        }
    }
}

struct Entry {
    id: u64,
    viewer: Viewer,
    /// The role its token gave; `viewer.role` is a player's while a host has
    /// granted it a pad slot.
    base: Role,
    request: Option<Request>,
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
        let user = viewer.user.clone();
        info!(id, user = %viewer.user, ?role, webrtc, "viewer joined");
        if let Role::Player(slot) = role {
            info!(id, slot, share = share_id(&viewer.user), "player joined");
        }
        inner.entries.push(Entry {
            id,
            viewer,
            base: role,
            request: None,
            running: None,
            active: true,
        });
        inner.settle();
        // Someone else using the session keeps the floor: a joiner takes it
        // only from nobody, from an idle session, or from the same user's
        // other device. A guest controller never takes a host's, and a guest
        // of a public environment never takes any.
        let holder = self
            .controller()
            .and_then(|c| inner.entries.iter().find(|e| e.id == c));
        let busy = holder.is_some_and(|h| h.active && h.viewer.user != user);
        let takes = match role {
            Role::Owner | Role::Admin | Role::Member => !busy,
            Role::Controller => holder.is_none(),
            Role::Player(_) | Role::Viewer | Role::Guest => false,
        };
        if takes {
            self.give_floor(Some(id));
        } else {
            // The count changed: let the sessions say so.
            self.floor.send_modify(|_| ());
        }
        Ok(Seat {
            viewers: Arc::clone(self),
            id,
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
            // The newest of the highest role that is an owner or admin; a
            // guest controller has to be handed the floor.
            let next = inner
                .entries
                .iter()
                .filter(|e| e.viewer.role.is_host())
                .max_by_key(|e| (e.viewer.role, e.id))
                .map(|e| e.id);
            self.give_floor(next);
        } else {
            // The count changed: let the sessions say so.
            self.floor.send_modify(|_| ());
        }
        info!(id, left = inner.entries.len(), "viewer left");
    }

    /// The role of the session holding the floor.
    fn holder_role(inner: &Inner, holder: Option<u64>) -> Option<Role> {
        holder
            .and_then(|c| inner.entries.iter().find(|e| e.id == c))
            .map(|e| e.viewer.role)
    }

    fn take(&self, id: u64, role: Role) -> bool {
        let inner = self.lock();
        let holder = self.controller();
        if holder == Some(id) {
            return role.may_control();
        }
        if !role.can_take_from(Self::holder_role(&inner, holder)) {
            return false;
        }
        self.give_floor(Some(id));
        true
    }

    /// An owner or admin holding the floor hands it to the guest, member or
    /// controller session `to`; false (nothing moves) if `from` doesn't hold
    /// it, isn't a host, or `to` isn't one of those here.
    fn give(&self, from: u64, role: Role, to: u64) -> bool {
        let mut inner = self.lock();
        if !role.is_host() || self.controller() != Some(from) {
            return false;
        }
        let Some(target) = inner.entries.iter_mut().find(|e| {
            e.id == to && matches!(e.viewer.role, Role::Controller | Role::Guest | Role::Member)
        }) else {
            return false;
        };
        target.request = None;
        self.give_floor(Some(to));
        true
    }

    /// The role of session `id` now (a granted pad makes it a player).
    fn role_of(&self, id: u64) -> Role {
        self.lock()
            .entries
            .iter()
            .find(|e| e.id == id)
            .map_or(Role::Viewer, |e| e.viewer.role)
    }

    fn request_of(&self, id: u64) -> Option<Request> {
        self.lock()
            .entries
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.request)
    }

    /// A guest or member asks the holder for the floor or a pad (`None`
    /// withdraws). Only the roles that can use it may ask; the holder's page
    /// hears of it through the `viewers` list.
    fn ask(&self, id: u64, request: Option<Request>) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.entries.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        let allowed = matches!(
            (entry.viewer.role, request),
            (_, None)
                | (Role::Guest, Some(_))
                | (Role::Member | Role::Controller, Some(Request::Pad))
        );
        if !allowed || entry.request == request {
            return allowed;
        }
        entry.request = request;
        drop(inner);
        self.floor.send_modify(|_| ());
        true
    }

    /// A host holding the floor puts session `to` on pad slot `slot` (1 to 3,
    /// free of other players), making it a player until revoked.
    fn grant_pad(&self, from: u64, role: Role, to: u64, slot: u8) -> bool {
        let mut inner = self.lock();
        if !role.is_host() || self.controller() != Some(from) || !(1..=3).contains(&slot) {
            return false;
        }
        if inner
            .entries
            .iter()
            .any(|e| e.viewer.role == Role::Player(slot))
        {
            return false;
        }
        let Some(target) = inner.entries.iter_mut().find(|e| {
            e.id == to
                && matches!(
                    e.viewer.role,
                    Role::Guest | Role::Member | Role::Controller | Role::Viewer
                )
        }) else {
            return false;
        };
        target.viewer.role = Role::Player(slot);
        target.request = None;
        info!(id = to, slot, "a pad slot is granted");
        // The notification below makes every page re-read its floor.
        drop(inner);
        self.floor.send_modify(|_| ());
        true
    }

    /// A host holding the floor takes a granted pad slot back; the session
    /// returns to the role its token gave. A share link's own player stays.
    fn revoke_pad(&self, from: u64, role: Role, to: u64) -> bool {
        let mut inner = self.lock();
        if !role.is_host() || self.controller() != Some(from) {
            return false;
        }
        let Some(target) = inner.entries.iter_mut().find(|e| {
            e.id == to && matches!(e.viewer.role, Role::Player(_)) && e.base != e.viewer.role
        }) else {
            return false;
        };
        target.viewer.role = target.base;
        info!(id = to, "a pad slot is taken back");
        drop(inner);
        self.floor.send_modify(|_| ());
        true
    }

    /// Whether `id` could take the floor now (and doesn't hold it).
    fn can_take(&self, id: u64, role: Role) -> bool {
        let inner = self.lock();
        let holder = self.controller();
        holder != Some(id) && role.can_take_from(Self::holder_role(&inner, holder))
    }

    /// The other sessions, for the floor holder's page.
    fn watchers(&self, except: u64) -> Vec<Watcher> {
        self.lock()
            .entries
            .iter()
            .filter(|e| e.id != except)
            .map(|e| Watcher {
                id: e.id,
                role: e.viewer.role.name(),
                slot: match e.viewer.role {
                    Role::Player(slot) => Some(slot),
                    _ => None,
                },
                request: e.request.map(Request::name),
                granted: e.base != e.viewer.role,
                name: e.viewer.name.clone(),
            })
            .collect()
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
    floor: watch::Receiver<Option<u64>>,
}

impl Seat {
    /// This session's role now: its token's, or a player's while a host has
    /// granted it a pad slot.
    pub fn role(&self) -> Role {
        self.viewers.role_of(self.id)
    }

    /// What this session has asked the holder for.
    pub fn request(&self) -> Option<&'static str> {
        self.viewers.request_of(self.id).map(Request::name)
    }

    /// Asks the holder for the controls (`control`) or a gamepad (`pad`);
    /// `None` withdraws. False if this role may not ask.
    pub fn ask(&self, request: Option<Request>) -> bool {
        self.viewers.ask(self.id, request)
    }

    /// Puts session `to` on pad slot `slot`; only a host holding the floor.
    pub fn give_pad(&self, to: u64, slot: u8) -> bool {
        self.viewers.grant_pad(self.id, self.role(), to, slot)
    }

    /// Takes a granted pad slot from session `to`; only a host holding the floor.
    pub fn revoke_pad(&self, to: u64) -> bool {
        self.viewers.revoke_pad(self.id, self.role(), to)
    }

    pub fn has_control(&self) -> bool {
        *self.floor.borrow() == Some(self.id)
    }

    /// Takes the floor, if this role may; true if it has it now.
    pub fn take_control(&self) -> bool {
        self.viewers.take(self.id, self.role())
    }

    /// Hands the floor to the guest session `to`; true if it moved.
    /// Only an owner or admin holding the floor can.
    pub fn give_control(&self, to: u64) -> bool {
        self.viewers.give(self.id, self.role(), to)
    }

    /// Whether this session could take the floor now: an owner or admin
    /// without it, or a controller while nobody or a guest holds it.
    pub fn can_take(&self) -> bool {
        self.viewers.can_take(self.id, self.role())
    }

    /// The other sessions, if this one holds the floor (else empty).
    pub fn watchers(&self) -> Vec<Watcher> {
        if self.has_control() {
            self.viewers.watchers(self.id)
        } else {
            Vec::new()
        }
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
            name: None,
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
            !owner.has_control() && admin.has_control(),
            "joining doesn't take it from someone using it"
        );
        assert!(owner.can_take() && owner.take_control() && owner.has_control());
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
        assert_eq!(Role::from_claims("controller", None), Role::Controller);
        assert_eq!(
            Role::from_claims("controller", Some(2)),
            Role::Controller,
            "a slot means nothing to a controller"
        );
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

    #[test]
    fn a_controller_ranks_between_a_viewer_and_an_admin() {
        assert!(Role::Player(3) < Role::Viewer);
        assert!(Role::Viewer < Role::Controller);
        assert!(Role::Controller < Role::Admin);
        assert!(Role::Admin < Role::Owner);
        assert!(Role::Controller.may_control() && !Role::Controller.is_host());
        assert!(Role::Admin.is_host() && Role::Owner.is_host());
        assert!(!Role::Viewer.is_host() && !Role::Player(1).is_host());
        // Who may take the floor from whom.
        let holders = [
            None,
            Some(Role::Controller),
            Some(Role::Admin),
            Some(Role::Owner),
        ];
        for h in holders {
            assert!(Role::Owner.can_take_from(h) && Role::Admin.can_take_from(h));
            assert!(!Role::Viewer.can_take_from(h) && !Role::Player(1).can_take_from(h));
        }
        assert!(Role::Controller.can_take_from(None));
        assert!(Role::Controller.can_take_from(Some(Role::Controller)));
        assert!(!Role::Controller.can_take_from(Some(Role::Admin)));
        assert!(!Role::Controller.can_take_from(Some(Role::Owner)));
    }

    #[tokio::test]
    async fn a_viewer_never_has_the_floor() {
        let viewers = Viewers::new(Box::new(|| ()));
        let v = viewers
            .join(viewer("share:v", Role::Viewer), false)
            .await
            .unwrap();
        assert!(!v.has_control(), "not even alone");
        assert!(!v.take_control() && !v.has_control());
        assert!(!v.can_take());
        let owner = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        assert!(!v.take_control() && owner.has_control());
        assert!(!owner.give_control(v.id), "it can't be handed to a viewer");
        assert!(owner.has_control());
        assert!(v.watchers().is_empty(), "a viewer sees no list");
        drop(owner);
        assert!(!v.has_control(), "the floor doesn't fall to a viewer");
    }

    #[tokio::test]
    async fn a_controller_takes_a_free_floor_and_a_guests_never_a_hosts() {
        let viewers = Viewers::new(Box::new(|| ()));
        let c1 = viewers
            .join(viewer("share:1", Role::Controller), false)
            .await
            .unwrap();
        assert!(
            c1.has_control(),
            "a controller takes an empty floor on joining"
        );
        let c2 = viewers
            .join(viewer("share:2", Role::Controller), false)
            .await
            .unwrap();
        assert!(
            c1.has_control() && !c2.has_control(),
            "joining doesn't take it from a guest"
        );
        assert!(c2.can_take() && !c1.can_take());
        assert!(
            c2.take_control() && c2.has_control() && !c1.has_control(),
            "controller to controller"
        );
        let owner = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        assert!(
            !owner.has_control() && c2.has_control(),
            "the owner joins as a viewer while a guest uses it"
        );
        assert!(owner.take_control() && !c2.has_control());
        assert!(!c1.can_take() && !c2.can_take(), "not from an owner");
        assert!(!c1.take_control() && !c2.take_control() && owner.has_control());
        // Only the holder, a host, hands it over, and only to a controller.
        assert!(!c1.give_control(c2.id), "a controller can't hand it on");
        assert!(!owner.give_control(9999), "no such session");
        assert!(owner.give_control(c1.id) && c1.has_control() && !owner.has_control());
        assert!(!owner.give_control(c2.id), "the owner no longer holds it");
        assert!(!c1.can_take(), "it holds the floor");
        // Take back, any time.
        assert!(owner.take_control() && owner.has_control() && !c1.has_control());
        assert!(owner.take_control(), "taking what you hold is fine");
        // An admin joining, with the floor in a guest's hands, takes it; an
        // admin with the floor keeps it against a joining controller.
        assert!(owner.give_control(c2.id) && c2.has_control());
        let admin = viewers.join(viewer("a", Role::Admin), false).await.unwrap();
        assert!(!admin.has_control() && c2.has_control());
        assert!(
            admin.take_control() && !c2.has_control(),
            "an admin takes it from a guest"
        );
        assert!(!c2.can_take(), "and a guest can't take it back");
        drop(admin);
        assert!(
            owner.has_control(),
            "a leaving admin: the floor goes to the owner"
        );
        drop(owner);
        assert!(
            !c1.has_control() && !c2.has_control(),
            "no host left: it waits for a controller to take it"
        );
        assert!(c1.can_take() && c2.can_take());
        assert!(c1.take_control() && c1.has_control());
    }

    #[tokio::test]
    async fn a_joining_controller_leaves_a_hosts_floor_alone() {
        let viewers = Viewers::new(Box::new(|| ()));
        let admin = viewers.join(viewer("a", Role::Admin), false).await.unwrap();
        let c = viewers
            .join(viewer("share:c", Role::Controller), false)
            .await
            .unwrap();
        assert!(admin.has_control() && !c.has_control());
        assert!(!c.take_control() && admin.has_control());
        // The host leaves: the guest isn't handed the floor, it has to take it.
        drop(admin);
        assert!(!c.has_control());
        // A controller joining an empty floor takes it.
        let c2 = viewers
            .join(viewer("share:d", Role::Controller), false)
            .await
            .unwrap();
        assert!(c2.has_control() && !c.has_control());
        assert!(c.can_take(), "from a guest");
        assert!(c.take_control() && c.has_control());
    }

    #[tokio::test]
    async fn a_handed_floor_goes_back_to_the_host_when_the_guest_leaves() {
        let viewers = Viewers::new(Box::new(|| ()));
        let owner = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        let c = viewers
            .join(viewer("share:c", Role::Controller), false)
            .await
            .unwrap();
        let admin = viewers.join(viewer("a", Role::Admin), false).await.unwrap();
        assert!(owner.has_control());
        assert!(owner.give_control(c.id) && c.has_control());
        drop(c);
        assert!(
            owner.has_control() && !admin.has_control(),
            "the owner, first by role"
        );
    }

    #[tokio::test]
    async fn the_floor_holder_sees_who_is_watching() {
        let viewers = Viewers::new(Box::new(|| ()));
        let owner = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        let c = viewers
            .join(viewer("share:c", Role::Controller), false)
            .await
            .unwrap();
        let p = viewers
            .join(viewer("share:p", Role::Player(2)), false)
            .await
            .unwrap();
        let v = viewers
            .join(viewer("share:v", Role::Viewer), false)
            .await
            .unwrap();
        let list = owner.watchers();
        assert_eq!(
            list,
            [
                Watcher {
                    id: c.id,
                    role: "controller",
                    slot: None,
                    request: None,
                    granted: false,
                    name: None
                },
                Watcher {
                    id: p.id,
                    role: "player",
                    slot: Some(2),
                    request: None,
                    granted: false,
                    name: None
                },
                Watcher {
                    id: v.id,
                    role: "viewer",
                    slot: None,
                    request: None,
                    granted: false,
                    name: None
                },
            ]
        );
        assert!(
            c.watchers().is_empty() && p.watchers().is_empty(),
            "only the holder"
        );
        assert!(owner.give_control(c.id));
        assert!(owner.watchers().is_empty());
        assert_eq!(
            c.watchers().len(),
            3,
            "the guest holder sees the rest, the owner included"
        );
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

    #[tokio::test]
    async fn a_joiner_leaves_a_busy_session_alone() {
        let viewers = Viewers::new(Box::new(|| ()));
        let mac = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        let other = viewers.join(viewer("x", Role::Admin), false).await.unwrap();
        assert!(mac.has_control() && !other.has_control() && other.can_take());
        // The holder's page goes quiet: the next to join may take it.
        mac.set_active(false);
        let late = viewers.join(viewer("y", Role::Admin), false).await.unwrap();
        assert!(late.has_control(), "an idle holder doesn't keep the floor");
        // The same user's other device takes over.
        let phone = viewers.join(viewer("y", Role::Admin), false).await.unwrap();
        assert!(phone.has_control() && !late.has_control());
    }

    #[tokio::test]
    async fn a_guest_asks_and_a_host_answers() {
        let viewers = Viewers::new(Box::new(|| ()));
        let owner = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        let guest = viewers.join(viewer("g", Role::Guest), false).await.unwrap();
        assert!(!guest.has_control() && !guest.can_take() && !guest.take_control());
        assert!(guest.ask(Some(Request::Control)));
        assert_eq!(guest.request(), Some("control"));
        let list = owner.watchers();
        assert_eq!(list[0].request, Some("control"));
        assert!(owner.give_control(guest.id) && guest.has_control());
        assert_eq!(guest.request(), None, "the answer clears the ask");
        assert!(owner.can_take() && owner.take_control());
        // A pad instead.
        assert!(guest.ask(Some(Request::Pad)));
        assert!(!guest.give_pad(guest.id, 1), "only a host");
        assert!(!owner.give_pad(guest.id, 0) && !owner.give_pad(guest.id, 4));
        assert!(owner.give_pad(guest.id, 2));
        assert_eq!(guest.role(), Role::Player(2));
        assert_eq!(viewers.player_pads(), 0b0100);
        assert!(!guest.take_control(), "a player never takes the floor");
        let guest2 = viewers.join(viewer("h", Role::Guest), false).await.unwrap();
        assert!(!owner.give_pad(guest2.id, 2), "the slot is taken");
        assert!(owner.give_pad(guest2.id, 1));
        assert!(owner.revoke_pad(guest.id));
        assert_eq!(guest.role(), Role::Guest);
        assert_eq!(viewers.player_pads(), 0b0010);
        assert!(!owner.revoke_pad(owner.id));
        // A link's own player can't be revoked.
        let link = viewers
            .join(viewer("share:p", Role::Player(3)), false)
            .await
            .unwrap();
        assert!(!owner.revoke_pad(link.id));
    }

    #[tokio::test]
    async fn a_member_takes_the_floor_from_anyone() {
        let viewers = Viewers::new(Box::new(|| ()));
        let owner = viewers.join(viewer("o", Role::Owner), false).await.unwrap();
        let member = viewers
            .join(viewer("m", Role::Member), false)
            .await
            .unwrap();
        assert!(!member.has_control() && member.can_take());
        assert!(member.take_control() && member.has_control() && !owner.has_control());
        assert!(owner.take_control() && owner.has_control());
        assert!(!member.ask(Some(Request::Control)), "it needn't ask");
        assert!(member.ask(Some(Request::Pad)));
    }
}

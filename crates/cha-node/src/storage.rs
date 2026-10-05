//! App data on this node: the **data root** (`CHA_DATA_ROOT`) and what the
//! agent keeps in it. The layout is `cha_wire`'s (`storage.rs` there):
//!
//! ```text
//! <root>/users/<user id>/<template id>/          1000:1000 0700, mounted as the app's home
//! <root>/users/<user id>/<template id>.migrated  a file: the legacy volume was copied in once
//! <root>/shared/<template id>/                   1000:1000 0770, shared across users
//! ```
//!
//! `<root>` is made by whoever installs the node (Docker does, from the
//! agent's bind mount) and stays theirs; the agent makes `users/`, `shared/`
//! and `users/<user id>/` (root-owned 0755: apps never see them) and the
//! directories apps mount, which belong to the app's uid so it can write them.
//! The agent runs as root in its container, which is what lets it hand
//! directories to uid 1000 and clear them out again.
//!
//! **Paths are never followed out of the root.** Apps run as uid 1000 and own
//! what is inside their mounts, so anything below a mount point may be a
//! symlink an app planted. Ids are validated (`cha_wire`), and every
//! directory the agent creates, changes or looks into is reached one
//! component at a time with `openat(O_NOFOLLOW | O_DIRECTORY)` from the root's
//! descriptor, then changed through that descriptor (`fchown`, `fchmod`),
//! never by path. The parents of everything an app can write are root-owned,
//! so an app can't swap one of them under us.
//!
//! **A shared directory the owner keeps elsewhere** (`CHA_SHARED_DIRS`, say
//! Steam's library on an NFS share) is not the agent's: it makes nothing in it,
//! changes no owner or mode, and removes nothing. It only looks (read-only, in
//! its own container) at whether the directory is there and has the places
//! each user's own copies are mounted over ([`check_external_dir`]); a launch
//! whose check fails goes without the shared directory.
//!
//! Everything here blocks: callers run it with `spawn_blocking`.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use cha_wire::{
    PER_USER_DIR, SHARED_DIR, USERS_DIR, valid_relative_path, valid_template_id, valid_user_id,
};
use rustix::fs::{
    AtFlags, CWD, Dir, Gid, Mode, OFlags, RawMode, Uid, fchmod, fchown, fstat, mkdirat, openat,
    renameat, statat, statvfs, unlinkat,
};
use rustix::io::Errno;
use tracing::{info, warn};

/// What `users/<user>/<template>` holds, as far as deciding how to start it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirState {
    Missing,
    Empty,
    Populated,
}

/// A user's directory for an app, as found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Home {
    pub state: DirState,
    /// A legacy volume was copied into this (user, app) before (and the
    /// directory may have been reset since): never copy it again.
    pub migrated: bool,
}

/// What to put in a home before the app starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seed {
    /// It has things in it: leave them.
    Keep,
    /// Copy the user's legacy volume in (once).
    Migrate,
    /// Start from the image's own `/home/cha`, as a new Docker volume does.
    Image,
}

/// The first start of a home (a new directory, or one that was reset) takes
/// the user's old volume if there is one that hasn't been taken, and the
/// image's files otherwise; a directory with anything in it is theirs.
pub fn plan_seed(home: Home, legacy_volume_exists: bool) -> Seed {
    match home.state {
        DirState::Populated => Seed::Keep,
        DirState::Missing | DirState::Empty if legacy_volume_exists && !home.migrated => {
            Seed::Migrate
        }
        DirState::Missing | DirState::Empty => Seed::Image,
    }
}

/// How the data root looks, for the doctor and the logs.
#[derive(Debug, Default)]
pub struct Stats {
    pub free_bytes: Option<u64>,
    /// `users/<user>/<template>` directories.
    pub user_dirs: usize,
    /// The distinct users among them.
    pub users: usize,
    /// `shared/<template>` directories.
    pub shared_dirs: usize,
    /// (user, template) pairs whose legacy volume was copied in.
    pub migrated: HashSet<(String, String)>,
}

#[derive(Clone, Copy)]
enum Kind {
    /// Made by the agent for its own use: root-owned, 0755.
    Infra,
    /// An app's home: its uid, 0700.
    Home,
    /// Shared across users: its uid, 0770.
    Shared,
}

impl Kind {
    fn mode(self) -> RawMode {
        match self {
            Self::Infra => 0o755,
            Self::Home => 0o700,
            Self::Shared => 0o770,
        }
    }

    fn app_owned(self) -> bool {
        !matches!(self, Self::Infra)
    }
}

#[derive(Debug, Clone)]
pub struct DataRoot {
    root: PathBuf,
    /// Who apps run as: owns what they mount.
    uid: u32,
    gid: u32,
}

impl DataRoot {
    /// The root at `root`, for apps running as `uid:gid`. It must be an
    /// absolute path with nothing odd in it: Docker is given host paths built
    /// from it, and the agent's container mounts it at the same path.
    pub fn new(root: impl Into<PathBuf>, uid: u32, gid: u32) -> Result<Self> {
        let root = root.into();
        let Some(root) = plain_absolute(&root) else {
            bail!(
                "the data root {} must be an absolute path, not /, with no ., .. or doubled slashes",
                root.display()
            );
        };
        Ok(Self { root, uid, gid })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Where a path of the layout is on the host (and here).
    pub fn host_path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// `users/<user>/<template>/` on the host: what the app mounts as its home.
    pub fn home_path(&self, user: &str, template: &str) -> PathBuf {
        self.root.join(cha_wire::user_dir(user, template))
    }

    fn ids(user: &str, template: &str) -> Result<()> {
        if !valid_user_id(user) {
            bail!("{user:?} isn't a user id");
        }
        if !valid_template_id(template) {
            bail!("{template:?} isn't a template id");
        }
        Ok(())
    }

    fn open_root(&self) -> Result<OwnedFd> {
        // The root itself is the owner's to arrange (a symlink to a bigger
        // disk is fine); everything below it isn't followed.
        openat(
            CWD,
            &self.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| {
            anyhow!(
                "the data root {} can't be opened ({e}); the node's owner makes it and the agent's container mounts it at the same path (deploy/node/compose.yaml)",
                self.root.display()
            )
        })
    }

    /// Hands a directory the agent just made to the app, or to nobody in
    /// particular for its own: through the descriptor, never a path.
    fn settle(&self, fd: BorrowedFd<'_>, kind: Kind) -> Result<()> {
        if kind.app_owned() {
            fchown(
                fd,
                Some(Uid::from_raw(self.uid)),
                Some(Gid::from_raw(self.gid)),
            )
            .map_err(|e| {
                anyhow!(
                    "giving a directory to uid {}:{} ({e}); the agent needs to run as root to do that",
                    self.uid,
                    self.gid
                )
            })?;
        }
        fchmod(fd, Mode::from_bits_truncate(kind.mode()))?;
        Ok(())
    }

    /// `name` in `parent` as a directory of this kind: made if it isn't there,
    /// and given to the app if it is one the app should own but doesn't (a
    /// directory Docker made as root, say). A symlink, or anything that
    /// isn't a directory, is refused, not followed.
    fn ensure_dir(&self, parent: BorrowedFd<'_>, name: &str, kind: Kind) -> Result<OwnedFd> {
        match open_dir(parent, name) {
            Ok(fd) => {
                if kind.app_owned() && fstat(&fd)?.st_uid != self.uid {
                    warn!(name, "handing a directory to the app's user");
                    self.settle(fd.as_fd(), kind)?;
                }
                Ok(fd)
            }
            Err(Errno::NOENT) => {
                match mkdirat(parent, name, Mode::from_bits_truncate(kind.mode())) {
                    Ok(()) | Err(Errno::EXIST) => {}
                    Err(e) => bail!("making {name}: {e}"),
                }
                let fd = open_dir(parent, name).map_err(|e| not_a_dir(name, e))?;
                self.settle(fd.as_fd(), kind)?;
                Ok(fd)
            }
            Err(e) => Err(not_a_dir(name, e)),
        }
    }

    /// The directory at `parts` below `start`, each made as needed.
    fn ensure_chain(&self, start: BorrowedFd<'_>, parts: &[&str], kind: Kind) -> Result<OwnedFd> {
        let mut dir = start.try_clone_to_owned()?;
        for part in parts {
            dir = self.ensure_dir(dir.as_fd(), part, kind)?;
        }
        Ok(dir)
    }

    /// `users/<user>`, made as needed: where the agent keeps one user's apps.
    fn user_parent(&self, root: BorrowedFd<'_>, user: &str) -> Result<OwnedFd> {
        let users = self.ensure_dir(root, USERS_DIR, Kind::Infra)?;
        self.ensure_dir(users.as_fd(), user, Kind::Infra)
    }

    /// What `users/<user>/<template>` is now. Makes `users/<user>`, nothing
    /// below it.
    pub fn inspect_home(&self, user: &str, template: &str) -> Result<Home> {
        Self::ids(user, template)?;
        let root = self.open_root()?;
        let parent = self.user_parent(root.as_fd(), user)?;
        let state = match open_dir(parent.as_fd(), template) {
            Ok(fd) => {
                if is_empty(&fd)? {
                    DirState::Empty
                } else {
                    DirState::Populated
                }
            }
            Err(Errno::NOENT) => DirState::Missing,
            Err(e) => return Err(not_a_dir(template, e)),
        };
        let migrated = exists(parent.as_fd(), &marker_name(template))?;
        Ok(Home { state, migrated })
    }

    /// The app's home, made if it isn't there: 1000:1000, 0700.
    pub fn ensure_home(&self, user: &str, template: &str) -> Result<()> {
        Self::ids(user, template)?;
        let root = self.open_root()?;
        let parent = self.user_parent(root.as_fd(), user)?;
        self.ensure_dir(parent.as_fd(), template, Kind::Home)?;
        Ok(())
    }

    /// A fresh `users/<user>/<template>.migrating` for a copy to be made in
    /// (any leftover of an interrupted one goes first); returns its path.
    pub fn begin_migration(&self, user: &str, template: &str) -> Result<PathBuf> {
        Self::ids(user, template)?;
        let root = self.open_root()?;
        let parent = self.user_parent(root.as_fd(), user)?;
        self.remove_entry(
            parent.as_fd(),
            &self.user_parent_path(user),
            &migrating_name(template),
        )?;
        self.ensure_dir(parent.as_fd(), &migrating_name(template), Kind::Home)?;
        Ok(self.home_path(user, &migrating_name(template)))
    }

    /// Puts the finished copy in place as the home and records that the
    /// legacy volume `volume` was taken. The directory that stood there
    /// (empty, by [`plan_seed`]) goes.
    pub fn finish_migration(&self, user: &str, template: &str, volume: &str) -> Result<()> {
        Self::ids(user, template)?;
        let root = self.open_root()?;
        let parent = self.user_parent(root.as_fd(), user)?;
        match unlinkat(parent.as_fd(), template, AtFlags::REMOVEDIR) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(e) => bail!("making room for the copied files ({e}): {template} isn't empty"),
        }
        renameat(
            parent.as_fd(),
            migrating_name(template).as_str(),
            parent.as_fd(),
            template,
        )?;
        // The copy kept its own owner and modes (the volume's root was the
        // app's); a restored home must be the app's and private whatever it was.
        let home = open_dir(parent.as_fd(), template).map_err(|e| not_a_dir(template, e))?;
        self.settle(home.as_fd(), Kind::Home)?;
        self.write_marker(parent.as_fd(), template, volume)
    }

    /// Drops an unfinished copy.
    pub fn abort_migration(&self, user: &str, template: &str) -> Result<()> {
        Self::ids(user, template)?;
        let root = self.open_root()?;
        let parent = self.user_parent(root.as_fd(), user)?;
        self.remove_entry(
            parent.as_fd(),
            &self.user_parent_path(user),
            &migrating_name(template),
        )
    }

    /// Records that `volume`'s content was taken for this (user, app), so a
    /// reset (or a directory removed by hand) isn't undone by copying it back.
    pub fn mark_migrated(&self, user: &str, template: &str, volume: &str) -> Result<()> {
        Self::ids(user, template)?;
        let root = self.open_root()?;
        let parent = self.user_parent(root.as_fd(), user)?;
        self.write_marker(parent.as_fd(), template, volume)
    }

    fn write_marker(&self, parent: BorrowedFd<'_>, template: &str, volume: &str) -> Result<()> {
        let fd = match openat(
            parent,
            marker_name(template).as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o644),
        ) {
            Ok(fd) => fd,
            Err(Errno::EXIST) => return Ok(()),
            Err(e) => bail!("writing the migration marker: {e}"),
        };
        let mut file = fs::File::from(fd);
        writeln!(
            file,
            "{volume} was copied here; it is left in place and never copied again."
        )?;
        Ok(())
    }

    /// `shared/<template>` for the app, and the places in it each user gets
    /// their own copy of ([`cha_wire::Shared::per_user`]), which Docker would
    /// otherwise make as root when it mounts over them.
    pub fn ensure_shared(&self, template: &str, per_user: &[String]) -> Result<()> {
        if !valid_template_id(template) {
            bail!("{template:?} isn't a template id");
        }
        let places = per_user
            .iter()
            .map(|p| relative_parts(p))
            .collect::<Result<Vec<_>>>()?;
        let root = self.open_root()?;
        let shared = self.ensure_dir(root.as_fd(), SHARED_DIR, Kind::Infra)?;
        let dir = self.ensure_dir(shared.as_fd(), template, Kind::Shared)?;
        for parts in places {
            self.ensure_chain(dir.as_fd(), &parts, Kind::Shared)?;
        }
        Ok(())
    }

    /// The user's own copies of an app's per-user shared paths, inside their
    /// app directory, which must exist ([`Self::ensure_home`]): the sources
    /// of the mounts over the shared directory.
    pub fn ensure_per_user(&self, user: &str, template: &str, per_user: &[String]) -> Result<()> {
        Self::ids(user, template)?;
        let places = per_user
            .iter()
            .map(|p| relative_parts(p))
            .collect::<Result<Vec<_>>>()?;
        let root = self.open_root()?;
        let parent = self.user_parent(root.as_fd(), user)?;
        let home = self.ensure_dir(parent.as_fd(), template, Kind::Home)?;
        let base = self.ensure_dir(home.as_fd(), PER_USER_DIR, Kind::Home)?;
        for parts in places {
            self.ensure_chain(base.as_fd(), &parts, Kind::Home)?;
        }
        Ok(())
    }

    /// Deletes everything this user keeps for the app: their directory and
    /// anything left of an interrupted copy. The migration marker stays, so
    /// the legacy volume doesn't come back. Whether there was anything.
    pub fn delete_user_data(&self, user: &str, template: &str) -> Result<bool> {
        Self::ids(user, template)?;
        let root = self.open_root()?;
        let users = match open_dir(root.as_fd(), USERS_DIR) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(false),
            Err(e) => return Err(not_a_dir(USERS_DIR, e)),
        };
        let parent = match open_dir(users.as_fd(), user) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(false),
            Err(e) => return Err(not_a_dir(user, e)),
        };
        let mut existed = false;
        for name in [template.to_string(), migrating_name(template)] {
            existed |= exists(parent.as_fd(), &name)?;
            self.remove_entry(parent.as_fd(), &self.user_parent_path(user), &name)?;
        }
        info!(user, template, existed, "deleted a user's app data");
        Ok(existed)
    }

    /// Removes `name` in `parent` and everything under it, never following a
    /// symlink: a link is removed as itself. A top-level entry that isn't a
    /// directory is refused, since the agent only ever makes directories here.
    ///
    /// `parent_path` is `parent` by name, for `remove_dir_all`: its parents are
    /// the agent's (root-owned), and it doesn't follow links below them
    /// either (the standard library's version opens each directory relative
    /// to the last, and unlinks links as links).
    fn remove_entry(&self, parent: BorrowedFd<'_>, parent_path: &Path, name: &str) -> Result<()> {
        match open_dir(parent, name) {
            Ok(dir) => {
                drop(dir);
                fs::remove_dir_all(parent_path.join(name))
                    .with_context(|| format!("removing {name}"))
            }
            Err(Errno::NOENT) => Ok(()),
            Err(e) => Err(not_a_dir(name, e)),
        }
    }

    /// `users/<user>` by path.
    fn user_parent_path(&self, user: &str) -> PathBuf {
        self.root.join(USERS_DIR).join(user)
    }

    /// Creates a file in the root, to see whether the agent can write there,
    /// and returns its name; [`Self::remove_probe`] removes it.
    pub fn create_probe(&self) -> Result<String> {
        let root = self.open_root()?;
        let name = format!(".cha-probe-{}", std::process::id());
        let fd = openat(
            root.as_fd(),
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o644),
        )
        .map_err(|e| anyhow!("the agent can't write to {}: {e}", self.root.display()))?;
        drop(fd);
        Ok(name)
    }

    pub fn remove_probe(&self, name: &str) {
        if let Ok(root) = self.open_root() {
            let _ = unlinkat(root.as_fd(), name, AtFlags::empty());
        }
    }

    pub fn stats(&self) -> Stats {
        let mut stats = Stats {
            free_bytes: statvfs(&self.root)
                .ok()
                .map(|s| s.f_bavail.saturating_mul(s.f_frsize)),
            ..Stats::default()
        };
        let dirs = |path: PathBuf| -> Vec<(String, bool)> {
            fs::read_dir(path)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    let is_dir = e.file_type().ok()?.is_dir();
                    Some((name, is_dir))
                })
                .collect()
        };
        for (user, is_dir) in dirs(self.root.join(USERS_DIR)) {
            if !is_dir || !valid_user_id(&user) {
                continue;
            }
            let mut any = false;
            for (name, is_dir) in dirs(self.root.join(USERS_DIR).join(&user)) {
                if is_dir && valid_template_id(&name) {
                    stats.user_dirs += 1;
                    any = true;
                } else if let Some(template) = name.strip_suffix(".migrated")
                    && valid_template_id(template)
                {
                    stats.migrated.insert((user.clone(), template.to_string()));
                }
            }
            stats.users += usize::from(any);
        }
        stats.shared_dirs = dirs(self.root.join(SHARED_DIR))
            .iter()
            .filter(|(name, is_dir)| *is_dir && valid_template_id(name))
            .count();
        stats
    }
}

/// `TEMPLATE=/absolute/path` entries (`--shared-dir`, `CHA_SHARED_DIRS`) as a
/// map: for those templates the shared directory is that path, mounted into
/// the app at the same path. Paths are absolute and normalised, not `/`, and
/// not inside the data root (or around it): the agent owns what is under the
/// root and mustn't be pointed at it from outside, nor at anything that holds
/// it. Empty entries are skipped (compose passes unset variables as empty).
pub fn parse_shared_dirs(
    entries: &[String],
    data_root: &Path,
) -> Result<BTreeMap<String, PathBuf>> {
    let mut dirs = BTreeMap::new();
    for entry in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
        let (template, path) = entry
            .split_once('=')
            .ok_or_else(|| anyhow!("shared directory {entry:?} isn't TEMPLATE=/absolute/path"))?;
        let (template, path) = (template.trim(), path.trim());
        if !valid_template_id(template) {
            bail!("shared directory {entry:?}: {template:?} isn't a template id");
        }
        let Some(dir) = plain_absolute(Path::new(path)) else {
            bail!(
                "shared directory {entry:?}: {path:?} must be an absolute path, not /, with no ., .. or doubled slashes"
            );
        };
        if dir.starts_with(data_root) || data_root.starts_with(&dir) {
            bail!(
                "shared directory {entry:?}: {} and the data root {} can't be inside one another",
                dir.display(),
                data_root.display()
            );
        }
        if dirs.insert(template.to_string(), dir).is_some() {
            bail!("shared directory for {template} given twice");
        }
    }
    Ok(dirs)
}

/// Whether `dir` (a shared directory kept outside the data root) is there and
/// has every per-user place as a real directory: opened component by component
/// without following symlinks, so none of them leads out of it. Reads
/// nothing but names, and creates nothing. The error says what is wrong and
/// the `mkdir -p` that fixes it, for whoever can write there.
pub fn check_external_dir(dir: &Path, per_user: &[String]) -> Result<()> {
    let fd = openat(
        CWD,
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| {
        anyhow!(
            "{} can't be opened ({e}): is the share mounted on this node, and bound into the agent at the same path?",
            dir.display()
        )
    })?;
    for path in per_user {
        let mut at = fd.try_clone()?;
        for part in relative_parts(path)? {
            at = open_dir(at.as_fd(), part).map_err(|e| match e {
                Errno::NOENT => anyhow!(
                    "{} has no {path} (is the share really mounted? to make it, as a user who can write there: mkdir -p {})",
                    dir.display(),
                    dir.join(path).display()
                ),
                Errno::LOOP | Errno::NOTDIR => anyhow!(
                    "{path} in {} isn't a plain directory (a symlink?): the agent doesn't follow it",
                    dir.display()
                ),
                e => anyhow!("opening {path} in {}: {e}", dir.display()),
            })?;
        }
    }
    Ok(())
}

/// The per-user places that `dir` doesn't have as real directories.
pub fn missing_per_user(dir: &Path, per_user: &[String]) -> Vec<String> {
    per_user
        .iter()
        .filter(|p| check_external_dir(dir, std::slice::from_ref(p)).is_err())
        .cloned()
        .collect()
}

/// `path` without a trailing slash, if it is absolute, isn't `/`, and is
/// already as `Path` would normalise it: no `.`, `..` or doubled slashes.
fn plain_absolute(path: &Path) -> Option<PathBuf> {
    let text = path.to_str()?;
    let trimmed = text.trim_end_matches('/');
    let dir = PathBuf::from(trimmed);
    let plain = dir.is_absolute()
        && dir.components().count() > 1
        && dir.components().all(|c| {
            matches!(
                c,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
        // Components drop interior `.` and doubled slashes: the text must be
        // what they give back.
        && dir.components().collect::<PathBuf>().to_str() == Some(trimmed);
    plain.then_some(dir)
}

fn marker_name(template: &str) -> String {
    format!("{template}.migrated")
}

fn migrating_name(template: &str) -> String {
    format!("{template}.migrating")
}

/// The components of a validated relative path.
fn relative_parts(path: &str) -> Result<Vec<&str>> {
    if !valid_relative_path(path) {
        bail!("{path:?} isn't a path inside the shared directory");
    }
    Ok(path.split('/').collect())
}

/// `name` in `parent`, opened as a directory without following a symlink.
fn open_dir(parent: BorrowedFd<'_>, name: &str) -> std::result::Result<OwnedFd, Errno> {
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

fn not_a_dir(name: &str, errno: Errno) -> anyhow::Error {
    match errno {
        // What opening a symlink, or a file, as a directory without following
        // it fails with, by system.
        Errno::LOOP | Errno::NOTDIR => anyhow!(
            "{name} isn't a plain directory (a symlink, or a file): the agent doesn't follow it"
        ),
        e => anyhow!("opening {name}: {e}"),
    }
}

fn exists(parent: BorrowedFd<'_>, name: &str) -> Result<bool> {
    match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(Errno::NOENT) => Ok(false),
        Err(e) => bail!("looking for {name}: {e}"),
    }
}

fn is_empty(dir: &OwnedFd) -> Result<bool> {
    for entry in Dir::read_from(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if name != b"." && name != b".." {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    use super::*;

    const USER: &str = "01a10527-f79f-761b-962f-4b26924a2e68";
    const OTHER: &str = "01a10834-b378-724f-8d25-a1d4034a79d5";

    /// A data root in a temp dir; "the app's uid" is whoever runs the tests,
    /// since only root can hand files to someone else.
    struct Fixture {
        dir: tempfile::TempDir,
        data: DataRoot,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cha-portal");
        fs::create_dir(&root).unwrap();
        let me = fs::metadata(&root).unwrap();
        let data = DataRoot::new(&root, me.uid(), me.gid()).unwrap();
        Fixture { dir, data }
    }

    fn mode(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn touch(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }

    #[test]
    fn a_root_must_be_plain_and_absolute() {
        for bad in ["", "relative/path", "/srv/../etc", "/srv/./x", "/"] {
            assert!(DataRoot::new(bad, 1000, 1000).is_err(), "{bad:?}");
        }
        assert!(DataRoot::new("/srv/cha-portal", 1000, 1000).is_ok());
        assert!(DataRoot::new("/srv/cha-portal/", 1000, 1000).is_ok());
    }

    #[test]
    fn a_missing_root_says_how_to_make_it() {
        let f = fixture();
        fs::remove_dir(f.data.path()).unwrap();
        let err = f.data.ensure_home(USER, "steam").unwrap_err();
        assert!(format!("{err:#}").contains("mounts it at the same path"));
        // And nothing was made in its place.
        assert!(!f.data.path().exists());
    }

    #[test]
    fn homes_are_private_and_their_parents_are_the_agents() {
        let f = fixture();
        let root = f.data.path();
        let root_mode = mode(root);
        f.data.ensure_home(USER, "steam").unwrap();
        let home = root.join("users").join(USER).join("steam");
        assert_eq!(mode(&home), 0o700);
        assert_eq!(fs::metadata(&home).unwrap().uid(), f.data.uid);
        // Apps never see these: the agent's own, 0755.
        assert_eq!(mode(&root.join("users")), 0o755);
        assert_eq!(mode(&root.join("users").join(USER)), 0o755);
        // The root is left as the owner made it.
        assert_eq!(mode(root), root_mode);
        // Again is fine, and changes nothing.
        fs::write(home.join("save"), "data").unwrap();
        f.data.ensure_home(USER, "steam").unwrap();
        assert_eq!(fs::read_to_string(home.join("save")).unwrap(), "data");
    }

    #[test]
    fn shared_directories_are_group_writable_and_carry_the_per_user_places() {
        let f = fixture();
        let per_user = vec![
            "steamapps/compatdata".to_string(),
            "steamapps/shadercache".to_string(),
        ];
        f.data.ensure_shared("steam", &per_user).unwrap();
        let shared = f.data.path().join("shared");
        assert_eq!(mode(&shared), 0o755);
        for dir in [
            "steam",
            "steam/steamapps",
            "steam/steamapps/compatdata",
            "steam/steamapps/shadercache",
        ] {
            let path = shared.join(dir);
            assert_eq!(mode(&path), 0o770, "{dir}");
            assert_eq!(fs::metadata(&path).unwrap().uid(), f.data.uid, "{dir}");
        }
    }

    #[test]
    fn per_user_sources_live_in_the_users_own_directory() {
        let f = fixture();
        f.data.ensure_home(USER, "steam").unwrap();
        let per_user = vec!["steamapps/compatdata".to_string()];
        f.data.ensure_per_user(USER, "steam", &per_user).unwrap();
        let source = f
            .data
            .home_path(USER, "steam")
            .join(".cha-shared/steamapps/compatdata");
        assert_eq!(mode(&source), 0o700);
        // Another user's are theirs.
        f.data.ensure_home(OTHER, "steam").unwrap();
        f.data.ensure_per_user(OTHER, "steam", &per_user).unwrap();
        assert!(
            f.data
                .home_path(OTHER, "steam")
                .join(".cha-shared/steamapps/compatdata")
                .is_dir()
        );
    }

    #[test]
    fn bad_ids_and_paths_never_reach_the_disk() {
        let f = fixture();
        for (user, template) in [
            ("..", "steam"),
            (USER, ".."),
            (USER, "../steam"),
            ("../..", "x"),
            (&USER.to_uppercase(), "steam"),
            (USER, "Steam"),
            ("", ""),
            (&format!("{USER}/.."), "steam"),
        ] {
            assert!(
                f.data.ensure_home(user, template).is_err(),
                "{user} {template}"
            );
            assert!(f.data.inspect_home(user, template).is_err());
            assert!(f.data.delete_user_data(user, template).is_err());
            assert!(f.data.begin_migration(user, template).is_err());
        }
        for path in ["../x", "/etc", "a/../b", "", "a//b"] {
            assert!(
                f.data.ensure_shared("steam", &[path.to_string()]).is_err(),
                "{path:?}"
            );
        }
        assert!(f.data.ensure_shared("../x", &[]).is_err());
        assert_eq!(
            fs::read_dir(f.data.path()).unwrap().count(),
            0,
            "nothing was made"
        );
    }

    #[test]
    fn a_symlink_for_a_directory_is_refused_and_not_followed() {
        let f = fixture();
        let outside = f.dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();

        // The leaf.
        let parent = f.data.path().join("users").join(USER);
        fs::create_dir_all(&parent).unwrap();
        symlink(&outside, parent.join("steam")).unwrap();
        assert!(f.data.ensure_home(USER, "steam").is_err());
        assert!(f.data.inspect_home(USER, "steam").is_err());
        assert!(f.data.delete_user_data(USER, "steam").is_err());
        assert_eq!(mode(&outside), 0o755, "the target was left alone");
        assert!(parent.join("steam").is_symlink());

        // `users` itself, and a user's.
        fs::remove_file(parent.join("steam")).unwrap();
        fs::remove_dir(&parent).unwrap();
        symlink(&outside, parent.parent().unwrap().join(USER)).unwrap();
        assert!(f.data.ensure_home(USER, "steam").is_err());
        assert!(
            outside.read_dir().unwrap().next().is_none(),
            "nothing made outside"
        );
        fs::remove_file(parent.parent().unwrap().join(USER)).unwrap();
        fs::remove_dir(parent.parent().unwrap()).unwrap();
        symlink(&outside, f.data.path().join("users")).unwrap();
        assert!(f.data.ensure_home(USER, "steam").is_err());
        assert!(outside.read_dir().unwrap().next().is_none());
        fs::remove_file(f.data.path().join("users")).unwrap();

        // A shared directory's inside, where apps write: a planted link on the
        // way to a per-user place.
        f.data.ensure_shared("steam", &[]).unwrap();
        let shared = f.data.path().join("shared/steam");
        symlink(&outside, shared.join("steamapps")).unwrap();
        let places = vec!["steamapps/compatdata".to_string()];
        assert!(f.data.ensure_shared("steam", &places).is_err());
        assert!(outside.read_dir().unwrap().next().is_none());
        assert_eq!(mode(&outside), 0o755);

        // The same inside a user's own directory.
        f.data.ensure_home(USER, "steam").unwrap();
        let home = f.data.home_path(USER, "steam");
        symlink(&outside, home.join(".cha-shared")).unwrap();
        assert!(f.data.ensure_per_user(USER, "steam", &places).is_err());
        assert!(outside.read_dir().unwrap().next().is_none());
    }

    #[test]
    fn a_file_where_a_directory_belongs_is_refused() {
        let f = fixture();
        fs::create_dir_all(f.data.path().join("users")).unwrap();
        fs::write(f.data.path().join("users").join(USER), "x").unwrap();
        assert!(f.data.ensure_home(USER, "steam").is_err());
    }

    #[test]
    fn what_a_home_holds_decides_where_it_starts_from() {
        let f = fixture();
        let home = f.data.home_path(USER, "steam");
        let state = |f: &Fixture| f.data.inspect_home(USER, "steam").unwrap();
        assert_eq!(
            state(&f),
            Home {
                state: DirState::Missing,
                migrated: false
            }
        );
        f.data.ensure_home(USER, "steam").unwrap();
        assert_eq!(state(&f).state, DirState::Empty);
        touch(&home.join(".profile"));
        assert_eq!(state(&f).state, DirState::Populated);
        // A marker is separate from the home.
        f.data.mark_migrated(USER, "steam", "cha-home-x").unwrap();
        assert!(state(&f).migrated);
        assert!(!f.data.inspect_home(USER, "chrome").unwrap().migrated);
        assert!(!f.data.inspect_home(OTHER, "steam").unwrap().migrated);
        // Writing it twice is fine.
        f.data.mark_migrated(USER, "steam", "cha-home-x").unwrap();
    }

    #[test]
    fn the_seed_follows_the_directory_the_volume_and_the_marker() {
        let h = |state, migrated| Home { state, migrated };
        use DirState::*;
        // Something there: never touched, whatever else.
        for volume in [false, true] {
            for migrated in [false, true] {
                assert_eq!(plan_seed(h(Populated, migrated), volume), Seed::Keep);
            }
        }
        // New or empty, with an old volume not yet taken: take it.
        assert_eq!(plan_seed(h(Missing, false), true), Seed::Migrate);
        assert_eq!(plan_seed(h(Empty, false), true), Seed::Migrate);
        // The volume was taken before (the user reset since): start fresh.
        assert_eq!(plan_seed(h(Missing, true), true), Seed::Image);
        assert_eq!(plan_seed(h(Empty, true), true), Seed::Image);
        // No volume: the image's own files.
        assert_eq!(plan_seed(h(Missing, false), false), Seed::Image);
        assert_eq!(plan_seed(h(Empty, false), false), Seed::Image);
        assert_eq!(plan_seed(h(Missing, true), false), Seed::Image);
    }

    #[test]
    fn a_migration_lands_whole_or_not_at_all() {
        let f = fixture();
        let home = f.data.home_path(USER, "steam");
        let temp = f.data.begin_migration(USER, "steam").unwrap();
        assert_eq!(temp, f.data.home_path(USER, "steam.migrating"));
        assert_eq!(mode(&temp), 0o700);
        touch(&temp.join(".local/share/Steam/steam.sh"));
        assert_eq!(
            f.data.inspect_home(USER, "steam").unwrap().state,
            DirState::Missing,
            "half a copy is not a home"
        );

        // Interrupted: the next try starts clean.
        let temp = f.data.begin_migration(USER, "steam").unwrap();
        assert!(fs::read_dir(&temp).unwrap().next().is_none());
        touch(&temp.join("file"));
        f.data.abort_migration(USER, "steam").unwrap();
        assert!(!temp.exists());

        // Finished, over the empty directory that stood there.
        f.data.ensure_home(USER, "steam").unwrap();
        let temp = f.data.begin_migration(USER, "steam").unwrap();
        touch(&temp.join(".local/share/Steam/steam.sh"));
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o750)).unwrap();
        f.data
            .finish_migration(USER, "steam", "cha-home-x")
            .unwrap();
        assert!(home.join(".local/share/Steam/steam.sh").is_file());
        assert!(!temp.exists());
        assert_eq!(mode(&home), 0o700, "private whatever the volume's root was");
        let found = f.data.inspect_home(USER, "steam").unwrap();
        assert_eq!(
            found,
            Home {
                state: DirState::Populated,
                migrated: true
            }
        );
        let marker = fs::read_to_string(f.data.home_path(USER, "steam.migrated")).unwrap();
        assert!(marker.contains("cha-home-x"));
    }

    #[test]
    fn finishing_never_replaces_a_home_that_has_things_in_it() {
        let f = fixture();
        f.data.ensure_home(USER, "steam").unwrap();
        touch(&f.data.home_path(USER, "steam").join("keep-me"));
        let temp = f.data.begin_migration(USER, "steam").unwrap();
        touch(&temp.join("copied"));
        assert!(f.data.finish_migration(USER, "steam", "v").is_err());
        assert!(f.data.home_path(USER, "steam").join("keep-me").is_file());
    }

    #[test]
    fn resetting_deletes_the_users_data_and_nothing_beside_it() {
        let f = fixture();
        let outside = f.dir.path().join("outside");
        touch(&outside.join("precious"));

        for user in [USER, OTHER] {
            f.data.ensure_home(user, "steam").unwrap();
            f.data.ensure_home(user, "chrome").unwrap();
            touch(&f.data.home_path(user, "steam").join("a/b/c"));
        }
        f.data.ensure_shared("steam", &[]).unwrap();
        touch(&f.data.path().join("shared/steam/game"));
        // What an app can leave in its own home: links to anywhere.
        let home = f.data.home_path(USER, "steam");
        symlink(&outside, home.join("escape")).unwrap();
        symlink(f.data.home_path(OTHER, "steam"), home.join("other")).unwrap();
        symlink("/etc/passwd", home.join("passwd")).unwrap();
        // A copy that was interrupted, and the marker of an earlier one.
        f.data.begin_migration(USER, "steam").unwrap();
        f.data.mark_migrated(USER, "steam", "v").unwrap();

        assert!(f.data.delete_user_data(USER, "steam").unwrap());
        assert!(!home.exists());
        assert!(!f.data.home_path(USER, "steam.migrating").exists());
        assert!(
            f.data.home_path(USER, "steam.migrated").exists(),
            "marker kept"
        );
        // Nothing else of theirs, nor anyone else's, nor what the links named.
        assert!(f.data.home_path(USER, "chrome").is_dir());
        assert!(f.data.home_path(OTHER, "steam").join("a/b/c").is_file());
        assert!(outside.join("precious").is_file());
        assert!(f.data.path().join("shared/steam/game").is_file());

        // Nothing there is nothing to do.
        assert!(!f.data.delete_user_data(USER, "steam").unwrap());
        assert!(!f.data.delete_user_data(OTHER, "kde").unwrap());
        // After a reset the home starts over (and doesn't take the volume again).
        assert_eq!(
            f.data.inspect_home(USER, "steam").unwrap(),
            Home {
                state: DirState::Missing,
                migrated: true
            }
        );
    }

    #[test]
    fn deleting_with_no_users_yet_is_fine() {
        let f = fixture();
        assert!(!f.data.delete_user_data(USER, "steam").unwrap());
        assert_eq!(fs::read_dir(f.data.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_probe_shows_whether_the_root_is_writable() {
        let f = fixture();
        let name = f.data.create_probe().unwrap();
        assert!(f.data.path().join(&name).is_file());
        f.data.remove_probe(&name);
        assert!(!f.data.path().join(&name).exists());
        fs::set_permissions(f.data.path(), fs::Permissions::from_mode(0o555)).unwrap();
        // (root writes anywhere: the check only means something for others.)
        if fs::File::create(f.data.path().join("x")).is_err() {
            assert!(f.data.create_probe().is_err());
        }
        fs::set_permissions(f.data.path(), fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn dirs(entries: &[&str]) -> Result<BTreeMap<String, PathBuf>> {
        let entries: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        parse_shared_dirs(&entries, Path::new("/srv/cha-portal"))
    }

    #[test]
    fn shared_dirs_kept_elsewhere_are_templates_and_plain_paths() {
        assert!(dirs(&[]).unwrap().is_empty());
        assert!(dirs(&["", "  "]).unwrap().is_empty(), "unset in compose");
        let found = dirs(&["steam=/mnt/games/steam", " kde = /srv/other/kde/ "]).unwrap();
        assert_eq!(found["steam"], Path::new("/mnt/games/steam"));
        assert_eq!(found["kde"], Path::new("/srv/other/kde"));
        for bad in [
            "steam",
            "=/mnt/x",
            "steam=",
            "steam=relative/path",
            "steam=/",
            "steam=//",
            "steam=/mnt/../etc",
            "steam=/mnt/./x",
            "steam=/mnt//x",
            "Steam=/mnt/x",
            "../x=/mnt/x",
            // The data root's own, inside it, or around it.
            "steam=/srv/cha-portal",
            "steam=/srv/cha-portal/shared/steam",
            "steam=/srv",
        ] {
            assert!(dirs(&[bad]).is_err(), "{bad:?}");
        }
        // Not the same prefix by name only.
        assert!(dirs(&["steam=/srv/cha-portal-games/steam"]).is_ok());
        assert!(dirs(&["steam=/mnt/a", "steam=/mnt/b"]).is_err(), "twice");
    }

    fn external() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let share = dir.path().join("steam");
        fs::create_dir_all(share.join("steamapps/compatdata")).unwrap();
        fs::create_dir_all(share.join("steamapps/shadercache")).unwrap();
        (dir, share)
    }

    fn places() -> Vec<String> {
        vec![
            "steamapps/compatdata".to_string(),
            "steamapps/shadercache".to_string(),
        ]
    }

    #[test]
    fn an_external_dir_with_its_places_passes_and_nothing_is_made_in_it() {
        let (_dir, share) = external();
        let before: Vec<_> = walk(&share);
        assert!(check_external_dir(&share, &places()).is_ok());
        assert!(check_external_dir(&share, &[]).is_ok());
        assert_eq!(walk(&share), before, "looking changes nothing");
        assert!(missing_per_user(&share, &places()).is_empty());
    }

    fn walk(dir: &Path) -> Vec<(PathBuf, u32)> {
        let mut out = Vec::new();
        let mut todo = vec![dir.to_path_buf()];
        while let Some(d) = todo.pop() {
            for e in fs::read_dir(&d).unwrap().flatten() {
                let meta = fs::symlink_metadata(e.path()).unwrap();
                out.push((e.path(), meta.permissions().mode()));
                if meta.is_dir() {
                    todo.push(e.path());
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn an_external_dir_that_isnt_there_or_is_empty_is_refused_with_the_fix() {
        let (dir, share) = external();
        // Not there at all (the share isn't mounted, nor a mountpoint made).
        let err = check_external_dir(&dir.path().join("nope"), &places()).unwrap_err();
        assert!(
            format!("{err:#}").contains("is the share mounted"),
            "{err:#}"
        );
        // An empty local directory where the share should be.
        let empty = dir.path().join("empty");
        fs::create_dir(&empty).unwrap();
        let err = check_external_dir(&empty, &places()).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("has no steamapps/compatdata"), "{message}");
        assert!(
            message.contains(&format!(
                "mkdir -p {}/steamapps/compatdata",
                empty.display()
            )),
            "{message}"
        );
        assert!(!empty.join("steamapps").exists(), "and it made nothing");
        assert_eq!(missing_per_user(&empty, &places()), places());
        // One of two.
        fs::remove_dir(share.join("steamapps/shadercache")).unwrap();
        assert_eq!(
            missing_per_user(&share, &places()),
            ["steamapps/shadercache"]
        );
    }

    #[test]
    fn an_external_places_must_be_real_directories() {
        let (dir, share) = external();
        let outside = dir.path().join("outside");
        fs::create_dir_all(outside.join("compatdata")).unwrap();
        // The place itself a link (to a directory elsewhere on the server)...
        fs::remove_dir(share.join("steamapps/compatdata")).unwrap();
        symlink(
            outside.join("compatdata"),
            share.join("steamapps/compatdata"),
        )
        .unwrap();
        let err = check_external_dir(&share, &places()).unwrap_err();
        assert!(
            format!("{err:#}").contains("isn't a plain directory"),
            "{err:#}"
        );
        // ...or a directory on the way to it.
        fs::remove_file(share.join("steamapps/compatdata")).unwrap();
        fs::remove_dir(share.join("steamapps/shadercache")).unwrap();
        fs::remove_dir(share.join("steamapps")).unwrap();
        symlink(&outside, share.join("steamapps")).unwrap();
        fs::create_dir_all(outside.join("shadercache")).unwrap();
        let err = check_external_dir(&share, &places()).unwrap_err();
        assert!(
            format!("{err:#}").contains("isn't a plain directory"),
            "{err:#}"
        );
        // A file is no directory either.
        fs::remove_file(share.join("steamapps")).unwrap();
        fs::write(share.join("steamapps"), "x").unwrap();
        assert!(check_external_dir(&share, &places()).is_err());
        // The share itself may be reached through a link its owner made.
        let (_dir2, real) = external();
        let link = dir.path().join("via-link");
        symlink(&real, &link).unwrap();
        assert!(check_external_dir(&link, &places()).is_ok());
        // Bad per-user paths never get as far as the disk.
        assert!(check_external_dir(&real, &["../x".to_string()]).is_err());
    }

    #[test]
    fn stats_count_what_is_kept() {
        let f = fixture();
        assert_eq!(f.data.stats().user_dirs, 0);
        for (user, template) in [(USER, "steam"), (USER, "chrome"), (OTHER, "steam")] {
            f.data.ensure_home(user, template).unwrap();
        }
        f.data.ensure_shared("steam", &[]).unwrap();
        f.data.mark_migrated(USER, "steam", "v").unwrap();
        // Things that are not homes: a copy under way, and strays.
        f.data.begin_migration(OTHER, "chrome").unwrap();
        fs::create_dir(f.data.path().join("users/not-a-user")).unwrap();
        let stats = f.data.stats();
        assert_eq!((stats.user_dirs, stats.users, stats.shared_dirs), (3, 2, 1));
        assert!(stats.migrated.contains(&(USER.into(), "steam".into())));
        assert_eq!(stats.migrated.len(), 1);
        assert!(stats.free_bytes.is_some_and(|b| b > 0));
    }
}

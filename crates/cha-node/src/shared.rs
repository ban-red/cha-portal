//! Whether a shared directory the owner keeps outside the data root
//! (`CHA_SHARED_DIRS`, a NAS share) is usable by launches on this node. The
//! doctor describes the same look in its own words ([`crate::doctor`]); the
//! agent turns it into a [`SharedDirStatus`] for the portal's inventory.
//!
//! Everything here blocks (a hard NFS mount makes reads wait for the
//! server): callers run it with `spawn_blocking` and a timeout.

use std::path::Path;

use cha_wire::{SharedDirState, SharedDirStatus};

use crate::environments::APP_UID;

/// What looking at a shared directory found.
#[derive(Debug, Default)]
pub(crate) struct SharedLook {
    /// `Err` is why it can't be read.
    pub(crate) stat: Option<Result<SharedStat, String>>,
    /// The filesystem it is on: type and source, from the mount table.
    pub(crate) fs: Option<(String, String)>,
    /// Per-user places it doesn't have as real directories.
    pub(crate) missing: Vec<String>,
    /// Empty, and on the same device as its parent: likely a mountpoint that
    /// nothing is mounted on.
    pub(crate) looks_unmounted: bool,
}

#[derive(Debug)]
pub(crate) struct SharedStat {
    pub(crate) mode: u32,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
}

pub(crate) fn inspect_shared(dir: &Path, per_user: &[String]) -> SharedLook {
    use std::os::unix::fs::MetadataExt;
    let mut look = SharedLook::default();
    let meta = match std::fs::metadata(dir) {
        Ok(m) if m.is_dir() => m,
        Ok(_) => {
            look.stat = Some(Err("not a directory".into()));
            return look;
        }
        Err(e) => {
            look.stat = Some(Err(e.to_string()));
            return look;
        }
    };
    look.stat = Some(Ok(SharedStat {
        mode: meta.mode() & 0o7777,
        uid: meta.uid(),
        gid: meta.gid(),
    }));
    look.fs = std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|table| mount_for(&table, dir));
    look.missing = crate::storage::missing_per_user(dir, per_user);
    let empty = std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none());
    let same_device = dir
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|parent| parent.dev() == meta.dev());
    look.looks_unmounted = empty && same_device;
    look
}

/// Whether the user `uid`:`gid` can write a directory by its mode bits (a NAS
/// may map users, so this is what the permissions say, not a test).
pub(crate) fn can_write(stat: &SharedStat, uid: u32, gid: u32) -> bool {
    if stat.uid == uid {
        stat.mode & 0o200 != 0
    } else if stat.gid == gid {
        stat.mode & 0o020 != 0
    } else {
        stat.mode & 0o002 != 0
    }
}

/// The filesystem `path` is on, from a mount table (`/proc/self/mountinfo`):
/// its type and source, from the mount that covers it most closely. Each line
/// is `id parent major:minor root mount-point options [fields] - type source
/// super-options`, with spaces in the mount point written `\040`.
pub(crate) fn mount_for(table: &str, path: &Path) -> Option<(String, String)> {
    let mut best: Option<(usize, String, String)> = None;
    for line in table.lines() {
        let Some((head, tail)) = line.split_once(" - ") else {
            continue;
        };
        let Some(point) = head.split_whitespace().nth(4) else {
            continue;
        };
        let point = point.replace("\\040", " ");
        let mut tail = tail.split_whitespace();
        let (Some(fs), Some(source)) = (tail.next(), tail.next()) else {
            continue;
        };
        if path.starts_with(&point) && best.as_ref().is_none_or(|(len, ..)| point.len() >= *len) {
            best = Some((point.len(), fs.to_string(), source.replace("\\040", " ")));
        }
    }
    best.map(|(_, fs, source)| (fs, source))
}

/// What a look at `dir` means for launches, in the portal's terms. The worst
/// problem wins: the share isn't there, then a place is missing, then it
/// can't be written.
pub(crate) fn status_from(dir: &Path, look: &SharedLook, checked_at: i64) -> SharedDirStatus {
    let path = dir.display();
    let (fs_type, source) = match &look.fs {
        Some((fs, source)) => (Some(fs.clone()), Some(source.clone())),
        None => (None, None),
    };
    let made = |state, detail: Option<String>| SharedDirStatus {
        state,
        fs_type: fs_type.clone(),
        source: source.clone(),
        detail,
        checked_at,
    };
    let stat = match &look.stat {
        Some(Ok(stat)) => stat,
        Some(Err(why)) => {
            return made(
                SharedDirState::Missing,
                Some(format!(
                    "{path} can't be opened ({why}): is the share mounted on this node, and bound into the agent at the same path?"
                )),
            );
        }
        None => {
            return made(
                SharedDirState::Missing,
                Some(format!("{path} was not looked at")),
            );
        }
    };
    if look.looks_unmounted {
        return made(
            SharedDirState::Missing,
            Some(format!(
                "{path} is empty and on the same device as its parent: is the share mounted on this node?"
            )),
        );
    }
    if !look.missing.is_empty() {
        return made(
            SharedDirState::Incomplete,
            Some(format!(
                "{path} has no plain directory for {} (is the share really mounted? make them on the share)",
                look.missing.join(", ")
            )),
        );
    }
    if !can_write(stat, APP_UID, APP_UID) {
        return made(
            SharedDirState::ReadOnly,
            Some(format!(
                "uid {APP_UID} can't write {path} ({:o} {}:{}): give it write access on the server",
                stat.mode, stat.uid, stat.gid
            )),
        );
    }
    made(SharedDirState::Ok, None)
}

/// Looks at `dir` and says what it found. Blocks.
pub fn check_blocking(dir: &Path, per_user: &[String], checked_at: i64) -> SharedDirStatus {
    status_from(dir, &inspect_shared(dir, per_user), checked_at)
}

/// The check didn't answer: `why` (a sentence) goes to the portal as is.
pub fn unreachable(dir: &Path, why: &str, checked_at: i64) -> SharedDirStatus {
    SharedDirStatus {
        state: SharedDirState::Unreachable,
        fs_type: None,
        source: None,
        detail: Some(format!("{}: {why}", dir.display())),
        checked_at,
    }
}

/// Unix seconds now.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    const COMPAT: &str = "steamapps/compatdata";
    const SHADER: &str = "steamapps/shadercache";

    fn places() -> Vec<String> {
        vec![COMPAT.into(), SHADER.into()]
    }

    fn open_up(dir: &Path) {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o777)).unwrap();
    }

    fn status(dir: &Path) -> SharedDirStatus {
        check_blocking(dir, &places(), 7)
    }

    #[test]
    fn a_share_that_is_not_there_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let s = status(&tmp.path().join("nope"));
        assert_eq!(s.state, SharedDirState::Missing);
        assert!(s.detail.unwrap().contains("is the share mounted"));
        assert_eq!(s.checked_at, 7);
    }

    #[test]
    fn an_empty_mountpoint_is_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("steam");
        std::fs::create_dir(&dir).unwrap();
        assert_eq!(status(&dir).state, SharedDirState::Missing);
    }

    #[test]
    fn a_share_without_the_places_is_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("steam");
        std::fs::create_dir_all(dir.join(COMPAT)).unwrap();
        open_up(&dir);
        let s = status(&dir);
        assert_eq!(s.state, SharedDirState::Incomplete);
        assert!(s.detail.unwrap().contains(SHADER));
    }

    #[test]
    fn a_symlinked_place_is_incomplete() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("steam");
        std::fs::create_dir_all(dir.join("steamapps")).unwrap();
        std::fs::create_dir_all(dir.join(SHADER)).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.join(COMPAT)).unwrap();
        open_up(&dir);
        let s = status(&dir);
        assert_eq!(s.state, SharedDirState::Incomplete);
        assert!(s.detail.unwrap().contains(COMPAT));
    }

    fn complete(tmp: &Path) -> std::path::PathBuf {
        let dir = tmp.join("steam");
        std::fs::create_dir_all(dir.join(COMPAT)).unwrap();
        std::fs::create_dir_all(dir.join(SHADER)).unwrap();
        dir
    }

    #[test]
    fn a_share_apps_cannot_write_is_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = complete(tmp.path());
        // Owned by the test's user, and not group or world writable: not
        // writable by uid 1000 unless that is the test's user, so make the
        // test's own mode say no to everyone.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert_eq!(status(&dir).state, SharedDirState::ReadOnly);
    }

    #[test]
    fn a_complete_writable_share_is_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = complete(tmp.path());
        open_up(&dir);
        let s = status(&dir);
        assert_eq!(s.state, SharedDirState::Ok);
        assert_eq!(s.detail, None);
        // Without known places it only has to open and be writable.
        assert_eq!(check_blocking(&dir, &[], 1).state, SharedDirState::Ok);
    }

    #[test]
    fn unreachable_names_the_directory() {
        let s = unreachable(Path::new("/mnt/x"), "didn't answer within 10 s", 3);
        assert_eq!(s.state, SharedDirState::Unreachable);
        assert!(s.detail.unwrap().starts_with("/mnt/x: "));
    }
}

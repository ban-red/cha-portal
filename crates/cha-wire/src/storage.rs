//! Where an app's data lives on a node, and how the portal names it in an
//! environment's spec.
//!
//! A node keeps app data under one directory, its **data root** (`CHA_DATA_ROOT`,
//! [`DEFAULT_DATA_ROOT`] unless the owner sets another):
//!
//! ```text
//! <root>/users/<user id>/<template id>/          the app's home, when the user keeps its data
//! <root>/users/<user id>/<template id>.migrated  a legacy home volume was copied here once
//! <root>/shared/<template id>/                   data every user of the app shares
//! ```
//!
//! A node's owner may keep one template's shared directory elsewhere (Steam's
//! library on a NAS): the node maps `shared/<template id>` to it, and the
//! portal still only ever names the layout's path.
//!
//! The portal sends these as paths relative to the root; the node checks them
//! against the layout ([`Storage::check`]) and refuses anything else, so a
//! bug or a hostile portal can't point an app at, say, another user's data
//! or the node's own state.

use serde::{Deserialize, Serialize};

use crate::{home_volume_name, is_home_volume_name};

/// The data root unless the node's owner sets another.
pub const DEFAULT_DATA_ROOT: &str = "/srv/cha-portal";
/// Where an app finds the shared data of its template inside its container,
/// whatever the node's data root is: `<this>/<template id>`. (A shared
/// directory the owner keeps elsewhere is mounted at its own path instead, and
/// the app is told it in `CHA_SHARED_DIR`.)
pub const SHARED_MOUNT_ROOT: &str = "/srv/cha-portal/shared";
/// Under the root: every user's directories.
pub const USERS_DIR: &str = "users";
/// Under the root: the data apps share across users.
pub const SHARED_DIR: &str = "shared";
/// In a user's app directory: that user's own copies of the parts of the
/// shared directory that mustn't be shared ([`Shared::per_user`]), at the same
/// relative paths.
pub const PER_USER_DIR: &str = ".cha-shared";

/// An environment's persistent data: what the node mounts into the app.
/// All paths are relative to the node's data root.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Storage {
    /// The user's directory for this app, `users/<user id>/<template id>`
    /// ([`user_dir`]): mounted as the app's home when the user keeps its data.
    /// Without it the home is the container's own and goes with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// Data shared across users, when the admin allows the app any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<Shared>,
    /// The Docker volume that held this user's home before data moved under
    /// the root ([`home_volume_name`]). With `home`: if the directory is
    /// new and the volume exists, the node copies it in first (once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_volume: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Shared {
    /// `shared/<template id>` ([`shared_dir`]), mounted in the container at
    /// `SHARED_MOUNT_ROOT/<template id>`, or where the node's owner keeps it.
    pub path: String,
    /// Read-write; otherwise the mount is read-only.
    pub writable: bool,
    /// Paths inside the shared directory (relative to it) that each user has
    /// a copy of: one directory per user is laid over each, so what an app
    /// keeps there, a Proton prefix say, doesn't mix between users. They live
    /// under the user's app directory in [`PER_USER_DIR`], so they need the
    /// user to keep their data: [`Storage::check`] refuses them without a
    /// `home`, and the portal doesn't share such a directory with a user who
    /// keeps nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_user: Vec<String>,
}

/// User ids are the portal's UUIDs: lowercase, hyphenated.
pub fn valid_user_id(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

/// Template ids are catalog slugs: lowercase letters, digits and inner
/// hyphens, at most 32 characters.
pub fn valid_template_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 32
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && b[0] != b'-'
        && b[b.len() - 1] != b'-'
}

/// A path inside a directory the node doesn't own: relative, a few components
/// of plain characters, never `.` or `..`.
pub fn valid_relative_path(path: &str) -> bool {
    const MAX_DEPTH: usize = 8;
    const MAX_COMPONENT: usize = 64;
    let parts: Vec<&str> = path.split('/').collect();
    !parts.is_empty()
        && parts.len() <= MAX_DEPTH
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.len() <= MAX_COMPONENT
                && *p != "."
                && *p != ".."
                && p.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        })
}

/// `users/<user id>/<template id>`.
pub fn user_dir(user: &str, template: &str) -> String {
    format!("{USERS_DIR}/{user}/{template}")
}

/// `shared/<template id>`.
pub fn shared_dir(template: &str) -> String {
    format!("{SHARED_DIR}/{template}")
}

/// The ids in a `users/<user id>/<template id>` path, if it is one.
pub fn parse_user_dir(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix(USERS_DIR)?.strip_prefix('/')?;
    let (user, template) = rest.split_once('/')?;
    (valid_user_id(user) && valid_template_id(template)).then_some((user, template))
}

/// The template id in a `shared/<template id>` path, if it is one.
pub fn parse_shared_dir(path: &str) -> Option<&str> {
    let template = path.strip_prefix(SHARED_DIR)?.strip_prefix('/')?;
    valid_template_id(template).then_some(template)
}

impl Storage {
    /// Whether this is something the node may set up for `owner`'s launch of
    /// `template`: every path is the layout's, for these ids and no others.
    pub fn check(&self, owner: &str, template: &str) -> Result<(), String> {
        if !valid_template_id(template) {
            return Err(format!("{template:?} isn't a template id"));
        }
        if let Some(home) = &self.home {
            if !valid_user_id(owner) {
                return Err(format!("{owner:?} isn't a user id"));
            }
            if home != &user_dir(owner, template) {
                return Err(format!(
                    "{home:?} isn't this user's directory ({})",
                    user_dir(owner, template)
                ));
            }
        }
        if let Some(shared) = &self.shared {
            if shared.path != shared_dir(template) {
                return Err(format!(
                    "{:?} isn't this template's shared directory ({})",
                    shared.path,
                    shared_dir(template)
                ));
            }
            if !shared.per_user.is_empty() && self.home.is_none() {
                return Err(
                    "per-user paths in the shared directory need a home to keep them in".into(),
                );
            }
            let mut paths = shared.per_user.clone();
            paths.sort();
            for (i, p) in paths.iter().enumerate() {
                if !valid_relative_path(p) {
                    return Err(format!("{p:?} isn't a path inside the shared directory"));
                }
                // One inside another would be a mount laid over a mount.
                if paths
                    .get(i + 1)
                    .is_some_and(|next| next.starts_with(&format!("{p}/")) || next == p)
                {
                    return Err(format!("per-user paths can't nest or repeat ({p:?})"));
                }
            }
        }
        if let Some(volume) = &self.legacy_volume {
            if self.home.is_none() {
                return Err("a legacy volume without a home to move it to".into());
            }
            if !is_home_volume_name(volume) || volume != &home_volume_name(owner, template) {
                return Err(format!(
                    "{volume:?} isn't this user's legacy home volume ({})",
                    home_volume_name(owner, template)
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "01a10527-f79f-761b-962f-4b26924a2e68";

    fn steam() -> Storage {
        Storage {
            home: Some(user_dir(USER, "steam")),
            shared: Some(Shared {
                path: shared_dir("steam"),
                writable: true,
                per_user: vec![
                    "steamapps/compatdata".into(),
                    "steamapps/shadercache".into(),
                ],
            }),
            legacy_volume: Some(home_volume_name(USER, "steam")),
        }
    }

    #[test]
    fn ids_are_what_the_portal_makes() {
        assert!(valid_user_id(USER));
        assert!(valid_user_id(&uuid_like('a')));
        for bad in [
            "",
            "..",
            "../x",
            "01A10527-F79F-761B-962F-4B26924A2E68",
            "01a10527-f79f-761b-962f-4b26924a2e6",
            "01a10527-f79f-761b-962f-4b26924a2e68/x",
            "01a10527_f79f_761b_962f_4b26924a2e68",
            "01a10527-f79f-761b-962f-4b26924a2e6g",
        ] {
            assert!(!valid_user_id(bad), "{bad:?}");
        }
        for ok in ["steam", "test-pattern", "kde", "a", "xfce4"] {
            assert!(valid_template_id(ok), "{ok:?}");
        }
        for bad in [
            "",
            "-x",
            "x-",
            "..",
            "a/b",
            "Steam",
            "a.b",
            "a b",
            "é",
            &"a".repeat(33),
        ] {
            assert!(!valid_template_id(bad), "{bad:?}");
        }
    }

    fn uuid_like(c: char) -> String {
        let hex = |n: usize| c.to_string().repeat(n);
        format!("{}-{}-{}-{}-{}", hex(8), hex(4), hex(4), hex(4), hex(12))
    }

    #[test]
    fn relative_paths_stay_inside() {
        assert!(valid_relative_path("library"));
        assert!(valid_relative_path("library/steamapps/compatdata"));
        assert!(valid_relative_path(".hidden/a-b_c.d"));
        for bad in [
            "",
            "/abs",
            "a//b",
            "a/",
            "./a",
            "a/./b",
            "../a",
            "a/../b",
            "a/..",
            "a b",
            "a\\b",
            "a\0b",
            "1/2/3/4/5/6/7/8/9",
        ] {
            assert!(!valid_relative_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn paths_parse_back_to_their_ids() {
        assert_eq!(
            parse_user_dir(&user_dir(USER, "steam")),
            Some((USER, "steam"))
        );
        assert_eq!(parse_shared_dir(&shared_dir("steam")), Some("steam"));
        for bad in [
            "users",
            "users/x/steam",
            "users/01a10527-f79f-761b-962f-4b26924a2e68",
            "users/01a10527-f79f-761b-962f-4b26924a2e68/..",
            "users/01a10527-f79f-761b-962f-4b26924a2e68/steam/x",
            "/users/01a10527-f79f-761b-962f-4b26924a2e68/steam",
        ] {
            assert_eq!(parse_user_dir(bad), None, "{bad:?}");
        }
        assert_eq!(parse_shared_dir("shared/../etc"), None);
        assert_eq!(parse_shared_dir("shared/a/b"), None);
    }

    #[test]
    fn the_portals_storage_passes_its_own_check() {
        assert_eq!(steam().check(USER, "steam"), Ok(()));
        assert_eq!(Storage::default().check(USER, "steam"), Ok(()));
        // Shared without a home is fine for a directory with nothing per-user.
        let mut shared_only = Storage {
            home: None,
            legacy_volume: None,
            ..steam()
        };
        shared_only.shared.as_mut().unwrap().per_user.clear();
        assert_eq!(shared_only.check("", "steam"), Ok(()));
        // Per-user parts have to live somewhere.
        shared_only.shared.as_mut().unwrap().per_user = vec!["steamapps/compatdata".into()];
        assert!(shared_only.check("", "steam").is_err());
    }

    #[test]
    fn storage_for_someone_else_or_somewhere_else_is_refused() {
        let other = uuid_like('b');
        assert!(steam().check(&other, "steam").is_err(), "another user's");
        assert!(steam().check(USER, "chrome").is_err(), "another template's");
        assert!(steam().check("..", "steam").is_err());
        assert!(steam().check(USER, "../steam").is_err());

        for home in [
            "users/../steam",
            "/srv/cha-portal/users/x/steam",
            "cha-node_state",
            "users/01a10527-f79f-761b-962f-4b26924a2e68/steam/../../..",
        ] {
            let s = Storage {
                home: Some(home.into()),
                ..steam()
            };
            assert!(s.check(USER, "steam").is_err(), "{home:?}");
        }
        let mut s = steam();
        s.shared.as_mut().unwrap().path = "shared/../users".into();
        assert!(s.check(USER, "steam").is_err());
        let mut s = steam();
        s.shared.as_mut().unwrap().path = "shared/chrome".into();
        assert!(s.check(USER, "steam").is_err());
    }

    #[test]
    fn per_user_paths_are_checked_and_can_not_nest() {
        for bad in ["../x", "/etc", "a/../..", "", "a//b"] {
            let mut s = steam();
            s.shared.as_mut().unwrap().per_user = vec![bad.into()];
            assert!(s.check(USER, "steam").is_err(), "{bad:?}");
        }
        let mut s = steam();
        s.shared.as_mut().unwrap().per_user = vec!["a".into(), "a/b".into()];
        assert!(s.check(USER, "steam").is_err());
        s.shared.as_mut().unwrap().per_user = vec!["a".into(), "a".into()];
        assert!(s.check(USER, "steam").is_err());
        // Siblings that share a prefix aren't nested.
        s.shared.as_mut().unwrap().per_user = vec!["a".into(), "ab".into(), "a-b/c".into()];
        assert_eq!(s.check(USER, "steam"), Ok(()));
    }

    #[test]
    fn a_legacy_volume_must_be_this_users_and_have_a_home() {
        let mut s = steam();
        s.legacy_volume = Some(home_volume_name(&uuid_like('c'), "steam"));
        assert!(s.check(USER, "steam").is_err());
        s.legacy_volume = Some("cha-node_state".into());
        assert!(s.check(USER, "steam").is_err());
        let mut s = steam();
        s.home = None;
        assert!(s.check(USER, "steam").is_err());
        let s = Storage {
            legacy_volume: Some(home_volume_name(USER, "steam")),
            ..Storage::default()
        };
        assert!(s.check(USER, "steam").is_err());
    }
}

//! The host's identity: a certificate and key that paired clients pin, and
//! the `uniqueid` clients remember the host by. They live under
//! `<data root>/node/gamestream/` (root, 0700; the key 0600), made the first
//! time and kept, so a restart is the same host to every client.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use cha_gamestream::{Identity, generate_unique_id};
use tracing::info;

const UNIQUE_ID: &str = "unique_id";

/// Where the host's files are.
pub fn dir(data_root: &Path) -> PathBuf {
    data_root.join("node").join("gamestream")
}

pub struct HostIdentity {
    pub identity: Identity,
    /// 32 hex digits.
    pub unique_id: String,
}

/// Creates `path` and what is missing above it, owner-only.
fn make_dir(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(path)
        .with_context(|| format!("creating {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Made by something else, or by an older agent: it is ours to guard.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing {}", path.display()))?;
    }
    Ok(())
}

fn valid_unique_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reads the identity from `dir`, or makes it. Generating a key takes a
/// moment: call this off the async threads.
pub fn load_or_create(dir: &Path) -> Result<HostIdentity> {
    make_dir(dir)?;
    let identity = Identity::load_or_create(dir)
        .map_err(|e| anyhow::anyhow!("the host's certificate in {}: {e}", dir.display()))?;
    let path = dir.join(UNIQUE_ID);
    let unique_id = match std::fs::read_to_string(&path) {
        Ok(text) if valid_unique_id(text.trim()) => text.trim().to_ascii_uppercase(),
        Ok(_) => bail!(
            "{} isn't a host id (32 hex digits); delete it to get a new one, \
             and clients will see a new host",
            path.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let id = generate_unique_id();
            write_private(&path, &id).with_context(|| format!("writing {}", path.display()))?;
            info!(dir = %dir.display(), "made this node's GameStream host identity");
            id
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    Ok(HostIdentity {
        identity,
        unique_id,
    })
}

fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_is_made_once_kept_and_guarded() {
        let root = tempfile::tempdir().unwrap();
        let dir = dir(root.path());
        let first = load_or_create(&dir).unwrap();
        let second = load_or_create(&dir).unwrap();
        assert_eq!(
            first.unique_id, second.unique_id,
            "the same host after a restart"
        );
        assert_eq!(first.identity.fingerprint(), second.identity.fingerprint());
        assert!(valid_unique_id(&first.unique_id));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(
                mode(dir.parent().unwrap()),
                0o700,
                "the directory above is made owner-only too"
            );
            assert_eq!(mode(&dir.join("key.pem")), 0o600);
            assert_eq!(mode(&dir.join(UNIQUE_ID)), 0o600);
        }
    }

    #[test]
    fn a_directory_left_open_is_closed_and_a_bad_id_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        let dir = dir(root.path());
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        load_or_create(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        std::fs::write(dir.join(UNIQUE_ID), "not an id").unwrap();
        let err = load_or_create(&dir)
            .err()
            .expect("a bad id is an error")
            .to_string();
        assert!(err.contains("isn't a host id"), "{err}");
    }
}

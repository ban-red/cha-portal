//! What this install keeps about portals: one `portals.json` under the
//! player's data directory (0600, written atomically), holding the install id
//! and, per portal origin, the device token and who it signs in as.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const FILE: &str = "portals.json";

/// A portal this install knows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalEntry {
    /// `None` for a portal added but not signed in to.
    pub token: Option<String>,
    pub device_id: Option<String>,
    pub username: Option<String>,
    pub role: Option<String>,
}

impl PortalEntry {
    pub fn signed_in(&self) -> bool {
        self.token.is_some()
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Data {
    install_id: String,
    #[serde(default)]
    portals: BTreeMap<String, PortalEntry>,
}

pub struct PortalStore {
    path: PathBuf,
    data: Mutex<Data>,
}

impl PortalStore {
    /// Reads `portals.json` under `dir` (made, owner-only, if missing), making
    /// the install id the first time.
    pub fn open(dir: &Path) -> Result<Self> {
        make_dir(dir)?;
        let path = dir.join(FILE);
        let mut data: Data = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("reading {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let fresh = data.install_id.is_empty();
        if fresh {
            data.install_id = uuid::Uuid::new_v4().to_string();
        }
        let store = Self {
            path,
            data: Mutex::new(data),
        };
        if fresh {
            store.save(&store.data.lock().expect("store lock"))?;
        }
        Ok(store)
    }

    /// The UUID this install signs in as, made once.
    pub fn install_id(&self) -> String {
        self.data.lock().expect("store lock").install_id.clone()
    }

    /// Every known portal origin with what is kept for it.
    pub fn portals(&self) -> Vec<(String, PortalEntry)> {
        let data = self.data.lock().expect("store lock");
        data.portals
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    pub fn get(&self, origin: &str) -> Option<PortalEntry> {
        self.data
            .lock()
            .expect("store lock")
            .portals
            .get(origin)
            .cloned()
    }

    /// Remembers a portal without signing in to it (a no-op if known).
    pub fn add(&self, origin: &str) -> Result<()> {
        let mut data = self.data.lock().expect("store lock");
        if data.portals.contains_key(origin) {
            return Ok(());
        }
        data.portals
            .insert(origin.to_string(), PortalEntry::default());
        self.save(&data)
    }

    pub fn set(&self, origin: &str, entry: PortalEntry) -> Result<()> {
        let mut data = self.data.lock().expect("store lock");
        data.portals.insert(origin.to_string(), entry);
        self.save(&data)
    }

    /// Forgets the token for `origin` and keeps the portal listed.
    pub fn sign_out(&self, origin: &str) -> Result<()> {
        let mut data = self.data.lock().expect("store lock");
        if let Some(entry) = data.portals.get_mut(origin) {
            *entry = PortalEntry::default();
            self.save(&data)?;
        }
        Ok(())
    }

    fn save(&self, data: &Data) -> Result<()> {
        write_private(&self.path, &serde_json::to_vec_pretty(data)?)
    }
}

fn make_dir(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(path)
        .with_context(|| format!("creating {}", path.display()))
}

/// Writes `bytes` to `path` owner-only (0600) by way of a temporary file.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> PortalEntry {
        PortalEntry {
            token: Some("chadev_x".into()),
            device_id: Some("d1".into()),
            username: Some("alex".into()),
            role: Some("admin".into()),
        }
    }

    #[test]
    fn what_is_saved_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let store = PortalStore::open(dir.path()).unwrap();
        store.set("https://a.example", entry()).unwrap();
        store.add("https://b.example").unwrap();
        drop(store);
        let store = PortalStore::open(dir.path()).unwrap();
        assert_eq!(store.get("https://a.example"), Some(entry()));
        assert_eq!(store.get("https://b.example"), Some(PortalEntry::default()));
        assert_eq!(store.get("https://c.example"), None);
        assert_eq!(store.portals().len(), 2);
    }

    #[test]
    fn the_install_id_is_made_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = PortalStore::open(dir.path()).unwrap().install_id();
        assert_eq!(first.len(), 36);
        assert_eq!(PortalStore::open(dir.path()).unwrap().install_id(), first);
    }

    #[test]
    fn signing_out_forgets_the_token_and_keeps_the_portal() {
        let dir = tempfile::tempdir().unwrap();
        let store = PortalStore::open(dir.path()).unwrap();
        store.set("https://a.example", entry()).unwrap();
        store.sign_out("https://a.example").unwrap();
        let again = PortalStore::open(dir.path()).unwrap();
        assert_eq!(again.get("https://a.example"), Some(PortalEntry::default()));
        assert!(
            !std::fs::read_to_string(dir.path().join(FILE))
                .unwrap()
                .contains("chadev_")
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = PortalStore::open(dir.path()).unwrap();
        store.set("https://a.example", entry()).unwrap();
        let mode = std::fs::metadata(dir.path().join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

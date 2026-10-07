//! What this install keeps on disk, under the directory the app gives:
//! `client-cert.pem` and `client-key.pem` (the one identity every host
//! pairs with), `hosts/<unique id>/server-cert.pem` (each paired host's
//! certificate) and `hosts.json` (the hosts to show before they answer).
//! Directories are 0700, files 0600.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use moonlight_common::crypto::rustcrypto::RustCryptoBackend;
use moonlight_common::http::pair::PairingCryptoBackend;
use moonlight_common::http::{ClientIdentifier, ClientSecret, ServerIdentifier};
use pem::Pem;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const CLIENT_CERT: &str = "client-cert.pem";
const CLIENT_KEY: &str = "client-key.pem";
const SERVER_CERT: &str = "server-cert.pem";
const HOSTS: &str = "hosts.json";

/// A host worth remembering: paired, or added by hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedHost {
    pub id: String,
    pub name: String,
    /// As the library takes it: a name, an IPv4 address or a bracketed IPv6 one.
    pub address: String,
    pub port: u16,
    pub added: bool,
    pub paired: bool,
}

/// Whether `id` is a host's `uniqueid` as we write it (hex digits): it names a
/// directory, so nothing else will do.
pub fn valid_unique_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

pub struct Store {
    dir: PathBuf,
    /// Two first uses at once must not make two identities.
    identity: Mutex<()>,
}

impl Store {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            identity: Mutex::new(()),
        }
    }

    fn host_dir(&self, unique_id: &str) -> PathBuf {
        self.dir.join("hosts").join(unique_id)
    }

    /// Creates `path` and what's missing above it, owner-only.
    fn make_dir(path: &Path) -> Result<()> {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .create(path)
            .with_context(|| format!("creating {}", path.display()))
    }

    /// Writes `bytes` to `path` owner-only (0600), replacing what is there.
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

    fn read_pem(path: &Path) -> Result<Pem> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Pem::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// This install's client identity, generated the first time it's needed
    /// (RSA key generation: take it off the async threads).
    pub fn client_identity(&self) -> Result<(ClientIdentifier, ClientSecret)> {
        let _once = self.identity.lock().expect("identity lock");
        let cert = self.dir.join(CLIENT_CERT);
        let key = self.dir.join(CLIENT_KEY);
        if cert.exists() && key.exists() {
            return Ok((
                ClientIdentifier::from_pem(Self::read_pem(&cert)?),
                ClientSecret::from_pem(Self::read_pem(&key)?),
            ));
        }
        Self::make_dir(&self.dir)?;
        let (client, secret) = RustCryptoBackend
            .generate_client_identity()
            .map_err(|e| anyhow!("generating the client identity: {e:?}"))?;
        // The key first: a certificate without its key is never read back.
        Self::write_private(&key, secret.to_pem().to_string().as_bytes())?;
        Self::write_private(&cert, client.to_pem().to_string().as_bytes())?;
        info!(dir = %self.dir.display(), "made this install's Moonlight client identity");
        Ok((client, secret))
    }

    /// The certificate a paired host showed, if this install is paired with it.
    pub fn server_identity(&self, unique_id: &str) -> Result<Option<ServerIdentifier>> {
        if !valid_unique_id(unique_id) {
            bail!("{unique_id:?} isn't a Moonlight host id");
        }
        let path = self.host_dir(unique_id).join(SERVER_CERT);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(ServerIdentifier::from_pem(Self::read_pem(&path)?)))
    }

    pub fn save_server(&self, unique_id: &str, server: &ServerIdentifier) -> Result<()> {
        if !valid_unique_id(unique_id) {
            bail!("{unique_id:?} isn't a Moonlight host id");
        }
        let dir = self.host_dir(unique_id);
        Self::make_dir(&dir)?;
        Self::write_private(
            &dir.join(SERVER_CERT),
            server.to_pem().to_string().as_bytes(),
        )
    }

    /// The hosts saved last time; none if the file is missing or unreadable
    /// (the next save replaces it).
    pub fn load_hosts(&self) -> Vec<SavedHost> {
        let path = self.dir.join(HOSTS);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        match serde_json::from_str::<Vec<SavedHost>>(&text) {
            Ok(hosts) => hosts
                .into_iter()
                .filter(|h| valid_unique_id(&h.id))
                .collect(),
            Err(err) => {
                warn!("ignoring {}: {err}", path.display());
                Vec::new()
            }
        }
    }

    pub fn save_hosts(&self, hosts: &[SavedHost]) -> Result<()> {
        Self::make_dir(&self.dir)?;
        Self::write_private(&self.dir.join(HOSTS), &serde_json::to_vec_pretty(hosts)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cha-client-gs-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_identity_is_made_once_and_read_back() {
        let dir = temp_dir("identity");
        let store = Store::new(dir.clone());
        let (cert, _) = store.client_identity().unwrap();
        // A second store over the same directory (a restart) gets the same one.
        let (again, _) = Store::new(dir.clone()).client_identity().unwrap();
        assert_eq!(cert.to_pem().to_string(), again.to_pem().to_string());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.join(CLIENT_KEY)), 0o600);
            assert_eq!(mode(&dir.join(CLIENT_CERT)), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_hosts_certificate_is_kept_by_its_id() {
        let dir = temp_dir("server");
        let store = Store::new(dir.clone());
        assert!(store.server_identity("ABCD").unwrap().is_none());
        let (cert, _) = RustCryptoBackend.generate_client_identity().unwrap();
        let server = ServerIdentifier::from_pem(cert.to_pem());
        store.save_server("ABCD", &server).unwrap();
        let back = Store::new(dir.clone())
            .server_identity("ABCD")
            .unwrap()
            .expect("saved");
        assert_eq!(back.to_pem().to_string(), server.to_pem().to_string());
        // An id that could climb out of the directory is refused.
        assert!(store.server_identity("../x").is_err());
        assert!(store.save_server("../x", &server).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file = dir.join("hosts/ABCD").join(SERVER_CERT);
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn saved_hosts_survive_and_junk_is_ignored() {
        let dir = temp_dir("hosts");
        let store = Store::new(dir.clone());
        assert!(store.load_hosts().is_empty());
        let host = SavedHost {
            id: "ABCDEF".into(),
            name: "Desk".into(),
            address: "192.168.1.20".into(),
            port: 47989,
            added: true,
            paired: false,
        };
        store.save_hosts(std::slice::from_ref(&host)).unwrap();
        assert_eq!(Store::new(dir.clone()).load_hosts(), [host]);
        std::fs::write(dir.join(HOSTS), "not json").unwrap();
        assert!(store.load_hosts().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

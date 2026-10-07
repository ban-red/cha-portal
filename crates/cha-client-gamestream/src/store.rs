//! What this install keeps on disk, under the directory the app gives:
//! `client-cert.pem` and `client-key.pem` (the one identity every host
//! pairs with), `hosts/<unique id>/server-cert.pem` (each paired host's
//! certificate) and `hosts.json` (the hosts to show before they answer).
//! Directories are 0700, files 0600.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use cha_gamestream::client::front::ClientIdentity;
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
    /// A name, an IPv4 address or a bracketed IPv6 one.
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

    fn read_text(path: &Path) -> Result<String> {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
    }

    /// This install's client identity, generated the first time it's needed
    /// (RSA key generation: take it off the async threads).
    pub fn client_identity(&self) -> Result<ClientIdentity> {
        let _once = self.identity.lock().expect("identity lock");
        let cert = self.dir.join(CLIENT_CERT);
        let key = self.dir.join(CLIENT_KEY);
        if cert.exists() && key.exists() {
            return ClientIdentity::from_pem(&Self::read_text(&cert)?, &Self::read_text(&key)?)
                .with_context(|| format!("reading {} and {}", cert.display(), key.display()));
        }
        Self::make_dir(&self.dir)?;
        let identity = ClientIdentity::generate()
            .map_err(|e| anyhow!("generating the client identity: {e}"))?;
        // The key first: a certificate without its key is never read back.
        Self::write_private(&key, identity.key_pem().as_bytes())?;
        Self::write_private(&cert, identity.cert_pem().as_bytes())?;
        info!(dir = %self.dir.display(), "made this install's Moonlight client identity");
        Ok(identity)
    }

    /// The certificate (PEM) a paired host showed, if this install is paired with it.
    pub fn server_cert(&self, unique_id: &str) -> Result<Option<String>> {
        if !valid_unique_id(unique_id) {
            bail!("{unique_id:?} isn't a Moonlight host id");
        }
        let path = self.host_dir(unique_id).join(SERVER_CERT);
        if !path.exists() {
            return Ok(None);
        }
        Self::read_text(&path).map(Some)
    }

    pub fn save_server(&self, unique_id: &str, pem: &str) -> Result<()> {
        if !valid_unique_id(unique_id) {
            bail!("{unique_id:?} isn't a Moonlight host id");
        }
        let dir = self.host_dir(unique_id);
        Self::make_dir(&dir)?;
        Self::write_private(&dir.join(SERVER_CERT), pem.as_bytes())
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
        let identity = store.client_identity().unwrap();
        // A second store over the same directory (a restart) gets the same one.
        let again = Store::new(dir.clone()).client_identity().unwrap();
        assert_eq!(identity.fingerprint(), again.fingerprint());
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
        assert!(store.server_cert("ABCD").unwrap().is_none());
        let server = ClientIdentity::generate().unwrap().cert_pem().to_owned();
        store.save_server("ABCD", &server).unwrap();
        let back = Store::new(dir.clone())
            .server_cert("ABCD")
            .unwrap()
            .expect("saved");
        assert_eq!(back, server);
        // An id that could climb out of the directory is refused.
        assert!(store.server_cert("../x").is_err());
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

    /// Installs paired before our own client wrote these files with another
    /// library: PKCS#8 "PRIVATE KEY" and "CERTIFICATE" blocks, CRLF line ends.
    #[test]
    fn an_identity_written_by_the_old_library_still_loads() {
        let dir = temp_dir("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        let made = ClientIdentity::generate().unwrap();
        let crlf = |pem: &str| pem.replace('\n', "\r\n");
        std::fs::write(dir.join(CLIENT_CERT), crlf(made.cert_pem())).unwrap();
        std::fs::write(dir.join(CLIENT_KEY), crlf(made.key_pem())).unwrap();
        let loaded = Store::new(dir.clone()).client_identity().unwrap();
        assert_eq!(loaded.fingerprint(), made.fingerprint());
        // The host's certificate comes back as written, and pins.
        let server = crlf(ClientIdentity::generate().unwrap().cert_pem());
        Store::new(dir.clone())
            .save_server("AB12", &server)
            .unwrap();
        let back = Store::new(dir.clone())
            .server_cert("AB12")
            .unwrap()
            .unwrap();
        assert_eq!(back, server);
        cha_gamestream::client::front::HostClient::new("127.0.0.1", loaded)
            .unwrap()
            .with_server_cert(&back)
            .expect("the CRLF certificate pins");
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

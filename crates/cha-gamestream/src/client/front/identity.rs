//! The client's certificate and key: what a host pins when we pair, and what
//! we present over HTTPS afterwards.

use std::path::Path;

use crate::front::identity::{Identity, IdentityError};
use crate::handoff::ClientId;

/// A self-signed RSA-2048 certificate like the ones Moonlight clients make
/// (the host knows a client by its fingerprint only). Keep it for as long as
/// the client should stay paired.
#[derive(Clone)]
pub struct ClientIdentity(Identity);

impl ClientIdentity {
    pub fn generate() -> Result<Self, IdentityError> {
        Identity::generate().map(Self)
    }

    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Self, IdentityError> {
        Identity::from_pem(cert_pem, key_pem).map(Self)
    }

    /// Reads `cert.pem` and `key.pem` from `dir`.
    pub fn load(dir: &Path) -> Result<Self, IdentityError> {
        Identity::load(dir).map(Self)
    }

    /// Writes `cert.pem` and `key.pem` to `dir` (the key readable by its owner only).
    pub fn save(&self, dir: &Path) -> Result<(), IdentityError> {
        self.0.save(dir)
    }

    /// Loads what is in `dir`, or makes an identity and saves it there.
    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        Identity::load_or_create(dir).map(Self)
    }

    pub fn cert_pem(&self) -> &str {
        self.0.cert_pem()
    }
    pub fn key_pem(&self) -> &str {
        self.0.key_pem()
    }
    pub fn cert_der(&self) -> &[u8] {
        self.0.cert_der()
    }
    /// SHA-256 of the certificate, lower-case hex.
    pub fn fingerprint(&self) -> String {
        self.0.fingerprint()
    }
    /// The name the host's pairing store knows this client by.
    pub fn client_id(&self) -> ClientId {
        ClientId(self.fingerprint())
    }
    /// A `uniqueid` for requests: 16 upper-case hex digits of the fingerprint,
    /// so every identity has its own (Moonlight's fixed `0123456789ABCDEF`
    /// makes hosts that key on it mix clients up).
    pub fn default_unique_id(&self) -> String {
        self.fingerprint()[..16].to_ascii_uppercase()
    }
}

impl std::fmt::Debug for ClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIdentity")
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identity_is_saved_loaded_and_named_by_its_certificate() {
        let id = ClientIdentity::generate().unwrap();
        let dir = std::env::temp_dir().join(format!("cha-client-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        id.save(&dir).unwrap();
        let again = ClientIdentity::load(&dir).unwrap();
        assert_eq!(again.fingerprint(), id.fingerprint());
        assert_eq!(id.client_id().fingerprint(), id.fingerprint());
        assert_eq!(id.default_unique_id().len(), 16);
        assert!(ClientIdentity::load(&dir.join("none")).is_err());
        assert_eq!(
            ClientIdentity::load_or_create(&dir).unwrap().fingerprint(),
            id.fingerprint()
        );
        assert!(!format!("{id:?}").contains("PRIVATE"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

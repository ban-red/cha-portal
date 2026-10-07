// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: the key pair comes from aws-lc-rs (fast in debug builds), the identity is a value with its
// fingerprint rather than two paths, and the private key file is written readable by its owner only.

//! The host's certificate and key: what paired clients pin, and what the
//! pairing handshake signs with.

use std::path::Path;
use std::time::{Duration, SystemTime};

use aws_lc_rs::encoding::AsDer;
use aws_lc_rs::rsa::{KeySize, PrivateDecryptingKey};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair, KeyUsagePurpose,
    PKCS_RSA_SHA256,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("generating the host key: {0}")]
    Generate(String),
    #[error("the certificate or key isn't usable: {0}")]
    Invalid(String),
    #[error("{path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

/// The host's TLS certificate (self-signed RSA-2048, ten years) and key.
#[derive(Clone)]
pub struct Identity {
    cert_pem: String,
    key_pem: String,
    cert_der: Vec<u8>,
}

impl Identity {
    pub fn generate() -> Result<Self, IdentityError> {
        let rsa = PrivateDecryptingKey::generate(KeySize::Rsa2048)
            .map_err(|e| IdentityError::Generate(e.to_string()))?;
        let pkcs8 = AsDer::<aws_lc_rs::encoding::Pkcs8V1Der>::as_der(&rsa)
            .map_err(|e| IdentityError::Generate(e.to_string()))?;
        let key_pair = KeyPair::from_pkcs8_der_and_sign_algo(
            &PrivatePkcs8KeyDer::from(pkcs8.as_ref().to_vec()),
            &PKCS_RSA_SHA256,
        )
        .map_err(|e| IdentityError::Generate(e.to_string()))?;

        let mut params = CertificateParams::default();
        let now = SystemTime::now();
        params.not_before = (now - Duration::from_secs(24 * 3600)).into();
        params.not_after = (now + Duration::from_secs(3650 * 24 * 3600)).into();
        let mut name = DistinguishedName::new();
        name.push(DnType::CommonName, "NVIDIA GameStream");
        params.distinguished_name = name;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
            KeyUsagePurpose::KeyAgreement,
        ];
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| IdentityError::Generate(e.to_string()))?;
        Ok(Self {
            cert_pem: cert.pem(),
            key_pem: key_pair.serialize_pem(),
            cert_der: cert.der().to_vec(),
        })
    }

    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Self, IdentityError> {
        let cert = CertificateDer::from_pem_slice(cert_pem.as_bytes())
            .map_err(|e| IdentityError::Invalid(e.to_string()))?;
        let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes())
            .map_err(|e| IdentityError::Invalid(e.to_string()))?;
        // The key must be one we can sign with.
        crate::front::pairing::crypto::RsaSigner::from_der(key.secret_der())
            .map_err(|e| IdentityError::Invalid(e.to_string()))?;
        Ok(Self {
            cert_pem: cert_pem.to_owned(),
            key_pem: key_pem.to_owned(),
            cert_der: cert.as_ref().to_vec(),
        })
    }

    /// Reads `cert.pem` and `key.pem` from `dir`.
    pub fn load(dir: &Path) -> Result<Self, IdentityError> {
        let (cert_path, key_path) = (dir.join("cert.pem"), dir.join("key.pem"));
        let cert = std::fs::read_to_string(&cert_path).map_err(io(&cert_path))?;
        let key = std::fs::read_to_string(&key_path).map_err(io(&key_path))?;
        Self::from_pem(&cert, &key)
    }

    /// Writes `cert.pem` and `key.pem` into `dir` (made if needed), the key
    /// readable by its owner only.
    pub fn save(&self, dir: &Path) -> Result<(), IdentityError> {
        let (cert_path, key_path) = (dir.join("cert.pem"), dir.join("key.pem"));
        std::fs::create_dir_all(dir).map_err(io(dir))?;
        std::fs::write(&cert_path, &self.cert_pem).map_err(io(&cert_path))?;
        write_private(&key_path, &self.key_pem).map_err(io(&key_path))
    }

    /// Reads `cert.pem` and `key.pem` from `dir`, or makes and writes them.
    pub fn load_or_create(dir: &Path) -> Result<Self, IdentityError> {
        if dir.join("cert.pem").exists() && dir.join("key.pem").exists() {
            return Self::load(dir);
        }
        let identity = Self::generate()?;
        identity.save(dir)?;
        Ok(identity)
    }

    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }
    pub fn key_pem(&self) -> &str {
        &self.key_pem
    }
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }
    /// SHA-256 of the certificate, lower-case hex.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert_der)
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> IdentityError {
    let path = path.display().to_string();
    move |source| IdentityError::Io { path, source }
}

/// SHA-256 of a DER certificate, lower-case hex: how the host names a client.
pub fn fingerprint(der: &[u8]) -> String {
    hex::encode(Sha256::digest(der))
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    std::fs::write(path, contents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_generated_identity_loads_back_and_names_itself_by_its_certificate() {
        let id = Identity::generate().unwrap();
        assert!(id.cert_pem().starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(id.key_pem().starts_with("-----BEGIN PRIVATE KEY-----"));
        let again = Identity::from_pem(id.cert_pem(), id.key_pem()).unwrap();
        assert_eq!(again.fingerprint(), id.fingerprint());
        assert_eq!(id.fingerprint(), fingerprint(id.cert_der()));
        assert_eq!(id.fingerprint().len(), 64);
        // A key that isn't ours to sign with is refused.
        assert!(
            Identity::from_pem(
                id.cert_pem(),
                "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n"
            )
            .is_err()
        );
        assert!(Identity::from_pem("nope", id.key_pem()).is_err());
        // Two identities differ.
        assert_ne!(
            Identity::generate().unwrap().fingerprint(),
            id.fingerprint()
        );
    }

    #[test]
    fn load_or_create_keeps_what_it_made_and_guards_the_key() {
        let dir =
            std::env::temp_dir().join(format!("cha-gamestream-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = Identity::load_or_create(&dir).unwrap();
        let second = Identity::load_or_create(&dir).unwrap();
        assert_eq!(
            first.fingerprint(),
            second.fingerprint(),
            "the same host after a restart"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("key.pem"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "the key is readable by its owner only: {mode:o}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

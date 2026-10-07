// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: built on an explicit crypto provider (no process default needed), loads the host Identity,
// returns the client's certificate fingerprint with the stream.

//! TLS for nvhttp over HTTPS: the host's certificate, and client
//! certificates accepted leniently (any self-signed certificate, expired or
//! not, of any version) because who a client is gets decided by its
//! fingerprint against the paired list, not by a chain of trust.

use std::sync::Arc;

use aws_lc_rs::signature::{
    ECDSA_P256_SHA256_ASN1, ECDSA_P384_SHA384_ASN1, ED25519, RSA_PKCS1_2048_8192_SHA256,
    RSA_PKCS1_2048_8192_SHA384, RSA_PKCS1_2048_8192_SHA512, RSA_PSS_2048_8192_SHA256,
    RSA_PSS_2048_8192_SHA384, RSA_PSS_2048_8192_SHA512, UnparsedPublicKey, VerificationAlgorithm,
};
use rustls::client::danger::HandshakeSignatureValid;
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error, ServerConfig,
    SignatureScheme,
};
use x509_parser::prelude::*;

use crate::front::identity::{Identity, IdentityError};

/// Accepts a client certificate that parses and whose handshake signature
/// verifies against its own key. Mirrors Sunshine's check: GFE-era clients
/// present certificates (v1, v2, expired) that a strict verifier refuses.
/// Presenting one is optional; the HTTP layer refuses protected requests
/// without a paired one.
struct LenientClientCerts {
    algs: WebPkiSupportedAlgorithms,
}

impl std::fmt::Debug for LenientClientCerts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LenientClientCerts")
    }
}

fn public_key(der: &[u8]) -> Result<Vec<u8>, Error> {
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
    Ok(cert.public_key().subject_public_key.data.to_vec())
}

fn verify(
    alg: &'static dyn VerificationAlgorithm,
    key: &[u8],
    message: &[u8],
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, Error> {
    UnparsedPublicKey::new(alg, key)
        .verify(message, dss.signature())
        .map(|()| HandshakeSignatureValid::assertion())
        .map_err(|_| Error::InvalidCertificate(CertificateError::BadSignature))
}

impl LenientClientCerts {
    fn verify_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        let key = public_key(cert.as_ref())?;
        let alg: &'static dyn VerificationAlgorithm = match dss.scheme {
            SignatureScheme::RSA_PKCS1_SHA256 => &RSA_PKCS1_2048_8192_SHA256,
            SignatureScheme::RSA_PKCS1_SHA384 => &RSA_PKCS1_2048_8192_SHA384,
            SignatureScheme::RSA_PKCS1_SHA512 => &RSA_PKCS1_2048_8192_SHA512,
            SignatureScheme::RSA_PSS_SHA256 => &RSA_PSS_2048_8192_SHA256,
            SignatureScheme::RSA_PSS_SHA384 => &RSA_PSS_2048_8192_SHA384,
            SignatureScheme::RSA_PSS_SHA512 => &RSA_PSS_2048_8192_SHA512,
            SignatureScheme::ECDSA_NISTP256_SHA256 => &ECDSA_P256_SHA256_ASN1,
            SignatureScheme::ECDSA_NISTP384_SHA384 => &ECDSA_P384_SHA384_ASN1,
            SignatureScheme::ED25519 => &ED25519,
            _ => {
                return Err(Error::InvalidCertificate(
                    CertificateError::UnsupportedSignatureAlgorithmContext {
                        signature_algorithm_id: vec![],
                        supported_algorithms: vec![],
                    },
                ));
            }
        };
        verify(alg, &key, message, dss)
    }
}

impl ClientCertVerifier for LenientClientCerts {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        // It has to be a certificate; whose it is, the paired list decides.
        X509Certificate::from_der(end_entity.as_ref())
            .map_err(|_| Error::InvalidCertificate(CertificateError::BadEncoding))?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// The TLS acceptor for nvhttp over HTTPS.
pub(crate) fn acceptor(identity: &Identity) -> Result<tokio_rustls::TlsAcceptor, IdentityError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = LenientClientCerts {
        algs: provider.signature_verification_algorithms,
    };
    let cert = CertificateDer::from_pem_slice(identity.cert_pem().as_bytes())
        .map_err(|e| IdentityError::Invalid(e.to_string()))?;
    let key = PrivateKeyDer::from_pem_slice(identity.key_pem().as_bytes())
        .map_err(|e| IdentityError::Invalid(e.to_string()))?;
    let config = ServerConfig::builder_with_provider(CryptoProvider::clone(&provider).into())
        .with_safe_default_protocol_versions()
        .map_err(|e| IdentityError::Invalid(e.to_string()))?
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(vec![cert], key)
        .map_err(|e| IdentityError::Invalid(e.to_string()))?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}

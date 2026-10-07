// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: the pairing maths is pure functions over bytes (the state machine in mod.rs calls them), the
// client's secret is checked against its certificate with errors that say which check failed.

//! The cryptography of the five-phase pairing, for hashing with SHA-256 (the
//! protocol Moonlight uses with hosts of app version 7 and above).
//!
//! The PIN and the client's salt make an AES-128 key. The client proves it
//! knows the PIN by encrypting a challenge; the host answers with a hash that
//! includes its certificate's signature and a secret, which the client
//! checks; then each reveals a secret signed with its certificate's key.
//! A client that had a wrong PIN fails the check on its own side.

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use aws_lc_rs::signature::{
    RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_SHA256, RsaKeyPair, UnparsedPublicKey,
};
use sha2::{Digest, Sha256};
use x509_parser::prelude::*;

#[derive(Debug, thiserror::Error)]
pub(crate) enum PairError {
    #[error("not a multiple of the AES block size")]
    BlockSize,
    #[error("certificate: {0}")]
    Certificate(String),
    #[error("the client's secret is {0} bytes, too short")]
    ShortSecret(usize),
    #[error("the client's hash doesn't match: wrong PIN, or someone in between")]
    HashMismatch,
    #[error("the client's signature doesn't verify against its certificate")]
    BadSignature,
    #[error("signing failed")]
    Sign,
    #[error("no randomness")]
    Random,
}

/// Key from the client's salt and the PIN: the first 16 bytes of
/// SHA-256(salt ++ pin).
pub(crate) fn derive_key(salt: &[u8; 16], pin: &str) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(pin.as_bytes());
    h.finalize()[..16].try_into().expect("16 of 32")
}

fn ecb(key: &[u8; 16], data: &[u8], encrypt: bool) -> Result<Vec<u8>, PairError> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return Err(PairError::BlockSize);
    }
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut out = data.to_vec();
    for block in out.chunks_exact_mut(16) {
        let block = GenericArray::from_mut_slice(block);
        if encrypt {
            cipher.encrypt_block(block);
        } else {
            cipher.decrypt_block(block);
        }
    }
    Ok(out)
}

pub(crate) fn ecb_encrypt(key: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, PairError> {
    ecb(key, data, true)
}

pub(crate) fn ecb_decrypt(key: &[u8; 16], data: &[u8]) -> Result<Vec<u8>, PairError> {
    ecb(key, data, false)
}

/// The bytes of a certificate's signature, which both sides hash.
pub(crate) fn certificate_signature(der: &[u8]) -> Result<Vec<u8>, PairError> {
    let (_, cert) =
        X509Certificate::from_der(der).map_err(|e| PairError::Certificate(e.to_string()))?;
    Ok(cert.signature_value.data.to_vec())
}

pub(crate) fn random16() -> Result<[u8; 16], PairError> {
    let mut b = [0u8; 16];
    SystemRandom::new()
        .fill(&mut b)
        .map_err(|_| PairError::Random)?;
    Ok(b)
}

/// What the host keeps between phase 2 and 4.
pub(crate) struct ChallengeState {
    pub server_secret: [u8; 16],
    pub server_challenge: [u8; 16],
}

/// Phase 2: answers the client's encrypted challenge. Returns the encrypted
/// response (hash of challenge ++ host certificate signature ++ host secret,
/// then the host's own challenge) and the state to remember.
pub(crate) fn answer_challenge(
    key: &[u8; 16],
    client_challenge: &[u8],
    host_cert_signature: &[u8],
) -> Result<(Vec<u8>, ChallengeState), PairError> {
    let mut data = ecb_decrypt(key, client_challenge)?;
    let state = ChallengeState {
        server_secret: random16()?,
        server_challenge: random16()?,
    };
    data.extend_from_slice(host_cert_signature);
    data.extend_from_slice(&state.server_secret);
    let mut response = Sha256::digest(&data).to_vec();
    response.extend_from_slice(&state.server_challenge);
    Ok((ecb_encrypt(key, &response)?, state))
}

/// Phase 3: the client's answer to the host's challenge, decrypted. The host
/// keeps it until the client reveals its secret.
pub(crate) fn open_client_hash(key: &[u8; 16], response: &[u8]) -> Result<Vec<u8>, PairError> {
    ecb_decrypt(key, response)
}

/// Signs with the host's RSA key (PKCS#1 v1.5, SHA-256).
pub(crate) struct RsaSigner(RsaKeyPair);

impl RsaSigner {
    pub fn from_der(pkcs8: &[u8]) -> Result<Self, PairError> {
        RsaKeyPair::from_pkcs8(pkcs8)
            .map(Self)
            .map_err(|e| PairError::Certificate(e.to_string()))
    }

    pub fn sign(&self, data: &[u8]) -> Result<Vec<u8>, PairError> {
        let mut sig = vec![0u8; self.0.public_modulus_len()];
        self.0
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), data, &mut sig)
            .map_err(|_| PairError::Sign)?;
        Ok(sig)
    }
}

/// Phase 3's answer: the host's secret and its signature.
pub(crate) fn pairing_secret(
    signer: &RsaSigner,
    server_secret: &[u8; 16],
) -> Result<Vec<u8>, PairError> {
    let mut out = server_secret.to_vec();
    out.extend(signer.sign(server_secret)?);
    Ok(out)
}

/// Phase 4: checks the client's revealed secret: its hash matches what the
/// client sent in phase 3, and its signature verifies against the client's
/// certificate.
pub(crate) fn verify_client_secret(
    client_cert_der: &[u8],
    client_hash: &[u8],
    server_challenge: &[u8; 16],
    client_secret: &[u8],
) -> Result<(), PairError> {
    if client_secret.len() < 16 {
        return Err(PairError::ShortSecret(client_secret.len()));
    }
    let (payload, signature) = client_secret.split_at(16);
    let mut h = Sha256::new();
    h.update(server_challenge);
    h.update(certificate_signature(client_cert_der)?);
    h.update(payload);
    if h.finalize().as_slice() != client_hash {
        return Err(PairError::HashMismatch);
    }
    let (_, cert) = X509Certificate::from_der(client_cert_der)
        .map_err(|e| PairError::Certificate(e.to_string()))?;
    UnparsedPublicKey::new(
        &RSA_PKCS1_2048_8192_SHA256,
        cert.tbs_certificate
            .subject_pki
            .subject_public_key
            .data
            .as_ref(),
    )
    .verify(payload, signature)
    .map_err(|_| PairError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_the_first_half_of_sha256_of_salt_and_pin() {
        let salt = [0u8; 16];
        let key = derive_key(&salt, "1234");
        let mut input = vec![0u8; 16];
        input.extend(b"1234");
        assert_eq!(&key[..], &Sha256::digest(&input)[..16]);
        assert_ne!(derive_key(&salt, "1235"), key);
    }

    #[test]
    fn ecb_round_trips_and_wants_whole_blocks() {
        let key = [5u8; 16];
        let ct = ecb_encrypt(&key, &[7u8; 32]).unwrap();
        assert_ne!(ct, vec![7u8; 32]);
        assert_eq!(ecb_decrypt(&key, &ct).unwrap(), vec![7u8; 32]);
        assert!(matches!(
            ecb_encrypt(&key, &[0; 15]),
            Err(PairError::BlockSize)
        ));
        assert!(matches!(ecb_decrypt(&key, &[]), Err(PairError::BlockSize)));
        // FIPS 197 appendix B.
        let key = hex::decode("2b7e151628aed2a6abf7158809cf4f3c")
            .unwrap()
            .try_into()
            .unwrap();
        let ct = ecb_encrypt(
            &key,
            &hex::decode("3243f6a8885a308d313198a2e0370734").unwrap(),
        )
        .unwrap();
        assert_eq!(hex::encode(ct), "3925841d02dc09fbdc118597196a0b32");
    }
}

// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: detached in-place AES-GCM with typed key and IV, plain error types, the CBC helper takes the
// padding from the cipher crate.

//! The symmetric ciphers of the media streams: AES-128-GCM for control (and
//! video when on) and AES-128-CBC for audio.

use aes::Aes128;
use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Key, Nonce, Tag};

pub(crate) const GCM_TAG_LEN: usize = 16;

#[derive(Debug, thiserror::Error)]
#[error("AES-GCM failed")]
pub(crate) struct GcmError;

/// Encrypts `buffer` in place and returns the tag.
pub(crate) fn gcm_encrypt(
    key: &[u8; 16],
    iv: &[u8; 12],
    buffer: &mut [u8],
) -> Result<[u8; GCM_TAG_LEN], GcmError> {
    let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(key));
    let tag = cipher
        .encrypt_in_place_detached(Nonce::from_slice(iv), b"", buffer)
        .map_err(|_| GcmError)?;
    Ok(tag.into())
}

/// Decrypts `buffer` in place, checking the tag.
pub(crate) fn gcm_decrypt(
    key: &[u8; 16],
    iv: &[u8; 12],
    buffer: &mut [u8],
    tag: &[u8; GCM_TAG_LEN],
) -> Result<(), GcmError> {
    let cipher = Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(key));
    cipher
        .decrypt_in_place_detached(Nonce::from_slice(iv), b"", buffer, Tag::from_slice(tag))
        .map_err(|_| GcmError)
}

/// AES-128-CBC with PKCS#7 padding, as the audio stream wants it.
pub(crate) fn cbc_encrypt(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    cbc::Encryptor::<Aes128>::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gcm_round_trips_and_rejects_tampering() {
        let key = [7u8; 16];
        let iv = [9u8; 12];
        let mut data = b"hello control".to_vec();
        let tag = gcm_encrypt(&key, &iv, &mut data).unwrap();
        assert_ne!(&data[..], b"hello control");
        let mut ok = data.clone();
        gcm_decrypt(&key, &iv, &mut ok, &tag).unwrap();
        assert_eq!(&ok[..], b"hello control");
        data[0] ^= 1;
        assert!(gcm_decrypt(&key, &iv, &mut data, &tag).is_err());
    }

    #[test]
    fn gcm_matches_the_nist_vector() {
        // NIST GCM test case 2: zero key, zero IV, one zero block.
        let mut block = [0u8; 16];
        let tag = gcm_encrypt(&[0; 16], &[0; 12], &mut block).unwrap();
        assert_eq!(hex::encode(block), "0388dace60b6a392f328c2b971b2fe78");
        assert_eq!(hex::encode(tag), "ab6e47d42cec13bdf53a67b21257bddf");
    }

    #[test]
    fn cbc_pads_to_a_block() {
        let out = cbc_encrypt(&[1; 16], &[2; 16], &[0u8; 16]);
        assert_eq!(out.len(), 32);
        assert_eq!(cbc_encrypt(&[1; 16], &[2; 16], &[0u8; 5]).len(), 16);
    }
}

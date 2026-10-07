// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: sealing and opening are one pair of functions with the direction in the IV, not inline in the loop.

//! The encrypted wrapper of control messages: AES-128-GCM with the session
//! key, the IV made of the sequence number and a direction tag.
//!
//! ```text
//! 0x0001 | length | sequence (u32 LE) | tag (16) | ciphertext of `type length body`
//! IV (12): sequence LE in 0..4, zeros, then 'C' or 'H' (client to host,
//! host to client), then 'C' ("control")
//! ```

use crate::crypto::{GcmError, gcm_decrypt, gcm_encrypt};

use super::messages::{self, ty};

fn iv(sequence: u32, direction: u8) -> [u8; 12] {
    let mut iv = [0u8; 12];
    iv[..4].copy_from_slice(&sequence.to_le_bytes());
    iv[10] = direction;
    iv[11] = b'C';
    iv
}

/// Wraps a host-to-client message.
pub(crate) fn seal(key: &[u8; 16], sequence: u32, inner: &[u8]) -> Result<Vec<u8>, GcmError> {
    let mut ciphertext = inner.to_vec();
    let tag = gcm_encrypt(key, &iv(sequence, b'H'), &mut ciphertext)?;
    let mut body = Vec::with_capacity(4 + 16 + ciphertext.len());
    body.extend_from_slice(&sequence.to_le_bytes());
    body.extend_from_slice(&tag);
    body.extend_from_slice(&ciphertext);
    Ok(messages::frame(ty::ENCRYPTED, &body))
}

/// Opens a client-to-host message.
pub(crate) fn open(
    key: &[u8; 16],
    sequence: u32,
    tag: &[u8; 16],
    ciphertext: &[u8],
) -> Result<Vec<u8>, GcmError> {
    let mut plain = ciphertext.to_vec();
    gcm_decrypt(key, &iv(sequence, b'C'), &mut plain, tag)?;
    Ok(plain)
}

/// Wraps a message as a client would, for tests of the host.
#[cfg(test)]
pub(crate) fn seal_as_client(key: &[u8; 16], sequence: u32, inner: &[u8]) -> Vec<u8> {
    let mut ciphertext = inner.to_vec();
    let tag = gcm_encrypt(key, &iv(sequence, b'C'), &mut ciphertext).unwrap();
    let mut body = Vec::new();
    body.extend_from_slice(&sequence.to_le_bytes());
    body.extend_from_slice(&tag);
    body.extend_from_slice(&ciphertext);
    messages::frame(ty::ENCRYPTED, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::control::messages::{Outer, parse_outer};

    const KEY: [u8; 16] = *b"0123456789abcdef";

    #[test]
    fn client_messages_open_with_the_session_key() {
        let inner = messages::frame(ty::PING, &[1, 2]);
        let wire = seal_as_client(&KEY, 9, &inner);
        let Ok(Outer::Encrypted {
            sequence,
            tag,
            ciphertext,
        }) = parse_outer(&wire)
        else {
            panic!()
        };
        assert_eq!(sequence, 9);
        assert_eq!(open(&KEY, sequence, &tag, ciphertext).unwrap(), inner);
        // Another key, another sequence number or a flipped bit all fail.
        assert!(open(&[0; 16], sequence, &tag, ciphertext).is_err());
        assert!(open(&KEY, 10, &tag, ciphertext).is_err());
        let mut bad = ciphertext.to_vec();
        bad[0] ^= 1;
        assert!(open(&KEY, sequence, &tag, &bad).is_err());
    }

    /// The directions differ in the IV: a host message can't be replayed back at the host.
    #[test]
    fn host_messages_do_not_open_as_client_messages() {
        let wire = seal(&KEY, 3, &messages::frame(ty::PING, &[])).unwrap();
        let Ok(Outer::Encrypted {
            sequence,
            tag,
            ciphertext,
        }) = parse_outer(&wire)
        else {
            panic!()
        };
        assert!(open(&KEY, sequence, &tag, ciphertext).is_err());
    }

    #[test]
    fn golden_host_message() {
        // key "0123456789abcdef", sequence 0, inner `0x0200 len 0`.
        let wire = seal(&KEY, 0, &[0x00, 0x02, 0x00, 0x00]).unwrap();
        assert_eq!(&wire[..8], &[0x01, 0x00, 0x18, 0x00, 0, 0, 0, 0]);
        assert_eq!(wire.len(), 4 + 4 + 16 + 4);
        let again = seal(&KEY, 0, &[0x00, 0x02, 0x00, 0x00]).unwrap();
        assert_eq!(wire, again, "deterministic for a sequence number");
        assert_eq!(hex::encode(&wire[8..]), GOLDEN_TAG_AND_CIPHERTEXT);
    }

    const GOLDEN_TAG_AND_CIPHERTEXT: &str = "415170222986064d7589b9af8e633b2e66ee8c82";
}

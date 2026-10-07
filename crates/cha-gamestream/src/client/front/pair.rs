//! The five-phase pairing, the client's half: interoperates with the
//! five-phase pairing that Moonlight clients and Sunshine implement, for
//! hosts of generation 7 and up (SHA-256). The host's half is `front::pairing`; the maths is shared.

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::pem::PemObject;
use sha2::{Digest, Sha256};

use super::http::Pinned;
use super::{ClientError, HostClient, PairingError};
use crate::front::pairing::crypto::{self, PairError, RsaSigner};

/// Four random digits, for [`HostClient::pair`]: what the user types into the
/// host, as Moonlight clients show.
pub fn random_pin() -> String {
    // Whole multiples of 10000 only, so every PIN is as likely as another.
    const LIMIT: u32 = u32::MAX / 10_000 * 10_000;
    let rng = SystemRandom::new();
    loop {
        let mut bytes = [0u8; 4];
        if rng.fill(&mut bytes).is_err() {
            continue;
        }
        let n = u32::from_le_bytes(bytes);
        if n < LIMIT {
            return format!("{:04}", n % 10_000);
        }
    }
}

fn crypto_err(stage: u8, e: PairError) -> ClientError {
    ClientError::Malformed(format!("pairing step {stage}: {e}"))
}

fn hex_of(root: &super::xml::Node, name: &str, stage: u8) -> Result<Vec<u8>, ClientError> {
    root.text_of(name)
        .and_then(|v| hex::decode(v).ok())
        .ok_or_else(|| ClientError::Malformed(format!("pairing step {stage} has no {name}")))
}

impl HostClient {
    /// Pairs with the host: `pin` is what we show the user to type into the
    /// host (a few digits we made up). Returns the host's certificate (PEM),
    /// which this client now pins; keep it, and give it to
    /// [`with_server_cert`](Self::with_server_cert) next time.
    ///
    /// Blocks (up to [`Timeouts::pair`](super::Timeouts::pair)) while the
    /// host waits for its user. A wrong PIN is [`PairingError::WrongPin`],
    /// found out from the host's answer, as Moonlight does.
    pub async fn pair(&self, pin: &str, device_name: &str) -> Result<String, ClientError> {
        if pin.is_empty() || pin.len() > 16 || !pin.is_ascii() {
            return Err(ClientError::Unsupported(
                "the PIN must be 1 to 16 ASCII characters".into(),
            ));
        }
        let info = self.server_info_http().await?;
        if info.app_version_quad()[0] < 7 {
            return Err(PairingError::Unsupported(format!(
                "the host is GameStream generation {}; pairing with SHA-1 hosts isn't supported",
                info.app_version_quad()[0]
            ))
            .into());
        }
        let previous = self.pinned();
        let outcome = self.pair_steps(pin, device_name).await;
        match &outcome {
            Ok(_) => {}
            Err(_) => {
                self.set_pinned(previous);
                // Tell the host to forget the attempt (it ignores this if it has nothing).
                let _ = self
                    .nvhttp(false, "unpair", Vec::new(), self.timeouts.request)
                    .await;
            }
        }
        outcome
    }

    async fn pair_steps(&self, pin: &str, device_name: &str) -> Result<String, ClientError> {
        let t = self.timeouts;
        let base = |extra: Vec<(&'static str, String)>| {
            let mut v = vec![
                ("devicename", device_name.to_owned()),
                ("updateState", "1".to_owned()),
            ];
            v.extend(extra);
            v
        };
        let random = |stage| crypto::random16().map_err(|e| crypto_err(stage, e));
        let der_of_key = PrivateKeyDer::from_pem_slice(self.identity.key_pem().as_bytes())
            .map_err(|e| ClientError::Tls(e.to_string()))?;
        let signer = RsaSigner::from_der(der_of_key.secret_der()).map_err(|e| crypto_err(0, e))?;
        let our_signature = crypto::certificate_signature(self.identity.cert_der())
            .map_err(|e| crypto_err(0, e))?;

        // 1: the salt and our certificate; the host's answer waits for its user's PIN.
        let salt = random(1)?;
        let key = crypto::derive_key(&salt, pin);
        let (r1, _) = self
            .nvhttp(
                false,
                "pair",
                base(vec![
                    ("phrase", "getservercert".into()),
                    ("salt", hex::encode(salt)),
                    ("clientcert", hex::encode(self.identity.cert_pem())),
                ]),
                t.pair,
            )
            .await?;
        if r1.text_of("paired") != Some("1") {
            return Err(PairingError::Declined.into());
        }
        let plain = match r1.text_of("plaincert").filter(|c| !c.is_empty()) {
            Some(_) => hex_of(&r1, "plaincert", 1)?,
            None => return Err(PairingError::AlreadyInProgress.into()),
        };
        let server_pem = String::from_utf8(plain)
            .map_err(|_| ClientError::Malformed("the host's certificate isn't text".into()))?;
        let pinned = Pinned::from_pem(&server_pem)?;
        let server_signature =
            crypto::certificate_signature(&pinned.der).map_err(|e| crypto_err(1, e))?;

        // 2: a challenge only the PIN's holder can answer.
        let challenge = random(2)?;
        let (r2, _) = self
            .nvhttp(
                false,
                "pair",
                base(vec![(
                    "clientchallenge",
                    hex::encode(
                        crypto::ecb_encrypt(&key, &challenge).map_err(|e| crypto_err(2, e))?,
                    ),
                )]),
                t.request,
            )
            .await?;
        if r2.text_of("paired") != Some("1") {
            return Err(PairingError::Refused { stage: 2 }.into());
        }
        let answer = crypto::ecb_decrypt(&key, &hex_of(&r2, "challengeresponse", 2)?)
            .map_err(|e| crypto_err(2, e))?;
        if answer.len() < 32 + 16 {
            return Err(ClientError::Malformed(
                "pairing step 2: the challenge response is too short".into(),
            ));
        }
        let (server_hash, server_challenge) = (&answer[..32], &answer[32..48]);

        // 3: our answer to the host's challenge, hiding a secret of ours.
        let client_secret = random(3)?;
        let mut h = Sha256::new();
        h.update(server_challenge);
        h.update(&our_signature);
        h.update(client_secret);
        let (r3, _) = self
            .nvhttp(
                false,
                "pair",
                base(vec![(
                    "serverchallengeresp",
                    hex::encode(
                        crypto::ecb_encrypt(&key, &h.finalize()).map_err(|e| crypto_err(3, e))?,
                    ),
                )]),
                t.request,
            )
            .await?;
        if r3.text_of("paired") != Some("1") {
            return Err(PairingError::Refused { stage: 3 }.into());
        }
        let secret = hex_of(&r3, "pairingsecret", 3)?;
        if secret.len() <= 16 {
            return Err(ClientError::Malformed(
                "pairing step 3: the pairing secret is too short".into(),
            ));
        }
        let (server_secret, signature) = secret.split_at(16);
        // The host's secret is signed with the key of the certificate it gave us...
        crypto::verify_signature(&pinned.der, server_secret, signature)
            .map_err(|_| PairingError::Mitm)?;
        // ... and its answer to our challenge proves it was given our PIN.
        let mut h = Sha256::new();
        h.update(challenge);
        h.update(&server_signature);
        h.update(server_secret);
        if h.finalize().as_slice() != server_hash {
            return Err(PairingError::WrongPin.into());
        }

        // 4: our secret, signed.
        let mut ours = client_secret.to_vec();
        ours.extend(signer.sign(&client_secret).map_err(|e| crypto_err(4, e))?);
        let (r4, _) = self
            .nvhttp(
                false,
                "pair",
                base(vec![("clientpairingsecret", hex::encode(ours))]),
                t.request,
            )
            .await?;
        if r4.text_of("paired") != Some("1") {
            return Err(PairingError::Refused { stage: 4 }.into());
        }

        // 5: over HTTPS with the certificates both sides now know.
        self.set_pinned(Some(pinned));
        let (r5, _) = self
            .nvhttp(
                true,
                "pair",
                base(vec![("phrase", "pairchallenge".into())]),
                t.request,
            )
            .await?;
        if r5.text_of("paired") != Some("1") {
            return Err(PairingError::Refused { stage: 5 }.into());
        }
        Ok(server_pem)
    }

    /// Removes this client from the host's paired clients, and forgets the
    /// host's certificate. Over HTTPS the host knows who asks by the
    /// certificate; if that is refused, plain HTTP names us by `uniqueid`
    /// (hosts that allow it).
    pub async fn unpair(&self) -> Result<(), ClientError> {
        let over_https = if self.pinned().is_some() && self.https_port() != 0 {
            self.nvhttp(true, "unpair", Vec::new(), self.timeouts.request)
                .await
                .map(|_| ())
        } else {
            Err(ClientError::NoPinnedCert)
        };
        let outcome = match over_https {
            Ok(()) => Ok(()),
            Err(_) => self
                .nvhttp(false, "unpair", Vec::new(), self.timeouts.request)
                .await
                .map(|_| ()),
        };
        self.set_pinned(None);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_is_four_digits() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let pin = random_pin();
            assert_eq!(pin.len(), 4, "{pin}");
            assert!(pin.bytes().all(|b| b.is_ascii_digit()), "{pin}");
            seen.insert(pin);
        }
        assert!(seen.len() > 100, "the PINs vary");
    }
}

// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: the PIN is not taken over HTTP (no /pin or /submit-pin page) but asked of the Directory, which
// resolves it from whoever may pair; pending pairings are bound to the client's address, capped and expire;
// pairing is stored through the PairingStore; failures answer `<paired>0</paired>` like GFE.

//! The five phases of `/pair`, as a state machine over the requests of one
//! client. Moonlight runs them in order, the first four over HTTP and the last
//! over HTTPS with the client certificate:
//!
//! 1. `phrase=getservercert` with the client's certificate and salt: the host
//!    asks the [`Directory`] for the PIN the client shows and holds the
//!    request until it comes; answers with its certificate.
//! 2. `clientchallenge`: the host answers the encrypted challenge.
//! 3. `serverchallengeresp`: the client answers the host's challenge; the host
//!    reveals its signed secret.
//! 4. `clientpairingsecret`: the client reveals its signed secret; the host
//!    checks it and, if it holds, stores the client as paired.
//! 5. `phrase=pairchallenge` over HTTPS: confirms the client's certificate is
//!    now accepted.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject;
use tokio::sync::watch;

use crate::directory::{Directory, PairedClient, PairingAttempt, PairingStore};
use crate::handoff::ClientId;
use crate::net::flagged;

use super::identity::{Identity, fingerprint};
use super::nvhttp::xml;

pub(crate) mod crypto;

use crypto::{ChallengeState, RsaSigner};

/// Pairings in flight at once; more are refused. Each is a client someone
/// may be about to type a PIN for.
const MAX_PENDING: usize = 32;
/// A pending pairing that has made no progress this long after the PIN
/// timeout is forgotten.
const PENDING_GRACE: Duration = Duration::from_secs(60);

struct Pending {
    client: ClientId,
    cert_der: Vec<u8>,
    name: String,
    peer: IpAddr,
    salt: [u8; 16],
    key: Option<[u8; 16]>,
    challenge: Option<ChallengeState>,
    client_hash: Option<Vec<u8>>,
    created: Instant,
}

pub(crate) struct Pairing {
    identity: Arc<Identity>,
    signer: RsaSigner,
    host_cert_signature: Vec<u8>,
    store: Arc<dyn PairingStore>,
    directory: Arc<dyn Directory>,
    pin_timeout: Duration,
    pending: Mutex<HashMap<String, Pending>>,
}

fn hex_param(params: &HashMap<String, String>, name: &str) -> Option<Vec<u8>> {
    hex::decode(params.get(name)?).ok()
}

impl Pairing {
    pub fn new(
        identity: Arc<Identity>,
        store: Arc<dyn PairingStore>,
        directory: Arc<dyn Directory>,
        pin_timeout: Duration,
    ) -> Result<Self, crypto::PairError> {
        let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(identity.key_pem().as_bytes())
            .map_err(|e| crypto::PairError::Certificate(e.to_string()))?;
        Ok(Self {
            signer: RsaSigner::from_der(key.secret_der())?,
            host_cert_signature: crypto::certificate_signature(identity.cert_der())?,
            identity,
            store,
            directory,
            pin_timeout,
            pending: Mutex::default(),
        })
    }

    /// Handles one `/pair` request. `peer` is the requester's address;
    /// `tls_client` is the client certificate of an HTTPS request. Returns the
    /// XML to answer with.
    pub async fn handle(
        &self,
        params: &HashMap<String, String>,
        peer: IpAddr,
        tls_client: Option<&ClientId>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> String {
        let Some(unique_id) = params
            .get("uniqueid")
            .filter(|u| !u.is_empty() && u.len() <= 64)
        else {
            return xml::error(400, "missing uniqueid");
        };
        match params.get("phrase").map(String::as_str) {
            Some("getservercert") => {
                self.get_server_cert(unique_id, params, peer, shutdown)
                    .await
            }
            Some("pairchallenge") => self.pair_challenge(unique_id, tls_client).await,
            Some(other) => xml::error(400, &format!("unknown pair phrase {other:?}")),
            None if params.contains_key("clientchallenge") => {
                self.client_challenge(unique_id, params, peer)
            }
            None if params.contains_key("serverchallengeresp") => {
                self.server_challenge_response(unique_id, params, peer)
            }
            None if params.contains_key("clientpairingsecret") => {
                self.client_pairing_secret(unique_id, params, peer).await
            }
            None => xml::error(400, "unknown pair request"),
        }
    }

    fn purge(&self, pending: &mut HashMap<String, Pending>) {
        let ttl = self.pin_timeout + PENDING_GRACE;
        pending.retain(|_, p| p.created.elapsed() < ttl);
    }

    async fn get_server_cert(
        &self,
        unique_id: &str,
        params: &HashMap<String, String>,
        peer: IpAddr,
        shutdown: &mut watch::Receiver<bool>,
    ) -> String {
        let Some(cert_pem) = hex_param(params, "clientcert") else {
            return xml::error(400, "bad clientcert");
        };
        let Ok(cert) = CertificateDer::from_pem_slice(&cert_pem) else {
            return xml::error(400, "clientcert isn't a PEM certificate");
        };
        let cert_der = cert.as_ref().to_vec();
        if crypto::certificate_signature(&cert_der).is_err() {
            return xml::error(400, "clientcert isn't a certificate");
        }
        let Some(salt) = hex_param(params, "salt").and_then(|s| <[u8; 16]>::try_from(s).ok())
        else {
            return xml::error(400, "salt must be 16 bytes of hex");
        };
        let client = ClientId(fingerprint(&cert_der));
        let name = params
            .get("devicename")
            .cloned()
            .unwrap_or_else(|| "Moonlight".into());

        {
            let mut pending = self.pending.lock().expect("pending");
            self.purge(&mut pending);
            if pending.len() >= MAX_PENDING && !pending.contains_key(unique_id) {
                return xml::error(503, "too many pairings in progress");
            }
            pending.insert(
                unique_id.to_owned(),
                Pending {
                    client: client.clone(),
                    cert_der,
                    name: name.clone(),
                    peer,
                    salt,
                    key: None,
                    challenge: None,
                    client_hash: None,
                    created: Instant::now(),
                },
            );
        }

        // Hold the client's request until a user types the PIN it shows.
        let waiter = self.directory.pin_for(PairingAttempt {
            client_unique_id: unique_id.to_owned(),
            client_name: Some(name),
            peer,
            client,
        });
        let pin = tokio::select! {
            pin = tokio::time::timeout(self.pin_timeout, waiter) => pin.ok().flatten(),
            _ = flagged(shutdown) => None,
        };
        let mut pending = self.pending.lock().expect("pending");
        let Some(pin) = pin else {
            pending.remove(unique_id);
            tracing::info!("pairing with {unique_id}: no PIN arrived");
            return xml::not_paired();
        };
        match pending.get_mut(unique_id) {
            Some(p) if p.peer == peer => {
                p.key = Some(crypto::derive_key(&p.salt, &pin));
                xml::paired(&format!(
                    "<plaincert>{}</plaincert>",
                    hex::encode(self.identity.cert_pem())
                ))
            }
            _ => xml::not_paired(),
        }
    }

    fn client_challenge(
        &self,
        unique_id: &str,
        params: &HashMap<String, String>,
        peer: IpAddr,
    ) -> String {
        let Some(challenge) = hex_param(params, "clientchallenge") else {
            return xml::error(400, "bad clientchallenge");
        };
        let mut pending = self.pending.lock().expect("pending");
        let Some(p) = pending.get_mut(unique_id).filter(|p| p.peer == peer) else {
            return xml::not_paired();
        };
        let Some(key) = p.key else {
            return xml::not_paired();
        };
        match crypto::answer_challenge(&key, &challenge, &self.host_cert_signature) {
            Ok((response, state)) => {
                p.challenge = Some(state);
                xml::paired(&format!(
                    "<challengeresponse>{}</challengeresponse>",
                    hex::encode(response)
                ))
            }
            Err(e) => {
                tracing::info!("pairing with {unique_id}: {e}");
                pending.remove(unique_id);
                xml::not_paired()
            }
        }
    }

    fn server_challenge_response(
        &self,
        unique_id: &str,
        params: &HashMap<String, String>,
        peer: IpAddr,
    ) -> String {
        let Some(response) = hex_param(params, "serverchallengeresp") else {
            return xml::error(400, "bad serverchallengeresp");
        };
        let mut pending = self.pending.lock().expect("pending");
        let Some(p) = pending.get_mut(unique_id).filter(|p| p.peer == peer) else {
            return xml::not_paired();
        };
        let (Some(key), Some(state)) = (p.key, p.challenge.as_ref()) else {
            return xml::not_paired();
        };
        let secret = crypto::open_client_hash(&key, &response).and_then(|hash| {
            let secret = crypto::pairing_secret(&self.signer, &state.server_secret)?;
            Ok((hash, secret))
        });
        match secret {
            Ok((hash, secret)) => {
                p.client_hash = Some(hash);
                xml::paired(&format!(
                    "<pairingsecret>{}</pairingsecret>",
                    hex::encode(secret)
                ))
            }
            Err(e) => {
                tracing::info!("pairing with {unique_id}: {e}");
                pending.remove(unique_id);
                xml::not_paired()
            }
        }
    }

    async fn client_pairing_secret(
        &self,
        unique_id: &str,
        params: &HashMap<String, String>,
        peer: IpAddr,
    ) -> String {
        let Some(secret) = hex_param(params, "clientpairingsecret") else {
            return xml::error(400, "bad clientpairingsecret");
        };
        let verified = {
            let mut pending = self.pending.lock().expect("pending");
            let Some(p) = pending.get(unique_id).filter(|p| p.peer == peer) else {
                return xml::not_paired();
            };
            let (Some(hash), Some(state)) = (p.client_hash.as_ref(), p.challenge.as_ref()) else {
                return xml::not_paired();
            };
            let result =
                crypto::verify_client_secret(&p.cert_der, hash, &state.server_challenge, &secret);
            // Whatever the outcome, this pairing is over.
            let p = pending.remove(unique_id).expect("just found");
            result.map(|()| (p.client, p.name))
        };
        match verified {
            Ok((client, name)) => {
                let added = self
                    .store
                    .add(PairedClient {
                        client: client.clone(),
                        unique_id: unique_id.to_owned(),
                        name,
                    })
                    .await;
                match added {
                    Ok(()) => {
                        tracing::info!("paired {client}");
                        xml::paired("")
                    }
                    Err(e) => {
                        tracing::error!("{e}");
                        xml::not_paired()
                    }
                }
            }
            Err(e) => {
                tracing::info!("pairing with {unique_id} failed: {e}");
                xml::not_paired()
            }
        }
    }

    async fn pair_challenge(&self, _unique_id: &str, tls_client: Option<&ClientId>) -> String {
        match tls_client {
            Some(client) if self.store.is_paired(client).await.unwrap_or(false) => xml::paired(""),
            _ => xml::not_paired(),
        }
    }
}

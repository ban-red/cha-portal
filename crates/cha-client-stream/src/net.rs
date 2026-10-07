//! The WebTransport connection: TLS pinned to the certificate hash the portal
//! gives, one UDP socket with a large receive buffer per attempt, and a race
//! over the streamer's addresses.
//!
//! ## The pin
//!
//! The streamer's certificate is self-signed and made when it starts, so the
//! only way to trust it is the SHA-256 of its DER that the portal vouches for
//! in `/connect` (the browser's `serverCertificateHashes`). [`PinnedCert`] is a
//! rustls `ServerCertVerifier` that accepts exactly the certificate with that
//! hash and nothing else: it never consults the system's roots, and it does
//! not check the validity dates (the browser must, and a streamer running
//! longer than the 13 days of its certificate would break the browser; the
//! hash already says it is the one the portal named). The TLS 1.3 handshake
//! signature is still verified against the certificate's key, so the peer
//! must hold the matching private key.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::WebPkiSupportedAlgorithms;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::task::JoinSet;
use tracing::{debug, info};
use wtransport::config::QuicTransportConfig;
use wtransport::endpoint::endpoint_side::Client;
use wtransport::{ClientConfig, Connection, Endpoint};

/// How long one address gets to answer.
pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(2500);
/// Each address after the first starts this much later than the one before.
/// Every attempt that reaches the streamer takes a seat there (and the
/// floor, as the newest owner session) until it is closed, so a healthy
/// first address should not be joined by a crowd; a dead one costs this
/// little.
pub const HEAD_START: Duration = Duration::from_millis(50);
/// The streamer's idle timeout is 10 s and it pings every 2 s; so do we.
const KEEP_ALIVE: Duration = Duration::from_secs(2);
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Datagrams queued for us before the oldest are dropped: a keyframe is a
/// burst of hundreds.
const DATAGRAM_BUFFER: usize = 16 << 20;
/// What we ask the OS for as the UDP receive buffer, trying smaller ones if
/// it refuses.
const SOCKET_BUFFERS: [usize; 4] = [8 << 20, 4 << 20, 2 << 20, 1 << 20];

/// Accepts the one certificate whose SHA-256 is `hash`.
#[derive(Debug)]
pub struct PinnedCert {
    hash: [u8; 32],
    algorithms: WebPkiSupportedAlgorithms,
}

impl PinnedCert {
    pub fn new(hash: [u8; 32]) -> Self {
        Self {
            hash,
            algorithms: rustls::crypto::ring::default_provider().signature_verification_algorithms,
        }
    }

    /// From the 64 hex characters of `/connect`'s `certHash`.
    pub fn from_hex(hex: &str) -> Result<Self> {
        Ok(Self::new(parse_hash(hex)?))
    }
}

/// 64 hex characters (either case) to 32 bytes.
pub fn parse_hash(hex: &str) -> Result<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 || !hex.is_ascii() {
        bail!("the certificate hash isn't 64 hex characters");
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| anyhow!("the certificate hash isn't hex"))?;
    }
    Ok(out)
}

impl ServerCertVerifier for PinnedCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = Sha256::digest(end_entity.as_ref());
        if got.as_slice() == self.hash {
            Ok(ServerCertVerified::assertion())
        } else {
            // Not the certificate the portal named.
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::UnknownIssuer,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn client_config(hash: [u8; 32], socket: UdpSocket) -> Result<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .context("TLS 1.3")?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedCert::new(hash)))
        .with_no_client_auth();
    tls.alpn_protocols = vec![b"h3".to_vec()];

    let mut transport = QuicTransportConfig::default();
    transport.datagram_receive_buffer_size(Some(DATAGRAM_BUFFER));
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    transport.max_idle_timeout(Some(IDLE_TIMEOUT.try_into().context("idle timeout")?));
    Ok(ClientConfig::builder()
        .with_bind_socket(socket)
        .with_custom_tls_and_transport(tls, transport)
        .build())
}

/// A UDP socket on any address, IPv6 dual-stack if the machine has IPv6,
/// with the biggest receive buffer the OS will give.
fn bind_socket() -> Result<UdpSocket> {
    let dual = || -> std::io::Result<Socket> {
        let s = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        s.set_only_v6(false)?;
        s.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)).into())?;
        Ok(s)
    };
    let socket = match dual() {
        Ok(s) => s,
        Err(e) => {
            debug!("no dual-stack socket ({e}); using IPv4");
            let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())?;
            s
        }
    };
    for size in SOCKET_BUFFERS {
        if socket.set_recv_buffer_size(size).is_ok() {
            break;
        }
    }
    debug!(
        receive_buffer = socket.recv_buffer_size().unwrap_or(0),
        "UDP socket"
    );
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

/// A connected session: the connection, and the endpoint it must outlive.
pub struct Link {
    pub url: String,
    pub endpoint: Endpoint<Client>,
    pub connection: Connection,
}

async fn attempt(url: String, hash: [u8; 32], timeout: Duration) -> Result<Link> {
    let socket = bind_socket().context("opening a UDP socket")?;
    let config = client_config(hash, socket)?;
    let endpoint = Endpoint::client(config).context("creating the QUIC endpoint")?;
    let connection = tokio::time::timeout(timeout, endpoint.connect(url.as_str()))
        .await
        .map_err(|_| anyhow!("no answer in {timeout:?}"))?
        .map_err(|e| anyhow!("{e}"))?;
    Ok(Link {
        url,
        endpoint,
        connection,
    })
}

/// Tries every address in parallel (each after the first with a small
/// [`HEAD_START`]) and keeps the first that is ready; the others are given up
/// on. Each gets `timeout`. All of them failing is an error that
/// says why for each.
pub async fn race(urls: &[String], hash: [u8; 32], timeout: Duration) -> Result<Link> {
    if urls.is_empty() {
        bail!("the portal gave no streamer addresses");
    }
    let mut set = JoinSet::new();
    for (i, url) in urls.iter().enumerate() {
        let shown = redact(url);
        let url = url.clone();
        set.spawn(async move {
            tokio::time::sleep(HEAD_START * i as u32).await;
            attempt(url, hash, timeout)
                .await
                .map_err(|e| format!("{shown}: {e:#}"))
        });
    }
    let mut failures = Vec::new();
    while let Some(done) = set.join_next().await {
        match done {
            Ok(Ok(link)) => {
                info!(url = %redact(&link.url), "WebTransport up");
                // The rest are not wanted: stop them, and close any that got
                // through in the meantime.
                set.abort_all();
                tokio::spawn(async move {
                    while let Some(done) = set.join_next().await {
                        if let Ok(Ok(late)) = done {
                            late.connection.close(0u32.into(), b"not needed");
                        }
                    }
                });
                return Ok(link);
            }
            Ok(Err(why)) => failures.push(why),
            Err(e) if e.is_cancelled() => {}
            Err(e) => failures.push(format!("connecting: {e}")),
        }
    }
    bail!(
        "none of the streamer's addresses answered over WebTransport ({})",
        failures.join("; ")
    )
}

/// A URL without its token, for logs and errors.
pub fn redact(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            u.set_query(None);
            u.to_string()
        }
        Err(_) => "(unparsable URL)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hash_is_64_hex_characters() {
        let hex = "00ff".repeat(16);
        let h = parse_hash(&hex).unwrap();
        assert_eq!(h[0], 0x00);
        assert_eq!(h[1], 0xff);
        assert_eq!(parse_hash(&hex.to_uppercase()).unwrap(), h);
        assert!(parse_hash("abcd").is_err());
        assert!(parse_hash(&"zz".repeat(32)).is_err());
        assert!(parse_hash(&"é".repeat(32)).is_err());
    }

    #[test]
    fn only_the_pinned_certificate_is_accepted() {
        let der = CertificateDer::from(vec![1u8, 2, 3, 4]);
        let hash: [u8; 32] = Sha256::digest([1u8, 2, 3, 4]).into();
        let name = ServerName::try_from("localhost").unwrap();
        let verify = |pin: [u8; 32], der: &CertificateDer<'_>| {
            PinnedCert::new(pin).verify_server_cert(der, &[], &name, &[], UnixTime::now())
        };
        assert!(verify(hash, &der).is_ok());
        let other = CertificateDer::from(vec![9u8, 9]);
        assert!(verify(hash, &other).is_err());
        assert!(verify([0; 32], &der).is_err());
        // Dates are not looked at: this isn't even a certificate.
    }

    #[test]
    fn urls_lose_their_token_in_messages() {
        assert_eq!(
            redact("https://10.0.0.5:4433/media?codec=hevc&token=secret.sig"),
            "https://10.0.0.5:4433/media"
        );
        assert_eq!(redact("nonsense"), "(unparsable URL)");
    }
}

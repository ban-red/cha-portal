//! nvhttp's transport: HTTP and HTTPS GETs with our client certificate, the
//! host's certificate pinned instead of verified by a chain, every step
//! bounded in time and size.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Limited};
use hyper::Request;
use hyper::header::{CONNECTION, HOST};
use hyper_util::rt::TokioIo;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use tokio::net::TcpStream;

use super::{ClientError, ClientIdentity};
use crate::front::nvhttp::tls::verify_handshake_signature;

/// A reply no host's `appasset` should exceed.
pub(super) const MAX_BODY: usize = 8 * 1024 * 1024;

/// How long each kind of request may take. Pairing's first step waits for a
/// person to type the PIN into the host, so it has the most.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Opening a TCP connection (and the TLS handshake over it).
    pub connect: Duration,
    /// Ordinary requests: `serverinfo`, `applist`, `appasset`, the later pairing steps.
    pub request: Duration,
    /// `/launch` and `/resume`, which answer when the app has started.
    pub launch: Duration,
    /// `/cancel`, which answers when the app has quit.
    pub cancel: Duration,
    /// Pairing's first step: the host holds it until its user gives the PIN.
    pub pair: Duration,
    /// One RTSP request, once connected.
    pub rtsp: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(5),
            request: Duration::from_secs(10),
            launch: Duration::from_secs(120),
            cancel: Duration::from_secs(30),
            pair: Duration::from_secs(330),
            rtsp: Duration::from_secs(10),
        }
    }
}

/// Opens a TCP connection to `host:port` (a name, or an address), trying each
/// address the name has, within `timeout` in all.
pub(super) async fn connect(
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<TcpStream, ClientError> {
    let addr = format!("{host}:{port}");
    let attempt = async {
        let addrs = tokio::net::lookup_host((host, port)).await?;
        let mut last = None;
        for a in addrs {
            match TcpStream::connect(a).await {
                Ok(s) => return Ok(s),
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| std::io::Error::other("the name has no addresses")))
    };
    match tokio::time::timeout(timeout, attempt).await {
        Err(_) => Err(ClientError::Timeout("connecting")),
        Ok(Err(source)) => Err(ClientError::Connect { addr, source }),
        Ok(Ok(stream)) => {
            let _ = stream.set_nodelay(true);
            Ok(stream)
        }
    }
}

/// The host's certificate, which TLS connections must present.
#[derive(Clone, Debug)]
pub(super) struct Pinned {
    pub pem: String,
    pub der: Vec<u8>,
}

impl Pinned {
    pub fn from_pem(pem: &str) -> Result<Self, ClientError> {
        let der = CertificateDer::from_pem_slice(pem.as_bytes())
            .map_err(|e| ClientError::Tls(format!("the host certificate isn't PEM: {e}")))?;
        x509_parser::parse_x509_certificate(der.as_ref())
            .map_err(|e| ClientError::Tls(format!("the host certificate isn't X.509: {e}")))?;
        Ok(Self {
            pem: pem.to_owned(),
            der: der.as_ref().to_vec(),
        })
    }
}

/// Accepts exactly the certificate pinned at pairing, and checks the
/// handshake was signed with its key.
#[derive(Debug)]
struct PinnedServer {
    der: Vec<u8>,
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        if end_entity.as_ref() == self.der.as_slice() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_handshake_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_handshake_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// TLS for HTTPS: our certificate as the client's, the pinned one as the host's.
pub(super) fn tls_config(
    identity: &ClientIdentity,
    pinned: &Pinned,
) -> Result<Arc<ClientConfig>, ClientError> {
    let tls = |e: &dyn std::fmt::Display| ClientError::Tls(e.to_string());
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let verifier = PinnedServer {
        der: pinned.der.clone(),
        algs: provider.signature_verification_algorithms,
    };
    let cert =
        CertificateDer::from_pem_slice(identity.cert_pem().as_bytes()).map_err(|e| tls(&e))?;
    let key = PrivateKeyDer::from_pem_slice(identity.key_pem().as_bytes()).map_err(|e| tls(&e))?;
    let mut config = ClientConfig::builder_with_provider(CryptoProvider::clone(&provider).into())
        .with_safe_default_protocol_versions()
        .map_err(|e| tls(&e))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_client_auth_cert(vec![cert], key)
        .map_err(|e| tls(&e))?;
    // Every request is a new connection, and rustls would resume the last
    // one's session. Sunshine and its forks (Apollo, Vibepollo) ask for a
    // client certificate without setting OpenSSL's session id context, so
    // OpenSSL refuses any resumption with an `internal_error` alert: the
    // first HTTPS request works and the second fails.
    config.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(config))
}

pub(super) struct Reply {
    pub status: u16,
    pub body: Bytes,
    /// The address the connection went to: what a stream must use too.
    pub peer: SocketAddr,
}

/// Stops a spawned task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// One `GET path?query`, over TLS when `tls` is given, on its own connection.
/// `request` bounds everything after connecting.
#[allow(clippy::too_many_arguments)]
pub(super) async fn get(
    host: &str,
    port: u16,
    tls: Option<&Arc<ClientConfig>>,
    path: &str,
    query: &str,
    connect_timeout: Duration,
    request: Duration,
    max_body: usize,
) -> Result<Reply, ClientError> {
    let stream = connect(host, port, connect_timeout).await?;
    let peer = stream.peer_addr().map_err(|source| ClientError::Connect {
        addr: host.to_owned(),
        source,
    })?;
    let host_header = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let uri = if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{query}")
    };
    let req = Request::get(uri)
        .header(HOST, host_header)
        .header(CONNECTION, "close")
        .body(Empty::<Bytes>::new())
        .map_err(|e| ClientError::Http(e.to_string()))?;

    let work = async move {
        match tls {
            None => exchange(TokioIo::new(stream), req, max_body, peer).await,
            Some(config) => {
                let connector = tokio_rustls::TlsConnector::from(config.clone());
                let name = ServerName::IpAddress(peer.ip().into());
                let stream = tokio::time::timeout(connect_timeout, connector.connect(name, stream))
                    .await
                    .map_err(|_| ClientError::Timeout("the TLS handshake"))?
                    .map_err(|e| ClientError::Tls(e.to_string()))?;
                exchange(TokioIo::new(stream), req, max_body, peer).await
            }
        }
    };
    tokio::time::timeout(request, work)
        .await
        .map_err(|_| ClientError::Timeout("the request"))?
}

async fn exchange<I>(
    io: I,
    req: Request<Empty<Bytes>>,
    max_body: usize,
    peer: SocketAddr,
) -> Result<Reply, ClientError>
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let http = |e: &dyn std::fmt::Display| ClientError::Http(e.to_string());
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|e| http(&e))?;
    let _driver = AbortOnDrop(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let response = sender.send_request(req).await.map_err(|e| http(&e))?;
    let status = response.status().as_u16();
    let body = Limited::new(response.into_body(), max_body)
        .collect()
        .await
        .map_err(|e| ClientError::Http(format!("reading the answer: {e}")))?
        .to_bytes();
    Ok(Reply { status, body, peer })
}

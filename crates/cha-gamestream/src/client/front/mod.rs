//! The client's control plane (ADR 0011): what a Moonlight client says to a
//! host before a stream exists. [`HostClient`] talks nvhttp over HTTP and
//! HTTPS with our certificate (serverinfo, the five-phase pairing, applist,
//! appasset, launch, resume, cancel) and RTSP, and a launch ends in a
//! [`StreamSetup`](crate::client::StreamSetup) for [`media`](crate::client::media).
//!
//! It is the other side of the host's `front`, and shares its pairing
//! maths, TLS signature checks and SDP vocabulary. Where the protocol is
//! ambiguous it follows moonlight-common-c. Every step is bounded in time
//! and size, and no reply from a host can make it panic.
//!
//! Discovery (`_nvstream._tcp`) isn't here: the node browses mDNS itself and
//! hands an address to [`HostClient::new`].

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use bytes::Bytes;

use crate::directory::App;

mod error;
mod http;
mod identity;
mod info;
mod launch;
mod pair;
mod rtsp;
mod sdp;
mod xml;

pub use error::{ClientError, PairingError};
pub use http::Timeouts;
pub use identity::ClientIdentity;
pub use info::HostInfo;
pub use launch::{ColorSpace, Encrypt, StreamRequest};
pub use pair::random_pin;

const DEFAULT_HTTP_PORT: u16 = 47989;
const DEFAULT_HTTPS_PORT: u16 = 47984;

struct State {
    http_port: u16,
    /// 0 until the host has told us (`serverinfo`) or we were given it.
    https_port: u16,
    pinned: Option<http::Pinned>,
    tls: Option<Arc<rustls::ClientConfig>>,
}

/// One host, as one client knows it. Cheap to share by reference: every
/// method takes `&self` and uses its own connection.
pub struct HostClient {
    host: String,
    identity: ClientIdentity,
    unique_id: String,
    timeouts: Timeouts,
    state: Mutex<State>,
}

/// `host`, `host:port`, `[v6]`, `[v6]:port` or a bare IPv6 address.
fn split_address(address: &str) -> Result<(String, Option<u16>), ClientError> {
    let bad = || ClientError::Address(address.to_owned());
    let address = address.trim();
    let (host, port) = if let Some(rest) = address.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or_else(bad)?;
        match after.strip_prefix(':') {
            Some(p) => (host, Some(p)),
            None if after.is_empty() => (host, None),
            None => return Err(bad()),
        }
    } else if address.matches(':').count() == 1 {
        let (host, port) = address.split_once(':').ok_or_else(bad)?;
        (host, Some(port))
    } else {
        (address, None)
    };
    let port = port
        .map(|p| p.parse::<u16>().ok().filter(|&p| p != 0).ok_or_else(bad))
        .transpose()?;
    if host.is_empty() || host.contains(|c: char| c.is_whitespace() || "/?#@".contains(c)) {
        return Err(bad());
    }
    Ok((host.to_owned(), port))
}

/// Percent-encodes a query value: everything but unreserved characters.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl HostClient {
    /// A client for the host at `address`: a name or IP, optionally with the
    /// plain HTTP port (47989 by default). The HTTPS port is learned from
    /// `serverinfo`.
    pub fn new(address: &str, identity: ClientIdentity) -> Result<Self, ClientError> {
        let (host, port) = split_address(address)?;
        Ok(Self {
            host,
            unique_id: identity.default_unique_id(),
            identity,
            timeouts: Timeouts::default(),
            state: Mutex::new(State {
                http_port: port.unwrap_or(DEFAULT_HTTP_PORT),
                https_port: 0,
                pinned: None,
                tls: None,
            }),
        })
    }

    /// The `uniqueid` sent with requests (by default 16 hex digits of our
    /// certificate's fingerprint). Hosts store it with the paired client.
    pub fn with_unique_id(mut self, unique_id: impl Into<String>) -> Self {
        self.unique_id = unique_id.into();
        self
    }

    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// The host's certificate (PEM) from an earlier pairing, which HTTPS
    /// connections will require.
    pub fn with_server_cert(self, pem: &str) -> Result<Self, ClientError> {
        self.set_pinned(Some(http::Pinned::from_pem(pem)?));
        Ok(self)
    }

    /// The HTTPS port, when it is known (`serverinfo` says it, as does the
    /// host's address book entry).
    pub fn with_https_port(self, port: u16) -> Self {
        self.lock().https_port = port;
        self
    }

    pub fn identity(&self) -> &ClientIdentity {
        &self.identity
    }

    /// The address given, without brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The pinned host certificate (PEM): set by [`pair`](Self::pair) or
    /// [`with_server_cert`](Self::with_server_cert).
    pub fn server_cert_pem(&self) -> Option<String> {
        self.pinned().map(|p| p.pem)
    }

    /// The HTTPS port, 0 while unknown.
    pub fn https_port(&self) -> u16 {
        self.lock().https_port
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("host client state")
    }

    fn pinned(&self) -> Option<http::Pinned> {
        self.lock().pinned.clone()
    }

    fn set_pinned(&self, pinned: Option<http::Pinned>) {
        let mut state = self.lock();
        state.pinned = pinned;
        state.tls = None;
    }

    fn tls(&self) -> Result<Arc<rustls::ClientConfig>, ClientError> {
        let mut state = self.lock();
        if let Some(tls) = &state.tls {
            return Ok(tls.clone());
        }
        let pinned = state.pinned.as_ref().ok_or(ClientError::NoPinnedCert)?;
        let config = http::tls_config(&self.identity, pinned)?;
        state.tls = Some(config.clone());
        Ok(config)
    }

    fn query(&self, params: &[(&str, String)]) -> Result<String, ClientError> {
        let mut uuid = [0u8; 16];
        SystemRandom::new()
            .fill(&mut uuid)
            .map_err(|_| ClientError::Tls("no randomness".into()))?;
        let mut parts = vec![
            format!("uniqueid={}", escape(&self.unique_id)),
            format!("uuid={}", hex::encode(uuid)),
        ];
        parts.extend(params.iter().map(|(k, v)| format!("{k}={}", escape(v))));
        Ok(parts.join("&"))
    }

    /// One nvhttp GET, answered with XML whose `status_code` is 200.
    async fn nvhttp(
        &self,
        https: bool,
        path: &str,
        params: Vec<(&str, String)>,
        timeout: std::time::Duration,
    ) -> Result<(xml::Node, SocketAddr), ClientError> {
        let reply = self.fetch(https, path, &params, timeout).await?;
        if reply.status != 200 {
            return Err(http_status(reply.status, &reply.body));
        }
        let doc = xml::parse(&String::from_utf8_lossy(&reply.body));
        let root = doc
            .find("root")
            .ok_or_else(|| ClientError::Malformed(format!("{path}: the answer has no <root>")))?;
        let code = root
            .attr("status_code")
            .and_then(|c| c.trim().parse::<i32>().ok())
            .ok_or_else(|| {
                ClientError::Malformed(format!("{path}: the answer has no status_code"))
            })?;
        if code != 200 {
            return Err(match code {
                401 => ClientError::NotPaired,
                _ => ClientError::Host {
                    code,
                    message: root.attr("status_message").unwrap_or("").to_owned(),
                },
            });
        }
        Ok((root.clone(), reply.peer))
    }

    async fn fetch(
        &self,
        https: bool,
        path: &str,
        params: &[(&str, String)],
        timeout: std::time::Duration,
    ) -> Result<http::Reply, ClientError> {
        let (tls, port) = if https {
            let port = self.lock().https_port;
            if port == 0 {
                // Needs the pinned certificate to be of any use; the port can be guessed.
                (Some(self.tls()?), DEFAULT_HTTPS_PORT)
            } else {
                (Some(self.tls()?), port)
            }
        } else {
            (None, self.lock().http_port)
        };
        http::get(
            &self.host,
            port,
            tls.as_ref(),
            &format!("/{path}"),
            &self.query(params)?,
            self.timeouts.connect,
            timeout,
            http::MAX_BODY,
        )
        .await
    }

    /// `serverinfo`: over HTTPS when we hold the host's certificate (the only
    /// way the host can say whether we are paired), else over plain HTTP.
    pub async fn server_info(&self) -> Result<HostInfo, ClientError> {
        if self.pinned().is_some() {
            if self.https_port() == 0 {
                // Learn the port first.
                self.server_info_http().await?;
            }
            match self
                .nvhttp(true, "serverinfo", Vec::new(), self.timeouts.request)
                .await
            {
                Ok((root, _)) => return HostInfo::from_xml(&root),
                // The host doesn't know our certificate (it was unpaired there):
                // plain HTTP still answers, as unpaired.
                Err(ClientError::NotPaired) => {}
                Err(e) => return Err(e),
            }
        }
        self.server_info_http().await
    }

    /// `serverinfo` over plain HTTP; learns the HTTPS port.
    async fn server_info_http(&self) -> Result<HostInfo, ClientError> {
        let (root, _) = self
            .nvhttp(false, "serverinfo", Vec::new(), self.timeouts.request)
            .await?;
        let info = HostInfo::from_xml(&root)?;
        self.lock().https_port = if info.https_port != 0 {
            info.https_port
        } else {
            DEFAULT_HTTPS_PORT
        };
        Ok(info)
    }

    /// The apps this host lets us launch. Needs pairing.
    pub async fn app_list(&self) -> Result<Vec<App>, ClientError> {
        let (root, _) = self
            .nvhttp(true, "applist", Vec::new(), self.timeouts.request)
            .await?;
        Ok(root
            .children_named("App")
            .filter_map(|app| {
                Some(App {
                    id: app.text_of("ID")?.trim().parse().ok()?,
                    title: app.text_of("AppTitle")?.to_owned(),
                    hdr: app.text_of("IsHdrSupported") == Some("1"),
                })
            })
            .collect())
    }

    /// An app's box art (PNG or JPEG, as the host has it). Needs pairing.
    pub async fn app_asset(&self, app_id: u32) -> Result<Bytes, ClientError> {
        let reply = self
            .fetch(
                true,
                "appasset",
                &[
                    ("appid", app_id.to_string()),
                    ("AssetType", "2".into()),
                    ("AssetIdx", "0".into()),
                ],
                self.timeouts.request,
            )
            .await?;
        match reply.status {
            200 => Ok(reply.body),
            404 => Err(ClientError::Host {
                code: 404,
                message: "the app has no box art".into(),
            }),
            status => Err(http_status(status, &reply.body)),
        }
    }
}

fn http_status(status: u16, body: &[u8]) -> ClientError {
    if status == 401 {
        return ClientError::NotPaired;
    }
    let text = String::from_utf8_lossy(&body[..body.len().min(120)]);
    ClientError::Http(format!("the host answered {status}: {}", text.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_split_into_host_and_port() {
        let ok = |s: &str| split_address(s).unwrap();
        assert_eq!(ok("192.168.1.5"), ("192.168.1.5".into(), None));
        assert_eq!(ok("192.168.1.5:5000"), ("192.168.1.5".into(), Some(5000)));
        assert_eq!(ok("host.lan"), ("host.lan".into(), None));
        assert_eq!(ok("host.lan:47989"), ("host.lan".into(), Some(47989)));
        assert_eq!(ok("[fe80::1]"), ("fe80::1".into(), None));
        assert_eq!(ok("[fe80::1]:48000"), ("fe80::1".into(), Some(48000)));
        assert_eq!(ok("fe80::1"), ("fe80::1".into(), None));
        for bad in [
            "",
            " ",
            "host:0",
            "host:99999",
            "host:x",
            "[fe80::1",
            "[::1]x",
            "a b",
            "a/b",
            "u@h",
            ":80",
        ] {
            assert!(split_address(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(escape("My Mac & Co"), "My%20Mac%20%26%20Co");
        assert_eq!(escape("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(escape("é"), "%C3%A9");
        assert_eq!(escape("a=b&c"), "a%3Db%26c");
    }
}

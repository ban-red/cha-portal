// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: apps, launching and sessions come from the Directory and the session state machine; the paired
// check is by certificate fingerprint through the PairingStore; /unpair unpairs; request sizes and times
// are bounded; boxart bytes come from the Directory; no PIN page, no desktop notifications.

//! nvhttp: the HTTP API of a GameStream host, on two ports. Plain HTTP
//! serves `serverinfo`, `pair` and `unpair`; HTTPS adds `applist`, `appasset`,
//! `launch`, `resume` and `cancel`, for clients whose certificate is paired.
//! The documents are in [`xml`].

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Body;
use hyper::header::{CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};

use crate::config::HostConfig;
use crate::directory::{Directory, LaunchRequest, PairingStore};
use crate::handoff::{ClientId, SessionKeys};
use crate::net::{flagged, unmap};

use super::identity::fingerprint;
use super::pairing::Pairing;
use super::session::{SessionError, Sessions};

pub(crate) mod tls;
pub mod xml;

/// Largest request body nvhttp looks at: none of its requests have one.
const MAX_BODY: u64 = 4096;
/// Request line and headers are bounded to this.
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MIN_SIZE: u32 = 64;
const MAX_SIZE: u32 = 16_384;
const MAX_FPS: u32 = 1000;

pub(crate) struct Context {
    pub config: Arc<HostConfig>,
    pub directory: Arc<dyn Directory>,
    pub store: Arc<dyn PairingStore>,
    pub pairing: Pairing,
    pub sessions: Arc<Sessions>,
    pub shutdown: watch::Receiver<bool>,
}

/// What a request knows about its connection.
struct Conn {
    peer: IpAddr,
    local: IpAddr,
    https: bool,
    /// The fingerprint of the certificate the client presented, if any.
    client: Option<ClientId>,
}

struct Reply {
    status: StatusCode,
    content_type: &'static str,
    body: Bytes,
}

impl Reply {
    fn xml(body: String) -> Self {
        Self {
            status: StatusCode::OK,
            content_type: "application/xml",
            body: body.into(),
        }
    }
    fn text(status: StatusCode, message: &str) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: Bytes::copy_from_slice(message.as_bytes()),
        }
    }
    fn into_response(self) -> Response<Full<Bytes>> {
        let mut response = Response::new(Full::new(self.body));
        *response.status_mut() = self.status;
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(self.content_type));
        response
    }
}

/// A bound listener and the loop that serves it.
pub(crate) async fn serve(
    listener: TcpListener,
    tls: Option<tokio_rustls::TlsAcceptor>,
    ctx: Arc<Context>,
) {
    let permits = Arc::new(Semaphore::new(ctx.config.max_connections));
    let mut shutdown = ctx.shutdown.clone();
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("nvhttp accept: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = flagged(&mut shutdown) => break,
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            tracing::debug!("nvhttp: too many connections, dropping {peer}");
            continue;
        };
        let (ctx, tls) = (ctx.clone(), tls.clone());
        tokio::spawn(async move {
            let _permit = permit;
            connection(stream, peer, tls, ctx).await;
        });
    }
}

async fn connection(
    stream: TcpStream,
    peer: SocketAddr,
    tls: Option<tokio_rustls::TlsAcceptor>,
    ctx: Arc<Context>,
) {
    let local = stream
        .local_addr()
        .map_or(IpAddr::from([0, 0, 0, 0]), |a| a.ip());
    let conn_base = |client| Conn {
        peer: unmap(peer.ip()),
        local: unmap(local),
        https: tls.is_some(),
        client,
    };
    let mut shutdown = ctx.shutdown.clone();
    let builder = || {
        let mut b = http1::Builder::new();
        b.timer(TokioTimer::new())
            .header_read_timeout(ctx.config.request_timeout)
            .max_buf_size(MAX_HEADER_BYTES);
        b
    };
    match &tls {
        None => {
            let conn = Arc::new(conn_base(None));
            let service = service_fn(|req| handle(ctx.clone(), conn.clone(), req));
            tokio::select! {
                _ = builder().serve_connection(TokioIo::new(stream), service) => {}
                _ = flagged(&mut shutdown) => {}
            }
        }
        Some(acceptor) => {
            let Ok(Ok(stream)) =
                tokio::time::timeout(ctx.config.request_timeout, acceptor.accept(stream)).await
            else {
                tracing::debug!("TLS handshake with {peer} failed");
                return;
            };
            let client = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|certs| certs.first())
                .map(|cert| ClientId(fingerprint(cert.as_ref())));
            let conn = Arc::new(conn_base(client));
            let service = service_fn(|req| handle(ctx.clone(), conn.clone(), req));
            tokio::select! {
                _ = builder().serve_connection(TokioIo::new(stream), service) => {}
                _ = flagged(&mut shutdown) => {}
            }
        }
    }
}

async fn handle(
    ctx: Arc<Context>,
    conn: Arc<Conn>,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    if req.body().size_hint().upper().is_none_or(|n| n > MAX_BODY) {
        return Ok(Reply::text(StatusCode::PAYLOAD_TOO_LARGE, "request too large").into_response());
    }
    let params: HashMap<String, String> = req
        .uri()
        .query()
        .map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default();
    let reply = route(&ctx, &conn, req.method(), req.uri().path(), params).await;
    drop(req.into_body().collect().await);
    Ok(reply.into_response())
}

async fn route(
    ctx: &Context,
    conn: &Conn,
    method: &Method,
    path: &str,
    params: HashMap<String, String>,
) -> Reply {
    if method != Method::GET {
        return Reply::text(StatusCode::NOT_FOUND, "NOT FOUND");
    }
    match path {
        "/serverinfo" => server_info(ctx, conn).await,
        "/pair" => {
            let mut shutdown = ctx.shutdown.clone();
            Reply::xml(
                ctx.pairing
                    .handle(&params, conn.peer, conn.client.as_ref(), &mut shutdown)
                    .await,
            )
        }
        "/unpair" => unpair(ctx, conn, &params).await,
        "/applist" | "/appasset" | "/launch" | "/resume" | "/cancel" if conn.https => {
            let client = match authorize(ctx, conn).await {
                Ok(client) => client,
                Err(reply) => return reply,
            };
            match path {
                "/applist" => Reply::xml(xml::app_list(&ctx.directory.apps(&client).await)),
                "/appasset" => app_asset(ctx, &client, &params).await,
                "/launch" => launch(ctx, conn, &client, &params).await,
                "/resume" => resume(ctx, conn, &client, &params).await,
                _ => cancel(ctx, &client).await,
            }
        }
        _ => Reply::text(StatusCode::NOT_FOUND, "NOT FOUND"),
    }
}

/// The paired client behind an HTTPS request, or the refusal.
async fn authorize(ctx: &Context, conn: &Conn) -> Result<ClientId, Reply> {
    let Some(client) = &conn.client else {
        return Err(Reply::text(
            StatusCode::UNAUTHORIZED,
            "No client certificate provided.",
        ));
    };
    match ctx.store.is_paired(client).await {
        Ok(true) => Ok(client.clone()),
        Ok(false) => Err(Reply::text(
            StatusCode::UNAUTHORIZED,
            "Client certificate is not from a paired client.",
        )),
        Err(e) => {
            tracing::error!("{e}");
            Err(Reply::text(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to check the paired clients.",
            ))
        }
    }
}

async fn server_info(ctx: &Context, conn: &Conn) -> Reply {
    // Only HTTPS can say whether this client is paired: it has a certificate.
    let paired = match (&conn.client, conn.https) {
        (Some(client), true) => ctx.store.is_paired(client).await.unwrap_or(false),
        _ => false,
    };
    let local_ip = conn.local.to_string();
    Reply::xml(xml::server_info(&xml::ServerInfo {
        hostname: &ctx.config.name,
        unique_id: &ctx.config.unique_id,
        https_port: ctx.config.ports.https,
        http_port: ctx.config.ports.http,
        mac: ctx.config.mac.as_deref().unwrap_or("00:00:00:00:00:00"),
        local_ip: &local_ip,
        codec_mode_support: xml::codec_mode_support(&ctx.config.capabilities),
        paired,
        current_game: ctx.sessions.current_app(),
    }))
}

fn sniff_image(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else {
        "image/png"
    }
}

async fn app_asset(ctx: &Context, client: &ClientId, params: &HashMap<String, String>) -> Reply {
    let Some(id) = params.get("appid").and_then(|v| v.parse::<u32>().ok()) else {
        return Reply::text(StatusCode::BAD_REQUEST, "bad appid");
    };
    match ctx.directory.app_image(client, id).await {
        Some(bytes) => Reply {
            status: StatusCode::OK,
            content_type: sniff_image(&bytes),
            body: bytes,
        },
        None => Reply::text(StatusCode::NOT_FOUND, "no box art"),
    }
}

async fn unpair(ctx: &Context, conn: &Conn, params: &HashMap<String, String>) -> Reply {
    let target = if let (true, Some(client)) = (conn.https, &conn.client) {
        // A client's own certificate says who it is.
        Some(client.clone())
    } else if ctx.config.unauthenticated_unpair {
        match (params.get("uniqueid"), ctx.store.list().await) {
            (Some(unique_id), Ok(all)) => all
                .into_iter()
                .find(|c| &c.unique_id == unique_id)
                .map(|c| c.client),
            _ => None,
        }
    } else {
        None
    };
    if let Some(client) = target {
        match ctx.store.remove(&client).await {
            Ok(true) => {
                tracing::info!("unpaired {client}");
                // Its session goes with it.
                let _ = ctx.sessions.cancel(ctx.directory.as_ref(), &client).await;
            }
            Ok(false) => {}
            Err(e) => tracing::error!("{e}"),
        }
    }
    Reply::xml(r#"<root status_code="200"/>"#.into())
}

fn session_url(ctx: &Context, conn: &Conn) -> String {
    let host = match conn.local {
        IpAddr::V6(v6) => format!("[{v6}]"),
        v4 => v4.to_string(),
    };
    format!(
        "<sessionUrl0>rtsp://{host}:{}</sessionUrl0>",
        ctx.config.ports.rtsp
    )
}

fn keys(params: &HashMap<String, String>) -> Result<SessionKeys, String> {
    let key = params
        .get("rikey")
        .and_then(|k| hex::decode(k).ok())
        .and_then(|k| <[u8; 16]>::try_from(k).ok())
        .ok_or("rikey must be 16 bytes of hex")?;
    let key_id = params
        .get("rikeyid")
        .and_then(|v| v.parse::<i64>().ok())
        .ok_or("bad rikeyid")?;
    Ok(SessionKeys { key, key_id })
}

fn parse_launch(params: &HashMap<String, String>) -> Result<LaunchRequest, String> {
    let app_id = params
        .get("appid")
        .and_then(|v| v.parse::<u32>().ok())
        .ok_or("bad appid")?;
    let mode = params.get("mode").ok_or("missing mode")?;
    let mut parts = mode.split('x').map(|p| p.parse::<u32>().ok());
    let (Some(Some(width)), Some(Some(height)), Some(Some(fps)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(format!("mode must be WxHxR, got {mode:?}"));
    };
    if !(MIN_SIZE..=MAX_SIZE).contains(&width)
        || !(MIN_SIZE..=MAX_SIZE).contains(&height)
        || !(1..=MAX_FPS).contains(&fps)
    {
        return Err(format!("mode {mode} is out of range"));
    }
    let flag = |name: &str| {
        params
            .get(name)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(0)
    };
    Ok(LaunchRequest {
        app_id,
        width,
        height,
        fps,
        hdr: flag("hdrMode") != 0,
        // Stereo when the client doesn't say.
        surround_audio_info: params
            .get("surroundAudioInfo")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0x30002),
        local_audio: flag("localAudioPlayMode") != 0,
        optimize_game_settings: flag("sops") != 0,
        gamepad_mask: flag("gcmap"),
    })
}

fn session_error(e: &SessionError) -> Reply {
    Reply::xml(match e {
        SessionError::Busy => xml::error(400, "An app is already running on this host"),
        SessionError::NoSession => xml::error(400, "No app is running on this host"),
        SessionError::NotOwner => xml::error(403, "The running app belongs to another client"),
        SessionError::NotReady => xml::error(400, "The session isn't ready"),
        SessionError::Directory(e) => xml::error(503, &e.to_string()),
    })
}

async fn launch(
    ctx: &Context,
    conn: &Conn,
    client: &ClientId,
    params: &HashMap<String, String>,
) -> Reply {
    let (request, keys) = match (parse_launch(params), keys(params)) {
        (Ok(r), Ok(k)) => (r, k),
        (Err(e), _) | (_, Err(e)) => return Reply::xml(xml::error(400, &e)),
    };
    match ctx
        .sessions
        .launch(ctx.directory.as_ref(), client, conn.peer, request, keys)
        .await
    {
        Ok(()) => Reply::xml(xml::ok(&format!(
            "<gamesession>1</gamesession>{}",
            session_url(ctx, conn)
        ))),
        Err(e) => session_error(&e),
    }
}

async fn resume(
    ctx: &Context,
    conn: &Conn,
    client: &ClientId,
    params: &HashMap<String, String>,
) -> Reply {
    let keys = match keys(params) {
        Ok(k) => k,
        Err(e) => return Reply::xml(xml::error(400, &e)),
    };
    let surround = params
        .get("surroundAudioInfo")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x30002);
    match ctx
        .sessions
        .resume(ctx.directory.as_ref(), client, conn.peer, keys, surround)
        .await
    {
        Ok(()) => Reply::xml(xml::ok(&format!(
            "{}<resume>1</resume>",
            session_url(ctx, conn)
        ))),
        Err(e) => session_error(&e),
    }
}

async fn cancel(ctx: &Context, client: &ClientId) -> Reply {
    match ctx.sessions.cancel(ctx.directory.as_ref(), client).await {
        Ok(()) => Reply::xml(xml::ok("<cancel>1</cancel>")),
        Err(e) => session_error(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn launch_parameters_parse() {
        let r = parse_launch(&p(&[
            ("appid", "3"),
            ("mode", "2560x1440x60"),
            ("hdrMode", "1"),
            ("sops", "1"),
            ("gcmap", "5"),
        ]))
        .unwrap();
        assert_eq!((r.app_id, r.width, r.height, r.fps), (3, 2560, 1440, 60));
        assert!(r.hdr && r.optimize_game_settings && !r.local_audio);
        assert_eq!((r.gamepad_mask, r.surround_audio_info), (5, 0x30002));
    }

    #[test]
    fn hostile_launch_parameters_are_refused() {
        for mode in [
            "1x1x1",
            "99999x1080x60",
            "1920x1080",
            "1920x1080x60x1",
            "axbxc",
            "1920x1080x0",
            "1920x1080x100000",
        ] {
            assert!(
                parse_launch(&p(&[("appid", "1"), ("mode", mode)])).is_err(),
                "{mode}"
            );
        }
        assert!(parse_launch(&p(&[("mode", "1920x1080x60")])).is_err());
        assert!(parse_launch(&p(&[("appid", "-1"), ("mode", "1920x1080x60")])).is_err());
    }

    #[test]
    fn keys_must_be_sixteen_bytes() {
        assert!(
            keys(&p(&[
                ("rikey", "00112233445566778899aabbccddeeff"),
                ("rikeyid", "-5")
            ]))
            .is_ok()
        );
        assert!(keys(&p(&[("rikey", "0011"), ("rikeyid", "1")])).is_err());
        assert!(keys(&p(&[("rikey", "zz"), ("rikeyid", "1")])).is_err());
        assert!(keys(&p(&[("rikey", "00112233445566778899aabbccddeeff")])).is_err());
    }

    #[test]
    fn images_are_sniffed() {
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(sniff_image(b"\x89PNG"), "image/png");
    }
}

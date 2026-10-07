// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: no rtsp-types dependency (a small parser for the one dialect Moonlight speaks, with bounded sizes and
// a read timeout), SETUP reports the ports the Directory reserved, ANNOUNCE and PLAY go through the session
// state machine, and only the launching client's address gets answers.

//! RTSP as Moonlight uses it: a connection per request, `OPTIONS`,
//! `DESCRIBE`, three `SETUP`s (audio, video, control) that tell the client the
//! media ports, `ANNOUNCE` carrying its choices as SDP and `PLAY` to start.
//!
//! A request from an address other than the one that launched the session is
//! closed on without an answer.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};

use crate::config::HostConfig;
use crate::directory::Directory;
use crate::net::{flagged, unmap};

use super::session::{RtspView, SessionError, Sessions};

pub(crate) mod sdp;

const MAX_HEAD: usize = 8 * 1024;
const MAX_BODY: usize = 16 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub method: String,
    pub target: String,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ParseError {
    #[error("the request head is too large")]
    HeadTooLarge,
    #[error("the body is too large")]
    BodyTooLarge,
    #[error("malformed request line")]
    RequestLine,
    #[error("malformed header")]
    Header,
    #[error("bad Content-Length")]
    ContentLength,
}

/// A request head that may not be complete yet.
pub(crate) enum Head {
    /// Needs more bytes.
    Incomplete,
    /// The request, and how much of the head was consumed.
    Done(Request, usize, usize),
}

/// Parses the head (request line and headers) from `buf`; returns the
/// request without its body, the head's length and the body's expected length.
pub(crate) fn parse_head(buf: &[u8]) -> Result<Head, ParseError> {
    let end = match buf.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => i + 4,
        None if buf.len() > MAX_HEAD => return Err(ParseError::HeadTooLarge),
        None => return Ok(Head::Incomplete),
    };
    if end > MAX_HEAD {
        return Err(ParseError::HeadTooLarge);
    }
    let text = std::str::from_utf8(&buf[..end]).map_err(|_| ParseError::RequestLine)?;
    let mut lines = text.split("\r\n");
    let request_line = lines.next().ok_or(ParseError::RequestLine)?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(ParseError::RequestLine);
    };
    if !version.starts_with("RTSP/")
        || method.is_empty()
        || !method.bytes().all(|b| b.is_ascii_uppercase())
    {
        return Err(ParseError::RequestLine);
    }
    let mut headers = Vec::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (k, v) = line.split_once(':').ok_or(ParseError::Header)?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
    }
    let length = match headers.iter().find(|(k, _)| k == "content-length") {
        Some((_, v)) => v.parse::<usize>().map_err(|_| ParseError::ContentLength)?,
        None => 0,
    };
    if length > MAX_BODY {
        return Err(ParseError::BodyTooLarge);
    }
    Ok(Head::Done(
        Request {
            method: method.to_owned(),
            target: target.to_owned(),
            headers,
            body: Vec::new(),
        },
        end,
        length,
    ))
}

/// One response.
struct Response {
    status: u16,
    reason: &'static str,
    headers: Vec<(&'static str, String)>,
    body: String,
}

impl Response {
    fn new(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            headers: Vec::new(),
            body: String::new(),
        }
    }
    fn ok() -> Self {
        Self::new(200, "OK")
    }
    fn bad() -> Self {
        Self::new(400, "Bad Request")
    }
    fn header(mut self, k: &'static str, v: impl Into<String>) -> Self {
        self.headers.push((k, v.into()));
        self
    }
    fn encode(&self, cseq: &str) -> Vec<u8> {
        let mut out = format!(
            "RTSP/1.0 {} {}\r\nCSeq: {cseq}\r\n",
            self.status, self.reason
        );
        for (k, v) in &self.headers {
            out += &format!("{k}: {v}\r\n");
        }
        if !self.body.is_empty() {
            out += &format!("Content-Length: {}\r\n", self.body.len());
        }
        out += "\r\n";
        out += &self.body;
        out.into_bytes()
    }
}

pub(crate) struct Context {
    pub config: Arc<HostConfig>,
    pub directory: Arc<dyn Directory>,
    pub sessions: Arc<Sessions>,
    pub shutdown: watch::Receiver<bool>,
}

pub(crate) async fn serve(listener: TcpListener, ctx: Arc<Context>) {
    let permits = Arc::new(Semaphore::new(ctx.config.max_connections));
    let mut shutdown = ctx.shutdown.clone();
    loop {
        let (stream, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!("RTSP accept: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            _ = flagged(&mut shutdown) => break,
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            continue;
        };
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let timeout = ctx.config.request_timeout;
            if tokio::time::timeout(timeout, connection(stream, peer, &ctx))
                .await
                .is_err()
            {
                tracing::debug!("RTSP request from {peer} timed out");
            }
        });
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    let (mut request, head_len, body_len) = loop {
        match parse_head(&buf) {
            Ok(Head::Done(r, head, body)) => break (r, head, body),
            Ok(Head::Incomplete) => {}
            Err(e) => {
                tracing::debug!("RTSP: {e}");
                return None;
            }
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    while buf.len() < head_len + body_len {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    request.body = buf[head_len..head_len + body_len].to_vec();
    Some(request)
}

async fn connection(mut stream: TcpStream, peer: SocketAddr, ctx: &Context) {
    let peer_ip = unmap(peer.ip());
    // Nobody but the launching client gets to speak RTSP.
    let Some(view) = ctx.sessions.rtsp_view(peer_ip) else {
        tracing::debug!("RTSP from {peer}: no session for that address");
        return;
    };
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let cseq = request.header("cseq").map(str::to_owned);
    let response = match &cseq {
        Some(c) if c.len() <= 11 && c.bytes().all(|b| b.is_ascii_digit()) => {
            dispatch(ctx, &view, peer_ip, &request).await
        }
        _ => Response::bad(),
    };
    let wire = response.encode(
        cseq.as_deref()
            .filter(|c| c.bytes().all(|b| b.is_ascii_digit()))
            .unwrap_or("0"),
    );
    if stream.write_all(&wire).await.is_ok() {
        // Moonlight reads a response to the end of the connection.
        let _ = stream.shutdown().await;
    }
}

fn session_token(view: &RtspView) -> String {
    format!("{:08X}", view.connect_data)
}

async fn dispatch(ctx: &Context, view: &RtspView, peer: IpAddr, request: &Request) -> Response {
    tracing::debug!("RTSP {} {}", request.method, request.target);
    match request.method.as_str() {
        "OPTIONS" => Response::ok().header("Public", "OPTIONS DESCRIBE SETUP ANNOUNCE PLAY"),
        "DESCRIBE" => {
            let mut r = Response::ok().header("Content-Type", "application/sdp");
            r.body = sdp::describe(&ctx.config);
            r
        }
        "SETUP" => setup(view, request),
        "ANNOUNCE" | "PLAY" => {
            // The client echoes the session token it was given in SETUP.
            let token = request
                .header("session")
                .and_then(|s| s.split(';').next())
                .map(str::trim);
            if token != Some(session_token(view).as_str()) {
                return Response::new(454, "Session Not Found");
            }
            if request.method == "ANNOUNCE" {
                announce(ctx, view, peer, request)
            } else {
                play(ctx, peer).await
            }
        }
        _ => Response::new(405, "Method Not Allowed"),
    }
}

fn setup(view: &RtspView, request: &Request) -> Response {
    // `streamid=audio/0/0`, bare or after a scheme and host.
    let Some(stream) = request
        .target
        .split("streamid=")
        .nth(1)
        .and_then(|s| s.split('/').next())
    else {
        return Response::bad();
    };
    let ports = view.media_ports;
    let (port, extra) = match stream {
        "video" => (
            ports.video,
            Some((
                "X-SS-Ping-Payload",
                String::from_utf8_lossy(&view.ping_payload).into_owned(),
            )),
        ),
        "audio" => (
            ports.audio,
            Some((
                "X-SS-Ping-Payload",
                String::from_utf8_lossy(&view.ping_payload).into_owned(),
            )),
        ),
        "control" => (
            ports.control,
            Some(("X-SS-Connect-Data", view.connect_data.to_string())),
        ),
        _ => return Response::bad(),
    };
    let mut r = Response::ok()
        .header("Session", format!("{};timeout = 90", session_token(view)))
        .header("Transport", format!("server_port={port}"));
    if let Some((k, v)) = extra {
        r = r.header(k, v);
    }
    r
}

fn announce(ctx: &Context, view: &RtspView, peer: IpAddr, request: &Request) -> Response {
    let Ok(body) = std::str::from_utf8(&request.body) else {
        return Response::bad();
    };
    let who = sdp::Peer {
        client: view.owner.clone(),
        client_ip: view.owner_ip,
        app_id: view.app_id,
        launch_surround_audio_info: view.launch.surround_audio_info,
    };
    match sdp::parse_announce(body, &ctx.config, who) {
        Ok((params, encryption)) => match ctx.sessions.announce(peer, params, encryption) {
            Ok(()) => Response::ok(),
            Err(_) => Response::new(454, "Session Not Found"),
        },
        Err(e) => {
            tracing::info!("ANNOUNCE refused: {e}");
            Response::bad()
        }
    }
}

async fn play(ctx: &Context, peer: IpAddr) -> Response {
    match ctx.sessions.play(ctx.directory.as_ref(), peer).await {
        Ok(()) => Response::ok(),
        Err(SessionError::NotReady) => Response::new(455, "Method Not Valid in This State"),
        Err(e) => {
            tracing::warn!("PLAY: {e}");
            Response::new(500, "Internal Server Error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(s: &str) -> Result<Head, ParseError> {
        parse_head(s.as_bytes())
    }

    #[test]
    fn parses_moonlights_requests() {
        // Moonlight's target is `streamid=...` with no scheme, and PLAY's is `/`.
        let text = "SETUP streamid=audio/0/0 RTSP/1.0\r\nCSeq: 3\r\nTransport: unicast;X-GS-ClientPort=50000-50001\r\nHost: 1.2.3.4\r\n\r\n";
        let Ok(Head::Done(r, len, body)) = head(text) else {
            panic!()
        };
        assert_eq!(
            (r.method.as_str(), r.target.as_str()),
            ("SETUP", "streamid=audio/0/0")
        );
        assert_eq!(r.header("cseq"), Some("3"));
        assert_eq!((len, body), (text.len(), 0));
        let Ok(Head::Done(r, _, body)) = head(
            "ANNOUNCE streamid=control/13/0 RTSP/1.0\r\nCSeq: 6\r\nContent-length: 120\r\n\r\n",
        ) else {
            panic!()
        };
        assert_eq!((r.method.as_str(), body), ("ANNOUNCE", 120));
        let Ok(Head::Done(r, ..)) = head("PLAY / RTSP/1.0\r\nCSeq: 7\r\n\r\n") else {
            panic!()
        };
        assert_eq!(r.target, "/");
    }

    #[test]
    fn incomplete_heads_wait() {
        assert!(matches!(
            head("OPTIONS rtsp://h RTSP/1.0\r\nCSeq: 1\r\n"),
            Ok(Head::Incomplete)
        ));
        assert!(matches!(head(""), Ok(Head::Incomplete)));
    }

    #[test]
    fn malformed_requests_are_errors() {
        assert_eq!(head("GARBAGE\r\n\r\n").err(), Some(ParseError::RequestLine));
        assert_eq!(
            head("get / RTSP/1.0\r\n\r\n").err(),
            Some(ParseError::RequestLine)
        );
        assert_eq!(
            head("GET / HTTP/1.1\r\n\r\n").err(),
            Some(ParseError::RequestLine)
        );
        assert_eq!(
            head("GET / RTSP/1.0\r\nno colon\r\n\r\n").err(),
            Some(ParseError::Header)
        );
        assert_eq!(
            head("GET / RTSP/1.0\r\nContent-Length: x\r\n\r\n").err(),
            Some(ParseError::ContentLength)
        );
        assert_eq!(
            head("GET / RTSP/1.0\r\nContent-Length: 999999999\r\n\r\n").err(),
            Some(ParseError::BodyTooLarge)
        );
        let huge = format!("GET / RTSP/1.0\r\nX: {}\r\n\r\n", "a".repeat(MAX_HEAD));
        assert_eq!(head(&huge).err(), Some(ParseError::HeadTooLarge));
        // No end of head in a flood of bytes: an error, not a wait for ever.
        assert_eq!(
            parse_head(&vec![b'a'; MAX_HEAD + 1]).err(),
            Some(ParseError::HeadTooLarge)
        );
    }

    #[test]
    fn responses_carry_cseq_and_length() {
        let r = Response::ok().header("Session", "X");
        assert_eq!(
            r.encode("9"),
            b"RTSP/1.0 200 OK\r\nCSeq: 9\r\nSession: X\r\n\r\n"
        );
        let mut r = Response::ok();
        r.body = "a=b\n".into();
        assert_eq!(
            r.encode("1"),
            b"RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: 4\r\n\r\na=b\n"
        );
    }

    /// Whatever bytes arrive, the parser returns.
    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0xDEAD_BEEF_CAFE_F00Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let atoms: [&[u8]; 8] = [
            b"SETUP ",
            b"RTSP/1.0",
            b"\r\n",
            b"\r\n\r\n",
            b"Content-Length: ",
            b"CSeq: 1",
            b": ",
            b"\xff\xfe",
        ];
        for _ in 0..20_000 {
            let mut buf = Vec::new();
            for _ in 0..(next() % 12) {
                if next() % 3 == 0 {
                    buf.extend((0..next() % 8).map(|_| next() as u8));
                } else {
                    buf.extend_from_slice(atoms[(next() % 8) as usize]);
                }
            }
            let _ = parse_head(&buf);
        }
    }
}

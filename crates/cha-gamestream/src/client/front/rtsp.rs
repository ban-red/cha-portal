//! RTSP as Moonlight speaks it: one TCP connection per request, the host
//! answers and closes. Responses are read within bounds and parsed without
//! trusting any length the host gives.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::ClientError;

/// Largest response (head and body) read.
const MAX_RESPONSE: usize = 256 * 1024;
/// How long to keep retrying a refused connection: some hosts answer
/// `/launch` before RTSP listens.
const CONNECT_RETRY: Duration = Duration::from_secs(10);
const RETRY_EVERY: Duration = Duration::from_millis(500);

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Response {
    pub status: u16,
    /// Header names lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub(super) enum ParseError {
    #[error("the answer ended before its head did")]
    Truncated,
    #[error("the answer's body is shorter than it says")]
    ShortBody,
    #[error("malformed status line")]
    StatusLine,
    #[error("bad Content-Length")]
    ContentLength,
    #[error("the answer is too large")]
    TooLarge,
}

/// What the bytes read so far make.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Parsed {
    /// Wait for more.
    Incomplete,
    Done(Response),
}

/// Parses a response from `buf`. `eof` says no more bytes will come. A body
/// is as long as `Content-Length` says, and is absent without one.
pub(super) fn parse(buf: &[u8], eof: bool) -> Result<Parsed, ParseError> {
    if buf.len() > MAX_RESPONSE {
        return Err(ParseError::TooLarge);
    }
    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4) else {
        return if eof {
            Err(ParseError::Truncated)
        } else {
            Ok(Parsed::Incomplete)
        };
    };
    let head = String::from_utf8_lossy(&buf[..head_end]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().ok_or(ParseError::StatusLine)?;
    let mut parts = status_line.splitn(3, ' ');
    let (Some(version), Some(code)) = (parts.next(), parts.next()) else {
        return Err(ParseError::StatusLine);
    };
    if !version.starts_with("RTSP/") {
        return Err(ParseError::StatusLine);
    }
    let status: u16 = code.trim().parse().map_err(|_| ParseError::StatusLine)?;
    let mut headers = Vec::new();
    for line in lines.filter(|l| !l.is_empty()).take(256) {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_owned()));
        }
    }
    let length = match headers.iter().find(|(k, _)| k == "content-length") {
        Some((_, v)) => v.parse::<usize>().map_err(|_| ParseError::ContentLength)?,
        None => 0,
    };
    if length > MAX_RESPONSE {
        return Err(ParseError::TooLarge);
    }
    let have = buf.len() - head_end;
    if have < length {
        return if eof {
            Err(ParseError::ShortBody)
        } else {
            Ok(Parsed::Incomplete)
        };
    }
    Ok(Parsed::Done(Response {
        status,
        headers,
        body: buf[head_end..head_end + length].to_vec(),
    }))
}

/// The RTSP conversation of one stream setup: sequence numbers and the
/// session token carry from request to request.
pub(super) struct Client {
    addr: SocketAddr,
    /// `Host:` header and the target of `OPTIONS` and `DESCRIBE`.
    host: String,
    target: String,
    version: u32,
    timeout: Duration,
    cseq: u32,
    pub session: Option<String>,
}

impl Client {
    pub fn new(addr: SocketAddr, version: u32, timeout: Duration) -> Self {
        let host = match addr.ip() {
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
            v4 => v4.to_string(),
        };
        Self {
            target: format!("rtsp://{host}:{}", addr.port()),
            host,
            addr,
            version,
            timeout,
            cseq: 0,
            session: None,
        }
    }

    /// The URL `OPTIONS` and `DESCRIBE` address.
    pub fn url(&self) -> &str {
        &self.target
    }

    /// One request on its own connection. `target` is a bare stream id or the
    /// session URL; `headers` come after the standard ones; the body, when
    /// there is one, is SDP.
    pub async fn request(
        &mut self,
        step: &'static str,
        method: &str,
        target: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> Result<Response, ClientError> {
        self.cseq += 1;
        let mut text = format!(
            "{method} {target} RTSP/1.0\r\nCSeq: {}\r\nX-GS-ClientVersion: {}\r\nHost: {}\r\n",
            self.cseq, self.version, self.host
        );
        for (k, v) in headers {
            text.push_str(&format!("{k}: {v}\r\n"));
        }
        if let Some(session) = &self.session
            && !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("session"))
            && method != "OPTIONS"
            && method != "DESCRIBE"
        {
            text.push_str(&format!("Session: {session}\r\n"));
        }
        if !body.is_empty() {
            text.push_str(&format!("Content-length: {}\r\n", body.len()));
        }
        text.push_str("\r\n");
        text.push_str(body);

        let err = |detail: String, status| ClientError::Rtsp {
            step,
            status,
            detail,
        };
        let work = async {
            let mut stream = self.connect().await?;
            stream
                .write_all(text.as_bytes())
                .await
                .map_err(|e| format!("sending: {e}"))?;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match parse(&buf, false).map_err(|e| e.to_string())? {
                    Parsed::Done(r) => return Ok::<_, String>(r),
                    Parsed::Incomplete => {}
                }
                let n = stream
                    .read(&mut chunk)
                    .await
                    .map_err(|e| format!("reading: {e}"))?;
                if n == 0 {
                    return match parse(&buf, true).map_err(|e| e.to_string())? {
                        Parsed::Done(r) => Ok(r),
                        Parsed::Incomplete => Err("the answer ended early".into()),
                    };
                }
                buf.extend_from_slice(&chunk[..n]);
            }
        };
        match tokio::time::timeout(self.timeout, work).await {
            Err(_) => Err(err("timed out".into(), None)),
            Ok(Err(detail)) => Err(err(detail, None)),
            Ok(Ok(response)) if response.status != 200 => Err(err(
                format!("the host answered {}", response.status),
                Some(response.status),
            )),
            Ok(Ok(response)) => Ok(response),
        }
    }

    async fn connect(&self) -> Result<TcpStream, String> {
        let deadline = tokio::time::Instant::now() + CONNECT_RETRY;
        loop {
            match TcpStream::connect(self.addr).await {
                Ok(s) => {
                    let _ = s.set_nodelay(true);
                    return Ok(s);
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::ConnectionRefused
                        && tokio::time::Instant::now() + RETRY_EVERY < deadline =>
                {
                    tokio::time::sleep(RETRY_EVERY).await;
                }
                Err(e) => return Err(format!("connecting to {}: {e}", self.addr)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn done(bytes: &[u8], eof: bool) -> Response {
        match parse(bytes, eof) {
            Ok(Parsed::Done(r)) => r,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_what_hosts_answer() {
        let r = done(
            b"RTSP/1.0 200 OK\r\nCSeq: 3\r\nSession: DEADBEEF;timeout = 90\r\nTransport: server_port=48000\r\n\r\n",
            false,
        );
        assert_eq!(r.status, 200);
        assert_eq!(r.header("session"), Some("DEADBEEF;timeout = 90"));
        assert_eq!(r.header("transport"), Some("server_port=48000"));
        assert!(r.body.is_empty());
        let r = done(
            b"RTSP/1.0 200 OK\r\nCSeq: 2\r\nContent-length: 4\r\n\r\na=b\nEXTRA",
            false,
        );
        assert_eq!(r.body, b"a=b\n");
        assert_eq!(
            done(b"RTSP/1.0 454 Session Not Found\r\n\r\n", true).status,
            454
        );
    }

    #[test]
    fn waits_for_what_has_not_come_and_says_when_it_never_will() {
        assert_eq!(
            parse(b"RTSP/1.0 200 OK\r\nCSeq", false),
            Ok(Parsed::Incomplete)
        );
        assert_eq!(
            parse(b"RTSP/1.0 200 OK\r\nContent-Length: 9\r\n\r\nabc", false),
            Ok(Parsed::Incomplete)
        );
        assert_eq!(
            parse(b"RTSP/1.0 200 OK\r\nCSeq", true),
            Err(ParseError::Truncated)
        );
        assert_eq!(
            parse(b"RTSP/1.0 200 OK\r\nContent-Length: 9\r\n\r\nabc", true),
            Err(ParseError::ShortBody)
        );
    }

    #[test]
    fn refuses_what_isnt_a_response() {
        for bad in [
            &b"HTTP/1.1 200 OK\r\n\r\n"[..],
            b"RTSP/1.0 abc OK\r\n\r\n",
            b"RTSP/1.0\r\n\r\n",
            b"\r\n\r\n",
        ] {
            assert_eq!(parse(bad, true), Err(ParseError::StatusLine), "{bad:?}");
        }
        assert_eq!(
            parse(b"RTSP/1.0 200 OK\r\nContent-Length: x\r\n\r\n", true),
            Err(ParseError::ContentLength)
        );
        assert_eq!(
            parse(
                b"RTSP/1.0 200 OK\r\nContent-Length: 99999999999\r\n\r\n",
                true
            ),
            Err(ParseError::TooLarge)
        );
        assert_eq!(
            parse(&vec![b'a'; MAX_RESPONSE + 1], false),
            Err(ParseError::TooLarge)
        );
    }

    /// Whatever bytes arrive, the parser returns.
    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0xFEED_FACE_DEAD_BEEFu64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let atoms: [&[u8]; 9] = [
            b"RTSP/1.0 ",
            b"200 OK",
            b"\r\n",
            b"\r\n\r\n",
            b"Content-Length: ",
            b"99",
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
                    buf.extend_from_slice(atoms[(next() % 9) as usize]);
                }
            }
            let _ = parse(&buf, next() % 2 == 0);
        }
    }
}

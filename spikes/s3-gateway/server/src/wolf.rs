//! Wolf's HTTP API on its unix socket: just enough to answer our own pair
//! request, so the gateway pairs without anyone typing a PIN.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::{Instant, sleep};
use tracing::info;

const PAIR_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct Pending {
    #[serde(default)]
    requests: Vec<PairRequest>,
}

#[derive(Debug, Deserialize)]
struct PairRequest {
    pair_secret: String,
    #[serde(default)]
    client_ip: Option<String>,
}

/// The pair requests Wolf is already holding. Take this *before* pairing, so
/// [`accept_pairing`] answers only the request our client makes.
pub async fn pending_secrets(socket: &Path) -> Result<Vec<String>> {
    Ok(pending(socket)
        .await?
        .into_iter()
        .map(|r| r.pair_secret)
        .collect())
}

/// Waits for a new pair request (one not in `before`) and answers it with `pin`.
/// In Moonlight pairing the client picks the PIN; Wolf holds the client's
/// request open until someone confirms it.
pub async fn accept_pairing(socket: &Path, before: &[String], pin: &str) -> Result<()> {
    let deadline = Instant::now() + PAIR_WAIT;
    loop {
        let fresh = pending(socket)
            .await?
            .into_iter()
            .find(|r| !before.contains(&r.pair_secret));
        if let Some(req) = fresh {
            info!(client_ip = ?req.client_ip, "answering Wolf pair request");
            let body = json!({ "pair_secret": req.pair_secret, "pin": pin });
            let reply = request(socket, "POST", "/api/v1/pair/client", Some(body)).await?;
            if reply.get("success").and_then(Value::as_bool) == Some(false) {
                bail!("Wolf refused the PIN: {reply}");
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("no new pair request showed up in Wolf within {PAIR_WAIT:?}");
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn pending(socket: &Path) -> Result<Vec<PairRequest>> {
    let reply = request(socket, "GET", "/api/v1/pair/pending", None).await?;
    let pending: Pending = serde_json::from_value(reply).context("parsing /api/v1/pair/pending")?;
    Ok(pending.requests)
}

/// One request over the unix socket, JSON in and out. Wolf answers with
/// HTTP/1.0 and wants `Content-Length` (no chunked bodies).
async fn request(socket: &Path, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connecting to {}", socket.display()))?;
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let head = format!(
        "{method} {path} HTTP/1.1\r\nHost: wolf\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8_lossy(&raw);
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .with_context(|| format!("malformed reply to {method} {path}"))?;
    let status = head.split_whitespace().nth(1).unwrap_or("?");
    if !status.starts_with('2') {
        bail!("{method} {path}: HTTP {status}: {payload}");
    }
    let payload = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(payload)
    } else {
        payload.to_string()
    };
    serde_json::from_str(&payload)
        .with_context(|| format!("{method} {path} returned non-JSON: {payload}"))
}

fn dechunk(mut body: &str) -> String {
    let mut out = String::new();
    while let Some((size, rest)) = body.split_once("\r\n") {
        let Ok(n) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if n == 0 || rest.len() < n {
            break;
        }
        out.push_str(&rest[..n]);
        body = rest[n..].trim_start_matches("\r\n");
    }
    out
}

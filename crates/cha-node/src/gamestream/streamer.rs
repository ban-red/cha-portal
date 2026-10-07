//! A streamer's local API (`/gamestream/*`), over plain HTTP/1.1 on
//! localhost with the environment's secret. The streamer runs the media of
//! a session; the host only starts it, stops it and asks whether it still runs.

use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;

use crate::environments::GameStreamAccess;

const TIMEOUT: Duration = Duration::from_secs(10);

pub struct Reply {
    pub status: u16,
    pub body: String,
}

/// One request to the streamer `access` names.
pub async fn request(
    access: &GameStreamAccess,
    method: Method,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<Reply> {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", access.http_port))
        .await
        .context("reaching the environment's streamer")?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let payload = Bytes::from(body.unwrap_or_default());
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1")
        .header("authorization", format!("Bearer {}", access.secret))
        .header("content-type", "application/json")
        .body(Full::new(payload))?;
    let response = tokio::time::timeout(TIMEOUT, sender.send_request(request))
        .await
        .context("the streamer didn't answer")??;
    let status = response.status().as_u16();
    let bytes = tokio::time::timeout(TIMEOUT, response.into_body().collect())
        .await
        .context("the streamer didn't finish its answer")??
        .to_bytes();
    Ok(Reply {
        status,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

/// The session the streamer's media runs now, if any.
pub async fn running_session(access: &GameStreamAccess) -> Result<Option<u64>> {
    let reply = request(access, Method::GET, "/gamestream/status", None).await?;
    anyhow::ensure!(
        reply.status == 200,
        "the streamer answered {}",
        reply.status
    );
    let status: serde_json::Value = serde_json::from_str(&reply.body)?;
    Ok(status["session_id"].as_u64())
}

//! A GameStream host for trying a real Moonlight client against a streamer
//! before the node's host exists (ADR 0009, G3).
//!
//! It runs the front (nvhttp, pairing, RTSP, mDNS) with an in-memory pairing
//! store, lists one app, "Environment", and hands every launched session to a
//! `cha-streamer` built with `--features gamestream`, through the streamer's
//! local API (`POST /gamestream/session`). The PIN a client shows is read from
//! stdin. See the crate's README for the steps.
//!
//! ```text
//! CHA_GAMESTREAM_SECRET=<the streamer's secret> \
//!   cargo run -p cha-gamestream --example dev_host -- \
//!   --streamer http://127.0.0.1:7660 --ports 7700,7701,7702
//! ```
//!
//! `--ports` are the streamer's `--gamestream-ports`, which this host tells
//! clients. Optional: `--name` (what clients call the host), `--bind` (an
//! address, default every one), `--http`, `--https`, `--rtsp` (the front's
//! ports, default 47989, 47984, 48010).

use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use cha_gamestream::{
    App, Capabilities, ClientId, Directory, DirectoryError, Host, HostConfig, HostHandle,
    LaunchRequest, MediaPorts, MemoryPairingStore, PairingAttempt, PinSender, PinWaiter,
    ResumeRequest, SessionHandoff, SessionTarget, VideoCodec, generate_unique_id, pin_channel,
};
use futures_util::future::BoxFuture;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

const APP_ID: u32 = 1;

struct Args {
    streamer: String,
    ports: MediaPorts,
    name: String,
    bind: IpAddr,
    http: u16,
    https: u16,
    rtsp: u16,
}

fn parse_args() -> Result<Args, String> {
    let mut streamer = None;
    let mut ports = None;
    let mut name = "Cha dev host".to_string();
    let mut bind: IpAddr = [0, 0, 0, 0].into();
    let defaults = cha_gamestream::Ports::default();
    let (mut http, mut https, mut rtsp) = (defaults.http, defaults.https, defaults.rtsp);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || args.next().ok_or_else(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--streamer" => streamer = Some(value()?),
            "--ports" => {
                let text = value()?;
                let p: Vec<u16> = text
                    .split(',')
                    .map(|p| p.trim().parse())
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("--ports: {e}"))?;
                let [video, control, audio] = p[..] else {
                    return Err("--ports takes video,control,audio".into());
                };
                ports = Some(MediaPorts {
                    video,
                    control,
                    audio,
                });
            }
            "--name" => name = value()?,
            "--bind" => bind = value()?.parse().map_err(|e| format!("--bind: {e}"))?,
            "--http" => http = value()?.parse().map_err(|e| format!("--http: {e}"))?,
            "--https" => https = value()?.parse().map_err(|e| format!("--https: {e}"))?,
            "--rtsp" => rtsp = value()?.parse().map_err(|e| format!("--rtsp: {e}"))?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Args {
        streamer: streamer.ok_or("--streamer http://HOST:PORT is required")?,
        ports: ports.ok_or("--ports VIDEO,CONTROL,AUDIO is required")?,
        name,
        bind,
        http,
        https,
        rtsp,
    })
}

/// The streamer's local API, over plain HTTP/1.1 (it listens on localhost, or
/// on the LAN in the dev loop).
struct Streamer {
    host: String,
    secret: String,
}

impl Streamer {
    fn new(url: &str, secret: String) -> Result<Self, String> {
        let host = url
            .strip_prefix("http://")
            .ok_or("--streamer must be an http:// URL")?
            .trim_end_matches('/')
            .to_string();
        Ok(Self { host, secret })
    }

    /// One request; the status and the body.
    async fn request(
        &self,
        method: &str,
        path: &str,
        authorized: bool,
        body: Option<&str>,
    ) -> Result<(u16, String), String> {
        let mut stream = TcpStream::connect(&self.host)
            .await
            .map_err(|e| format!("connecting to {}: {e}", self.host))?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            self.host
        );
        if authorized {
            head += &format!("Authorization: Bearer {}\r\n", self.secret);
        }
        let body = body.unwrap_or("");
        if !body.is_empty() {
            head += &format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            );
        }
        stream
            .write_all(format!("{head}\r\n{body}").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&response);
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or("the streamer's answer isn't HTTP")?;
        // The streamer sends small bodies with a length; `chunked` would put
        // sizes in them, which the few fields read below don't mind.
        Ok((status, body.to_string()))
    }

    /// The codecs the streamer offers, from its unauthenticated `/streams`.
    async fn codecs(&self) -> Vec<VideoCodec> {
        let Ok((200, body)) = self.request("GET", "/streams", false, None).await else {
            return Vec::new();
        };
        let Ok(streams) = serde_json::from_str::<Vec<serde_json::Value>>(&body) else {
            return Vec::new();
        };
        streams
            .iter()
            .filter_map(|s| match s["codec"].as_str()? {
                "h264" => Some(VideoCodec::H264),
                "hevc" => Some(VideoCodec::Hevc),
                "av1" => Some(VideoCodec::Av1),
                _ => None,
            })
            .collect()
    }

    async fn running_session(&self) -> Option<u64> {
        let (200, body) = self
            .request("GET", "/gamestream/status", true, None)
            .await
            .ok()?
        else {
            return None;
        };
        serde_json::from_str::<serde_json::Value>(&body).ok()?["session_id"].as_u64()
    }
}

struct Inner {
    streamer: Streamer,
    ports: MediaPorts,
    /// Where a PIN typed on stdin goes.
    pin: Arc<Mutex<Option<PinSender>>>,
    handle: OnceLock<HostHandle>,
    /// The session whose media runs, for the watcher.
    running: Mutex<Option<u64>>,
}

impl Inner {
    fn target(&self) -> SessionTarget {
        SessionTarget {
            media_ports: self.ports,
        }
    }
}

/// The directory the front is given: a handle to the shared state.
struct DevDirectory(Arc<Inner>);

impl Directory for DevDirectory {
    fn apps(&self, _client: &ClientId) -> BoxFuture<'_, Vec<App>> {
        Box::pin(async {
            vec![App {
                id: APP_ID,
                title: "Environment".into(),
                hdr: false,
            }]
        })
    }

    fn app_image(&self, _client: &ClientId, _app_id: u32) -> BoxFuture<'_, Option<Bytes>> {
        Box::pin(async { None })
    }

    fn launch(
        &self,
        client: &ClientId,
        request: LaunchRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let short = client.fingerprint()[..8].to_string();
        Box::pin(async move {
            if request.app_id != APP_ID {
                return Err(DirectoryError::NoSuchApp);
            }
            println!(
                "launch from {short}: {}x{} at {} fps, audio {:#x}",
                request.width, request.height, request.fps, request.surround_audio_info
            );
            Ok(self.0.target())
        })
    }

    fn resume(
        &self,
        client: &ClientId,
        _request: ResumeRequest,
    ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
        let short = client.fingerprint()[..8].to_string();
        Box::pin(async move {
            println!("resume from {short}");
            Ok(self.0.target())
        })
    }

    fn start_media(&self, handoff: SessionHandoff) -> BoxFuture<'_, Result<(), DirectoryError>> {
        Box::pin(async move {
            let id = handoff.session_id;
            let body = serde_json::to_string(&handoff)
                .map_err(|e| DirectoryError::Failed(e.to_string()))?;
            let (status, answer) = self
                .0
                .streamer
                .request("POST", "/gamestream/session", true, Some(&body))
                .await
                .map_err(DirectoryError::Failed)?;
            match status {
                200 => {}
                401 => {
                    return Err(DirectoryError::Failed(
                        "the streamer refused the secret (CHA_GAMESTREAM_SECRET)".into(),
                    ));
                }
                422 | 409 => return Err(DirectoryError::Refused(answer)),
                other => {
                    return Err(DirectoryError::Failed(format!(
                        "streamer {other}: {answer}"
                    )));
                }
            }
            println!("media started for session {id}");
            *self.0.running.lock().unwrap() = Some(id);
            // The streamer ends a session by itself when the client leaves:
            // tell the front, which then waits for a resume.
            let this = Arc::clone(&self.0);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if *this.running.lock().unwrap() != Some(id) {
                        return;
                    }
                    if this.streamer.running_session().await != Some(id) {
                        println!("media ended for session {id}");
                        this.running.lock().unwrap().take();
                        if let Some(handle) = this.handle.get() {
                            handle.media_ended(id);
                        }
                        return;
                    }
                }
            });
            Ok(())
        })
    }

    fn stop_media(&self, session_id: u64) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            {
                let mut running = self.0.running.lock().unwrap();
                if *running == Some(session_id) {
                    running.take();
                }
            }
            let path = format!("/gamestream/session/{session_id}");
            match self.0.streamer.request("DELETE", &path, true, None).await {
                Ok((200 | 404, _)) => println!("media stopped for session {session_id}"),
                Ok((status, answer)) => eprintln!("stopping media: streamer {status}: {answer}"),
                Err(e) => eprintln!("stopping media: {e}"),
            }
        })
    }

    fn cancel(&self, client: &ClientId) -> BoxFuture<'_, ()> {
        println!("cancel from {}", &client.fingerprint()[..8]);
        Box::pin(async {})
    }

    fn pin_for(&self, attempt: PairingAttempt) -> PinWaiter {
        let (sender, waiter) = pin_channel();
        println!(
            "\n{} ({}) wants to pair. Type the PIN it shows and press Enter:",
            attempt
                .client_name
                .as_deref()
                .unwrap_or(&attempt.client_unique_id),
            attempt.peer
        );
        // A new attempt replaces the old one, which is abandoned.
        *self.0.pin.lock().unwrap() = Some(sender);
        waiter
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let args = match parse_args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("{e}\n\n{}", include_str!("dev_host.usage"));
            std::process::exit(2);
        }
    };
    let secret = std::env::var("CHA_GAMESTREAM_SECRET")
        .map_err(|_| "set CHA_GAMESTREAM_SECRET to the streamer's secret")?;
    let streamer = Streamer::new(&args.streamer, secret)?;
    let mut codecs = streamer.codecs().await;
    if codecs.is_empty() {
        eprintln!(
            "couldn't read the streamer's codecs from /streams; offering H.264, HEVC and AV1"
        );
        codecs = vec![VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1];
    }
    if let Some(running) = streamer.running_session().await {
        eprintln!(
            "the streamer already runs session {running}: a launch from another client will be refused"
        );
    }

    let pin: Arc<Mutex<Option<PinSender>>> = Arc::default();
    let directory = Arc::new(Inner {
        streamer,
        ports: args.ports,
        pin: pin.clone(),
        handle: OnceLock::new(),
        running: Mutex::new(None),
    });

    let mut config = HostConfig::new(&args.name, generate_unique_id());
    config.bind = args.bind;
    config.ports = cha_gamestream::Ports {
        http: args.http,
        https: args.https,
        rtsp: args.rtsp,
    };
    config.capabilities = Capabilities {
        codecs,
        hdr: false,
        yuv444: false,
    };
    let host = Host::builder(config)
        .directory(Arc::new(DevDirectory(directory.clone())))
        .pairing_store(Arc::new(MemoryPairingStore::default()))
        .build()?
        .start()
        .await?;
    let _ = directory.handle.set(host.handle());
    let addrs = host.addrs();
    println!(
        "{} is up: nvhttp on {} and {}, RTSP on {}; media on UDP {}, {}, {} (the streamer's).",
        args.name,
        addrs.http,
        addrs.https,
        addrs.rtsp,
        args.ports.video,
        args.ports.control,
        args.ports.audio
    );
    println!(
        "In Moonlight, add this machine (it should appear by itself), then pair and enter the PIN here."
    );

    // PINs: a line on stdin goes to the pairing waiting for one.
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match pin.lock().unwrap().take() {
                Some(sender) => {
                    if !sender.send(line.to_string()) {
                        println!("(that pairing is over)");
                    }
                }
                None => println!("(no pairing is waiting for a PIN)"),
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    println!("stopping");
    host.handle().cancel_session().await;
    host.shutdown().await;
    Ok(())
}

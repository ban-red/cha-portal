//! The receiver against a tiny in-process `cha-stream/1` streamer on
//! WebTransport (wtransport server, self-signed certificate, `cha_proto`'s
//! fragmenter and FEC), over loopback with induced loss.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use cha_client::{Codec, Ended, Input, PadState, Session};
use cha_client_stream::control::LineBuf;
use cha_client_stream::{Target, connect};
use cha_proto::{DatagramHeader, Flags, Fragmenter, HEADER_LEN, Kind};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use wtransport::tls::self_signed::time::OffsetDateTime;
use wtransport::{Connection, Endpoint, Identity, ServerConfig};

const MAX_DATAGRAM: usize = 1000;
const TOKEN: &str = "tok.sig";

/// What the fake streamer saw.
#[derive(Debug)]
enum Seen {
    /// The CONNECT request's path and query.
    Path(String),
    Line(Value),
    Closed,
}

#[derive(Clone, Copy, PartialEq)]
enum Script {
    /// Hello, floor, the media with induced loss, and an answer to `rfi`.
    Media,
    /// PyroWave: hello says `pyrowave420`, then intra frames with induced loss.
    Pyro,
    /// Hello and floor, nothing else.
    Quiet,
    /// Hello and floor, then the streamer's encoder stops and it hangs up.
    Close,
}

struct Streamer {
    port: u16,
    hash: String,
    seen: mpsc::UnboundedReceiver<Seen>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn identity(expired: bool) -> Identity {
    let sans = ["localhost", "127.0.0.1"];
    if expired {
        let now = OffsetDateTime::now_utc();
        let day = wtransport::tls::self_signed::time::Duration::days(1);
        Identity::self_signed_builder()
            .subject_alt_names(sans)
            .validity_period(now - day * 30, now - day * 20)
            .build()
            .unwrap()
    } else {
        Identity::self_signed(sans).unwrap()
    }
}

fn frame_bytes(id: u32, len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 31 + id as usize * 7) as u8).collect()
}

fn header(kind: Kind, flags: u8, id: u32, ts: u32) -> DatagramHeader {
    DatagramHeader {
        kind,
        flags: Flags(flags),
        stream: 0,
        fec: 0,
        frame_id: id,
        frag_index: 0,
        frag_count: 1,
        send_ts_us: ts,
    }
}

fn start_streamer(script: Script, expired: bool) -> Streamer {
    let identity = identity(expired);
    let hash = hex(&Sha256::digest(
        identity.certificate_chain().as_slice()[0].der(),
    ));
    let config = ServerConfig::builder()
        .with_bind_address(SocketAddr::from(([127, 0, 0, 1], 0)))
        .with_identity(identity)
        .build();
    let endpoint = Endpoint::server(config).unwrap();
    let port = endpoint.local_addr().unwrap().port();
    let (tx, seen) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let request = endpoint.accept().await.await.unwrap();
        let _ = tx.send(Seen::Path(request.path().to_string()));
        let conn = request.accept().await.unwrap();
        serve(conn, script, tx).await;
        // Keep the endpoint until the end.
        drop(endpoint);
    });
    Streamer { port, hash, seen }
}

async fn serve(conn: Connection, script: Script, seen: mpsc::UnboundedSender<Seen>) {
    let epoch = Instant::now();
    let ts = move || epoch.elapsed().as_micros() as u32;
    let (mut send, mut recv) = conn.accept_bi().await.unwrap();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = out_rx.recv().await {
            if send
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let (rfi_tx, mut rfi_rx) = mpsc::unbounded_channel::<u32>();
    // The reader answers pings and resizes, and tells the test what it hears.
    let reader = {
        let (out_tx, seen) = (out_tx.clone(), seen.clone());
        let mut first = true;
        tokio::spawn(async move {
            let mut lines = LineBuf::default();
            let mut buf = vec![0u8; 4096];
            let mut hello_sent = false;
            let codec = if script == Script::Pyro {
                "pyrowave420"
            } else {
                "hevc"
            };
            loop {
                let Ok(Some(n)) = recv.read(&mut buf).await else {
                    break;
                };
                for line in lines.push(&buf[..n]).unwrap() {
                    let v: Value = serde_json::from_str(&line).unwrap();
                    if first {
                        // The first thing on the stream is a ping, at once.
                        assert_eq!(v["t"], "ping", "first line: {v}");
                        first = false;
                    }
                    if !hello_sent {
                        hello_sent = true;
                        out_tx
                            .send(
                                json!({"t":"hello","stream":{"codec":codec,"width":1280,"height":720,
                                    "input":true,"audio":true,"gamepads":true,"fps":60,
                                    "overlay":null,"transport":"webtransport","maxDatagram":MAX_DATAGRAM}})
                                .to_string(),
                            )
                            .unwrap();
                        out_tx
                            .send(json!({"t":"floor","control":true,"viewers":1}).to_string())
                            .unwrap();
                    }
                    match v["t"].as_str() {
                        Some("ping") => {
                            out_tx
                                .send(json!({"t":"pong","c":v["c"],"s_us":ts()}).to_string())
                                .unwrap();
                        }
                        Some("resize") => {
                            out_tx
                                .send(
                                    json!({"t":"resized","w":v["w"],"h":v["h"],"s_us":ts()})
                                        .to_string(),
                                )
                                .unwrap();
                        }
                        Some("rfi") => {
                            let _ = rfi_tx.send(v["id"].as_u64().unwrap() as u32);
                        }
                        _ => {}
                    }
                    let _ = seen.send(Seen::Line(v));
                }
            }
            let _ = seen.send(Seen::Closed);
        })
    };

    if script == Script::Media {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut fragmenter = Fragmenter::new(MAX_DATAGRAM);
        let mut frame = |id: u32, flags: u8, len: usize, fec: u8, drop: &[usize]| {
            let mut datagrams = Vec::new();
            let mut base = header(Kind::Video, flags, id, ts());
            base.fec = fec;
            fragmenter
                .fragment_fec(base, &frame_bytes(id, len), fec, |d| {
                    datagrams.push(d.to_vec())
                })
                .unwrap();
            for (i, d) in datagrams.into_iter().enumerate() {
                if !drop.contains(&i) {
                    conn.send_datagram(d).unwrap();
                }
            }
        };
        let audio = |id: u32| {
            let mut head = [0u8; HEADER_LEN];
            header(Kind::Audio, 0, id, ts()).encode(&mut head);
            let mut d = head.to_vec();
            d.extend_from_slice(&[0xF8, id as u8, 0, 0]);
            conn.send_datagram(d).unwrap();
        };
        let pause = || tokio::time::sleep(Duration::from_millis(30));
        // Audio: out of order and a duplicate on the way.
        for id in [100, 101, 102, 104, 103, 102, 105] {
            audio(id);
        }
        // 0: a keyframe of 7 fragments with parity 2; two data fragments lost.
        frame(0, Flags::KEYFRAME, 6000, 2, &[1, 4]);
        pause().await;
        // 1: plain.
        frame(1, 0, 3000, 0, &[]);
        pause().await;
        // 2: four fragments with one parity; two data fragments lost: beyond FEC.
        frame(2, 0, 3500, 1, &[0, 2]);
        pause().await;
        frame(3, 0, 2000, 0, &[]);
        pause().await;
        frame(4, 0, 2000, 0, &[]);
        // The client asks to refer around frame 2.
        let lost = tokio::time::timeout(Duration::from_secs(3), rfi_rx.recv())
            .await
            .expect("an rfi")
            .unwrap();
        assert_eq!(lost, 2);
        pause().await;
        // 5 refers around it (RECOVERY, not a keyframe); 6 follows.
        frame(5, Flags::RECOVERY, 2500, 1, &[]);
        pause().await;
        frame(6, 0, 1800, 0, &[]);
        audio(106);
    }
    if script == Script::Pyro {
        tokio::time::sleep(Duration::from_millis(300)).await;
        // A datagram of a PyroWave frame: `index` of `total`, a packet or a
        // slice of one.
        let dg = |id: u32, index: u16, total: u16, flags: u8, payload: &[u8]| {
            let mut base = header(
                Kind::Video,
                Flags::KEYFRAME | Flags::INTRA | flags,
                id,
                ts(),
            );
            base.frag_index = index;
            base.frag_count = total;
            let mut head = [0u8; HEADER_LEN];
            base.encode(&mut head);
            let mut d = head.to_vec();
            d.extend_from_slice(payload);
            conn.send_datagram(d).unwrap();
        };
        // 0: whole, out of order.
        dg(0, 2, 3, Flags::CONTINUED, b"b2");
        dg(0, 0, 3, 0, b"aa");
        dg(0, 1, 3, Flags::CONTINUES, b"b1");
        tokio::time::sleep(Duration::from_millis(30)).await;
        // 1: the datagram after a split packet's first part never comes; the
        // frame goes out after the deadline with the other packet only.
        dg(1, 0, 3, 0, b"cc");
        dg(1, 1, 3, Flags::CONTINUES, b"d1");
        tokio::time::sleep(Duration::from_millis(200)).await;
        // 2: whole again, after the loss.
        dg(2, 0, 1, 0, b"ee");
    }
    if script == Script::Close {
        tokio::time::sleep(Duration::from_millis(300)).await;
        conn.close(2u32.into(), b"hevc encoder stopped");
        return;
    }
    let _ = reader.await;
    conn.close(0u32.into(), b"done");
}

fn target(port: u16, hash: &str) -> Target {
    Target {
        urls: vec![
            // Nothing listens here: the race must not wait for it.
            format!("https://127.0.0.1:9/media?codec=hevc&token={TOKEN}"),
            format!("https://127.0.0.1:{port}/media?codec=hevc&token={TOKEN}"),
        ],
        cert_hash: hash.to_string(),
        codec: Codec::Hevc,
    }
}

async fn next_line(streamer: &mut Streamer, t: &str) -> Value {
    loop {
        let seen = tokio::time::timeout(Duration::from_secs(5), streamer.seen.recv())
            .await
            .unwrap_or_else(|_| panic!("the streamer never saw {t}"))
            .unwrap_or_else(|| panic!("the streamer's channel ended before {t}"));
        if let Seen::Line(v) = seen
            && v["t"] == t
        {
            return v;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_over_a_lossy_link() {
    let mut streamer = start_streamer(Script::Media, false);
    let mut session: Session = connect(&target(streamer.port, &streamer.hash), 2560, 1440)
        .await
        .expect("connect");

    // The token reached the streamer in the CONNECT path, the codec with it.
    let Some(Seen::Path(path)) = streamer.seen.recv().await else {
        panic!("no path")
    };
    assert!(
        path.contains(&format!("token={TOKEN}")) && path.contains("codec=hevc"),
        "{path}"
    );

    // The session says what the streamer made of what we asked for.
    assert_eq!(session.codec, Codec::Hevc);
    assert_eq!((session.width, session.height), (2560, 1440));

    // The frames: 0 rebuilt from parity, 1, then 2 is lost beyond the parity
    // (3 and 4 are held), and the recovery frame 5 and 6 resume without a keyframe.
    let mut frames = Vec::new();
    while frames.len() < 4 {
        let frame = tokio::time::timeout(Duration::from_secs(5), session.video.recv())
            .await
            .expect("a frame in time")
            .expect("the video channel is open");
        frames.push(frame);
    }
    let numbers: Vec<u64> = frames.iter().map(|f| f.number).collect();
    assert_eq!(numbers, [0, 1, 5, 6]);
    assert_eq!(
        frames.iter().map(|f| f.key).collect::<Vec<_>>(),
        [true, false, false, false]
    );
    assert_eq!(frames[0].data.as_ref(), frame_bytes(0, 6000));
    assert_eq!(frames[1].data.as_ref(), frame_bytes(1, 3000));
    assert_eq!(frames[2].data.as_ref(), frame_bytes(5, 2500));
    assert_eq!(frames[3].data.as_ref(), frame_bytes(6, 1800));
    assert!(frames.iter().all(|f| f.codec == Codec::Hevc));

    // Audio: in order, without the duplicate and the late one.
    let mut ids = Vec::new();
    while ids.len() < 5 {
        let a = tokio::time::timeout(Duration::from_secs(2), session.audio.recv())
            .await
            .expect("audio")
            .unwrap();
        assert_eq!((a.channels, a.sample_rate, a.samples), (2, 48_000, 480));
        ids.push(a.data[1]);
    }
    assert_eq!(ids, [100, 101, 102, 104, 105]);

    // What the streamer heard: the cursor mode, the resize, reports with a
    // rate, and the request to refer around the lost frame.
    assert_eq!(next_line(&mut streamer, "cursor").await["client"], false);
    let resize = next_line(&mut streamer, "resize").await;
    assert_eq!(
        (resize["w"].as_u64(), resize["h"].as_u64()),
        (Some(2560), Some(1440))
    );
    let rfi = next_line(&mut streamer, "rfi").await;
    assert_eq!(rfi["id"], 2);
    let mut reports = 0;
    let mut with_rate = 0;
    let mut with_delay = 0;
    let mut loss = 0.0_f64;
    // The test has consumed up to the rfi; reports keep coming every 100 ms.
    while reports < 12 {
        let r = next_line(&mut streamer, "report").await;
        reports += 1;
        if r["r"].as_f64().is_some_and(|r| r > 0.0) {
            with_rate += 1;
        }
        if r["d"].as_f64().is_some() {
            with_delay += 1;
        }
        loss = loss.max(r["l"].as_f64().unwrap_or(0.0));
    }
    assert!(with_rate >= 1, "no report carried a rate");
    // Pings gave the clock an offset, so reports carry the delay once frames came.
    assert!(with_delay <= reports);
    assert!(loss > 0.0, "the induced loss never showed in `l`");

    // Input goes out as the browser's lines, with the floor held.
    session.control.input(Input::Key {
        code: "KeyA".into(),
        down: true,
    });
    session.control.input(Input::Pad {
        index: 0,
        pad: PadState {
            buttons: vec![1.0; 4],
            axes: vec![0.5, -0.5, 0.0, 0.0],
        },
    });
    let key = next_line(&mut streamer, "input").await;
    assert_eq!(
        key,
        json!({"t":"input","k":"key","code":"KeyA","down":true})
    );
    let pad = next_line(&mut streamer, "input").await;
    assert_eq!(pad["k"], "pad");
    assert_eq!(pad["b"][3], 1.0);
    assert_eq!(pad["a"][0], 0.5);

    // Stopping lets go of what is held, then ends the session.
    session.control.stop(false);
    let up = next_line(&mut streamer, "input").await;
    assert_eq!(up["code"], "KeyA");
    assert_eq!(up["down"], false);
    let ended = tokio::time::timeout(Duration::from_secs(3), session.ended)
        .await
        .expect("ended in time")
        .unwrap();
    assert_eq!(ended, Ended::Stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn pings_are_answered_and_the_clock_syncs() {
    let mut streamer = start_streamer(Script::Quiet, false);
    let session = connect(&target(streamer.port, &streamer.hash), 1280, 720)
        .await
        .expect("connect");
    // No resize was needed: the size is the streamer's.
    assert_eq!((session.width, session.height), (1280, 720));
    let first = next_line(&mut streamer, "ping").await;
    assert!(first["c"].as_f64().is_some());
    // The session pings again every second.
    let second = next_line(&mut streamer, "ping").await;
    assert!(second["c"].as_f64().unwrap() > first["c"].as_f64().unwrap());
    // And reports meanwhile, with no frames: a rate, no delay.
    let report = next_line(&mut streamer, "report").await;
    assert!(report["r"].is_number());
    assert!(report.get("d").is_none());
    session.control.stop(false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streamer_that_hangs_up_ends_the_session_with_its_reason() {
    let streamer = start_streamer(Script::Close, false);
    let session = connect(&target(streamer.port, &streamer.hash), 1280, 720)
        .await
        .expect("connect");
    let ended = tokio::time::timeout(Duration::from_secs(5), session.ended)
        .await
        .expect("ended")
        .unwrap();
    assert!(
        matches!(&ended, Ended::ByHost(m) if m.contains("hevc encoder stopped")),
        "{ended:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_pinned_certificate_connects() {
    let streamer = start_streamer(Script::Quiet, false);
    let wrong = hex(&Sha256::digest(b"another certificate"));
    let err = connect(&target(streamer.port, &wrong), 1280, 720)
        .await
        .err()
        .expect("a certificate that isn't the pinned one is refused");
    let text = format!("{err:#}");
    assert!(text.contains("none of the streamer's addresses"), "{text}");
    assert!(
        !text.contains(TOKEN),
        "the token must not leak into errors: {text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pin_does_not_check_dates() {
    // The streamer's certificate is long past its validity: the hash is
    // what the portal vouches for.
    let streamer = start_streamer(Script::Quiet, true);
    let session = connect(&target(streamer.port, &streamer.hash), 1280, 720)
        .await
        .expect("connect with an expired certificate");
    session.control.stop(false);
}

#[tokio::test(flavor = "multi_thread")]
async fn pyrowave_frames_arrive_whole_or_partial_and_nothing_is_asked_for() {
    let mut streamer = start_streamer(Script::Pyro, false);
    let mut t = target(streamer.port, &streamer.hash);
    t.codec = Codec::PyroWave420;
    let mut session: Session = connect(&t, 1280, 720).await.expect("connect");
    assert_eq!(session.codec, Codec::PyroWave420);

    let mut frames = Vec::new();
    while frames.len() < 3 {
        let frame = tokio::time::timeout(Duration::from_secs(5), session.video.recv())
            .await
            .expect("a frame in time")
            .expect("the video channel is open");
        frames.push(frame);
    }
    assert!(
        frames
            .iter()
            .all(|f| f.codec == Codec::PyroWave420 && f.key)
    );
    assert_eq!(
        frames.iter().map(|f| f.number).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(frames[0].data.as_ref(), b"aab1b2");
    assert!(!frames[0].partial);
    assert_eq!(frames[1].data.as_ref(), b"cc");
    assert!(frames[1].partial);
    assert_eq!(frames[2].data.as_ref(), b"ee");
    assert!(!frames[2].partial);

    // Reports carry the loss; no keyframe or rfi is ever asked for.
    let mut loss = 0.0_f64;
    for _ in 0..8 {
        let r = next_line(&mut streamer, "report").await;
        loss = loss.max(r["l"].as_f64().unwrap_or(0.0));
    }
    assert!(loss > 0.0, "the induced loss never showed in `l`");
    session.control.stop(false);
    let mut asked = false;
    while let Ok(Some(seen)) =
        tokio::time::timeout(Duration::from_millis(300), streamer.seen.recv()).await
    {
        if let Seen::Line(v) = seen
            && (v["t"] == "keyframe" || v["t"] == "rfi")
        {
            asked = true;
        }
    }
    assert!(!asked, "PyroWave never asks for a keyframe or an rfi");
}

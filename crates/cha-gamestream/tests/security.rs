//! The security fixes, one by one, against the assembled host.

mod common;

use std::time::Duration;

use cha_gamestream::PairingStore;
use cha_gamestream::VideoCodec;
use common::{
    Rig, TestClient, connect_stream, header, launch_only, raw_http, raw_tcp, rtsp, settings,
};

/// Fix 3: a session belongs to the certificate that launched it.
#[tokio::test(flavor = "multi_thread")]
async fn another_paired_client_cannot_resume_or_cancel_a_running_session() {
    let rig = Rig::start().await;
    let a = TestClient::new("client-a");
    let b = TestClient::new("client-b");
    let host_a = a.paired_host(&rig).await;
    let host_b = b.paired_host(&rig).await;

    let _stream = connect_stream(&host_a, 1, settings(VideoCodec::H264, 1280, 720, 60)).await;
    let backend = rig.backend.next_stream().await;

    // B sees that an app runs, but cannot take over its session or end it.
    host_b.update().await.unwrap();
    assert_eq!(host_b.current_game().await.unwrap(), 1);
    let cancel = host_b.cancel().await;
    assert!(
        matches!(cancel, Err(_) | Ok(false)),
        "B cancelled A's session: {cancel:?}"
    );
    let crypto = moonlight_common::crypto::rustcrypto::RustCryptoBackend;
    let version = host_b.version().await.unwrap();
    let gfe = host_b.gfe_version().await.unwrap();
    let modes = host_b.server_codec_mode_support().await.unwrap();
    let mut s = settings(VideoCodec::H264, 1280, 720, 60);
    s.adjust_for_server(version, &gfe, modes).unwrap();
    // The session runs, so B's client asks to resume it.
    let resume = host_b
        .start_stream(
            moonlight_common::AppId(1),
            &s,
            moonlight_common::stream::AesKey::new_random(&crypto).unwrap(),
            moonlight_common::stream::AesIv::new_random(&crypto).unwrap(),
            moonlight_common::stream::proto::MoonlightStreamSetup::launch_query_parameters(),
        )
        .await;
    assert!(resume.is_err(), "B resumed A's session");

    assert!(rig.directory.cancels().is_empty(), "the app was quit for B");
    assert!(rig.directory.resumes().is_empty());
    assert!(rig.directory.stopped_media().is_empty());
    {
        let log = backend.log.lock().unwrap();
        assert!(!log.stopped, "A's media was stopped");
    }
    host_a.update().await.unwrap();
    assert_eq!(host_a.current_game().await.unwrap(), 1);

    // A, the owner, can.
    host_a.cancel().await.unwrap();
    assert_eq!(rig.directory.cancels(), vec![a.id()]);
    backend.wait_for("A's media to stop", |l| l.stopped).await;
    rig.host.shutdown().await;
}

/// Fix 4, and a session goes with its client.
#[tokio::test(flavor = "multi_thread")]
async fn unpairing_a_client_ends_its_session() {
    let rig = Rig::start().await;
    let a = TestClient::new("client-a");
    let host_a = a.paired_host(&rig).await;
    let _ = launch_only(&host_a, 1).await;
    host_a.unpair().await.unwrap();
    assert!(rig.store.list().await.unwrap().is_empty());
    assert_eq!(
        rig.directory.cancels(),
        vec![a.id()],
        "the app is quit when its owner is unpaired"
    );
    rig.host.shutdown().await;
}

/// Fix 7: RTSP, bounded and gated.
#[tokio::test(flavor = "multi_thread")]
async fn rtsp_answers_only_with_a_session_and_refuses_what_it_should() {
    let rig = Rig::with(|c| c.request_timeout = Duration::from_millis(400)).await;
    let rtsp_port = rig.host.addrs().rtsp.port();

    // No session, no answer.
    let (out, closed) = raw_tcp(
        rtsp_port,
        b"OPTIONS rtsp://x RTSP/1.0\r\nCSeq: 1\r\n\r\n",
        Duration::from_secs(3),
    )
    .await;
    assert!(
        out.is_empty() && closed,
        "answered with no session: {:?}",
        String::from_utf8_lossy(&out)
    );

    let client = TestClient::new("rtsp-client");
    let host = client.paired_host(&rig).await;
    let url = launch_only(&host, 1).await;
    assert!(url.starts_with("rtsp://127.0.0.1:"), "{url}");

    let options = rtsp(rtsp_port, "OPTIONS rtsp://x RTSP/1.0\r\nCSeq: 7\r\n\r\n").await;
    assert!(
        options.starts_with("RTSP/1.0 200 OK\r\nCSeq: 7\r\n"),
        "{options}"
    );
    assert!(header(&options, "Public").unwrap().contains("ANNOUNCE"));

    let describe = rtsp(
        rtsp_port,
        "DESCRIBE rtsp://x RTSP/1.0\r\nCSeq: 8\r\nAccept: application/sdp\r\n\r\n",
    )
    .await;
    assert!(describe.contains("surround-params=21101"), "{describe}");
    assert!(describe.contains("x-ss-general.encryptionRequested:1"));

    // SETUP: the ports the directory reserved, a session token, the ping payload and connect data.
    let reserved = rig.directory.launches().len();
    assert_eq!(reserved, 1);
    let video = rtsp(rtsp_port, "SETUP streamid=video/0/0 RTSP/1.0\r\nCSeq: 9\r\nTransport: unicast;X-GS-ClientPort=50000-50001\r\n\r\n").await;
    assert!(
        header(&video, "Transport")
            .unwrap()
            .starts_with("server_port="),
        "{video}"
    );
    let token = header(&video, "Session").unwrap();
    let token = token.split(';').next().unwrap().to_owned();
    assert_eq!(header(&video, "X-SS-Ping-Payload").unwrap().len(), 16);
    let control = rtsp(
        rtsp_port,
        "SETUP streamid=control/13/0 RTSP/1.0\r\nCSeq: 10\r\nTransport: unicast\r\n\r\n",
    )
    .await;
    assert!(
        header(&control, "X-SS-Connect-Data")
            .unwrap()
            .parse::<u32>()
            .is_ok()
    );
    let bad_stream = rtsp(
        rtsp_port,
        "SETUP streamid=nothing/0/0 RTSP/1.0\r\nCSeq: 11\r\n\r\n",
    )
    .await;
    assert!(bad_stream.starts_with("RTSP/1.0 400"), "{bad_stream}");

    // ANNOUNCE and PLAY need the session token.
    let announce = |token: &str, body: &str| {
        format!(
            "ANNOUNCE streamid=control/13/0 RTSP/1.0\r\nCSeq: 12\r\nSession: {token}\r\nContent-length: {}\r\n\r\n{body}",
            body.len()
        )
    };
    let r = rtsp(rtsp_port, &announce("WRONG", "a=x:1\r\n")).await;
    assert!(r.starts_with("RTSP/1.0 454"), "{r}");
    let r = rtsp(
        rtsp_port,
        &format!("PLAY / RTSP/1.0\r\nCSeq: 13\r\nSession: {token}\r\n\r\n"),
    )
    .await;
    assert!(r.starts_with("RTSP/1.0 455"), "PLAY before ANNOUNCE: {r}");
    // Garbage SDP, and an SDP that leaves control unencrypted.
    let r = rtsp(rtsp_port, &announce(&token, "not sdp at all")).await;
    assert!(r.starts_with("RTSP/1.0 400"), "{r}");
    let plain = "a=x-nv-video[0].clientViewportWd:1280\r\na=x-nv-video[0].clientViewportHt:720\r\na=x-nv-video[0].maxFPS:60\r\na=x-nv-video[0].packetSize:1392\r\na=x-ml-video.configuredBitrateKbps:10000\r\na=x-nv-vqos[0].bitStreamFormat:0\r\na=x-nv-aqos.packetDuration:5\r\na=x-ss-general.encryptionEnabled:0\r\n";
    let r = rtsp(rtsp_port, &announce(&token, plain)).await;
    assert!(
        r.starts_with("RTSP/1.0 400"),
        "plaintext control must be refused: {r}"
    );
    let r = rtsp(rtsp_port, "TEARDOWN / RTSP/1.0\r\nCSeq: 14\r\n\r\n").await;
    assert!(r.starts_with("RTSP/1.0 405"), "{r}");
    let r = rtsp(rtsp_port, "OPTIONS rtsp://x RTSP/1.0\r\n\r\n").await;
    assert!(r.starts_with("RTSP/1.0 400"), "a request with no CSeq: {r}");

    // Bounded: a huge head, a huge body, garbage and a stall all end in a closed connection.
    let huge = format!(
        "OPTIONS rtsp://x RTSP/1.0\r\nCSeq: 1\r\nX-Pad: {}\r\n\r\n",
        "a".repeat(20_000)
    );
    let (out, closed) = raw_tcp(rtsp_port, huge.as_bytes(), Duration::from_secs(3)).await;
    assert!(closed && out.is_empty(), "a 20 kB head was answered");
    let (out, closed) = raw_tcp(
        rtsp_port,
        b"ANNOUNCE x RTSP/1.0\r\nCSeq: 1\r\nContent-Length: 99999999\r\n\r\n",
        Duration::from_secs(3),
    )
    .await;
    assert!(closed && out.is_empty());
    let (out, closed) = raw_tcp(rtsp_port, &[0xFF; 3000], Duration::from_secs(3)).await;
    assert!(closed && out.is_empty());
    let started = std::time::Instant::now();
    let (out, closed) = raw_tcp(
        rtsp_port,
        b"OPTIONS rtsp://x RTSP/1.0\r\nCSeq: 1\r\n",
        Duration::from_secs(5),
    )
    .await;
    assert!(closed && out.is_empty(), "a stalled request was answered");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the stall wasn't cut off at the timeout"
    );
    rig.host.shutdown().await;
}

/// Fix 7: nvhttp's bounds.
#[tokio::test(flavor = "multi_thread")]
async fn nvhttp_bounds_request_size_time_and_connections() {
    let rig = Rig::with(|c| {
        c.request_timeout = Duration::from_millis(400);
        c.max_connections = 3;
    })
    .await;
    let port = rig.http_port();

    // A 64 kB header is refused, not buffered.
    let huge = format!(
        "GET /serverinfo HTTP/1.1\r\nHost: x\r\nX-Pad: {}\r\n\r\n",
        "a".repeat(64 * 1024)
    );
    let (out, closed) = raw_tcp(port, huge.as_bytes(), Duration::from_secs(3)).await;
    assert!(closed, "the connection stays open after a huge head");
    assert!(
        !String::from_utf8_lossy(&out).contains("200 OK"),
        "served a 64 kB head"
    );

    // A body is not welcome on a request that takes none.
    let body = format!(
        "GET /serverinfo HTTP/1.1\r\nHost: x\r\nContent-Length: 1000000\r\nConnection: close\r\n\r\n{}",
        "b".repeat(2000)
    );
    let r = raw_http(port, &body).await;
    assert!(r.starts_with("HTTP/1.1 413"), "{r}");

    // A client that sends half a request is cut off at the timeout.
    let started = std::time::Instant::now();
    let (_, closed) = raw_tcp(
        port,
        b"GET /serverinfo HTTP/1.1\r\nHost: x\r\n",
        Duration::from_secs(5),
    )
    .await;
    assert!(
        closed && started.elapsed() < Duration::from_secs(3),
        "a stalled request holds a connection"
    );

    // At the connection cap, a new connection is refused at once.
    use tokio::io::AsyncReadExt;
    let mut held = Vec::new();
    for _ in 0..3 {
        held.push(
            tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap(),
        );
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut extra = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let mut buf = [0u8; 8];
    let n = tokio::time::timeout(Duration::from_millis(300), extra.read(&mut buf)).await;
    assert!(
        matches!(n, Ok(Ok(0)) | Ok(Err(_))),
        "the fourth connection was held: {n:?}"
    );
    drop(held);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // And the server is fine afterwards.
    let r = raw_http(
        port,
        "GET /serverinfo HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    rig.host.shutdown().await;
}

/// Fix 7: a TLS port that is sent rubbish.
#[tokio::test(flavor = "multi_thread")]
async fn the_https_port_survives_rubbish() {
    let rig = Rig::with(|c| c.request_timeout = Duration::from_millis(400)).await;
    let port = rig.host.addrs().https.port();
    for junk in [
        &b"GET / HTTP/1.1\r\n\r\n"[..],
        &[0x16, 0x03, 0x01, 0xFF, 0xFF][..],
        &[0u8; 5000][..],
    ] {
        let (_, closed) = raw_tcp(port, junk, Duration::from_secs(3)).await;
        assert!(closed);
    }
    // A paired client still works.
    let client = TestClient::new("after-junk");
    client.paired_host(&rig).await;
    rig.host.shutdown().await;
}

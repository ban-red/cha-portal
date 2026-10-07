//! The media half without the front: a handoff built by hand and a control
//! client written independently of the crate's.

mod common;

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use cha_gamestream::{EncodedVideo, EndReason, Feedback, InputEvent, MediaConfig, OpusPacket};
use common::{
    CONNECT_DATA, ControlClient, KEY, MediaRig, PING_PAYLOAD, frame, handoff, key_packet, seal_with,
};
use tokio::net::UdpSocket;

const V4: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

fn v4(port: u16) -> SocketAddr {
    SocketAddr::new(V4, port)
}

async fn connected(rig: &MediaRig) -> ControlClient {
    let mut c = ControlClient::connect(
        "127.0.0.1:0".parse().unwrap(),
        v4(rig.ports.control),
        CONNECT_DATA,
        KEY,
    )
    .await;
    assert!(
        c.wait_connected(Duration::from_secs(5)).await,
        "the control connection is accepted"
    );
    c
}

async fn rig() -> MediaRig {
    MediaRig::start(V4, handoff(V4), MediaConfig::default()).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_control_packet_is_dropped_and_counted_never_fatal() {
    let rig = rig().await;
    let mut c = connected(&rig).await;

    let garbage: Vec<Vec<u8>> = vec![
        vec![],
        vec![1],
        vec![1, 0, 0],              // no length
        vec![1, 0, 40, 0, 1, 2, 3], // length lies
        frame(1, &[0; 10]),         // encrypted wrapper too short
        frame(1, &[0; 40]),         // authenticates as nothing
        vec![0xFF; 100],
        c.seal(&frame(0x0206, &[0, 0, 0, 99, 1, 2])), // input length lies
        c.seal(&frame(0x0206, &[0, 0, 0, 4, 0xEF, 0xBE, 0xAD, 0xDE])), // unknown input type
        c.seal(&[1, 2]),                              // decrypts to less than a header
    ];
    let n = garbage.len() as u64;
    for g in &garbage {
        c.send_raw(g).await;
    }
    // A packet sealed under another key is refused, not malformed.
    c.send_raw(&seal_with(&[9; 16], 1, &frame(0x0200, &[]), b'C'))
        .await;
    // So is a plain message: it is what anyone who can reach the port could send.
    c.send_raw(&frame(0x0206, &[0, 0, 0, 8, 3, 0, 0, 0, 0, 0x41, 0x80, 0]))
        .await;

    // The session lives on: a valid message after all that is acted on.
    c.send_input(&key_packet(0x42, true)).await;
    rig.stream
        .wait_for("the valid key press", |l| {
            l.inputs.contains(&InputEvent::Key {
                down: true,
                vk: 0x42,
                modifiers: 0,
            })
        })
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), rig.session.closed())
            .await
            .is_err(),
        "the session ended"
    );

    let stats = rig.session.stats();
    assert!(
        stats.malformed_control_packets >= 6,
        "malformed packets counted: {stats:?} (sent {n})"
    );
    assert!(
        stats.rejected_control_packets >= 3,
        "refused packets counted: {stats:?}"
    );
    // Nothing the garbage said got through.
    assert_eq!(rig.stream.log.lock().unwrap().inputs.len(), 1);
    rig.session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_session_client_with_the_connect_data_may_connect() {
    let rig = rig().await;
    // Wrong connect data: let in by ENet, then dropped by the host.
    let mut wrong = ControlClient::connect(
        "127.0.0.1:0".parse().unwrap(),
        v4(rig.ports.control),
        CONNECT_DATA + 1,
        KEY,
    )
    .await;
    wrong.wait_connected(Duration::from_secs(5)).await;
    assert!(
        wrong.wait_disconnected(Duration::from_secs(5)).await,
        "the host drops it"
    );
    assert!(rig.session.stats().rejected_control_peers >= 1);

    // The right one still gets in, and a second connection while it is up is refused.
    let mut right = connected(&rig).await;
    let mut second = ControlClient::connect(
        "127.0.0.1:0".parse().unwrap(),
        v4(rig.ports.control),
        CONNECT_DATA,
        KEY,
    )
    .await;
    second.wait_connected(Duration::from_secs(5)).await;
    assert!(
        second.wait_disconnected(Duration::from_secs(5)).await,
        "a second peer is dropped"
    );
    right.send_input(&key_packet(0x43, true)).await;
    rig.stream
        .wait_for("input from the first client", |l| !l.inputs.is_empty())
        .await;
    rig.session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_whose_client_is_elsewhere_refuses_a_control_peer_from_this_address() {
    // The session is for 10.1.2.3; the peer is on loopback.
    let rig = MediaRig::start(
        V4,
        handoff("10.1.2.3".parse().unwrap()),
        MediaConfig::default(),
    )
    .await;
    let mut c = ControlClient::connect(
        "127.0.0.1:0".parse().unwrap(),
        v4(rig.ports.control),
        CONNECT_DATA,
        KEY,
    )
    .await;
    c.wait_connected(Duration::from_secs(5)).await;
    assert!(c.wait_disconnected(Duration::from_secs(5)).await);
    assert!(rig.session.stats().rejected_control_peers >= 1);
    rig.session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn video_and_audio_follow_only_pings_from_the_sessions_client() {
    // The media sockets listen on both families. The client is [::1]; a datagram from
    // 127.0.0.1 is another address, and so is anything that isn't a PING.
    let ip: IpAddr = "::1".parse().unwrap();
    let rig = MediaRig::start("::".parse().unwrap(), handoff(ip), MediaConfig::default()).await;
    let server = |port| SocketAddr::new(ip, port);
    let mut control = ControlClient::connect(
        "[::1]:0".parse().unwrap(),
        server(rig.ports.control),
        CONNECT_DATA,
        KEY,
    )
    .await;
    assert!(control.wait_connected(Duration::from_secs(5)).await);
    control.send(0x0307, &[0]).await; // StartB
    rig.stream
        .wait_for("a keyframe request at StartB", |l| l.keyframes >= 1)
        .await;

    let client_video = UdpSocket::bind("[::1]:0").await.unwrap();
    let attacker_video = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let client_audio = UdpSocket::bind("[::1]:0").await.unwrap();
    let attacker_audio = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    let before = rig.stream.log.lock().unwrap().keyframes;
    // The attacker pings first, with a legacy PING and with the right payload: both refused.
    attacker_video
        .send_to(b"PING", v4(rig.ports.video))
        .await
        .unwrap();
    let mut sunshine = PING_PAYLOAD.to_vec();
    sunshine.extend(1u32.to_be_bytes());
    attacker_video
        .send_to(&sunshine, v4(rig.ports.video))
        .await
        .unwrap();
    attacker_audio
        .send_to(b"PING", v4(rig.ports.audio))
        .await
        .unwrap();
    // A datagram from the client that isn't a PING is refused as well.
    client_video
        .send_to(b"hello", server(rig.ports.video))
        .await
        .unwrap();
    // The client's own pings are accepted: legacy, and Sunshine-style with the payload.
    client_video
        .send_to(b"PING", server(rig.ports.video))
        .await
        .unwrap();
    client_audio
        .send_to(&sunshine, server(rig.ports.audio))
        .await
        .unwrap();
    rig.stream
        .wait_for(
            "a keyframe request when the client's address is learned",
            |l| l.keyframes > before,
        )
        .await;

    // Media goes to the client, and nowhere else.
    let au = common::access_unit(cha_gamestream::VideoCodec::H264, true, 3000, 1);
    for i in 0..50 {
        rig.stream
            .video
            .send(EncodedVideo {
                data: au.clone().into(),
                key: true,
                index: i,
                captured: std::time::Instant::now(),
            })
            .await
            .unwrap();
        rig.stream
            .audio
            .send(OpusPacket {
                data: vec![1; 40].into(),
                samples: 240,
            })
            .await
            .unwrap();
    }
    let mut buf = [0u8; 2048];
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), client_video.recv_from(&mut buf))
        .await
        .expect("video reaches the client")
        .unwrap();
    assert_eq!(buf[0], 0x90, "an RTP video packet ({n} bytes)");
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), client_audio.recv_from(&mut buf))
        .await
        .expect("audio reaches the client")
        .unwrap();
    assert_eq!((buf[0], buf[1], n), (0x80, 97, 12 + 40 + 8 - 8));
    for attacker in [&attacker_video, &attacker_audio] {
        assert!(
            tokio::time::timeout(Duration::from_millis(300), attacker.recv_from(&mut buf))
                .await
                .is_err(),
            "media leaked to the attacker"
        );
    }
    assert!(
        rig.session.stats().rejected_pings >= 4,
        "{:?}",
        rig.session.stats()
    );
    rig.session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn every_feedback_message_of_a_burst_reaches_the_client() {
    let rig = rig().await;
    let mut c = connected(&rig).await;
    // Feedback is for a client that is there: wait until the host has acted on one message.
    c.send_input(&key_packet(0x41, true)).await;
    rig.stream
        .wait_for("the host to have the peer", |l| !l.inputs.is_empty())
        .await;
    for n in 0..60u16 {
        rig.stream
            .feedback
            .send(Feedback::Rumble {
                pad: 1,
                low: n,
                high: n * 2,
            })
            .await
            .unwrap();
    }
    rig.stream
        .feedback
        .send(Feedback::Led {
            pad: 1,
            rgb: (1, 2, 3),
        })
        .await
        .unwrap();
    rig.stream
        .feedback
        .send(Feedback::RumbleTriggers {
            pad: 1,
            left: 7,
            right: 8,
        })
        .await
        .unwrap();
    let got = c.receive_for(Duration::from_millis(1500)).await;
    let rumbles: Vec<u16> = got
        .iter()
        .filter(|(t, _)| *t == 0x010b)
        .map(|(_, b)| u16::from_le_bytes([b[6], b[7]]))
        .collect();
    assert_eq!(
        rumbles,
        (0..60).collect::<Vec<_>>(),
        "all rumble messages, in order"
    );
    assert!(
        got.iter()
            .any(|(t, b)| *t == 0x5502 && b[..] == [1, 0, 1, 2, 3])
    );
    assert!(
        got.iter()
            .any(|(t, b)| *t == 0x5500 && b[..] == [1, 0, 7, 0, 8, 0])
    );
    rig.session.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_says_goodbye_and_stops_the_backend() {
    let rig = rig().await;
    let mut c = connected(&rig).await;
    // Once the host has acted on a message, it has the peer.
    c.send_input(&key_packet(0x41, true)).await;
    rig.stream
        .wait_for("the host to have the peer", |l| !l.inputs.is_empty())
        .await;
    let stop = tokio::spawn({
        let session = rig.session;
        async move { session.stop().await }
    });
    let got = c.receive_for(Duration::from_secs(2)).await;
    stop.await.unwrap();
    let termination = got
        .iter()
        .find(|(t, _)| *t == 0x0109)
        .expect("a termination message");
    assert_eq!(termination.1, 0x8003_0023u32.to_be_bytes());
    let log = rig.stream.log.lock().unwrap();
    assert!(log.stopped && log.released);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_client_times_the_session_out() {
    let rig = MediaRig::start(
        V4,
        handoff(V4),
        MediaConfig {
            stream_timeout: Duration::from_millis(400),
        },
    )
    .await;
    let mut c = connected(&rig).await;
    // One ping keeps it alive past the first deadline.
    tokio::time::sleep(Duration::from_millis(250)).await;
    c.send(0x0200, &[]).await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), rig.session.closed())
            .await
            .unwrap(),
        EndReason::TimedOut
    );
    rig.session.join().await;
    assert!(rig.stream.log.lock().unwrap().stopped);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_leaves_ends_the_session_and_releases_its_input() {
    let rig = rig().await;
    let mut c = connected(&rig).await;
    c.send_input(&key_packet(0x41, true)).await;
    rig.stream
        .wait_for("the key", |l| !l.inputs.is_empty())
        .await;
    c.host.peer_mut(c.peer).unwrap().disconnect(0);
    let _ = c.host.flush().await;
    for _ in 0..20 {
        let _ = c.host.service(Duration::from_millis(10)).await;
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), rig.session.closed())
            .await
            .unwrap(),
        EndReason::ClientLeft
    );
    rig.session.join().await;
    let log = rig.stream.log.lock().unwrap();
    assert!(
        log.released && log.stopped,
        "held input is let go and the backend stopped"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backend_that_stops_producing_video_ends_the_session() {
    let rig = rig().await;
    let _c = connected(&rig).await;
    rig.stream.end_video();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), rig.session.closed())
            .await
            .unwrap(),
        EndReason::BackendEnded
    );
    rig.session.join().await;
    assert!(rig.stream.log.lock().unwrap().stopped);
}

/// HDR: the client is told the mode (with the fallback metadata until the backend knows better),
/// and keyframes carry the backend's metadata as SEI.
#[tokio::test(flavor = "multi_thread")]
async fn hdr_mode_is_announced_and_keyframes_carry_the_metadata() {
    let backend = common::FakeBackend::with_caps(cha_gamestream::Capabilities {
        codecs: vec![cha_gamestream::VideoCodec::H264],
        hdr: true,
        yuv444: false,
    });
    let mut h = handoff(V4);
    h.params.hdr = true;
    let rig = MediaRig::start_with(backend, V4, h, MediaConfig::default()).await;
    let mut control = connected(&rig).await;
    control.send(0x0307, &[0]).await; // StartB
    let got = control.receive_for(Duration::from_millis(500)).await;
    let mode = got
        .iter()
        .find(|(t, _)| *t == 0x010e)
        .expect("HDR mode at StartB");
    assert_eq!(mode.1.len(), 31);
    assert_eq!(mode.1[0], 1, "HDR on");
    assert_eq!(
        &mode.1[1..5],
        &[0xD0, 0x84, 0x80, 0x3E],
        "BT.2020 red from the fallback"
    );

    // The backend knows its metadata now.
    let meta = cha_gamestream::HdrMetadata {
        display_primaries: [(1, 2), (3, 4), (5, 6)],
        white_point: (7, 8),
        max_luminance: 40_000_000,
        min_luminance: 50,
        max_cll: 900,
        max_fall: 300,
    };
    rig.stream
        .feedback
        .send(Feedback::Hdr {
            enabled: true,
            metadata: Some(meta),
        })
        .await
        .unwrap();
    let got = control.receive_for(Duration::from_millis(500)).await;
    let mode = got
        .iter()
        .find(|(t, _)| *t == 0x010e)
        .expect("HDR mode after the update");
    assert_eq!(&mode.1[1..5], &[1, 0, 2, 0]);
    assert_eq!(
        &mode.1[17..19],
        &4000u16.to_le_bytes(),
        "max luminance in nits"
    );

    // A keyframe now carries an MDCV SEI NAL before its slice.
    let client_video = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client_video
        .send_to(b"PING", v4(rig.ports.video))
        .await
        .unwrap();
    let au = common::access_unit(cha_gamestream::VideoCodec::H264, true, 600, 1);
    let mut buf = [0u8; 2048];
    let mut payload = Vec::new();
    'wait: for i in 0..100 {
        rig.stream
            .video
            .send(EncodedVideo {
                data: au.clone().into(),
                key: true,
                index: i,
                captured: std::time::Instant::now(),
            })
            .await
            .unwrap();
        if let Ok(Ok((n, _))) =
            tokio::time::timeout(Duration::from_millis(100), client_video.recv_from(&mut buf)).await
        {
            // The first shard is the start of the frame.
            payload = buf[32..n].to_vec();
            break 'wait;
        }
    }
    assert!(!payload.is_empty(), "no video arrived");
    // Frame header (8), then the access unit: SPS and PPS NALs, then the SEI, then the IDR slice.
    let sei = payload
        .windows(5)
        .position(|w| w == [0, 0, 0, 1, 0x06])
        .expect("an SEI NAL (type 6)");
    let idr = payload
        .windows(5)
        .position(|w| w == [0, 0, 0, 1, 0x65])
        .expect("the IDR slice");
    assert!(sei < idr, "the SEI comes before the slice");
    rig.session.stop().await;
}

//! The client's media against the host's, on loopback: our `client::media`
//! on one side, `media::MediaSession` with a fake backend on the other, and
//! between them a lossy UDP proxy (written here) on the video and audio
//! ports that drops, holds back and repeats chosen packets.

mod common;

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Key, Nonce, Tag};
use bytes::Bytes;
use cha_gamestream::client::media::{Ended, Media, MediaClient, MediaOptions, VideoTiming};
use cha_gamestream::client::{StreamSetup, VideoFrame};
use cha_gamestream::handoff::Chroma;
use cha_gamestream::input::{
    BatteryState, GamepadKind, MotionKind, MouseButton, Pen, PenTool, PointerKind, Touch,
    capabilities, modifiers,
};
use cha_gamestream::{
    EncodedVideo, EndReason, Feedback, HdrMetadata, InputEvent, MediaConfig, OpusPacket, VideoCodec,
};
use common::{KEY, MediaRig, PING_PAYLOAD, handoff};
use tokio::net::UdpSocket;

const PACKET_SIZE: usize = 1392;
const LOOPBACK: std::net::IpAddr = std::net::IpAddr::V4(Ipv4Addr::LOCALHOST);

// ---------------------------------------------------------------------------
// The lossy proxy

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Forward,
    Drop,
    /// Send after the next packet instead of before it.
    Hold,
    /// Send twice.
    Repeat,
}

type Policy = Box<dyn FnMut(&[u8]) -> Action + Send>;

/// Sits between the client and one host port. Datagrams from the client go
/// to the host; the host's go back through the policy.
struct Proxy {
    port: u16,
    policy: Arc<Mutex<Policy>>,
    seen: Arc<Mutex<usize>>,
}

fn big_socket(addr: SocketAddr) -> UdpSocket {
    let s = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    let _ = s.set_recv_buffer_size(8 << 20);
    s.bind(&addr.into()).unwrap();
    s.set_nonblocking(true).unwrap();
    UdpSocket::from_std(s.into()).unwrap()
}

impl Proxy {
    async fn start(target: u16) -> Self {
        let front = Arc::new(big_socket("127.0.0.1:0".parse().unwrap()));
        let back = Arc::new(big_socket("127.0.0.1:0".parse().unwrap()));
        let port = front.local_addr().unwrap().port();
        let policy: Arc<Mutex<Policy>> = Arc::new(Mutex::new(Box::new(|_| Action::Forward)));
        let seen = Arc::new(Mutex::new(0usize));
        let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
        let target = SocketAddr::new(LOOPBACK, target);

        // Client to host: verbatim.
        tokio::spawn({
            let (front, back, client) = (front.clone(), back.clone(), client.clone());
            async move {
                let mut buf = vec![0u8; 65_536];
                while let Ok((n, from)) = front.recv_from(&mut buf).await {
                    *client.lock().unwrap() = Some(from);
                    let _ = back.send_to(&buf[..n], target).await;
                }
            }
        });
        // Host to client: through the policy.
        tokio::spawn({
            let (policy, seen) = (policy.clone(), seen.clone());
            async move {
                let mut buf = vec![0u8; 65_536];
                let mut held: Option<Vec<u8>> = None;
                while let Ok((n, _)) = back.recv_from(&mut buf).await {
                    let Some(to) = *client.lock().unwrap() else {
                        continue;
                    };
                    *seen.lock().unwrap() += 1;
                    let action = (policy.lock().unwrap())(&buf[..n]);
                    match action {
                        Action::Drop => continue,
                        Action::Hold => {
                            if let Some(old) = held.replace(buf[..n].to_vec()) {
                                let _ = front.send_to(&old, to).await;
                            }
                            continue;
                        }
                        Action::Repeat => {
                            let _ = front.send_to(&buf[..n], to).await;
                            let _ = front.send_to(&buf[..n], to).await;
                        }
                        Action::Forward => {
                            let _ = front.send_to(&buf[..n], to).await;
                        }
                    }
                    if let Some(old) = held.take() {
                        let _ = front.send_to(&old, to).await;
                    }
                }
            }
        });
        Self { port, policy, seen }
    }

    fn set(&self, policy: impl FnMut(&[u8]) -> Action + Send + 'static) {
        *self.policy.lock().unwrap() = Box::new(policy);
    }

    fn forward_all(&self) {
        self.set(|_| Action::Forward);
    }

    fn seen(&self) -> usize {
        *self.seen.lock().unwrap()
    }
}

/// Where a video datagram sits in its frame.
#[derive(Clone, Copy, Debug)]
struct Shard {
    frame: u32,
    block: u8,
    index: usize,
    data: usize,
    parity: usize,
}

/// Reads a video datagram's place from its headers, decrypting it first when
/// video is encrypted: the test knows the key.
fn shard(datagram: &[u8], key: Option<[u8; 16]>) -> Option<Shard> {
    let plain = match key {
        None => datagram.to_vec(),
        Some(key) => {
            let (prefix, body) = datagram.split_at(32);
            let mut body = body.to_vec();
            Aes128Gcm::new(Key::<Aes128Gcm>::from_slice(&key))
                .decrypt_in_place_detached(
                    Nonce::from_slice(&prefix[..12]),
                    b"",
                    &mut body,
                    Tag::from_slice(&prefix[16..32]),
                )
                .ok()?;
            body
        }
    };
    let nv = plain.get(16..32)?;
    let info = u32::from_le_bytes(nv[12..16].try_into().unwrap());
    let data = (info >> 22) as usize & 0x3FF;
    let percent = (info >> 4) as usize & 0xFF;
    Some(Shard {
        frame: u32::from_le_bytes(nv[4..8].try_into().unwrap()),
        block: (nv[11] >> 4) & 3,
        index: (info >> 12) as usize & 0x3FF,
        data,
        parity: (data * percent).div_ceil(100),
    })
}

// ---------------------------------------------------------------------------
// The rig

struct Harness {
    rig: MediaRig,
    media: Media,
    video: Proxy,
    audio: Proxy,
    video_key: Option<[u8; 16]>,
    next_index: u64,
    /// Frames the test has pushed, so each gets a fresh seed.
    pushed: u8,
}

async fn harness(video_encrypted: bool, audio_encrypted: bool) -> Harness {
    // Generous clocks: a machine under load must not look like a lossy network.
    harness_with(video_encrypted, audio_encrypted, patient()).await
}

/// Options whose give-up clocks are seconds, so scheduling stalls can't trigger them.
fn patient() -> MediaOptions {
    MediaOptions {
        video_timing: VideoTiming {
            reorder_window: Duration::from_secs(5),
            stall: Duration::from_secs(20),
            request_gap: Duration::from_secs(5),
            request_retry: Duration::from_secs(20),
        },
        audio_reorder_window: Duration::from_secs(5),
        ..Default::default()
    }
}

async fn harness_with(
    video_encrypted: bool,
    audio_encrypted: bool,
    options: MediaOptions,
) -> Harness {
    common::init();
    let mut h = handoff(LOOPBACK);
    h.encryption.video = video_encrypted;
    h.encryption.audio = audio_encrypted;
    h.params.packet_size = PACKET_SIZE;
    h.params.min_fec_packets = 2;
    let rig = MediaRig::start(LOOPBACK, h.clone(), MediaConfig::default()).await;
    let video = Proxy::start(rig.ports.video).await;
    let audio = Proxy::start(rig.ports.audio).await;
    let setup = StreamSetup {
        host: LOOPBACK,
        ports: cha_gamestream::MediaPorts {
            video: video.port,
            control: rig.ports.control,
            audio: audio.port,
        },
        keys: h.keys.clone(),
        encryption: h.encryption,
        control_connect_data: h.control_connect_data,
        ping_payload: PING_PAYLOAD,
        codec: VideoCodec::H264,
        width: 1280,
        height: 720,
        fps: 60,
        packet_size: PACKET_SIZE,
        hdr: false,
        chroma: Chroma::Yuv420,
        audio: h.params.audio.clone(),
    };
    let media = MediaClient::start_with(&setup, options).await.unwrap();
    let mut h = Harness {
        rig,
        media,
        video,
        audio,
        video_key: video_encrypted.then_some(KEY),
        next_index: 1,
        pushed: 0,
    };
    h.warm_up().await;
    h
}

/// An access unit with no run of zeros (nothing in it looks like padding).
fn access_unit(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(13).wrapping_add(seed) | 1)
        .collect()
}

impl Harness {
    async fn push(&mut self, key: bool, len: usize) -> Vec<u8> {
        self.pushed = self.pushed.wrapping_add(1);
        let au = access_unit(len, self.pushed);
        self.next_index += 1;
        self.rig
            .stream
            .video
            .send(EncodedVideo {
                data: Bytes::from(au.clone()),
                key,
                index: self.next_index,
                captured: Instant::now(),
            })
            .await
            .unwrap();
        au
    }

    /// Keyframes until the first one gets through (the host waits for the
    /// client's start and its pings), then quiet.
    async fn warm_up(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.push(true, 500).await;
            if let Ok(Some(_)) =
                tokio::time::timeout(Duration::from_millis(100), self.media.video.recv()).await
            {
                break;
            }
            assert!(Instant::now() < deadline, "no video ever arrived");
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        while self.media.video.try_recv().is_ok() {}
    }

    /// The next frame. A machine under load is slow, not broken: this gives up only
    /// when the host's packets stop coming for 20 s.
    async fn frame(&mut self) -> VideoFrame {
        let mut seen = self.video.seen();
        let mut quiet = 0;
        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.media.video.recv()).await {
                Ok(frame) => return frame.expect("the stream is open"),
                Err(_) => {
                    let now = self.video.seen();
                    quiet = if now == seen { quiet + 1 } else { 0 };
                    seen = now;
                    assert!(
                        quiet < 4,
                        "no frame, and no packets for 20 s: {:?}",
                        self.media.control.stats().video
                    );
                }
            }
        }
    }

    async fn no_frame(&mut self, within: Duration) {
        if let Ok(f) = tokio::time::timeout(within, self.media.video.recv()).await {
            panic!("unexpected frame {:?}", f.map(|f| (f.number, f.data.len())));
        }
    }

    fn keyframe_requests(&self) -> usize {
        self.rig.stream.log.lock().unwrap().keyframes
    }
}

// ---------------------------------------------------------------------------
// Video

type Drop_ = fn(&Shard) -> bool;

/// Loses shards in every block, by position in the block.
const LOSS_PATTERNS: &[(&str, Drop_)] = &[
    ("the first data shard", |s| s.index == 0),
    ("the last data shard", |s| s.index + 1 == s.data),
    ("the first parity shard", |s| s.index == s.data),
    ("all the parity", |s| s.index >= s.data),
    (
        "as many data shards as there is parity, from the front",
        |s| s.index < s.parity.min(s.data),
    ),
    (
        "as many data shards as there is parity, ending at the last",
        |s| s.index < s.data && s.index >= s.data.saturating_sub(s.parity),
    ),
];

#[tokio::test(flavor = "multi_thread")]
async fn frames_arrive_byte_exact_with_losses_up_to_the_fec() {
    for encrypted in [false, true] {
        let mut h = harness(encrypted, false).await;
        let key = h.video_key;
        let baseline = h.keyframe_requests();
        for len in [200usize, 100_000, 400_000, 1_000_000] {
            for (name, drop) in LOSS_PATTERNS {
                h.video.set(move |d| match shard(d, key) {
                    Some(s) if drop(&s) => Action::Drop,
                    _ => Action::Forward,
                });
                println!("{len} B, encrypted {encrypted}, losing {name}");
                let sent = h.push(false, len).await;
                let got = h.frame().await;
                assert_eq!(
                    got.data.len(),
                    sent.len(),
                    "{len} B, encrypted {encrypted}, losing {name}"
                );
                assert_eq!(
                    got.data.as_ref(),
                    &sent[..],
                    "{len} B, encrypted {encrypted}, losing {name}"
                );
            }
        }
        h.video.forward_all();
        assert_eq!(
            h.keyframe_requests(),
            baseline,
            "no keyframe was needed: every loss was within the FEC"
        );
        assert_eq!(h.media.control.stats().video.frames_lost, 0);
        h.media.control.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_frame_beyond_the_fec_is_dropped_a_keyframe_is_asked_for_and_the_stream_resumes() {
    for encrypted in [false, true] {
        // The give-up path on purpose: the production clocks.
        let mut h = harness_with(encrypted, false, MediaOptions::default()).await;
        let key = h.video_key;
        let baseline = h.keyframe_requests();

        // One shard more than the first block's parity.
        // The first frame seen from now on is the victim; later ones pass untouched.
        let mut victim = None;
        h.video.set(move |d| match shard(d, key) {
            Some(s) if *victim.get_or_insert(s.frame) == s.frame => {
                if s.block == 0 && s.index <= s.parity {
                    Action::Drop
                } else {
                    Action::Forward
                }
            }
            _ => Action::Forward,
        });
        h.push(false, 100_000).await;
        h.no_frame(Duration::from_millis(400)).await;
        h.rig
            .stream
            .wait_for("the client's keyframe request", |l| l.keyframes > baseline)
            .await;

        // A P frame can't start a picture; it is dropped, quietly.
        h.push(false, 20_000).await;
        h.no_frame(Duration::from_millis(300)).await;

        // The keyframe the host was asked for resumes it, and what follows flows.
        let key_au = h.push(true, 60_000).await;
        let f = h.frame().await;
        assert!(f.key);
        assert_eq!(f.data.as_ref(), &key_au[..]);
        let p_au = h.push(false, 30_000).await;
        let f = h.frame().await;
        assert!(!f.key);
        assert_eq!(f.data.as_ref(), &p_au[..]);

        let stats = h.media.control.stats().video;
        assert!(stats.frames_lost >= 1, "{stats:?}");
        assert!(stats.frames_discarded >= 1, "{stats:?}");
        // The session on the host never noticed anything wrong.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), h.rig.session.closed())
                .await
                .is_err()
        );
        h.media.control.stop();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_frame_with_invalidation_on_sends_the_invalidation() {
    let mut h = harness_with(
        false,
        false,
        MediaOptions {
            invalidate_refs: true,
            ..Default::default()
        },
    )
    .await;
    let baseline = h.keyframe_requests();
    // A frame of one packet and two parity shards: all three lost.
    let mut victim = None;
    h.video.set(move |d| match shard(d, None) {
        Some(s) if *victim.get_or_insert(s.frame) == s.frame && s.index < 3 => Action::Drop,
        _ => Action::Forward,
    });
    h.push(false, 100).await;
    h.push(false, 100).await;
    // The fake backend sees the invalidation for the wire frame, and the
    // client asked for no keyframe of its own.
    h.rig
        .stream
        .wait_for("the invalidation", |l| !l.invalidations.is_empty())
        .await;
    assert_eq!(h.keyframe_requests(), baseline);
    h.media.control.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn reordered_and_repeated_packets_change_nothing() {
    for encrypted in [false, true] {
        let mut h = harness(encrypted, false).await;
        let mut n = 0usize;
        h.video.set(move |_| {
            n += 1;
            match n % 7 {
                0 | 3 => Action::Hold,
                5 => Action::Repeat,
                _ => Action::Forward,
            }
        });
        for len in [3_000usize, 100_000, 400_000] {
            let sent = h.push(false, len).await;
            let got = h.frame().await;
            assert_eq!(
                got.data.as_ref(),
                &sent[..],
                "{len} B, encrypted {encrypted}"
            );
        }
        assert!(h.media.control.stats().video.duplicates > 0);
        h.media.control.stop();
    }
}

// ---------------------------------------------------------------------------
// Audio

fn opus(id: u16) -> Vec<u8> {
    let mut v = id.to_le_bytes().to_vec();
    v.extend((2..40).map(|i| (id as u8).wrapping_add(i)));
    v
}

async fn audio_round(encrypted: bool, lossy: bool) {
    let mut h = harness(encrypted, encrypted).await;
    if lossy {
        // Two of every block's four data packets, which RS(4,2) can rebuild.
        h.audio.set(|d| {
            let seq = u16::from_be_bytes([d[2], d[3]]);
            if d[1] & 0x7F == 97 && matches!(seq % 4, 1 | 2) {
                Action::Drop
            } else {
                Action::Forward
            }
        });
    }
    // Warm up until audio flows, then the numbered packets.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut warm = 0u16;
    loop {
        h.rig
            .stream
            .audio
            .send(OpusPacket {
                data: Bytes::from(opus(warm)),
                samples: 240,
            })
            .await
            .unwrap();
        warm += 1;
        if tokio::time::timeout(Duration::from_millis(30), h.media.audio.recv())
            .await
            .is_ok()
        {
            break;
        }
        assert!(Instant::now() < deadline, "no audio ever arrived");
    }
    // Fill out the block the warm-up ended in and let the receiver settle (the gaps
    // it left are given up on), so the numbered packets meet a synced receiver.
    while !warm.is_multiple_of(4) {
        h.rig
            .stream
            .audio
            .send(OpusPacket {
                data: Bytes::from(opus(warm)),
                samples: 240,
            })
            .await
            .unwrap();
        warm += 1;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    while h.media.audio.try_recv().is_ok() {}
    for id in 1000..1040u16 {
        h.rig
            .stream
            .audio
            .send(OpusPacket {
                data: Bytes::from(opus(id)),
                samples: 240,
            })
            .await
            .unwrap();
    }
    let mut got = Vec::new();
    while let Ok(Some(p)) = tokio::time::timeout(Duration::from_secs(3), h.media.audio.recv()).await
    {
        let id = u16::from_le_bytes([p.data[0], p.data[1]]);
        assert_eq!(p.data.as_ref(), &opus(id)[..]);
        if id >= 1000 {
            got.push(id);
            if id == 1039 {
                break;
            }
        }
    }
    assert_eq!(
        got,
        (1000..1040).collect::<Vec<u16>>(),
        "encrypted {encrypted}, lossy {lossy}: {:?}",
        h.media.control.stats().audio
    );
    h.media.control.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_arrives_in_order_unencrypted_and_encrypted() {
    audio_round(false, false).await;
    audio_round(true, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_losses_within_the_fec_are_rebuilt() {
    audio_round(false, true).await;
    audio_round(true, true).await;
}

// ---------------------------------------------------------------------------
// Control

#[tokio::test(flavor = "multi_thread")]
async fn every_kind_of_input_reaches_the_backend() {
    let h = harness(false, false).await;
    let touch = Touch {
        kind: PointerKind::Down,
        pointer_id: 42,
        x: 0.25,
        y: 0.75,
        pressure: 0.5,
        rotation: 270,
        contact_minor: 0.0625,
        contact_major: 0.125,
    };
    let pen = Pen {
        kind: PointerKind::Move,
        tool: PenTool::Eraser,
        buttons: 3,
        x: 0.25,
        y: 0.75,
        pressure_or_distance: 0.6,
        rotation: 270,
        tilt: 35,
        contact_minor: 0.0625,
        contact_major: 0.125,
    };
    let sent_and_expected = vec![
        (
            InputEvent::Key {
                down: true,
                vk: 0x41,
                modifiers: modifiers::SHIFT,
            },
            None,
        ),
        (
            InputEvent::Key {
                down: false,
                vk: 0x41,
                modifiers: 0,
            },
            None,
        ),
        (
            InputEvent::MouseMoveAbsolute {
                x: 100,
                y: 200,
                width: 1920,
                height: 1080,
            },
            None,
        ),
        (InputEvent::MouseMoveRelative { dx: -7, dy: 9 }, None),
        (
            InputEvent::MouseButton {
                button: MouseButton::Right,
                down: true,
            },
            None,
        ),
        (InputEvent::ScrollVertical { amount: 120 }, None),
        (InputEvent::ScrollHorizontal { amount: -120 }, None),
        (InputEvent::Touch(touch), None),
        (InputEvent::Pen(pen), None),
        (
            InputEvent::GamepadArrival {
                pad: 1,
                kind: GamepadKind::PlayStation,
                capabilities: capabilities::RUMBLE | capabilities::GYRO,
                supported_buttons: 0x3FFFF,
            },
            None,
        ),
        (
            InputEvent::GamepadState {
                pad: 1,
                active_mask: 0b10,
                buttons: 0x0002_1001,
                left_trigger: 200,
                right_trigger: 10,
                left_stick: (-100, 200),
                right_stick: (300, -400),
            },
            None,
        ),
        (
            InputEvent::GamepadTouch {
                pad: 1,
                kind: PointerKind::Down,
                touchpad: 0,
                pointer_id: 3,
                x: 0.5,
                y: 0.25,
                pressure: 1.0,
            },
            None,
        ),
        (
            InputEvent::GamepadMotion {
                pad: 1,
                kind: MotionKind::Gyroscope,
                x: 1.0,
                y: 2.0,
                z: 3.0,
            },
            None,
        ),
        (
            InputEvent::GamepadBattery {
                pad: 1,
                state: BatteryState::Charging,
                percent: 80,
            },
            None,
        ),
        (InputEvent::Text("héllo ✓".into()), None),
    ];
    for (event, _) in &sent_and_expected {
        h.media.control.input(event.clone());
    }
    let expected: Vec<InputEvent> = sent_and_expected
        .into_iter()
        .map(|(sent, expected)| expected.unwrap_or(sent))
        .collect();
    h.rig
        .stream
        .wait_for("every input event", |l| {
            expected.iter().all(|e| l.inputs.contains(e))
        })
        .await;
    h.media.control.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn mouse_motion_is_batched_and_none_of_it_is_lost() {
    let h = harness(false, false).await;
    for _ in 0..500 {
        h.media
            .control
            .input(InputEvent::MouseMoveRelative { dx: 2, dy: -1 });
    }
    h.rig
        .stream
        .wait_for("all the motion", |l| {
            let (mut x, mut y) = (0i32, 0i32);
            for e in &l.inputs {
                if let InputEvent::MouseMoveRelative { dx, dy } = e {
                    x += i32::from(*dx);
                    y += i32::from(*dy);
                }
            }
            (x, y) == (1000, -500)
        })
        .await;
    let moves = h
        .rig
        .stream
        .log
        .lock()
        .unwrap()
        .inputs
        .iter()
        .filter(|e| matches!(e, InputEvent::MouseMoveRelative { .. }))
        .count();
    assert!(moves < 500, "batched into {moves} packets");
    h.media.control.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn feedback_from_the_backend_reaches_the_client() {
    let mut h = harness(false, false).await;
    let all = vec![
        Feedback::Rumble {
            pad: 1,
            low: 0x1234,
            high: 0xABCD,
        },
        Feedback::RumbleTriggers {
            pad: 1,
            left: 10,
            right: 20,
        },
        Feedback::Led {
            pad: 2,
            rgb: (9, 8, 7),
        },
        Feedback::MotionEnable {
            pad: 1,
            rate_hz: 100,
            kind: 2,
        },
        Feedback::TriggerEffect {
            pad: 1,
            event_flags: 3,
            type_left: 4,
            type_right: 5,
            left: [6; 10],
            right: [7; 10],
        },
        Feedback::Hdr {
            enabled: true,
            metadata: Some(HdrMetadata::fallback()),
        },
    ];
    for fb in &all {
        h.rig.stream.feedback.send(fb.clone()).await.unwrap();
    }
    for expected in &all {
        let got = tokio::time::timeout(Duration::from_secs(5), h.media.feedback.recv())
            .await
            .expect("feedback in time")
            .expect("open");
        assert_eq!(&got, expected);
    }
    h.media.control.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_host_stopping_ends_the_stream_with_its_reason() {
    let mut h = harness(false, false).await;
    h.rig.session.stop().await;
    let ended = tokio::time::timeout(Duration::from_secs(5), &mut h.media.ended)
        .await
        .expect("ended in time")
        .unwrap();
    assert_eq!(
        ended,
        Ended::Terminated {
            code: 0x8003_0023,
            graceful: true
        }
    );
    // The channels close with it.
    assert!(
        tokio::time::timeout(Duration::from_secs(2), async {
            while h.media.video.recv().await.is_some() {}
        })
        .await
        .is_ok()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_client_says_goodbye_to_the_host() {
    let mut h = harness(false, false).await;
    h.media.control.input(InputEvent::Key {
        down: true,
        vk: 0x41,
        modifiers: 0,
    });
    h.rig
        .stream
        .wait_for("the key", |l| !l.inputs.is_empty())
        .await;
    h.media.control.stop();
    let ended = tokio::time::timeout(Duration::from_secs(5), &mut h.media.ended)
        .await
        .expect("ended in time")
        .unwrap();
    assert_eq!(ended, Ended::Stopped);
    let reason = tokio::time::timeout(Duration::from_secs(5), h.rig.session.closed())
        .await
        .expect("the host noticed");
    assert_eq!(reason, EndReason::ClientLeft);
    h.rig.session.join().await;
    assert!(
        h.rig.stream.log.lock().unwrap().released,
        "held input let go"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_the_last_handle_stops_the_stream() {
    let h = harness(false, false).await;
    let Harness { rig, media, .. } = h;
    let Media { control, .. } = media;
    drop(control);
    let reason = tokio::time::timeout(Duration::from_secs(5), rig.session.closed())
        .await
        .expect("the host noticed");
    assert_eq!(reason, EndReason::ClientLeft);
}

#[tokio::test(flavor = "multi_thread")]
async fn no_answer_from_the_host_is_an_error_not_a_hang() {
    common::init();
    let h = handoff(LOOPBACK);
    // Ports nobody listens on.
    let dead = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = dead.local_addr().unwrap().port();
    let setup = StreamSetup {
        host: LOOPBACK,
        ports: cha_gamestream::MediaPorts {
            video: port,
            control: port,
            audio: port,
        },
        keys: h.keys.clone(),
        encryption: h.encryption,
        control_connect_data: 1,
        ping_payload: PING_PAYLOAD,
        codec: VideoCodec::H264,
        width: 1280,
        height: 720,
        fps: 60,
        packet_size: PACKET_SIZE,
        hdr: false,
        chroma: Chroma::Yuv420,
        audio: h.params.audio.clone(),
    };
    let started = Instant::now();
    let err = MediaClient::start_with(
        &setup,
        MediaOptions {
            connect_timeout: Duration::from_millis(500),
            ..Default::default()
        },
    )
    .await
    .err()
    .expect("no connection");
    assert!(started.elapsed() < Duration::from_secs(5), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_proxy_really_carries_the_media() {
    // Guard for the rig itself: video and audio went through the proxies.
    let h = harness(false, false).await;
    assert!(h.video.seen() > 0);
    h.media.control.stop();
}

/// A client that stalls (everything shares one thread, which is blocked for a
/// while) finds a backlog of whole frames in its socket when it wakes. The
/// time it spent away is not the network going quiet: none of them is dropped.
#[tokio::test(flavor = "current_thread")]
async fn a_busy_client_does_not_drop_frames_waiting_in_its_socket() {
    let mut h = harness(false, false).await;
    let baseline = h.keyframe_requests();
    let mut sent = Vec::new();
    for len in [60_000usize, 80_000, 40_000] {
        sent.push(h.push(false, len).await);
    }
    // Let the host send and the proxy forward everything, then stall the thread.
    // (Quiet for half a second: all of it has gone through the proxy.)
    let (mut seen, mut quiet) = (0, 0);
    while quiet < 5 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let now = h.video.seen();
        quiet = if now == seen { quiet + 1 } else { 0 };
        seen = now;
    }
    std::thread::sleep(Duration::from_millis(300));
    for want in &sent {
        let got = h.frame().await;
        assert_eq!(got.data.as_ref(), &want[..]);
    }
    assert_eq!(h.keyframe_requests(), baseline);
    assert_eq!(h.media.control.stats().video.frames_lost, 0);
    h.media.control.stop();
}

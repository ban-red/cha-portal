//! A whole session on loopback: launch, RTSP, control, then media and input,
//! with moonlight-common-rust as the client and fake traits behind the host.

mod common;

use std::time::Duration;

use cha_gamestream::{
    EncodedVideo, Feedback, InputEvent, MediaStatsSnapshot, OpusPacket, VideoCodec,
};
use common::{
    ClientCommand, ClientEvent, Rig, TestClient, access_unit, connect_stream, settings,
    settings_encrypting,
};
use moonlight_common::stream::EncryptionFlags;
use moonlight_common::stream::control::{
    ControllerButtons, ControllerCapabilities, ControllerType, KeyAction, KeyCode, KeyFlags,
    KeyModifiers, MouseButton, MouseButtonAction,
};
use moonlight_common::stream::proto::control::input_batcher::ClientInputEvent;
use moonlight_common::stream::proto::control::packet::ControlPacket;

/// A client hands H.264 and HEVC frames on as it got them: the access unit
/// then the zero padding of the last packet (decoders skip trailing zeros).
fn assert_access_unit(got: &[u8], sent: &[u8], what: &str) {
    assert!(
        got.len() >= sent.len(),
        "{what}: {} bytes for {}",
        got.len(),
        sent.len()
    );
    assert!(got[..sent.len()] == *sent, "{what}: the bytes differ");
    assert!(
        got[sent.len()..].iter().all(|&b| b == 0),
        "{what}: the padding is not zeros"
    );
}

async fn push_video(backend: &common::BackendStream, key: bool, au: &[u8], index: u64) {
    backend
        .video
        .send(EncodedVideo {
            data: au.to_vec().into(),
            key,
            index,
            captured: std::time::Instant::now(),
        })
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_launches_and_receives_video_and_audio_and_its_input_arrives() {
    let rig = Rig::start().await;
    let client = TestClient::new("e2e-client");
    let host = client.paired_host(&rig).await;

    let mut stream = connect_stream(&host, 1, settings(VideoCodec::H264, 1280, 720, 60)).await;
    let backend = rig.backend.next_stream().await;

    // The backend was told what the client chose.
    let p = &backend.params;
    assert_eq!(
        (p.width, p.height, p.fps, p.codec, p.app_id),
        (1280, 720, 60, VideoCodec::H264, 1)
    );
    assert_eq!(p.client, client.id());
    assert_eq!(p.client_ip, common::LOOPBACK);
    assert_eq!(
        (p.audio.channels, p.audio.streams, p.audio.coupled_streams),
        (2, 1, 1)
    );
    // The directory saw the launch with the client's request.
    let launches = rig.directory.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!(
        (
            launches[0].0.clone(),
            launches[0].1.app_id,
            launches[0].1.width
        ),
        (client.id(), 1, 1280)
    );
    // serverinfo says the app runs.
    host.update().await.unwrap();
    assert_eq!(host.current_game().await.unwrap(), 1);

    // StartB and the first PINGs make the host ask the backend for a keyframe.
    backend
        .wait_for("a keyframe request", |l| l.keyframes >= 1)
        .await;

    // Video: a keyframe, then predicted frames of several packet counts, keep pushing
    // until the client has the keyframe (frames before its PING are dropped).
    let key_au = access_unit(VideoCodec::H264, true, 9000, 1);
    let mut sent_key = false;
    let mut got_key = None;
    for attempt in 0..100u64 {
        push_video(&backend, true, &key_au, attempt).await;
        sent_key = true;
        if let Ok(Some(ClientEvent::Video { data, key, index })) =
            tokio::time::timeout(Duration::from_millis(100), stream.events.recv()).await
        {
            got_key = Some((data, key, index));
            break;
        }
    }
    assert!(sent_key);
    let (data, key, index) = got_key.expect("the client received a keyframe");
    assert!(key, "flagged as an IDR");
    assert_access_unit(&data, &key_au, "the keyframe");
    assert_eq!(index, 1, "frame numbers start at 1");

    for (i, len) in [200usize, 1400, 30_000, 100_000].into_iter().enumerate() {
        let au = access_unit(VideoCodec::H264, false, len, 10 + i as u8);
        push_video(&backend, false, &au, 100 + i as u64).await;
        let (data, key, index) = stream
            .next(10, |e| match e {
                ClientEvent::Video { data, key, index } => Ok((data, key, index)),
                other => Err(other),
            })
            .await;
        assert!(!key);
        assert_access_unit(&data, &au, &format!("a {len}-byte frame"));
        assert_eq!(index, 2 + i as u32);
    }

    // Audio: Opus packets in, packets out in order (FEC packets are the client's business).
    for n in 0..12u8 {
        backend
            .audio
            .send(OpusPacket {
                data: vec![n; 60].into(),
                samples: 240,
            })
            .await
            .unwrap();
    }
    for n in 0..12u8 {
        let data = stream
            .next(10, |e| match e {
                ClientEvent::Audio { data } => Ok(data),
                other => Err(other),
            })
            .await;
        assert_eq!(&data[..], &vec![n; 60][..], "audio packet {n}");
    }

    // Input: keyboard, mouse, buttons, scroll, a pad; each reaches the backend neutral.
    stream.send(ClientCommand::Input(ClientInputEvent::Keyboard {
        action: KeyAction::Down,
        flags: KeyFlags::empty(),
        key_code: KeyCode(0x41),
        modifiers: KeyModifiers::SHIFT,
    }));
    stream.send(ClientCommand::Input(ClientInputEvent::MouseMoveRelative {
        delta_x: -4,
        delta_y: 9,
    }));
    stream.send(ClientCommand::Input(ClientInputEvent::MouseMoveAbsolute {
        x: 100,
        y: 200,
        reference_width: 1280,
        reference_height: 720,
    }));
    stream.send(ClientCommand::Input(ClientInputEvent::MouseButton {
        action: MouseButtonAction::Press,
        button: MouseButton::Right,
    }));
    stream.send(ClientCommand::Input(
        ClientInputEvent::MouseScrollVertical { scroll_y: 120 },
    ));
    stream.send(ClientCommand::Input(
        ClientInputEvent::MouseScrollHorizontal { scroll_x: -120 },
    ));
    stream.send(ClientCommand::Input(ClientInputEvent::ControllerConnect {
        controller_number: 0,
        ty: ControllerType::Xbox,
        capabilities: ControllerCapabilities::empty(),
        supported_buttons: ControllerButtons::all(),
    }));
    backend
        .wait_for("input", |l| {
            let has = |f: &dyn Fn(&InputEvent) -> bool| l.inputs.iter().any(f);
            has(&|e| {
                matches!(
                    e,
                    InputEvent::Key {
                        down: true,
                        vk: 0x41,
                        modifiers: 1
                    }
                )
            }) && has(&|e| matches!(e, InputEvent::MouseMoveRelative { dx: -4, dy: 9 }))
                && has(&|e| {
                    matches!(
                        e,
                        InputEvent::MouseMoveAbsolute {
                            x: 100,
                            y: 200,
                            width: 1280,
                            height: 720
                        }
                    )
                })
                && has(&|e| matches!(e, InputEvent::MouseButton { down: true, .. }))
                && has(&|e| matches!(e, InputEvent::ScrollVertical { amount: 120 }))
                && has(&|e| matches!(e, InputEvent::ScrollHorizontal { amount: -120 }))
                && has(&|e| matches!(e, InputEvent::GamepadArrival { pad: 0, .. }))
        })
        .await;

    // A keyframe request and a reference invalidation reach the backend.
    let before = backend.log.lock().unwrap().keyframes;
    stream.send(ClientCommand::Raw(ControlPacket::RequestIdr));
    backend
        .wait_for("an IDR request", |l| l.keyframes > before)
        .await;
    stream.send(ClientCommand::Raw(
        ControlPacket::InvalidateReferenceFrames {
            first_frame_index: 3,
            reserved1: 0,
            last_frame_index: 4,
            reserved2: [0; 3],
        },
    ));
    // Wire frames 3 and 4 were backend frames 101 and 102.
    backend
        .wait_for("an invalidation", |l| l.invalidations.contains(&(101, 102)))
        .await;

    // Feedback from the backend reaches the client, every message of a burst.
    for n in 0..5u16 {
        backend
            .feedback
            .send(Feedback::Rumble {
                pad: 0,
                low: 1000 * (n + 1),
                high: n,
            })
            .await
            .unwrap();
    }
    for n in 0..5u16 {
        let (pad, low, high) = stream
            .next(10, |e| match e {
                ClientEvent::Rumble { pad, low, high } => Ok((pad, low, high)),
                other => Err(other),
            })
            .await;
        assert_eq!((pad, low, high), (0, 1000 * (n + 1), n));
    }

    // Cancelling quits the app through the directory and stops the media.
    host.cancel().await.unwrap();
    backend
        .wait_for("the backend to stop", |l| l.stopped && l.released)
        .await;
    assert_eq!(rig.directory.cancels(), vec![client.id()]);
    let _: MediaStatsSnapshot = rig
        .directory
        .media(1)
        .map(|m| m.stats())
        .unwrap_or_default();
    host.update().await.unwrap();
    assert_eq!(host.current_game().await.unwrap(), 0);
    rig.host.shutdown().await;
}

/// A client that leaves keeps its session; resuming it starts new media under a new key.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_leaves_can_resume_its_session() {
    let rig = Rig::start().await;
    let client = TestClient::new("resumer");
    let host = client.paired_host(&rig).await;

    let mut first = connect_stream(&host, 1, settings(VideoCodec::H264, 1280, 720, 60)).await;
    let backend1 = rig.backend.next_stream().await;
    backend1
        .wait_for("a keyframe request", |l| l.keyframes >= 1)
        .await;
    first.send(ClientCommand::Disconnect);
    first
        .next(10, |e| {
            matches!(e, ClientEvent::Disconnected)
                .then_some(())
                .ok_or(e)
        })
        .await;
    // The media ends and the backend stops, but the app keeps running.
    backend1
        .wait_for("the first media to stop", |l| l.stopped && l.released)
        .await;
    assert!(rig.directory.cancels().is_empty());
    host.update().await.unwrap();
    assert_eq!(
        host.current_game().await.unwrap(),
        1,
        "the session waits for its client"
    );

    // The client comes back: /resume, new RTSP, new media.
    let mut second = connect_stream(&host, 1, settings(VideoCodec::H264, 1280, 720, 60)).await;
    let backend2 = rig.backend.next_stream().await;
    assert_eq!(rig.directory.resumes(), vec![client.id()]);
    assert_eq!(
        rig.directory.launches().len(),
        1,
        "a resume is not a second launch"
    );
    backend2
        .wait_for("a keyframe request", |l| l.keyframes >= 1)
        .await;
    let au = access_unit(VideoCodec::H264, true, 4000, 3);
    let mut got = None;
    for i in 0..100u64 {
        push_video(&backend2, true, &au, i).await;
        if let Ok(Some(ClientEvent::Video { data, .. })) =
            tokio::time::timeout(Duration::from_millis(100), second.events.recv()).await
        {
            got = Some(data);
            break;
        }
    }
    assert_access_unit(
        &got.expect("video flows after the resume"),
        &au,
        "the resumed keyframe",
    );
    // Frame numbering starts again at 1 for the new connection.
    host.cancel().await.unwrap();
    backend2
        .wait_for("the second media to stop", |l| l.stopped)
        .await;
    assert_eq!(rig.directory.cancels(), vec![client.id()]);
    rig.host.shutdown().await;
}

/// Audio encryption is checked against an independent implementation: the client decrypts
/// what the host encrypts with the key id and sequence number as the IV.
#[tokio::test(flavor = "multi_thread")]
async fn encrypted_audio_decrypts_in_the_client() {
    let rig = Rig::start().await;
    let client = TestClient::new("audio-enc");
    let host = client.paired_host(&rig).await;
    let mut stream = connect_stream(
        &host,
        1,
        settings_encrypting(VideoCodec::H264, 1280, 720, 60, EncryptionFlags::AUDIO),
    )
    .await;
    let backend = rig.backend.next_stream().await;
    backend
        .wait_for("a keyframe request", |l| l.keyframes >= 1)
        .await;
    // The client's PING reaches the audio port a moment after the control stream is up.
    for round in 0..40u8 {
        for n in 0..8u8 {
            backend
                .audio
                .send(OpusPacket {
                    data: vec![round.wrapping_add(n); 77].into(),
                    samples: 240,
                })
                .await
                .unwrap();
        }
        let got = tokio::time::timeout(Duration::from_millis(100), async {
            loop {
                match stream.events.recv().await {
                    Some(ClientEvent::Audio { data }) => return Some(data),
                    Some(_) => {}
                    None => return None,
                }
            }
        })
        .await;
        if let Ok(Some(data)) = got {
            assert_eq!(data.len(), 77, "a decrypted Opus packet");
            assert!(
                data.iter().all(|&b| b == data[0]),
                "decrypted to what was sent: {data:?}"
            );
            rig.host.shutdown().await;
            return;
        }
    }
    panic!("no audio arrived");
}

#[tokio::test(flavor = "multi_thread")]
async fn hevc_is_negotiated_and_delivered() {
    let rig = Rig::start().await;
    let client = TestClient::new("hevc-client");
    let host = client.paired_host(&rig).await;
    let mut stream = connect_stream(&host, 2, settings(VideoCodec::Hevc, 1920, 1080, 120)).await;
    let backend = rig.backend.next_stream().await;
    assert_eq!(
        (
            backend.params.codec,
            backend.params.fps,
            backend.params.app_id
        ),
        (VideoCodec::Hevc, 120, 2)
    );
    backend
        .wait_for("a keyframe request", |l| l.keyframes >= 1)
        .await;
    let au = access_unit(VideoCodec::Hevc, true, 20_000, 9);
    let mut got = None;
    for i in 0..100u64 {
        push_video(&backend, true, &au, i).await;
        if let Ok(Some(ClientEvent::Video { data, key, .. })) =
            tokio::time::timeout(Duration::from_millis(100), stream.events.recv()).await
        {
            assert!(key);
            got = Some(data);
            break;
        }
    }
    assert_access_unit(&got.expect("an HEVC keyframe"), &au, "the HEVC keyframe");
    rig.host.shutdown().await;
}

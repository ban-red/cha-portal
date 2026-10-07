//! The transport against our own GameStream host (`cha-gamestream`'s front and
//! media, with the fake directory and backend its own tests use), in process on
//! loopback: add by address, pair, apps, launch, video, audio, input, rumble,
//! keyframes and the ways a session ends.

#[path = "../../cha-gamestream/tests/common/rig.rs"]
mod common;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use cha_client::{
    Codec, Ended, Feedback, Host, Input, PadState, Pairing, Session, StreamConfig, Transport,
};
use cha_client_gamestream::GameStream;
use cha_gamestream::input::buttons;
use cha_gamestream::{EncodedVideo, InputEvent, OpusPacket, VideoCodec};
use common::{BackendStream, Rig, access_unit};

/// A directory under the system's temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("cha-client-gs-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const HOST_ID: &str = "ABCDEF0123456789ABCDEF0123456789";

fn config() -> StreamConfig {
    StreamConfig {
        width: 1280,
        height: 720,
        fps: 60,
        bitrate_kbps: 20_000,
        codecs: vec![Codec::Av1, Codec::Hevc, Codec::H264],
        audio_channels: 2,
    }
}

/// Adds the rig's host by address and pairs with it, the way a user does: the
/// PIN the transport returns is the one typed "on the host".
async fn paired(rig: &Rig, player: &GameStream) -> Host {
    let host = player
        .add_host(&format!("127.0.0.1:{}", rig.http_port()))
        .await
        .expect("the host answers");
    let Pairing { pin, done, .. } = player.pair(&host.id).await.expect("pairing starts");
    assert_eq!(pin.len(), 4);
    assert!(pin.bytes().all(|b| b.is_ascii_digit()));
    // The user types the PIN while the pairing waits for it; the pairing's own
    // failure ends the wait.
    let user = rig.directory.answer_next_pin(&pin);
    tokio::pin!(done, user);
    let mut typed = false;
    let outcome = loop {
        tokio::select! {
            outcome = &mut done => break outcome,
            () = &mut user, if !typed => typed = true,
        }
    };
    outcome.expect("the host accepts the PIN");
    host
}

async fn push_video(backend: &BackendStream, key: bool, data: &[u8], index: u64) {
    backend
        .video
        .send(EncodedVideo {
            data: data.to_vec().into(),
            key,
            index,
            captured: Instant::now(),
        })
        .await
        .unwrap();
}

/// A client hands frames on as it got them: the access unit, then the zero
/// padding of the last packet.
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
        "{what}: padding is not zeros"
    );
}

/// Pushes keyframes until the session has one: frames before the client's
/// first PING are dropped by the host.
async fn first_keyframe(
    session: &mut Session,
    backend: &BackendStream,
    au: &[u8],
) -> cha_client::VideoFrame {
    for attempt in 0..100u64 {
        push_video(backend, true, au, attempt).await;
        if let Ok(Some(frame)) =
            tokio::time::timeout(Duration::from_millis(100), session.video.recv()).await
        {
            return frame;
        }
    }
    panic!("the session never received a keyframe");
}

async fn next_event<T>(rx: &mut tokio::sync::mpsc::Receiver<T>, what: &str) -> T {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap_or_else(|| panic!("the channel closed waiting for {what}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_player_pairs_with_a_cha_host_and_lists_its_apps() {
    let rig = Rig::start().await;
    let dir = TempDir::new("pair");
    let player = GameStream::without_discovery(dir.0.clone()).unwrap();
    assert_eq!(player.name(), "Moonlight");
    assert!(player.hosts().is_empty());

    let host = player
        .add_host(&format!("127.0.0.1:{}", rig.http_port()))
        .await
        .unwrap();
    assert_eq!(host.id, HOST_ID);
    assert_eq!(host.name, "Test Host");
    assert_eq!(host.address, format!("127.0.0.1:{}", rig.http_port()));
    assert!(!host.paired);
    assert_eq!(host.running_app, None);
    assert_eq!(player.hosts(), std::slice::from_ref(&host));

    // Nothing to list or launch before pairing.
    let err = player.apps(&host.id).await.unwrap_err();
    assert!(err.to_string().contains("pair it first"), "{err:#}");
    let err = player
        .launch(&host.id, 1, config())
        .await
        .err()
        .expect("not paired");
    assert!(err.to_string().contains("pair it first"), "{err:#}");
    // Neither is a host nobody knows.
    assert!(player.pair("0123").await.is_err());
    // Nor an address that isn't one.
    assert!(player.add_host("not an address").await.is_err());

    let host = paired(&rig, &player).await;
    let host_now = player.hosts().remove(0);
    assert!(host_now.paired, "the list says paired once pairing is done");
    assert_eq!(host_now.id, host.id);

    // The host knows this player by its certificate, under the name it gave.
    use cha_gamestream::PairingStore;
    let known = rig.store.list().await.unwrap();
    assert_eq!(known.len(), 1);
    assert_eq!(known[0].name, "ChaPlayer");
    assert_eq!(known[0].unique_id, "cha-player");
    assert_eq!(rig.directory.attempts().len(), 1);

    let apps = player.apps(&host.id).await.unwrap();
    assert_eq!(
        apps.iter()
            .map(|a| (a.id, a.name.as_str(), a.hdr))
            .collect::<Vec<_>>(),
        [(1, "Desktop", false), (2, "Steam & Co", true)]
    );

    // A restart (same directory, nothing found yet) still has the paired host,
    // and can use it without pairing again.
    drop(player);
    let again = GameStream::without_discovery(dir.0.clone()).unwrap();
    let remembered = again.hosts();
    assert_eq!(remembered.len(), 1);
    assert!(remembered[0].paired);
    assert_eq!(remembered[0].id, HOST_ID);
    assert_eq!(again.apps(HOST_ID).await.unwrap().len(), 2);
    rig.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_streams_video_and_audio_and_takes_input_rumble_and_keyframe_requests() {
    let rig = Rig::start().await;
    let dir = TempDir::new("stream");
    let player = GameStream::without_discovery(dir.0.clone()).unwrap();
    let host = paired(&rig, &player).await;

    let mut session = player.launch(&host.id, 1, config()).await.unwrap();
    let backend = rig.backend.next_stream().await;

    // HEVC: the player's first choice (AV1, ahead of it, isn't offered).
    assert_eq!(session.codec, Codec::Hevc);
    assert_eq!((session.width, session.height), (1280, 720));
    let p = &backend.params;
    assert_eq!(
        (p.width, p.height, p.fps, p.codec, p.app_id),
        (1280, 720, 60, VideoCodec::Hevc, 1)
    );
    assert_eq!(p.bitrate_bps, 20_000_000);
    let launches = rig.directory.launches();
    assert_eq!(launches.len(), 1);
    assert_eq!((launches[0].1.app_id, launches[0].1.width), (1, 1280));
    // The host says the app runs.
    let running = player.hosts().remove(0).running_app;
    assert!(running.is_none() || running == Some(1));

    // Video: whole access units, keyframe flag as the host sent it.
    backend
        .wait_for("a keyframe request", |l| l.keyframes >= 1)
        .await;
    let key_au = access_unit(VideoCodec::Hevc, true, 9000, 1);
    let frame = first_keyframe(&mut session, &backend, &key_au).await;
    assert!(frame.key);
    assert_eq!(frame.codec, Codec::Hevc);
    assert_eq!(frame.number, 1, "the host's frame numbers start at 1");
    assert_access_unit(&frame.data, &key_au, "the keyframe");
    for (i, len) in [200usize, 1400, 30_000, 100_000].into_iter().enumerate() {
        let au = access_unit(VideoCodec::Hevc, false, len, 10 + i as u8);
        push_video(&backend, false, &au, 100 + i as u64).await;
        let frame = next_event(&mut session.video, "a frame").await;
        assert!(!frame.key);
        assert_eq!(frame.number, 2 + i as u64);
        assert_access_unit(&frame.data, &au, &format!("a {len}-byte frame"));
    }

    // Audio: stereo Opus, in order.
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
        let packet = next_event(&mut session.audio, "an audio packet").await;
        assert_eq!(&packet.data[..], &vec![n; 60][..], "audio packet {n}");
        assert_eq!((packet.channels, packet.sample_rate), (2, 48_000));
        assert_eq!(packet.samples, 240, "5 ms of 48 kHz, as the backend sent");
    }

    // Input, in the browser model, arrives mapped.
    let control = &session.control;
    control.input(Input::Key {
        code: "ShiftLeft".into(),
        down: true,
    });
    control.input(Input::Key {
        code: "KeyB".into(),
        down: true,
    });
    control.input(Input::MouseMotion { dx: 3.0, dy: -2.0 });
    control.input(Input::MouseMove { x: 0.5, y: 1.0 });
    control.input(Input::MouseButton {
        button: 0,
        down: true,
    });
    control.input(Input::Wheel { dx: 0.0, dy: 100.0 });
    let mut buttons = vec![0.0; 17];
    buttons[0] = 1.0; // A
    buttons[7] = 1.0; // the right trigger
    control.input(Input::Pad {
        index: 0,
        pad: PadState {
            buttons,
            axes: vec![0.0, -1.0, 0.0, 0.0],
        },
    });
    backend
        .wait_for("the mapped input", |l| {
            let has = |f: &dyn Fn(&InputEvent) -> bool| l.inputs.iter().any(f);
            has(&|e| matches!(e, InputEvent::Key { down: true, vk: 0xA0, .. }))
                // B under Shift
                && has(&|e| matches!(e, InputEvent::Key { down: true, vk: 0x42, modifiers: 1 }))
                && has(&|e| matches!(e, InputEvent::MouseMoveRelative { dx: 3, dy: -2 }))
                && has(&|e| {
                    matches!(
                        e,
                        InputEvent::MouseMoveAbsolute { x: 640, y: 719, width: 1280, height: 720 }
                    )
                })
                && has(&|e| {
                    matches!(
                        e,
                        InputEvent::MouseButton { button: cha_gamestream::input::MouseButton::Left, down: true }
                    )
                })
                && has(&|e| matches!(e, InputEvent::ScrollVertical { amount: -120 }))
                && has(&|e| matches!(e, InputEvent::GamepadArrival { pad: 0, .. }))
                && has(&|e| {
                    matches!(
                        e,
                        InputEvent::GamepadState { pad: 0, buttons, right_trigger: 255, left_stick: (0, y), .. }
                            if buttons & buttons::A != 0 && *y > 0
                    )
                })
        })
        .await;

    // Focus lost: everything held is let go.
    session.control.release_all();
    backend
        .wait_for("the release", |l| {
            let has = |f: &dyn Fn(&InputEvent) -> bool| l.inputs.iter().any(f);
            has(&|e| {
                matches!(
                    e,
                    InputEvent::Key {
                        down: false,
                        vk: 0xA0,
                        ..
                    }
                )
            }) && has(&|e| {
                matches!(
                    e,
                    InputEvent::Key {
                        down: false,
                        vk: 0x42,
                        ..
                    }
                )
            }) && has(&|e| {
                matches!(
                    e,
                    InputEvent::MouseButton {
                        button: cha_gamestream::input::MouseButton::Left,
                        down: false
                    }
                )
            })
        })
        .await;

    // A keyframe request reaches the backend.
    let before = backend.log.lock().unwrap().keyframes;
    session.control.request_keyframe();
    backend
        .wait_for("the keyframe request", |l| l.keyframes > before)
        .await;

    // Rumble from the game reaches the player as levels.
    backend
        .feedback
        .send(cha_gamestream::Feedback::Rumble {
            pad: 0,
            low: u16::MAX,
            high: 0x8000,
        })
        .await
        .unwrap();
    match next_event(&mut session.feedback, "rumble").await {
        Feedback::Rumble {
            index: 0,
            low,
            high,
        } => {
            assert_eq!(low, 1.0);
            assert!((high - 0.5).abs() < 0.001, "{high}");
        }
        other => panic!("expected rumble, got {other:?}"),
    }
    backend
        .feedback
        .send(cha_gamestream::Feedback::Rumble {
            pad: 0,
            low: 0,
            high: 0,
        })
        .await
        .unwrap();
    assert_eq!(
        next_event(&mut session.feedback, "the rumble stopping").await,
        Feedback::Rumble {
            index: 0,
            low: 0.0,
            high: 0.0
        }
    );

    // Stop and quit: the host's app is cancelled.
    session.control.stop(true);
    assert_eq!(session.ended.await.unwrap(), Ended::Stopped);
    assert_eq!(
        rig.directory.cancels().len(),
        1,
        "quit_app cancels on the host"
    );
    backend.wait_for("the media stopping", |l| l.stopped).await;
    rig.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_without_quitting_leaves_the_app_to_be_resumed_and_another_is_refused() {
    let rig = Rig::start().await;
    let dir = TempDir::new("resume");
    let player = GameStream::without_discovery(dir.0.clone()).unwrap();
    let host = paired(&rig, &player).await;

    let session = player.launch(&host.id, 1, config()).await.unwrap();
    let first = rig.backend.next_stream().await;
    session.control.stop(false);
    assert_eq!(session.ended.await.unwrap(), Ended::Stopped);
    first.wait_for("the media stopping", |l| l.stopped).await;
    assert!(
        rig.directory.cancels().is_empty(),
        "the app was left running"
    );

    // Another app is not started over it.
    let err = player
        .launch(&host.id, 2, config())
        .await
        .err()
        .expect("refused");
    assert!(err.to_string().contains("another app"), "{err:#}");

    // The same app resumes.
    let session = player.launch(&host.id, 1, config()).await.unwrap();
    let second = rig.backend.next_stream().await;
    assert_eq!(rig.directory.resumes().len(), 1);
    assert_eq!(rig.directory.launches().len(), 1);

    // Dropping the session stops the stream and, again, leaves the app.
    drop(session);
    second
        .wait_for("the resumed media stopping", |l| l.stopped)
        .await;
    assert!(rig.directory.cancels().is_empty());
    rig.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_launch_asks_only_for_what_it_can_play() {
    let rig = Rig::start().await;
    let dir = TempDir::new("refuse");
    let player = GameStream::without_discovery(dir.0.clone()).unwrap();
    let host = paired(&rig, &player).await;

    let only_av1 = StreamConfig {
        codecs: vec![Codec::Av1],
        ..config()
    };
    let err = player
        .launch(&host.id, 1, only_av1)
        .await
        .err()
        .expect("no codec");
    assert!(
        err.to_string().contains("can't encode any of the codecs"),
        "{err:#}"
    );
    let surround = StreamConfig {
        audio_channels: 6,
        ..config()
    };
    let err = player
        .launch(&host.id, 1, surround)
        .await
        .err()
        .expect("no surround");
    assert!(err.to_string().contains("stereo"), "{err:#}");
    // H.264 only: the host gives H.264.
    let h264 = StreamConfig {
        codecs: vec![Codec::H264],
        ..config()
    };
    let session = player.launch(&host.id, 1, h264).await.unwrap();
    assert_eq!(session.codec, Codec::H264);
    let backend = rig.backend.next_stream().await;
    assert_eq!(backend.params.codec, VideoCodec::H264);
    // Nothing else was started by the refused launches.
    assert_eq!(rig.directory.launches().len(), 1);
    rig.host.shutdown().await;
}

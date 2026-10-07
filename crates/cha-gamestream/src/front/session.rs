// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: not a task with channels but a small state machine behind a mutex; a session belongs to the
// certificate that launched it and to that client's address; the media half is started through the Directory.

//! The one session a host runs: launched by a paired client, negotiated over
//! RTSP, streaming, and — if the client leaves — waiting to be resumed.
//!
//! ```text
//!            launch                 PLAY
//! (none) ─────────────▶ Launched ─────────▶ Active
//!   ▲   Initialized       ▲  │ ▲  media ended │
//!   │   while the         │  │ └──────────────┘
//!   │   Directory         │  └─ resume (the old media is stopped first)
//!   └─── cancel ──────────┴──────────────────────
//! ```
//!
//! Only the client whose certificate launched the session may resume or
//! cancel it, and only from the address it launched from may RTSP proceed.

use std::net::IpAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};

use crate::directory::{Directory, DirectoryError, LaunchRequest, ResumeRequest, SessionTarget};
use crate::handoff::{ClientId, Encryption, MediaPorts, SessionHandoff, SessionKeys, StreamParams};
use crate::net::same_ip;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SessionError {
    #[error("a session is already running on this host")]
    Busy,
    #[error("no session is running")]
    NoSession,
    #[error("the session belongs to another client")]
    NotOwner,
    #[error("the session isn't ready for that")]
    NotReady,
    #[error(transparent)]
    Directory(#[from] DirectoryError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum State {
    /// Reserved while the Directory launches.
    Initialized,
    Launched,
    Active,
}

struct Negotiated {
    params: StreamParams,
    encryption: Encryption,
}

struct Session {
    owner: ClientId,
    owner_ip: IpAddr,
    app_id: u32,
    launch: LaunchRequest,
    keys: SessionKeys,
    target: Option<SessionTarget>,
    state: State,
    negotiated: Option<Negotiated>,
    /// The ENet connect data and ping payload this connection's RTSP told the client.
    connect_data: u32,
    ping_payload: [u8; 16],
    /// The stream instance whose media runs, while Active.
    stream_id: Option<u64>,
}

/// What RTSP needs of the session.
pub(crate) struct RtspView {
    pub owner: ClientId,
    pub owner_ip: IpAddr,
    pub app_id: u32,
    pub launch: LaunchRequest,
    pub media_ports: MediaPorts,
    pub connect_data: u32,
    pub ping_payload: [u8; 16],
}

#[derive(Default)]
pub(crate) struct Sessions {
    current: Mutex<Option<Session>>,
    next_stream: AtomicU64,
}

fn fresh_connection() -> (u32, [u8; 16]) {
    let rng = SystemRandom::new();
    let mut connect = [0u8; 4];
    let mut raw = [0u8; 16];
    let _ = rng.fill(&mut connect);
    let _ = rng.fill(&mut raw);
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let payload = raw.map(|b| ALPHABET[usize::from(b) % ALPHABET.len()]);
    (u32::from_le_bytes(connect), payload)
}

impl Sessions {
    /// The app of the running session, 0 when there is none.
    pub fn current_app(&self) -> u32 {
        self.current
            .lock()
            .expect("session")
            .as_ref()
            .map_or(0, |s| s.app_id)
    }

    pub fn owner(&self) -> Option<ClientId> {
        self.current
            .lock()
            .expect("session")
            .as_ref()
            .map(|s| s.owner.clone())
    }

    /// A client launches. Fails if any session exists.
    pub async fn launch(
        &self,
        directory: &dyn Directory,
        owner: &ClientId,
        owner_ip: IpAddr,
        launch: LaunchRequest,
        keys: SessionKeys,
    ) -> Result<(), SessionError> {
        {
            let mut current = self.current.lock().expect("session");
            if current.is_some() {
                return Err(SessionError::Busy);
            }
            let (connect_data, ping_payload) = fresh_connection();
            *current = Some(Session {
                owner: owner.clone(),
                owner_ip,
                app_id: launch.app_id,
                launch: launch.clone(),
                keys,
                target: None,
                state: State::Initialized,
                negotiated: None,
                connect_data,
                ping_payload,
                stream_id: None,
            });
        }
        match directory.launch(owner, launch).await {
            Ok(target) => {
                if let Some(s) = self.current.lock().expect("session").as_mut() {
                    s.target = Some(target);
                    s.state = State::Launched;
                }
                Ok(())
            }
            Err(e) => {
                *self.current.lock().expect("session") = None;
                Err(e.into())
            }
        }
    }

    /// The owner comes back with a new key; whatever media ran stops and the
    /// Directory answers with the ports for the next connection.
    pub async fn resume(
        &self,
        directory: &dyn Directory,
        owner: &ClientId,
        owner_ip: IpAddr,
        keys: SessionKeys,
        surround_audio_info: u32,
    ) -> Result<(), SessionError> {
        let (stream, app_id) = {
            let mut current = self.current.lock().expect("session");
            let s = current.as_mut().ok_or(SessionError::NoSession)?;
            if &s.owner != owner {
                return Err(SessionError::NotOwner);
            }
            if s.state == State::Initialized {
                return Err(SessionError::NotReady);
            }
            let stream = s.stream_id.take();
            s.state = State::Launched;
            s.negotiated = None;
            s.keys = keys;
            s.owner_ip = owner_ip;
            s.launch.surround_audio_info = surround_audio_info;
            let (connect_data, ping_payload) = fresh_connection();
            s.connect_data = connect_data;
            s.ping_payload = ping_payload;
            (stream, s.app_id)
        };
        if let Some(id) = stream {
            directory.stop_media(id).await;
        }
        let target = directory
            .resume(
                owner,
                ResumeRequest {
                    app_id,
                    surround_audio_info,
                },
            )
            .await?;
        if let Some(s) = self.current.lock().expect("session").as_mut() {
            s.target = Some(target);
        }
        Ok(())
    }

    /// The owner cancels: stop the media, quit the app, forget the session.
    /// Cancelling when nothing runs is fine.
    pub async fn cancel(
        &self,
        directory: &dyn Directory,
        owner: &ClientId,
    ) -> Result<(), SessionError> {
        let stream = {
            let mut current = self.current.lock().expect("session");
            match current.as_ref() {
                None => return Ok(()),
                Some(s) if &s.owner != owner => return Err(SessionError::NotOwner),
                Some(_) => {}
            }
            current.take().and_then(|s| s.stream_id)
        };
        if let Some(id) = stream {
            directory.stop_media(id).await;
        }
        directory.cancel(owner).await;
        Ok(())
    }

    /// Ends the session whoever launched it (the embedder's decision).
    pub async fn cancel_any(&self, directory: &dyn Directory) {
        let owner = self.owner();
        if let Some(owner) = owner {
            let _ = self.cancel(directory, &owner).await;
        }
    }

    /// The media of stream `id` ended on its own; the session waits for a resume.
    pub fn media_ended(&self, id: u64) {
        if let Some(s) = self.current.lock().expect("session").as_mut()
            && s.stream_id == Some(id)
        {
            s.stream_id = None;
            s.state = State::Launched;
            s.negotiated = None;
        }
    }

    /// The session as RTSP from `peer` may see it: launched and from the
    /// owner's address.
    pub fn rtsp_view(&self, peer: IpAddr) -> Option<RtspView> {
        let current = self.current.lock().expect("session");
        let s = current.as_ref()?;
        if s.state != State::Launched || !same_ip(s.owner_ip, peer) {
            return None;
        }
        Some(RtspView {
            owner: s.owner.clone(),
            owner_ip: s.owner_ip,
            app_id: s.app_id,
            launch: s.launch.clone(),
            media_ports: s.target?.media_ports,
            connect_data: s.connect_data,
            ping_payload: s.ping_payload,
        })
    }

    /// RTSP `ANNOUNCE`: what the client chose.
    pub fn announce(
        &self,
        peer: IpAddr,
        params: StreamParams,
        encryption: Encryption,
    ) -> Result<(), SessionError> {
        let mut current = self.current.lock().expect("session");
        let s = current.as_mut().ok_or(SessionError::NoSession)?;
        if s.state != State::Launched || !same_ip(s.owner_ip, peer) {
            return Err(SessionError::NotReady);
        }
        s.negotiated = Some(Negotiated { params, encryption });
        Ok(())
    }

    /// RTSP `PLAY`: start the media.
    pub async fn play(&self, directory: &dyn Directory, peer: IpAddr) -> Result<(), SessionError> {
        let (handoff, id) = {
            let mut current = self.current.lock().expect("session");
            let s = current.as_mut().ok_or(SessionError::NoSession)?;
            if s.state != State::Launched || !same_ip(s.owner_ip, peer) {
                return Err(SessionError::NotReady);
            }
            let negotiated = s.negotiated.as_ref().ok_or(SessionError::NotReady)?;
            let id = self.next_stream.fetch_add(1, Ordering::Relaxed) + 1;
            let handoff = SessionHandoff {
                session_id: id,
                keys: s.keys.clone(),
                encryption: negotiated.encryption,
                control_connect_data: s.connect_data,
                ping_payload: s.ping_payload,
                params: negotiated.params.clone(),
            };
            s.state = State::Active;
            s.stream_id = Some(id);
            (handoff, id)
        };
        if let Err(e) = directory.start_media(handoff).await {
            self.media_ended(id);
            return Err(e.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use bytes::Bytes;
    use futures_util::future::BoxFuture;

    use super::*;
    use crate::directory::{App, PairingAttempt, PinWaiter};
    use crate::handoff::{AudioParams, Chroma, VideoCodec};

    /// A directory that records what it is asked and can be told to fail a launch.
    #[derive(Default)]
    struct Stub {
        log: Mutex<Vec<String>>,
        fail_launch: AtomicBool,
        fail_media: AtomicBool,
    }

    impl Stub {
        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }
    }

    const PORTS: MediaPorts = MediaPorts {
        video: 1,
        control: 2,
        audio: 3,
    };

    impl Directory for Stub {
        fn apps(&self, _: &ClientId) -> BoxFuture<'_, Vec<App>> {
            Box::pin(async { vec![] })
        }
        fn app_image(&self, _: &ClientId, _: u32) -> BoxFuture<'_, Option<Bytes>> {
            Box::pin(async { None })
        }
        fn launch(
            &self,
            _: &ClientId,
            _: LaunchRequest,
        ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
            self.log.lock().unwrap().push("launch".into());
            let fail = self.fail_launch.load(Ordering::SeqCst);
            Box::pin(async move {
                if fail {
                    Err(DirectoryError::Failed("no".into()))
                } else {
                    Ok(SessionTarget { media_ports: PORTS })
                }
            })
        }
        fn resume(
            &self,
            _: &ClientId,
            _: ResumeRequest,
        ) -> BoxFuture<'_, Result<SessionTarget, DirectoryError>> {
            self.log.lock().unwrap().push("resume".into());
            Box::pin(async { Ok(SessionTarget { media_ports: PORTS }) })
        }
        fn start_media(&self, h: SessionHandoff) -> BoxFuture<'_, Result<(), DirectoryError>> {
            self.log
                .lock()
                .unwrap()
                .push(format!("start_media {}", h.session_id));
            let fail = self.fail_media.load(Ordering::SeqCst);
            Box::pin(async move {
                if fail {
                    Err(DirectoryError::Failed("no".into()))
                } else {
                    Ok(())
                }
            })
        }
        fn stop_media(&self, id: u64) -> BoxFuture<'_, ()> {
            self.log.lock().unwrap().push(format!("stop_media {id}"));
            Box::pin(async {})
        }
        fn cancel(&self, _: &ClientId) -> BoxFuture<'_, ()> {
            self.log.lock().unwrap().push("cancel".into());
            Box::pin(async {})
        }
        fn pin_for(&self, _: PairingAttempt) -> PinWaiter {
            PinWaiter::refused()
        }
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }
    fn client(n: u8) -> ClientId {
        ClientId(format!("{n:02x}"))
    }
    fn request() -> LaunchRequest {
        LaunchRequest {
            app_id: 5,
            width: 1280,
            height: 720,
            fps: 60,
            hdr: false,
            surround_audio_info: 0x30002,
            local_audio: false,
            optimize_game_settings: false,
            gamepad_mask: 0,
        }
    }
    fn keys(id: i64) -> SessionKeys {
        SessionKeys {
            key: [1; 16],
            key_id: id,
        }
    }
    fn params(owner: &ClientId, ip: IpAddr) -> StreamParams {
        StreamParams {
            client: owner.clone(),
            client_ip: ip,
            app_id: 5,
            width: 1280,
            height: 720,
            fps: 60,
            bitrate_bps: 1,
            codec: VideoCodec::H264,
            hdr: false,
            chroma: Chroma::Yuv420,
            full_range: false,
            max_ref_frames: 1,
            packet_size: 1024,
            fec_percent: 20,
            min_fec_packets: 0,
            audio: AudioParams::select(2, 3, true, 5),
        }
    }
    const ENC: Encryption = Encryption {
        control: true,
        video: false,
        audio: false,
    };

    async fn launched(sessions: &Sessions, dir: &Stub, owner: &ClientId, from: &str) {
        sessions
            .launch(dir, owner, ip(from), request(), keys(1))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn one_session_at_a_time_and_a_failed_launch_frees_the_slot() {
        let (sessions, dir) = (Sessions::default(), Stub::default());
        dir.fail_launch.store(true, Ordering::SeqCst);
        let r = sessions
            .launch(&dir, &client(1), ip("10.0.0.1"), request(), keys(1))
            .await;
        assert!(matches!(r, Err(SessionError::Directory(_))));
        assert_eq!(
            sessions.current_app(),
            0,
            "a failed launch leaves nothing behind"
        );

        dir.fail_launch.store(false, Ordering::SeqCst);
        launched(&sessions, &dir, &client(1), "10.0.0.1").await;
        assert_eq!(sessions.current_app(), 5);
        let r = sessions
            .launch(&dir, &client(2), ip("10.0.0.2"), request(), keys(1))
            .await;
        assert_eq!(r, Err(SessionError::Busy));
        let r = sessions
            .launch(&dir, &client(1), ip("10.0.0.1"), request(), keys(1))
            .await;
        assert_eq!(
            r,
            Err(SessionError::Busy),
            "not even the owner launches twice"
        );
    }

    #[tokio::test]
    async fn only_the_owner_resumes_or_cancels() {
        let (sessions, dir) = (Sessions::default(), Stub::default());
        launched(&sessions, &dir, &client(1), "10.0.0.1").await;
        assert_eq!(
            sessions
                .resume(&dir, &client(2), ip("10.0.0.2"), keys(2), 0x30002)
                .await,
            Err(SessionError::NotOwner)
        );
        assert_eq!(
            sessions.cancel(&dir, &client(2)).await,
            Err(SessionError::NotOwner)
        );
        assert_eq!(
            dir.log(),
            ["launch"],
            "the directory heard nothing of the refused calls"
        );
        sessions
            .resume(&dir, &client(1), ip("10.0.0.1"), keys(2), 0x30002)
            .await
            .unwrap();
        sessions.cancel(&dir, &client(1)).await.unwrap();
        assert_eq!(dir.log(), ["launch", "resume", "cancel"]);
        assert_eq!(sessions.current_app(), 0);
        assert_eq!(
            sessions
                .resume(&dir, &client(1), ip("10.0.0.1"), keys(3), 0x30002)
                .await,
            Err(SessionError::NoSession)
        );
        // Cancelling nothing is fine.
        sessions.cancel(&dir, &client(9)).await.unwrap();
    }

    #[tokio::test]
    async fn rtsp_is_for_the_launching_address_while_launched() {
        let (sessions, dir) = (Sessions::default(), Stub::default());
        assert!(sessions.rtsp_view(ip("10.0.0.1")).is_none(), "no session");
        launched(&sessions, &dir, &client(1), "10.0.0.1").await;
        assert!(
            sessions.rtsp_view(ip("10.0.0.2")).is_none(),
            "another address"
        );
        let view = sessions
            .rtsp_view(ip("10.0.0.1"))
            .expect("the owner's address");
        assert_eq!((view.app_id, view.media_ports), (5, PORTS));
        assert!(
            sessions.rtsp_view(ip("::ffff:10.0.0.1")).is_some(),
            "a mapped address is the same address"
        );
        // Another address can neither announce nor play.
        let p = params(&client(1), ip("10.0.0.1"));
        assert_eq!(
            sessions.announce(ip("10.0.0.2"), p.clone(), ENC),
            Err(SessionError::NotReady)
        );
        assert_eq!(
            sessions.play(&dir, ip("10.0.0.2")).await,
            Err(SessionError::NotReady)
        );
    }

    #[tokio::test]
    async fn play_needs_an_announce_and_hands_off_once() {
        let (sessions, dir) = (Sessions::default(), Stub::default());
        launched(&sessions, &dir, &client(1), "10.0.0.1").await;
        assert_eq!(
            sessions.play(&dir, ip("10.0.0.1")).await,
            Err(SessionError::NotReady)
        );
        sessions
            .announce(ip("10.0.0.1"), params(&client(1), ip("10.0.0.1")), ENC)
            .unwrap();
        sessions.play(&dir, ip("10.0.0.1")).await.unwrap();
        assert_eq!(dir.log(), ["launch", "start_media 1"]);
        // Active: RTSP is closed to everyone until the media ends.
        assert!(sessions.rtsp_view(ip("10.0.0.1")).is_none());
        assert_eq!(
            sessions.play(&dir, ip("10.0.0.1")).await,
            Err(SessionError::NotReady)
        );
        // A stale end of some other stream changes nothing; the real one frees RTSP again.
        sessions.media_ended(99);
        assert!(sessions.rtsp_view(ip("10.0.0.1")).is_none());
        sessions.media_ended(1);
        assert!(sessions.rtsp_view(ip("10.0.0.1")).is_some());
        assert_eq!(
            sessions.current_app(),
            5,
            "the app keeps running for a resume"
        );
    }

    #[tokio::test]
    async fn a_failed_media_start_returns_to_launched() {
        let (sessions, dir) = (Sessions::default(), Stub::default());
        launched(&sessions, &dir, &client(1), "10.0.0.1").await;
        sessions
            .announce(ip("10.0.0.1"), params(&client(1), ip("10.0.0.1")), ENC)
            .unwrap();
        dir.fail_media.store(true, Ordering::SeqCst);
        assert!(matches!(
            sessions.play(&dir, ip("10.0.0.1")).await,
            Err(SessionError::Directory(_))
        ));
        assert!(
            sessions.rtsp_view(ip("10.0.0.1")).is_some(),
            "the client may try again"
        );
    }

    #[tokio::test]
    async fn resuming_an_active_session_stops_its_media_first() {
        let (sessions, dir) = (Sessions::default(), Stub::default());
        launched(&sessions, &dir, &client(1), "10.0.0.1").await;
        sessions
            .announce(ip("10.0.0.1"), params(&client(1), ip("10.0.0.1")), ENC)
            .unwrap();
        sessions.play(&dir, ip("10.0.0.1")).await.unwrap();
        sessions
            .resume(&dir, &client(1), ip("10.0.0.1"), keys(2), 0x30002)
            .await
            .unwrap();
        assert_eq!(
            dir.log(),
            ["launch", "start_media 1", "stop_media 1", "resume"]
        );
        // The next connection gets its own secrets.
        let first = sessions.rtsp_view(ip("10.0.0.1")).unwrap();
        sessions
            .resume(&dir, &client(1), ip("10.0.0.1"), keys(3), 0x30002)
            .await
            .unwrap();
        let second = sessions.rtsp_view(ip("10.0.0.1")).unwrap();
        assert!(
            first.connect_data != second.connect_data || first.ping_payload != second.ping_payload
        );
    }

    #[tokio::test]
    async fn cancelling_an_active_session_stops_media_then_quits_the_app() {
        let (sessions, dir) = (Sessions::default(), Stub::default());
        launched(&sessions, &dir, &client(1), "10.0.0.1").await;
        sessions
            .announce(ip("10.0.0.1"), params(&client(1), ip("10.0.0.1")), ENC)
            .unwrap();
        sessions.play(&dir, ip("10.0.0.1")).await.unwrap();
        sessions.cancel_any(&dir).await;
        assert_eq!(
            dir.log(),
            ["launch", "start_media 1", "stop_media 1", "cancel"]
        );
    }

    #[test]
    fn each_connection_gets_fresh_secrets() {
        let (c1, p1) = fresh_connection();
        let (c2, p2) = fresh_connection();
        assert!(c1 != c2 || p1 != p2);
        assert!(p1.iter().all(|b| b.is_ascii_alphanumeric()));
    }
}

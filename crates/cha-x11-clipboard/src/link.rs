//! The streamer's socket: frames in and out, and trying again when it's not
//! there. Blocking, because the streamer always reads; reads happen only when
//! the descriptor is readable.

use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cha_proto::clipboard::{Frame, FrameDecoder};

/// Between tries to connect.
const RETRY: Duration = Duration::from_secs(1);
/// A streamer that reads nothing for this long is gone.
const WRITE_PATIENCE: Duration = Duration::from_secs(2);

pub struct Link {
    path: PathBuf,
    stream: Option<UnixStream>,
    decoder: FrameDecoder,
    next_try: Instant,
}

impl Link {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            stream: None,
            decoder: FrameDecoder::default(),
            next_try: Instant::now(),
        }
    }

    pub fn connected(&self) -> bool {
        self.stream.is_some()
    }

    pub fn fd(&self) -> Option<BorrowedFd<'_>> {
        self.stream.as_ref().map(AsFd::as_fd)
    }

    /// Connects if it's time and we aren't.
    pub fn maintain(&mut self, now: Instant) {
        if self.stream.is_some() || now < self.next_try {
            return;
        }
        self.next_try = now + RETRY;
        let Ok(stream) = UnixStream::connect(&self.path) else {
            return;
        };
        if stream.set_write_timeout(Some(WRITE_PATIENCE)).is_err() {
            return;
        }
        info!("connected to the streamer at {}", self.path.display());
        self.decoder = FrameDecoder::default();
        self.stream = Some(stream);
    }

    /// How long until the next try to connect (`None`: connected).
    pub fn retry_in(&self, now: Instant) -> Option<Duration> {
        self.stream
            .is_none()
            .then(|| self.next_try.saturating_duration_since(now))
    }

    fn drop_connection(&mut self, why: &str) {
        info!("lost the streamer: {why}");
        self.stream = None;
        self.next_try = Instant::now() + RETRY;
    }

    /// The frames that arrived; call when `fd` is readable.
    pub fn read(&mut self) -> Vec<Frame> {
        let Some(stream) = &mut self.stream else {
            return Vec::new();
        };
        let mut buf = [0u8; 64 * 1024];
        match stream.read(&mut buf) {
            Ok(0) => {
                self.drop_connection("it closed the socket");
                return Vec::new();
            }
            Ok(n) => self.decoder.push(&buf[..n]),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                return Vec::new();
            }
            Err(err) => {
                self.drop_connection(&err.to_string());
                return Vec::new();
            }
        }
        let mut frames = Vec::new();
        loop {
            match self.decoder.pop() {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) => return frames,
                Err(err) => {
                    self.drop_connection(&err.to_string());
                    return frames;
                }
            }
        }
    }

    /// Sends a frame, if connected.
    pub fn send(&mut self, frame: &Frame) {
        let Some(stream) = &mut self.stream else {
            return;
        };
        if let Err(err) = stream.write_all(&frame.encode()) {
            self.drop_connection(&err.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cha-x11-clipboard-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clipboard");
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn retries_until_the_streamer_is_there() {
        let path = temp_socket("retry");
        let mut link = Link::new(path.clone());
        let now = Instant::now();
        link.maintain(now);
        assert!(!link.connected());
        // It waits a second between tries, however often it's asked.
        let listener = UnixListener::bind(&path).unwrap();
        link.maintain(now + Duration::from_millis(500));
        assert!(!link.connected());
        assert_eq!(
            link.retry_in(now + Duration::from_millis(400)),
            Some(Duration::from_millis(600))
        );
        link.maintain(now + Duration::from_millis(1001));
        assert!(link.connected());
        assert_eq!(link.retry_in(now), None);
        drop(listener);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn frames_both_ways_and_a_closed_socket() {
        let path = temp_socket("frames");
        let listener = UnixListener::bind(&path).unwrap();
        let mut link = Link::new(path.clone());
        link.maintain(Instant::now());
        let (mut peer, _) = listener.accept().unwrap();

        link.send(&Frame::copied("héllo ✓"));
        link.send(&Frame::ack(3));
        let expected: Vec<u8> = [Frame::copied("héllo ✓"), Frame::ack(3)]
            .iter()
            .flat_map(Frame::encode)
            .collect();
        let mut got = vec![0u8; expected.len()];
        peer.read_exact(&mut got).unwrap();
        assert_eq!(got, expected);

        // Two frames, the second split across reads.
        let bytes: Vec<u8> = [Frame::set(1, "one"), Frame::set(2, "two")]
            .iter()
            .flat_map(Frame::encode)
            .collect();
        let (first, rest) = bytes.split_at(bytes.len() - 2);
        peer.write_all(first).unwrap();
        assert_eq!(link.read(), [Frame::set(1, "one")]);
        peer.write_all(rest).unwrap();
        assert_eq!(link.read(), [Frame::set(2, "two")]);

        drop(peer);
        assert!(link.read().is_empty());
        assert!(!link.connected());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

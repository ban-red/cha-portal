//! X11 apps' clipboard (P2.6), through the helper in the app's container.
//!
//! X11 apps (XFCE, in rootful Xwayland) keep their clipboard inside the X
//! server, which only the app's container reaches. `cha-x11-clipboard` runs
//! there and connects to a socket we listen on in the shared `/run/cha`,
//! speaking `cha_proto::clipboard`'s frames:
//!
//! - **`copied`**: an X app copied this text. It's published where Wayland
//!   apps' copies are, so the sessions send it to the page.
//! - **`set`** (ours): the page's text. The helper owns the X clipboard with it
//!   and answers **`ack`**. A paste's keys follow the text on one ordered
//!   channel, so [`X11Clipboard::set`] waits for the ack (30 ms at most) before
//!   they're passed on: the app must find the text when it asks.
//!
//! Plain threads, no runtime: one accepts, each helper connection has a reader
//! and a writer (so a stuck helper blocks no session). Expect one helper;
//! several each get every `set`.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cha_proto::clipboard::{Frame, FrameDecoder, Kind, encode};
use tracing::{debug, info, warn};

use crate::compositor::ClipboardPublisher;

/// How long a paste waits for the helper to own the X clipboard.
const ACK_WAIT: Duration = Duration::from_millis(30);
/// A helper that takes nothing for this long is stuck.
const WRITE_PATIENCE: Duration = Duration::from_secs(2);
/// `set`s queued per helper; a helper that far behind isn't listening.
const QUEUE: usize = 16;

struct Helper {
    id: u64,
    frames: mpsc::SyncSender<Arc<[u8]>>,
}

/// A `set` waiting for its ack.
struct Waiter {
    seq: u32,
    acked: mpsc::Sender<()>,
}

pub struct X11Clipboard {
    publisher: ClipboardPublisher,
    helpers: Mutex<Vec<Helper>>,
    waiting: Mutex<Vec<Waiter>>,
    next_seq: AtomicU32,
    next_id: AtomicU64,
}

impl X11Clipboard {
    /// Listens at `socket` (handed to `app_uid`, whose helper connects) and
    /// publishes what X11 apps copy to `publisher`.
    pub fn start(
        socket: &Path,
        app_uid: Option<u32>,
        publisher: ClipboardPublisher,
    ) -> Result<Arc<Self>> {
        let listener = bind(socket, app_uid)?;
        let clipboard = Arc::new(Self {
            publisher,
            helpers: Mutex::default(),
            waiting: Mutex::default(),
            next_seq: AtomicU32::new(0),
            next_id: AtomicU64::new(0),
        });
        let accepting = Arc::clone(&clipboard);
        std::thread::Builder::new()
            .name("x11-clipboard".into())
            .spawn(move || accepting.accept(listener))
            .context("starting the X11 clipboard listener")?;
        info!(socket = %socket.display(), "x11 clipboard: listening for the helper");
        Ok(clipboard)
    }

    /// The page's clipboard, for X11 apps to paste: sent to the helper, which
    /// owns the X clipboard with it. Returns once it says so (or after 30 ms),
    /// and at once if no helper is connected. Blocks the caller. False if a
    /// helper didn't answer in time.
    pub fn set(&self, text: &str) -> bool {
        let helpers = self.helpers.lock().expect("helpers lock");
        let helpers_count = helpers.len();
        if helpers_count == 0 {
            return true;
        }
        let seq = self
            .next_seq
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let frame: Arc<[u8]> = encode(Kind::Set, seq, text).into();
        let (acked, acks) = mpsc::channel();
        self.waiting
            .lock()
            .expect("waiting lock")
            .push(Waiter { seq, acked });
        let sent = helpers
            .iter()
            .filter(|helper| helper.frames.try_send(Arc::clone(&frame)).is_ok())
            .count();
        drop(helpers);
        let started = Instant::now();
        let mut acked = 0;
        // Only helpers that took the frame will answer.
        while acked < sent {
            let left = ACK_WAIT.saturating_sub(started.elapsed());
            if acks.recv_timeout(left).is_err() {
                break;
            }
            acked += 1;
        }
        self.waiting
            .lock()
            .expect("waiting lock")
            .retain(|waiter| waiter.seq != seq);
        debug!(
            bytes = text.len(),
            sent,
            acked,
            waited_us = started.elapsed().as_micros() as u64,
            "x11 clipboard: set"
        );
        acked == helpers_count
    }

    fn accept(self: Arc<Self>, listener: UnixListener) {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let clipboard = Arc::clone(&self);
                    let spawned = std::thread::Builder::new()
                        .name("x11-clipboard-conn".into())
                        .spawn(move || clipboard.connection(stream));
                    if let Err(err) = spawned {
                        warn!("x11 clipboard: couldn't serve a helper: {err}");
                    }
                }
                Err(err) => {
                    warn!("x11 clipboard: accept: {err}");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    /// One helper, from connecting to hanging up (this thread reads).
    fn connection(&self, stream: UnixStream) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let writer = match stream.try_clone() {
            Ok(writer) => writer,
            Err(err) => {
                warn!("x11 clipboard: {err}");
                return;
            }
        };
        let _ = writer.set_write_timeout(Some(WRITE_PATIENCE));
        let (frames, queue) = mpsc::sync_channel::<Arc<[u8]>>(QUEUE);
        let spawned = std::thread::Builder::new()
            .name("x11-clipboard-out".into())
            .spawn(move || {
                // Ends when the helper is dropped from the list (the queue
                // closes) or can't be written to.
                for frame in queue {
                    if (&writer).write_all(&frame).is_err() {
                        let _ = writer.shutdown(Shutdown::Both);
                        break;
                    }
                }
            });
        if let Err(err) = spawned {
            warn!("x11 clipboard: couldn't serve a helper: {err}");
            return;
        }
        let connected = {
            let mut helpers = self.helpers.lock().expect("helpers lock");
            helpers.push(Helper { id, frames });
            helpers.len()
        };
        info!(helpers = connected, "x11 clipboard: helper connected");
        let why = self.read(stream);
        let remaining = {
            let mut helpers = self.helpers.lock().expect("helpers lock");
            helpers.retain(|helper| helper.id != id);
            helpers.len()
        };
        info!(
            helpers = remaining,
            "x11 clipboard: helper disconnected: {why}"
        );
    }

    /// Handles a helper's frames until it hangs up; why it ended.
    fn read(&self, mut stream: UnixStream) -> String {
        let mut decoder = FrameDecoder::default();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => return "closed".into(),
                Ok(n) => decoder.push(&buf[..n]),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => return err.to_string(),
            }
            loop {
                match decoder.pop() {
                    Ok(Some(frame)) => self.frame(frame),
                    Ok(None) => break,
                    Err(err) => return err.to_string(),
                }
            }
        }
    }

    fn frame(&self, frame: Frame) {
        match frame.kind {
            Kind::Copied => {
                let published = self.publisher.copied_in_x11(&frame.text);
                debug!(
                    bytes = frame.text.len(),
                    published, "x11 clipboard: an app copied"
                );
            }
            Kind::Ack => {
                let waiting = self.waiting.lock().expect("waiting lock");
                if let Some(waiter) = waiting.iter().find(|w| w.seq == frame.seq) {
                    let _ = waiter.acked.send(());
                }
            }
            Kind::Set => debug!("x11 clipboard: ignoring a set from the helper"),
        }
    }
}

/// Binds the socket and hands it to the app's user: connecting takes write
/// permission, so only that user (and us) can.
fn bind(path: &Path, app_uid: Option<u32>) -> Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    if let Some(uid) = app_uid {
        std::os::unix::fs::chown(path, Some(uid), Some(uid))
            .with_context(|| format!("handing {} to uid {uid}", path.display()))?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::ClipboardWatch;

    struct Fixture {
        clipboard: Arc<X11Clipboard>,
        watch: ClipboardWatch,
        dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("cha-x11-clipboard-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let (publisher, watch) = ClipboardPublisher::new();
            let clipboard = X11Clipboard::start(&dir.join("clipboard"), None, publisher).unwrap();
            Self {
                clipboard,
                watch,
                dir,
            }
        }

        /// A helper that's connected (the streamer has counted it).
        fn helper(&self) -> UnixStream {
            let stream = UnixStream::connect(self.dir.join("clipboard")).unwrap();
            let started = Instant::now();
            while self.clipboard.helpers.lock().unwrap().is_empty() {
                assert!(started.elapsed() < Duration::from_secs(5));
                std::thread::sleep(Duration::from_millis(5));
            }
            stream
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn read_frame(stream: &mut UnixStream) -> Frame {
        let mut decoder = FrameDecoder::default();
        let mut byte = [0u8; 1];
        loop {
            if let Some(frame) = decoder.pop().unwrap() {
                return frame;
            }
            stream.read_exact(&mut byte).unwrap();
            decoder.push(&byte);
        }
    }

    #[test]
    fn copies_are_published_once() {
        let fixture = Fixture::new("copies");
        let mut helper = fixture.helper();
        let mut watch = fixture.watch.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        helper
            .write_all(&Frame::copied("héllo ✓").encode())
            .unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), watch.changed())
                .await
                .unwrap()
                .unwrap();
        });
        assert_eq!(watch.borrow_and_update().as_deref(), Some("héllo ✓"));

        // The same text again goes out again: the device's clipboard may have
        // changed since.
        helper
            .write_all(&Frame::copied("héllo ✓").encode())
            .unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), watch.changed())
                .await
                .unwrap()
                .unwrap();
        });
        assert_eq!(watch.borrow_and_update().as_deref(), Some("héllo ✓"));

        // An empty one is nothing; then a new one.
        helper.write_all(&Frame::copied("").encode()).unwrap();
        helper.write_all(&Frame::copied("next").encode()).unwrap();
        rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), watch.changed())
                .await
                .unwrap()
                .unwrap();
        });
        assert_eq!(watch.borrow_and_update().as_deref(), Some("next"));
    }

    #[test]
    fn set_waits_for_the_ack_and_no_longer() {
        let fixture = Fixture::new("set");
        let mut helper = fixture.helper();

        // A helper that acks at once.
        let acking = std::thread::spawn(move || {
            let frame = read_frame(&mut helper);
            assert_eq!((frame.kind, frame.text.as_str()), (Kind::Set, "paste ✓"));
            helper.write_all(&Frame::ack(frame.seq).encode()).unwrap();
            helper
        });
        assert!(fixture.clipboard.set("paste ✓"));
        let mut helper = acking.join().unwrap();

        // One that doesn't: the paste isn't held up for more than the wait.
        let started = Instant::now();
        assert!(!fixture.clipboard.set("again"));
        let waited = started.elapsed();
        assert!(waited >= ACK_WAIT && waited < ACK_WAIT * 5, "{waited:?}");
        assert_eq!(read_frame(&mut helper).text, "again");
    }

    #[test]
    fn no_helper_no_wait() {
        let fixture = Fixture::new("none");
        let started = Instant::now();
        assert!(fixture.clipboard.set("nobody listens"));
        assert!(started.elapsed() < Duration::from_millis(10));
    }

    #[test]
    fn a_helper_that_hangs_up_is_forgotten() {
        let fixture = Fixture::new("hangup");
        let helper = fixture.helper();
        drop(helper);
        let started = Instant::now();
        while !fixture.clipboard.helpers.lock().unwrap().is_empty() {
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = Instant::now();
        assert!(fixture.clipboard.set("nobody listens"));
        assert!(started.elapsed() < Duration::from_millis(10));
    }
}

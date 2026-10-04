//! The helper against a real X server (Xvfb) and a pretend streamer, with the
//! helper's own `--put` and `--get` as the other X clients (and `xclip`, if
//! it's installed, for STRING). Skipped without Xvfb.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use cha_proto::clipboard::{Frame, FrameDecoder};

const HELPER: &str = env!("CARGO_BIN_EXE_cha-x11-clipboard");
const PATIENCE: Duration = Duration::from_secs(10);

static COUNTER: AtomicU32 = AtomicU32::new(0);

struct Xvfb {
    child: Child,
    display: String,
}

impl Xvfb {
    /// Starts a server of its own, or `None` where there's no Xvfb.
    fn start() -> Option<Self> {
        // Servers starting together race to make the socket directory.
        static DIR: std::sync::Once = std::sync::Once::new();
        DIR.call_once(|| {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::create_dir_all("/tmp/.X11-unix");
            let _ =
                std::fs::set_permissions("/tmp/.X11-unix", std::fs::Permissions::from_mode(0o1777));
        });
        let n = 300 + (std::process::id() % 500) + COUNTER.fetch_add(1, Ordering::SeqCst) * 7;
        let display = format!(":{n}");
        let child = match Command::new("Xvfb")
            .args([&display, "-screen", "0", "640x480x24", "-nolisten", "tcp"])
            .stdout(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping: no Xvfb on this machine");
                return None;
            }
            Err(err) => panic!("starting Xvfb: {err}"),
        };
        let mut xvfb = Self { child, display };
        let socket = format!("/tmp/.X11-unix/X{n}");
        let started = Instant::now();
        while !Path::new(&socket).exists() {
            if let Some(status) = xvfb.child.try_wait().unwrap() {
                panic!("Xvfb {} exited: {status}", xvfb.display);
            }
            assert!(started.elapsed() < PATIENCE, "Xvfb never made {socket}");
            std::thread::sleep(Duration::from_millis(20));
        }
        Some(xvfb)
    }

    /// A helper process on this display.
    fn helper(&self) -> Command {
        let mut command = Command::new(HELPER);
        command.env("DISPLAY", &self.display);
        command
    }

    /// `--put text`, once it owns the clipboard.
    fn put(&self, text: &str, incr_bytes: Option<usize>) -> Child {
        let mut command = self.helper();
        // Through stdin: an argument is limited to 128 KiB.
        command
            .args(["--put", "-"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .stdout(Stdio::null());
        if let Some(bytes) = incr_bytes {
            command.env("CHA_INCR_BYTES", bytes.to_string());
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let started = Instant::now();
        loop {
            let line = lines
                .next()
                .expect("--put exited before it owned the clipboard")
                .unwrap();
            if line.contains("owning the clipboard") {
                break;
            }
            assert!(started.elapsed() < PATIENCE, "--put never owned it");
        }
        // Keep its stderr open (and show it) until it exits.
        std::thread::spawn(move || {
            lines
                .map_while(Result::ok)
                .for_each(|line| eprintln!("put: {line}"))
        });
        child
    }

    /// `--get`: its stdout, or `None` if there's no text.
    fn get(&self) -> Option<String> {
        let output = self.helper().arg("--get").output().unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).unwrap())
    }
}

impl Xvfb {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Stops a helper process whatever the test does.
struct Reaped(Child);

impl Reaped {
    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(started.elapsed() < PATIENCE, "the helper never exited");
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The streamer's end of the socket, with the helper connected to it.
struct Streamer {
    conn: UnixStream,
    decoder: FrameDecoder,
    dir: PathBuf,
    helper: Reaped,
}

impl Streamer {
    fn start(xvfb: &Xvfb, incr_bytes: Option<usize>) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "cha-x11-clipboard-it-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("clipboard");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut command = xvfb.helper();
        command.env("CHA_CLIPBOARD_SOCKET", &socket);
        if let Some(bytes) = incr_bytes {
            command.env("CHA_INCR_BYTES", bytes.to_string());
        }
        let helper = Reaped(command.spawn().unwrap());
        let started = Instant::now();
        let conn = loop {
            match listener.accept() {
                Ok((conn, _)) => break conn,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(started.elapsed() < PATIENCE, "the helper never connected");
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => panic!("accept: {err}"),
            }
        };
        conn.set_nonblocking(false).unwrap();
        Self {
            conn,
            decoder: FrameDecoder::default(),
            dir,
            helper,
        }
    }

    /// The next frame from the helper, if one comes within `wait`.
    fn recv(&mut self, wait: Duration) -> Option<Frame> {
        let until = Instant::now() + wait;
        loop {
            if let Some(frame) = self.decoder.pop().unwrap() {
                return Some(frame);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            self.conn.set_read_timeout(Some(left)).unwrap();
            let mut buf = [0u8; 64 * 1024];
            match self.conn.read(&mut buf) {
                Ok(0) => panic!("the helper closed the socket"),
                Ok(n) => self.decoder.push(&buf[..n]),
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return None;
                }
                Err(err) => panic!("read: {err}"),
            }
        }
    }

    fn expect(&mut self, frame: Frame) {
        let got = self.recv(PATIENCE);
        assert_eq!(got.as_ref().map(summary), Some(summary(&frame)));
        assert_eq!(got, Some(frame));
    }

    fn expect_nothing(&mut self) {
        let got = self.recv(Duration::from_millis(600));
        assert_eq!(got.as_ref().map(summary), None);
    }

    fn send(&mut self, frame: &Frame) {
        self.conn.write_all(&frame.encode()).unwrap();
    }
}

impl Drop for Streamer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A frame without a megabyte of text, for failure messages.
fn summary(frame: &Frame) -> String {
    let head: String = frame.text.chars().take(40).collect();
    format!(
        "{:?} #{} {head:?} ({} bytes)",
        frame.kind,
        frame.seq,
        frame.text.len()
    )
}

#[test]
fn what_x_apps_copy_reaches_the_streamer() {
    let Some(xvfb) = Xvfb::start() else { return };
    let mut streamer = Streamer::start(&xvfb, None);
    let text = "héllo wörld ✓ 🎉";

    let mut first = Reaped(xvfb.put(text, None));
    streamer.expect(Frame::copied(text));

    // The same text again goes up too (the device's clipboard may have
    // changed since), and a different one.
    let mut second = Reaped(xvfb.put(text, None));
    streamer.expect(Frame::copied(text));
    let third = Reaped(xvfb.put("other", None));
    streamer.expect(Frame::copied("other"));

    // Whoever loses the clipboard leaves.
    first.wait_for_exit();
    second.wait_for_exit();
    drop(third);
}

#[test]
fn what_the_page_sets_is_owned_then_acked() {
    let Some(xvfb) = Xvfb::start() else { return };
    let mut streamer = Streamer::start(&xvfb, None);

    streamer.send(&Frame::set(7, "ünï ✓ from the page"));
    streamer.expect(Frame::ack(7));
    assert_eq!(xvfb.get().as_deref(), Some("ünï ✓ from the page"));
    // Our own taking it isn't a copy.
    streamer.expect_nothing();

    streamer.send(&Frame::set(8, "second"));
    streamer.expect(Frame::ack(8));
    assert_eq!(xvfb.get().as_deref(), Some("second"));

    // An app that takes it over is a copy, even of the same text...
    let mut same = Reaped(xvfb.put("second", None));
    streamer.expect(Frame::copied("second"));
    // ...and of different text.
    let different = Reaped(xvfb.put("taken", None));
    streamer.expect(Frame::copied("taken"));
    same.wait_for_exit();
    drop(different);
}

#[test]
fn exits_when_the_x_server_goes() {
    let Some(mut xvfb) = Xvfb::start() else {
        return;
    };
    let mut streamer = Streamer::start(&xvfb, None);
    xvfb.stop();
    assert!(streamer.helper.wait_for_exit().success());
}

#[test]
fn connects_when_the_streamer_appears_and_again_after_it_leaves() {
    let Some(xvfb) = Xvfb::start() else { return };
    let dir =
        std::env::temp_dir().join(format!("cha-x11-clipboard-it-{}-late", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("clipboard");
    let mut command = xvfb.helper();
    command.env("CHA_CLIPBOARD_SOCKET", &socket);
    // Started with no streamer there.
    let _helper = Reaped(command.spawn().unwrap());
    std::thread::sleep(Duration::from_millis(1200));

    let accept = |listener: &UnixListener| {
        let started = Instant::now();
        loop {
            match listener.accept() {
                Ok((conn, _)) => return conn,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(started.elapsed() < PATIENCE, "the helper never connected");
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => panic!("accept: {err}"),
            }
        }
    };
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let first = accept(&listener);
    drop(first);
    // The helper notices, and tries again.
    let mut second = accept(&listener);
    second.set_nonblocking(false).unwrap();
    second.write_all(&Frame::set(1, "back").encode()).unwrap();
    let mut buf = [0u8; 9];
    second.set_read_timeout(Some(PATIENCE)).unwrap();
    second.read_exact(&mut buf).unwrap();
    assert_eq!(buf, Frame::ack(1).encode().as_slice());
    assert_eq!(xvfb.get().as_deref(), Some("back"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nothing_to_get_without_an_owner() {
    let Some(xvfb) = Xvfb::start() else { return };
    assert_eq!(xvfb.get(), None);
}

#[test]
fn big_text_goes_by_incr_both_ways() {
    let Some(xvfb) = Xvfb::start() else { return };
    // Everything over 1000 bytes is INCR, from us and from `--put`.
    let mut streamer = Streamer::start(&xvfb, Some(1000));
    // Several 64 KiB chunks, with multi-byte characters across the seams.
    let text: String = "aé✓🎉".repeat(30_000);
    assert!(text.len() > 3 * 64 * 1024);

    let owner = Reaped(xvfb.put(&text, Some(1000)));
    streamer.expect(Frame::copied(text.clone()));

    streamer.send(&Frame::set(1, text.clone()));
    streamer.expect(Frame::ack(1));
    assert_eq!(xvfb.get().as_deref(), Some(text.as_str()));
    drop(owner);
}

#[test]
fn text_over_the_limit_is_not_forwarded() {
    let Some(xvfb) = Xvfb::start() else { return };
    let mut streamer = Streamer::start(&xvfb, None);
    let too_big = "x".repeat(cha_proto::clipboard::MAX_TEXT + 1);
    let _owner = Reaped(xvfb.put(&too_big, None));
    streamer.expect_nothing();
    // And it keeps working afterwards.
    let _next = Reaped(xvfb.put("small", None));
    streamer.expect(Frame::copied("small"));
}

/// `xclip`, if there is one: another implementation's idea of the protocol.
fn xclip(xvfb: &Xvfb) -> Option<Command> {
    let mut command = Command::new("xclip");
    command.env("DISPLAY", &xvfb.display);
    command.arg("-version");
    command.stdout(Stdio::null()).stderr(Stdio::null());
    command.status().ok()?;
    let mut command = Command::new("xclip");
    command.env("DISPLAY", &xvfb.display);
    Some(command)
}

#[test]
fn xclip_reads_what_the_page_sets() {
    let Some(xvfb) = Xvfb::start() else { return };
    let Some(_) = xclip(&xvfb) else {
        eprintln!("skipping: no xclip");
        return;
    };
    let mut streamer = Streamer::start(&xvfb, None);
    streamer.send(&Frame::set(1, "café ✓"));
    streamer.expect(Frame::ack(1));
    let read = |target: &str| {
        let mut command = xclip(&xvfb).unwrap();
        let out = command
            .args(["-selection", "clipboard", "-o", "-t", target])
            .output()
            .unwrap();
        assert!(out.status.success(), "xclip -t {target} failed");
        out.stdout
    };
    assert_eq!(read("UTF8_STRING"), "café ✓".as_bytes());
    assert_eq!(read("text/plain;charset=utf-8"), "café ✓".as_bytes());
    // Latin-1, with `?` for what it lacks.
    assert_eq!(read("STRING"), b"caf\xe9 ?");
    let targets = String::from_utf8(read("TARGETS")).unwrap();
    assert!(
        targets.contains("UTF8_STRING") && targets.contains("STRING"),
        "{targets}"
    );
}

#[test]
fn xclip_owning_only_string_is_read_as_latin1() {
    let Some(xvfb) = Xvfb::start() else { return };
    let Some(mut xclip) = xclip(&xvfb) else {
        eprintln!("skipping: no xclip");
        return;
    };
    let mut streamer = Streamer::start(&xvfb, None);
    let mut child = xclip
        // In the foreground, so it's ours to stop.
        .args(["-selection", "clipboard", "-i", "-t", "STRING", "-verbose"])
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"caf\xe9 au lait")
        .unwrap();
    let _child = Reaped(child);
    streamer.expect(Frame::copied("café au lait"));
}

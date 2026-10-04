//! `cha-x11-clipboard`: bridges an X11 desktop's clipboard to the streamer.
//!
//! X11 apps keep their clipboard inside the X server, and the streamer (a
//! container of its own) can't reach the X display: the only thing the two
//! share is the `/run/cha` volume. So this helper runs beside the desktop, as
//! its user, and talks to the streamer over `/run/cha/clipboard`:
//!
//! - **X → page:** when an app takes CLIPBOARD, the helper reads its text and
//!   sends it up (a `copied` frame).
//! - **Page → X:** on a `set` frame the helper owns CLIPBOARD with that text,
//!   then answers with an `ack`. The streamer waits for it before it passes on
//!   the paste keys that follow, so the app finds the text.
//!
//! The frames are `cha_proto::clipboard`'s. Only CLIPBOARD and only text are
//! bridged, up to 1 MiB (PRIMARY isn't, as with Wayland apps). The streamer
//! may be absent or restart; the helper tries again every second, and exits
//! when the X connection closes.
//!
//! Environment: `DISPLAY` (the X server), `CHA_CLIPBOARD_SOCKET` (default
//! `/run/cha/clipboard`), `CHA_DEBUG=1` (a line per transfer),
//! `CHA_INCR_BYTES` (send INCR from this size, to try it out).
//!
//! For tests and ops, as an ordinary X client:
//! - `cha-x11-clipboard --get` prints the clipboard's text;
//! - `cha-x11-clipboard --put TEXT [--hold SECONDS]` owns the clipboard with
//!   TEXT (`-`: read from stdin) until another client takes it, or SECONDS pass.

/// A line on stderr (the container's log). A closed stderr isn't worth dying for.
macro_rules! log {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr().lock(),
            "cha-x11-clipboard: {}",
            format_args!($($arg)*)
        );
    }};
}

macro_rules! info {
    ($($arg:tt)*) => { log!($($arg)*) };
}

macro_rules! warn {
    ($($arg:tt)*) => { log!("warning: {}", format_args!($($arg)*)) };
}

macro_rules! debug {
    ($($arg:tt)*) => {
        if crate::debugging() {
            log!($($arg)*);
        }
    };
}

mod atoms;
mod incr;
mod link;
mod text;
mod x11;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use cha_proto::clipboard::{Frame, Kind};
use link::Link;
use x11::{Action, Engine, Result};
use x11rb::errors::{ConnectionError, ReplyError};

const DEFAULT_SOCKET: &str = "/run/cha/clipboard";

const USAGE: &str = "\
usage: cha-x11-clipboard                     bridge CLIPBOARD to the streamer
       cha-x11-clipboard --get               print the clipboard's text
       cha-x11-clipboard --put TEXT [--hold SECONDS]
                                             own the clipboard with TEXT (- for
                                             stdin) until another client takes it
environment: DISPLAY, CHA_CLIPBOARD_SOCKET, CHA_DEBUG, CHA_INCR_BYTES";

/// Whether `CHA_DEBUG` asks for a line per transfer.
fn debugging() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("CHA_DEBUG").is_ok_and(|v| !v.is_empty() && v != "0"))
}

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Bridge,
    Get,
    Put {
        text: String,
        hold: Option<Duration>,
    },
}

fn parse_args(args: impl IntoIterator<Item = String>) -> std::result::Result<Mode, String> {
    let mut args = args.into_iter();
    let mut mode = Mode::Bridge;
    let mut hold = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--get" => mode = Mode::Get,
            "--put" => {
                let text = args.next().ok_or("--put needs the text to own")?;
                mode = Mode::Put { text, hold: None };
            }
            "--hold" => {
                let secs = args.next().ok_or("--hold needs a number of seconds")?;
                let secs: f64 = secs
                    .parse()
                    .ok()
                    .filter(|s: &f64| s.is_finite() && *s >= 0.0)
                    .ok_or_else(|| format!("--hold {secs}: not a number of seconds"))?;
                hold = Some(Duration::from_secs_f64(secs));
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    match (&mut mode, hold) {
        (Mode::Put { hold: slot, .. }, Some(hold)) => *slot = Some(hold),
        (_, Some(_)) => return Err("--hold goes with --put".into()),
        _ => {}
    }
    Ok(mode)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let mode = match parse_args(args) {
        Ok(mode) => mode,
        Err(why) => {
            eprintln!("cha-x11-clipboard: {why}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let result = match mode {
        Mode::Bridge => bridge(),
        Mode::Get => get(),
        Mode::Put { text, hold } => put(text, hold),
    };
    match result {
        Ok(code) => code,
        Err(err) if x_closed(err.as_ref()) => {
            info!("the X connection closed: {err}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("cha-x11-clipboard: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Whether `err` says the X server went away (or never answered).
fn x_closed(err: &(dyn std::error::Error + 'static)) -> bool {
    err.is::<ConnectionError>()
        || matches!(
            err.downcast_ref::<ReplyError>(),
            Some(ReplyError::ConnectionError(_))
        )
}

fn incr_threshold() -> Option<usize> {
    std::env::var("CHA_INCR_BYTES").ok()?.parse().ok()
}

/// The streamer's socket, and what the X side tells it, until X goes away.
fn bridge() -> Result<ExitCode> {
    let socket = std::env::var_os("CHA_CLIPBOARD_SOCKET")
        .map_or_else(|| PathBuf::from(DEFAULT_SOCKET), PathBuf::from);
    let mut engine = Engine::connect(true, incr_threshold())?;
    info!("bridging CLIPBOARD to {}", socket.display());
    let mut link = Link::new(socket);
    // Every copy goes up, the same text again too: since the last one, the
    // device's clipboard may have changed. The page's own text can't echo
    // back, since the engine ignores the clipboard changing to our window.
    loop {
        link.maintain(Instant::now());
        for action in engine.take_actions() {
            match action {
                Action::Copied(text) => {
                    if !link.connected() {
                        debug!("copied {} bytes; no streamer to tell", text.len());
                    } else {
                        debug!("copied {} bytes", text.len());
                        link.send(&Frame::copied(&text));
                    }
                }
                Action::Acked(seq) => link.send(&Frame::ack(seq)),
                Action::Lost => debug!("another app took the clipboard"),
            }
        }
        let timeout = link.retry_in(Instant::now());
        if engine.wait(timeout, link.fd())? {
            for frame in link.read() {
                if frame.kind == Kind::Set {
                    debug!("set {} bytes (#{})", frame.text.len(), frame.seq);
                    engine.set_text(frame.seq, frame.text)?;
                } else {
                    warn!("unexpected {:?} frame from the streamer", frame.kind);
                }
            }
        }
    }
}

/// `--get`: the clipboard's text on stdout.
fn get() -> Result<ExitCode> {
    use std::io::Write;
    let mut engine = Engine::connect(false, None)?;
    engine.start_fetch()?;
    let mut text = None;
    while engine.fetching() {
        engine.wait(None, None)?;
        for action in engine.take_actions() {
            if let Action::Copied(copied) = action {
                text = Some(copied);
            }
        }
    }
    match text {
        Some(text) => {
            let mut out = std::io::stdout().lock();
            out.write_all(text.as_bytes())?;
            out.flush()?;
            Ok(ExitCode::SUCCESS)
        }
        None => {
            eprintln!("cha-x11-clipboard: the clipboard has no text");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// `--put`: owns the clipboard until another client takes it.
fn put(text: String, hold: Option<Duration>) -> Result<ExitCode> {
    let text = if text == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        text
    };
    let mut engine = Engine::connect(false, incr_threshold())?;
    let until = hold.map(|hold| Instant::now() + hold);
    let size = text.len();
    engine.set_text(0, text)?;
    loop {
        for action in engine.take_actions() {
            match action {
                Action::Acked(_) => info!("owning the clipboard ({size} bytes)"),
                Action::Lost => {
                    info!("another client took the clipboard");
                    return Ok(ExitCode::SUCCESS);
                }
                Action::Copied(_) => {}
            }
        }
        let timeout = match until {
            Some(until) if Instant::now() >= until => return Ok(ExitCode::SUCCESS),
            Some(until) => Some(until.saturating_duration_since(Instant::now())),
            None => None,
        };
        engine.wait(timeout, None)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<Mode, String> {
        parse_args(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn modes() {
        assert_eq!(parse(&[]), Ok(Mode::Bridge));
        assert_eq!(parse(&["--get"]), Ok(Mode::Get));
        assert_eq!(
            parse(&["--put", "héllo"]),
            Ok(Mode::Put {
                text: "héllo".into(),
                hold: None
            })
        );
        assert_eq!(
            parse(&["--put", "x", "--hold", "1.5"]),
            Ok(Mode::Put {
                text: "x".into(),
                hold: Some(Duration::from_millis(1500))
            })
        );
        // The text is whatever follows, even something that looks like a flag.
        assert_eq!(
            parse(&["--hold", "2", "--put", "--get"]),
            Ok(Mode::Put {
                text: "--get".into(),
                hold: Some(Duration::from_secs(2))
            })
        );
    }

    #[test]
    fn bad_arguments() {
        assert!(parse(&["--put"]).is_err());
        assert!(parse(&["--hold"]).is_err());
        assert!(parse(&["--hold", "soon", "--put", "x"]).is_err());
        assert!(parse(&["--hold", "-1", "--put", "x"]).is_err());
        assert!(parse(&["--hold", "3"]).is_err());
        assert!(parse(&["--get", "--hold", "3"]).is_err());
        assert!(parse(&["--nope"]).is_err());
    }
}

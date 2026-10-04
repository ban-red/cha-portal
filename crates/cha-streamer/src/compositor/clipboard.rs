//! The clipboard between the environment and the browser (P2.6), text only.
//!
//! - An app's new clipboard selection is read in the text type it offers that
//!   we like best, at the next tick, on a thread of its own (an app writes it
//!   at its own pace), and published on a watch channel the sessions forward
//!   to the page.
//! - Text from the page becomes the compositor's own selection, which apps
//!   read when they paste; each read is written on a thread too.
//!
//! Wayland apps take part directly. X11 apps under rootful Xwayland (XFCE)
//! keep their clipboard inside the X server.

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use smithay::input::Seat;
use smithay::wayland::selection::SelectionSource;
use smithay::wayland::selection::data_device::request_data_device_client_selection;
use tokio::sync::watch;
use tracing::{debug, warn};

use super::State;

/// Text types, best first: what we offer, and what we look for in an app's.
pub const TEXT_MIMES: &[&str] = &[
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
    "TEXT",
];
/// Larger clipboards aren't forwarded either way.
pub const MAX_BYTES: usize = 1 << 20;

/// The environment's latest clipboard text, for the sessions.
pub type ClipboardWatch = watch::Receiver<Option<Arc<str>>>;

pub struct Clipboard {
    publish: watch::Sender<Option<Arc<str>>>,
    /// Bumped per selection, so a slow read can't overwrite a newer one.
    generation: Arc<AtomicU64>,
    /// An app set the clipboard offering these types; it's read at the next
    /// tick, since Smithay stores a selection only after telling us of it.
    pending: Option<Vec<String>>,
}

impl Clipboard {
    pub fn new() -> (Self, ClipboardWatch) {
        let (publish, watch) = watch::channel(None);
        let clipboard = Self {
            publish,
            generation: Arc::default(),
            pending: None,
        };
        (clipboard, watch)
    }

    /// An app set the clipboard (or cleared it).
    pub fn changed(&mut self, source: Option<&SelectionSource>) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.pending = source.map(SelectionSource::mime_types);
    }

    /// Reads a new selection's text, if it has any, on a thread of its own.
    pub fn poll(&mut self, seat: &Seat<State>) {
        let Some(offered) = self.pending.take() else {
            return;
        };
        let Some(mime) = TEXT_MIMES.iter().find(|m| offered.iter().any(|o| o == *m)) else {
            return;
        };
        let (mut reader, writer) = match std::io::pipe() {
            Ok(pipe) => pipe,
            Err(err) => {
                warn!("clipboard: no pipe: {err}");
                return;
            }
        };
        if let Err(err) =
            request_data_device_client_selection(seat, mime.to_string(), OwnedFd::from(writer))
        {
            debug!("clipboard: the app's selection is gone: {err:?}");
            return;
        }
        let generation = self.generation.load(Ordering::SeqCst);
        let publish = self.publish.clone();
        let latest = Arc::clone(&self.generation);
        let spawned = std::thread::Builder::new()
            .name("clipboard-read".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                let read = (&mut reader)
                    .take(MAX_BYTES as u64 + 1)
                    .read_to_end(&mut bytes);
                if let Err(err) = read {
                    debug!("clipboard: reading the app's selection: {err}");
                    return;
                }
                if bytes.len() > MAX_BYTES || latest.load(Ordering::SeqCst) != generation {
                    return;
                }
                let text: Arc<str> = String::from_utf8_lossy(&bytes).into();
                publish.send_replace(Some(text));
            });
        if let Err(err) = spawned {
            warn!("clipboard: couldn't start a reader: {err}");
        }
    }
}

/// An app pastes our selection: write `text` into its pipe.
pub fn write(text: Arc<str>, fd: OwnedFd) {
    let spawned = std::thread::Builder::new()
        .name("clipboard-write".into())
        .spawn(move || {
            let mut pipe = std::fs::File::from(fd);
            if let Err(err) = pipe.write_all(text.as_bytes()) {
                debug!("clipboard: writing to the app: {err}");
            }
        });
    if let Err(err) = spawned {
        warn!("clipboard: couldn't start a writer: {err}");
    }
}

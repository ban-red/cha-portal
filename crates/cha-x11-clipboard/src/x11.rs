//! The X side: one invisible window that watches CLIPBOARD for new owners,
//! reads what they copied, and owns CLIPBOARD when the page sets it.
//!
//! One thread drives it: [`Engine::wait`] drains X's events, then sleeps in
//! `poll(2)` until X (or a descriptor the caller adds) has something, or a
//! deadline passes. Each transfer is a small state machine advanced by
//! events, so a slow owner or requestor delays nothing else.
//!
//! - **Reading** (an owner changed): ask for `TARGETS`, pick UTF-8 or Latin-1
//!   text from it, convert to that, and read the property. An owner that sends
//!   INCR is read chunk by chunk. At most one conversion runs; owners that
//!   change meanwhile are read after it. An owner that goes quiet for 2 s is
//!   given up on.
//! - **Owning** (the page set text): learn the server's time from a property
//!   change on our window, take the selection with it, check we got it, and
//!   only then say so ([`Action::Acked`]). Requestors get `TARGETS`,
//!   `TIMESTAMP`, and the text as UTF-8 or Latin-1; anything else is refused.
//!   Text over the server's request limit goes by INCR in 64 KiB chunks.

use std::os::fd::BorrowedFd;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cha_proto::clipboard::MAX_TEXT;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask, PropMode,
    Property, PropertyNotifyEvent, SELECTION_NOTIFY_EVENT, SelectionClearEvent,
    SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

use crate::atoms::Atoms;
use crate::incr::{Incoming, Outgoing, Progress};
use crate::text::{
    Answer, Source, answer_for, before, latin1_to_utf8, offered, pick_source, utf8_to_latin1,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// How long an owner has to answer a conversion, and to send each INCR chunk.
const OWNER_PATIENCE: Duration = Duration::from_secs(2);
/// How long a requestor has to take each INCR chunk we write.
const REQUESTOR_PATIENCE: Duration = Duration::from_secs(5);
/// How long the server has to report its time.
const STAMP_PATIENCE: Duration = Duration::from_millis(500);
/// `get_property` length in 32-bit words: a whole text and a little more.
const MAX_WORDS: u32 = (MAX_TEXT / 4 + 1) as u32;

/// What the engine asks of its caller.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// An X app copied this text (never empty).
    Copied(String),
    /// We own the clipboard with the text of `set_text`'s `seq`.
    Acked(u32),
    /// Someone else took the clipboard from us.
    Lost,
}

/// What we are asking the selection's owner for.
enum Want {
    Targets,
    /// This text type, or if it's refused, that one.
    Text {
        source: Source,
        fallback: Option<Source>,
    },
}

enum Fetch {
    Idle,
    /// A conversion request is out.
    Waiting {
        want: Want,
        deadline: Instant,
    },
    /// The owner sends INCR chunks.
    Receiving {
        source: Source,
        incoming: Incoming,
        deadline: Instant,
    },
}

/// The text we own CLIPBOARD with.
struct Owned {
    text: Arc<str>,
    /// When we took it (0: unknown).
    time: u32,
}

/// Text to own, waiting for the server's time.
struct PendingSet {
    seqs: Vec<u32>,
    text: Arc<str>,
    deadline: Instant,
}

/// Text going to a requestor in INCR chunks.
struct IncrSend {
    requestor: Window,
    property: u32,
    type_: u32,
    outgoing: Outgoing,
    deadline: Instant,
}

pub struct Engine {
    conn: RustConnection,
    window: Window,
    atoms: Atoms,
    /// Text longer than this goes by INCR.
    incr_threshold: usize,
    fetch: Fetch,
    /// The owner changed during a fetch: read again after it.
    refetch: bool,
    owned: Option<Owned>,
    pending_set: Option<PendingSet>,
    sends: Vec<IncrSend>,
    actions: Vec<Action>,
}

impl Engine {
    /// Connects to `$DISPLAY`. With `watch`, new owners are read as they
    /// appear; otherwise only [`start_fetch`](Self::start_fetch) reads.
    /// `incr_threshold` lowers the size from which we send INCR (the server's
    /// request limit otherwise).
    pub fn connect(watch: bool, incr_threshold: Option<usize>) -> Result<Self> {
        let (conn, screen) = RustConnection::connect(None)?;
        let setup = conn.setup();
        let screen = &setup.roots[screen];
        let (root, depth, visual) = (screen.root, screen.root_depth, screen.root_visual);
        let atoms = Atoms::intern(&conn)?;
        let window = conn.generate_id()?;
        conn.create_window(
            depth,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            visual,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        if watch {
            xfixes::query_version(&conn, 5, 0)?.reply()?;
            conn.xfixes_select_selection_input(
                window,
                atoms.clipboard,
                xfixes::SelectionEventMask::SET_SELECTION_OWNER,
            )?;
        }
        conn.flush()?;
        // A header's worth is left for the request around the data.
        let request_limit = conn.maximum_request_bytes().saturating_sub(64);
        Ok(Self {
            conn,
            window,
            atoms,
            incr_threshold: incr_threshold.map_or(request_limit, |t| t.min(request_limit)),
            fetch: Fetch::Idle,
            refetch: false,
            owned: None,
            pending_set: None,
            sends: Vec::new(),
            actions: Vec::new(),
        })
    }

    pub fn take_actions(&mut self) -> Vec<Action> {
        std::mem::take(&mut self.actions)
    }

    pub fn fetching(&self) -> bool {
        !matches!(self.fetch, Fetch::Idle)
    }

    /// Sleeps until X has events, `extra` is readable (true), `timeout`
    /// passes or one of the engine's own deadlines does; handles the events
    /// and deadlines. Never sleeps while events are waiting: call again.
    pub fn wait(
        &mut self,
        timeout: Option<Duration>,
        extra: Option<BorrowedFd<'_>>,
    ) -> Result<bool> {
        // Everything buffered is handled first, or it'd sit until X next speaks.
        let handled = self.drain_events()?;
        self.conn.flush()?;
        let now = Instant::now();
        let until = [self.next_deadline(), timeout.map(|t| now + t)]
            .into_iter()
            .flatten()
            .min();
        let timeout = if handled > 0 {
            Some(Duration::ZERO)
        } else {
            until.map(|at| at.saturating_duration_since(now))
        };
        let timespec = timeout.map(|t| Timespec {
            tv_sec: t.as_secs() as _,
            tv_nsec: t.subsec_nanos() as _,
        });
        let mut fds = vec![PollFd::new(self.conn.stream(), PollFlags::IN)];
        if let Some(fd) = &extra {
            fds.push(PollFd::new(fd, PollFlags::IN));
        }
        match poll(&mut fds, timespec.as_ref()) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(err) => return Err(err.into()),
        }
        let readable = fds.get(1).is_some_and(|fd| {
            fd.revents()
                .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
        });
        self.tick(Instant::now())?;
        Ok(readable)
    }

    fn drain_events(&mut self) -> Result<usize> {
        let mut handled = 0;
        while let Some(event) = self.conn.poll_for_event()? {
            handled += 1;
            self.handle(event)?;
        }
        Ok(handled)
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::XfixesSelectionNotify(ev) if ev.selection == self.atoms.clipboard => {
                // Not our own taking it, nor its being cleared.
                if ev.owner != self.window && ev.owner != NONE {
                    self.start_fetch()?;
                }
            }
            Event::SelectionNotify(ev)
                if ev.requestor == self.window && ev.selection == self.atoms.clipboard =>
            {
                self.on_selection_notify(ev.property)?;
            }
            Event::PropertyNotify(ev) => self.on_property(ev)?,
            Event::SelectionRequest(ev) => self.on_request(ev)?,
            Event::SelectionClear(ev) => self.on_clear(ev),
            Event::Error(err) => debug!("X error: {err:?}"),
            _ => {}
        }
        Ok(())
    }

    fn next_deadline(&self) -> Option<Instant> {
        let fetch = match &self.fetch {
            Fetch::Idle => None,
            Fetch::Waiting { deadline, .. } | Fetch::Receiving { deadline, .. } => Some(*deadline),
        };
        fetch
            .into_iter()
            .chain(self.pending_set.as_ref().map(|p| p.deadline))
            .chain(self.sends.iter().map(|s| s.deadline))
            .min()
    }

    fn tick(&mut self, now: Instant) -> Result<()> {
        let late = match &self.fetch {
            Fetch::Idle => false,
            Fetch::Waiting { deadline, .. } | Fetch::Receiving { deadline, .. } => now >= *deadline,
        };
        if late {
            debug!("the owner didn't answer in time");
            self.conn
                .delete_property(self.window, self.atoms.transfer)?;
            self.end_fetch()?;
        }
        if self.pending_set.as_ref().is_some_and(|p| now >= p.deadline) {
            debug!("the server didn't report its time; taking the clipboard without it");
            if let Some(pending) = self.pending_set.take() {
                self.become_owner(pending, CURRENT_TIME)?;
            }
        }
        let (late, live): (Vec<_>, Vec<_>) = std::mem::take(&mut self.sends)
            .into_iter()
            .partition(|s| now >= s.deadline);
        self.sends = live;
        for send in late {
            debug!("window {:#x} stopped taking INCR chunks", send.requestor);
            self.stop_watching(send.requestor)?;
        }
        Ok(())
    }

    // ---- Reading what X apps copied ----

    /// Reads the clipboard's text, as an `Action::Copied` if it has some.
    pub fn start_fetch(&mut self) -> Result<()> {
        if self.fetching() {
            self.refetch = true;
            return Ok(());
        }
        self.conn
            .delete_property(self.window, self.atoms.transfer)?;
        self.conn.convert_selection(
            self.window,
            self.atoms.clipboard,
            self.atoms.targets,
            self.atoms.transfer,
            CURRENT_TIME,
        )?;
        self.fetch = Fetch::Waiting {
            want: Want::Targets,
            deadline: Instant::now() + OWNER_PATIENCE,
        };
        Ok(())
    }

    fn convert(&mut self, source: Source, fallback: Option<Source>) -> Result<()> {
        self.conn.convert_selection(
            self.window,
            self.atoms.clipboard,
            source.target,
            self.atoms.transfer,
            CURRENT_TIME,
        )?;
        self.fetch = Fetch::Waiting {
            want: Want::Text { source, fallback },
            deadline: Instant::now() + OWNER_PATIENCE,
        };
        Ok(())
    }

    fn end_fetch(&mut self) -> Result<()> {
        self.fetch = Fetch::Idle;
        if std::mem::take(&mut self.refetch) {
            self.start_fetch()?;
        }
        Ok(())
    }

    /// The owner answered (`property` is `None` if it refused).
    fn on_selection_notify(&mut self, property: u32) -> Result<()> {
        let atoms = self.atoms;
        let want = match std::mem::replace(&mut self.fetch, Fetch::Idle) {
            Fetch::Waiting { want, .. } => want,
            other => {
                // An answer to a request we gave up on.
                self.fetch = other;
                return Ok(());
            }
        };
        if property == NONE {
            return match want {
                // Not every owner knows TARGETS: just try the text types.
                Want::Targets => self.convert(Source::utf8(&atoms), Some(Source::latin1(&atoms))),
                Want::Text {
                    fallback: Some(next),
                    ..
                } => self.convert(next, None),
                Want::Text { fallback: None, .. } => self.end_fetch(),
            };
        }
        let reply = self
            .conn
            .get_property(
                true,
                self.window,
                atoms.transfer,
                AtomEnum::ANY,
                0,
                MAX_WORDS,
            )?
            .reply()?;
        match want {
            Want::Targets => {
                let list: Vec<u32> = reply.value32().map(Iterator::collect).unwrap_or_default();
                if list.is_empty() {
                    return self.convert(Source::utf8(&atoms), Some(Source::latin1(&atoms)));
                }
                match pick_source(&list, &atoms) {
                    Some(source) => self.convert(source, None),
                    None => {
                        debug!("the clipboard holds no text ({} targets)", list.len());
                        self.end_fetch()
                    }
                }
            }
            Want::Text { source, .. } => {
                if reply.type_ == atoms.incr {
                    // Reading it deleted the property, which starts the transfer.
                    self.fetch = Fetch::Receiving {
                        source,
                        incoming: Incoming::new(MAX_TEXT),
                        deadline: Instant::now() + OWNER_PATIENCE,
                    };
                    Ok(())
                } else if reply.bytes_after > 0 || reply.value.len() > MAX_TEXT {
                    debug!("the clipboard's text is over {MAX_TEXT} bytes; not forwarded");
                    self.conn.delete_property(self.window, atoms.transfer)?;
                    self.end_fetch()
                } else {
                    self.finish(source, reply.value)
                }
            }
        }
    }

    /// An INCR chunk was written to our property.
    fn on_chunk(&mut self) -> Result<()> {
        // Every answer first changes the property, INCR or not: only an INCR
        // transfer in progress wants the chunk.
        let (source, mut incoming) = match std::mem::replace(&mut self.fetch, Fetch::Idle) {
            Fetch::Receiving {
                source, incoming, ..
            } => (source, incoming),
            other => {
                self.fetch = other;
                return Ok(());
            }
        };
        let reply = self
            .conn
            .get_property(
                true,
                self.window,
                self.atoms.transfer,
                AtomEnum::ANY,
                0,
                MAX_WORDS,
            )?
            .reply()?;
        let progress = if reply.bytes_after > 0 {
            Progress::TooBig
        } else {
            incoming.push(&reply.value)
        };
        match progress {
            Progress::More => {
                self.fetch = Fetch::Receiving {
                    source,
                    incoming,
                    deadline: Instant::now() + OWNER_PATIENCE,
                };
                Ok(())
            }
            Progress::Done => self.finish(source, incoming.into_data()),
            Progress::TooBig => {
                debug!("the clipboard's text is over {MAX_TEXT} bytes; not forwarded");
                self.conn
                    .delete_property(self.window, self.atoms.transfer)?;
                self.end_fetch()
            }
        }
    }

    fn finish(&mut self, source: Source, bytes: Vec<u8>) -> Result<()> {
        let text = if source.latin1 {
            latin1_to_utf8(&bytes)
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        };
        self.end_fetch()?;
        if !text.is_empty() {
            self.actions.push(Action::Copied(text));
        }
        Ok(())
    }

    // ---- Owning the clipboard for the page ----

    /// Takes CLIPBOARD with `text`; `Action::Acked(seq)` follows once we own it.
    pub fn set_text(&mut self, seq: u32, text: String) -> Result<()> {
        let text: Arc<str> = text.into();
        if let Some(pending) = &mut self.pending_set {
            pending.seqs.push(seq);
            pending.text = text;
            return Ok(());
        }
        // Even an empty write to a property is announced, with the server's time.
        self.conn.change_property8(
            PropMode::APPEND,
            self.window,
            self.atoms.stamp,
            AtomEnum::STRING,
            &[],
        )?;
        self.pending_set = Some(PendingSet {
            seqs: vec![seq],
            text,
            deadline: Instant::now() + STAMP_PATIENCE,
        });
        Ok(())
    }

    fn become_owner(&mut self, pending: PendingSet, time: u32) -> Result<()> {
        let clipboard = self.atoms.clipboard;
        self.conn
            .set_selection_owner(self.window, clipboard, time)?;
        // The reply comes after the server handled the request, and says whether it took.
        let owner = self.conn.get_selection_owner(clipboard)?.reply()?.owner;
        if owner != self.window {
            warn!("couldn't take the clipboard (window {owner:#x} still owns it)");
            return Ok(());
        }
        debug!("owning the clipboard: {} bytes", pending.text.len());
        self.owned = Some(Owned {
            text: pending.text,
            time,
        });
        self.actions
            .extend(pending.seqs.into_iter().map(Action::Acked));
        Ok(())
    }

    fn on_clear(&mut self, ev: SelectionClearEvent) {
        if ev.selection != self.atoms.clipboard {
            return;
        }
        // A clear that's older than our taking it is about an earlier ownership.
        if self
            .owned
            .as_ref()
            .is_some_and(|o| !before(ev.time, o.time))
        {
            self.owned = None;
            self.actions.push(Action::Lost);
        }
    }

    /// Someone wants our selection converted.
    fn on_request(&mut self, ev: SelectionRequestEvent) -> Result<()> {
        // Clients from before ICCCM name no property: they mean the target's.
        let property = if ev.property == NONE {
            ev.target
        } else {
            ev.property
        };
        // ICCCM says to refuse a request dated before we took the selection.
        // We don't: its time may come from an input event that Xwayland
        // stamped, and a request that old still just wants this text.
        let owned = self
            .owned
            .as_ref()
            .filter(|_| ev.selection == self.atoms.clipboard && ev.owner == self.window);
        let granted = match owned {
            Some(owned) => {
                let (text, time) = (Arc::clone(&owned.text), owned.time);
                self.answer(&ev, property, &text, time)?
            }
            None => false,
        };
        let notify = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: ev.time,
            requestor: ev.requestor,
            selection: ev.selection,
            target: ev.target,
            property: if granted { property } else { NONE },
        };
        self.conn
            .send_event(false, ev.requestor, EventMask::NO_EVENT, notify)?;
        Ok(())
    }

    /// Writes the answer to `ev` on the requestor's `property`; false if refused.
    fn answer(
        &mut self,
        ev: &SelectionRequestEvent,
        property: u32,
        text: &str,
        time: u32,
    ) -> Result<bool> {
        let atoms = self.atoms;
        match answer_for(ev.target, &atoms) {
            Answer::Refuse => {
                debug!("refusing target {:#x} for {:#x}", ev.target, ev.requestor);
                Ok(false)
            }
            Answer::Targets => {
                self.conn.change_property32(
                    PropMode::REPLACE,
                    ev.requestor,
                    property,
                    atoms.atom,
                    &offered(&atoms),
                )?;
                Ok(true)
            }
            // We don't know ours if the server didn't tell us.
            Answer::Timestamp if time == 0 => Ok(false),
            Answer::Timestamp => {
                self.conn.change_property32(
                    PropMode::REPLACE,
                    ev.requestor,
                    property,
                    atoms.integer,
                    &[time],
                )?;
                Ok(true)
            }
            Answer::Text { type_, latin1 } => {
                let data = if latin1 {
                    utf8_to_latin1(text)
                } else {
                    text.as_bytes().to_vec()
                };
                debug!("{} bytes for {:#x}", data.len(), ev.requestor);
                if data.len() > self.incr_threshold {
                    self.start_incr(ev.requestor, property, type_, data)?;
                } else {
                    self.conn.change_property8(
                        PropMode::REPLACE,
                        ev.requestor,
                        property,
                        type_,
                        &data,
                    )?;
                }
                Ok(true)
            }
        }
    }

    fn start_incr(
        &mut self,
        requestor: Window,
        property: u32,
        type_: u32,
        data: Vec<u8>,
    ) -> Result<()> {
        // Its deletes of the property are how it asks for the next chunk.
        self.conn.change_window_attributes(
            requestor,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        self.conn.change_property32(
            PropMode::REPLACE,
            requestor,
            property,
            self.atoms.incr,
            &[u32::try_from(data.len()).unwrap_or(u32::MAX)],
        )?;
        self.sends
            .retain(|s| !(s.requestor == requestor && s.property == property));
        self.sends.push(IncrSend {
            requestor,
            property,
            type_,
            outgoing: Outgoing::new(data),
            deadline: Instant::now() + REQUESTOR_PATIENCE,
        });
        Ok(())
    }

    fn stop_watching(&self, window: Window) -> Result<()> {
        self.conn.change_window_attributes(
            window,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
        )?;
        Ok(())
    }

    // ---- Property events ----

    fn on_property(&mut self, ev: PropertyNotifyEvent) -> Result<()> {
        if ev.window == self.window && ev.state == Property::NEW_VALUE {
            if ev.atom == self.atoms.transfer {
                return self.on_chunk();
            }
            if ev.atom == self.atoms.stamp
                && let Some(pending) = self.pending_set.take()
            {
                return self.become_owner(pending, ev.time);
            }
        }
        if ev.state == Property::DELETE {
            return self.on_delete(ev.window, ev.atom);
        }
        Ok(())
    }

    /// A requestor took the chunk we wrote: write the next.
    fn on_delete(&mut self, window: Window, property: u32) -> Result<()> {
        let Some(i) = self
            .sends
            .iter()
            .position(|s| s.requestor == window && s.property == property)
        else {
            return Ok(());
        };
        let send = &mut self.sends[i];
        match send.outgoing.next_chunk() {
            Some(chunk) => {
                self.conn.change_property8(
                    PropMode::REPLACE,
                    window,
                    property,
                    send.type_,
                    chunk,
                )?;
                send.deadline = Instant::now() + REQUESTOR_PATIENCE;
            }
            None => {
                debug!("sent {} bytes to {window:#x} by INCR", send.outgoing.len());
                self.sends.swap_remove(i);
                self.stop_watching(window)?;
            }
        }
        Ok(())
    }
}

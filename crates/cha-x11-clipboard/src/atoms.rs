//! The atoms we speak.

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as _};

#[derive(Clone, Copy, Debug)]
pub struct Atoms {
    pub clipboard: Atom,
    pub targets: Atom,
    pub timestamp: Atom,
    pub utf8_string: Atom,
    pub text: Atom,
    /// `text/plain;charset=utf-8`
    pub mime_utf8: Atom,
    /// `text/plain`
    pub mime_plain: Atom,
    pub incr: Atom,
    /// Where owners put what we asked them to convert.
    pub transfer: Atom,
    /// What we touch to learn the server's time.
    pub stamp: Atom,
    // Predefined by the protocol.
    pub string: Atom,
    pub atom: Atom,
    pub integer: Atom,
}

impl Atoms {
    pub fn intern<C: Connection>(conn: &C) -> Result<Self, ReplyError> {
        let names: [&[u8]; 10] = [
            b"CLIPBOARD",
            b"TARGETS",
            b"TIMESTAMP",
            b"UTF8_STRING",
            b"TEXT",
            b"text/plain;charset=utf-8",
            b"text/plain",
            b"INCR",
            b"CHA_CLIPBOARD",
            b"CHA_TIMESTAMP",
        ];
        // All requests out before the first reply: one round trip.
        let cookies = names
            .iter()
            .map(|name| conn.intern_atom(false, name))
            .collect::<Result<Vec<_>, _>>()?;
        let mut ids = Vec::with_capacity(names.len());
        for cookie in cookies {
            ids.push(cookie.reply()?.atom);
        }
        Ok(Self {
            clipboard: ids[0],
            targets: ids[1],
            timestamp: ids[2],
            utf8_string: ids[3],
            text: ids[4],
            mime_utf8: ids[5],
            mime_plain: ids[6],
            incr: ids[7],
            transfer: ids[8],
            stamp: ids[9],
            string: AtomEnum::STRING.into(),
            atom: AtomEnum::ATOM.into(),
            integer: AtomEnum::INTEGER.into(),
        })
    }

    /// Distinct made-up atoms, for tests of what needs no server.
    #[cfg(test)]
    pub fn numbered() -> Self {
        Self {
            clipboard: 100,
            targets: 101,
            timestamp: 102,
            utf8_string: 103,
            text: 104,
            mime_utf8: 105,
            mime_plain: 106,
            incr: 107,
            transfer: 108,
            stamp: 109,
            string: 31,
            atom: 4,
            integer: 19,
        }
    }
}

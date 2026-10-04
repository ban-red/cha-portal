//! The clipboard bridge's frames: how `cha-x11-clipboard`, in an X11
//! desktop's app container, talks to the streamer over `/run/cha/clipboard`
//! (a Unix stream socket the streamer listens on).
//!
//! ```text
//!  0      1..5        5..9        9..
//! +------+-----------+-----------+------------------+
//! | kind | seq (LE)  | len (LE)  | len bytes, UTF-8 |
//! +------+-----------+-----------+------------------+
//! ```
//!
//! - [`Kind::Copied`] (helper → streamer): an X11 app copied this text.
//!   `seq` is unused (0).
//! - [`Kind::Set`] (streamer → helper): own the clipboard with this text.
//!   `seq` names the request.
//! - [`Kind::Ack`] (helper → streamer): the helper owns the clipboard as
//!   `seq` asked. No text.
//!
//! This is local plumbing between our own processes, not `cha-stream/1`.

/// Bytes before the text.
pub const HEADER_LEN: usize = 9;

/// The longest text either side forwards; a longer frame is an error.
pub const MAX_TEXT: usize = 1 << 20;

/// What a frame says. Byte 0 on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `b'c'`: an app copied this text.
    Copied,
    /// `b's'`: own the clipboard with this text.
    Set,
    /// `b'a'`: owning `seq`'s text.
    Ack,
}

impl Kind {
    pub const fn byte(self) -> u8 {
        match self {
            Kind::Copied => b'c',
            Kind::Set => b's',
            Kind::Ack => b'a',
        }
    }

    pub const fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'c' => Some(Kind::Copied),
            b's' => Some(Kind::Set),
            b'a' => Some(Kind::Ack),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub kind: Kind,
    pub seq: u32,
    pub text: String,
}

impl Frame {
    pub fn copied(text: impl Into<String>) -> Self {
        Self {
            kind: Kind::Copied,
            seq: 0,
            text: text.into(),
        }
    }

    pub fn set(seq: u32, text: impl Into<String>) -> Self {
        Self {
            kind: Kind::Set,
            seq,
            text: text.into(),
        }
    }

    pub fn ack(seq: u32) -> Self {
        Self {
            kind: Kind::Ack,
            seq,
            text: String::new(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        encode(self.kind, self.seq, &self.text)
    }
}

/// A frame's bytes, for a sender that holds the text as a `&str` already.
pub fn encode(kind: Kind, seq: u32, text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN + text.len());
    out.push(kind.byte());
    out.extend_from_slice(&seq.to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
    out
}

/// Why a stream can't be read on. It is out of step: drop the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    UnknownKind(u8),
    /// The frame announces more than [`MAX_TEXT`] bytes.
    TooLong(usize),
    NotUtf8,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::UnknownKind(byte) => write!(f, "unknown frame kind {byte:#04x}"),
            FrameError::TooLong(len) => {
                write!(f, "a frame of {len} bytes (the limit is {MAX_TEXT})")
            }
            FrameError::NotUtf8 => f.write_str("a frame's text isn't UTF-8"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Cuts a byte stream into frames: [`push`](Self::push) what was read,
/// [`pop`](Self::pop) until it returns `None`.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole frame, or `None` if more bytes are needed.
    pub fn pop(&mut self) -> Result<Option<Frame>, FrameError> {
        let Some(header) = self.buf.first_chunk::<HEADER_LEN>() else {
            return Ok(None);
        };
        let kind = Kind::from_byte(header[0]).ok_or(FrameError::UnknownKind(header[0]))?;
        let seq = u32::from_le_bytes(header[1..5].try_into().expect("4 bytes"));
        let len = u32::from_le_bytes(header[5..9].try_into().expect("4 bytes")) as usize;
        if len > MAX_TEXT {
            return Err(FrameError::TooLong(len));
        }
        let end = HEADER_LEN + len;
        if self.buf.len() < end {
            return Ok(None);
        }
        let text = std::str::from_utf8(&self.buf[HEADER_LEN..end])
            .map_err(|_| FrameError::NotUtf8)?
            .to_owned();
        self.buf.drain(..end);
        Ok(Some(Frame { kind, seq, text }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(bytes: &[u8]) -> Vec<Frame> {
        let mut decoder = FrameDecoder::default();
        decoder.push(bytes);
        std::iter::from_fn(|| decoder.pop().unwrap()).collect()
    }

    #[test]
    fn layout() {
        assert_eq!(
            Frame::set(0x0102_0304, "hé").encode(),
            [b's', 4, 3, 2, 1, 3, 0, 0, 0, b'h', 0xc3, 0xa9]
        );
        assert_eq!(Frame::ack(7).encode(), [b'a', 7, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(Frame::copied("").encode(), [b'c', 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn round_trip() {
        let frames = vec![
            Frame::copied("héllo wörld ✓ 🎉"),
            Frame::set(u32::MAX, "paste me"),
            Frame::ack(u32::MAX),
            Frame::copied(""),
        ];
        let bytes: Vec<u8> = frames.iter().flat_map(Frame::encode).collect();
        assert_eq!(decode_all(&bytes), frames);
    }

    #[test]
    fn partial_reads() {
        let frames = [Frame::set(1, "first ✓"), Frame::copied("second")];
        let bytes: Vec<u8> = frames.iter().flat_map(Frame::encode).collect();
        let mut decoder = FrameDecoder::default();
        let mut got = Vec::new();
        // One byte at a time: nothing comes out before its last byte.
        for (i, byte) in bytes.iter().enumerate() {
            decoder.push(&[*byte]);
            while let Some(frame) = decoder.pop().unwrap() {
                got.push((i, frame));
            }
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (frames[0].encode().len() - 1, frames[0].clone()));
        assert_eq!(got[1], (bytes.len() - 1, frames[1].clone()));
    }

    #[test]
    fn longest_text() {
        let text = "x".repeat(MAX_TEXT);
        let bytes = Frame::copied(text.clone()).encode();
        assert_eq!(decode_all(&bytes)[0].text, text);
    }

    #[test]
    fn rejects_what_is_out_of_step() {
        let mut bad_kind = FrameDecoder::default();
        bad_kind.push(&[b'x', 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(bad_kind.pop(), Err(FrameError::UnknownKind(b'x')));

        // Refused on the header: no waiting for megabytes that won't come.
        let mut too_long = FrameDecoder::default();
        let mut header = vec![b'c', 0, 0, 0, 0];
        header.extend_from_slice(&(MAX_TEXT as u32 + 1).to_le_bytes());
        too_long.push(&header);
        assert_eq!(too_long.pop(), Err(FrameError::TooLong(MAX_TEXT + 1)));

        let mut not_utf8 = FrameDecoder::default();
        not_utf8.push(&[b'c', 0, 0, 0, 0, 2, 0, 0, 0, 0xff, 0xfe]);
        assert_eq!(not_utf8.pop(), Err(FrameError::NotUtf8));
    }
}

//! INCR: a selection too big for one property moves in chunks, the receiver
//! deleting the property after each (ICCCM §2.7.2).

/// The chunk size we send: small enough for any server's requests.
pub const CHUNK: usize = 64 * 1024;

/// The data we are sending, a chunk per [`next_chunk`](Self::next_chunk).
#[derive(Debug)]
pub struct Outgoing {
    data: Vec<u8>,
    offset: usize,
    ended: bool,
}

impl Outgoing {
    pub fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            offset: 0,
            ended: false,
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// The next chunk to write: all the data in pieces of at most
    /// [`CHUNK`], then an empty one that ends the transfer, then `None`.
    pub fn next_chunk(&mut self) -> Option<&[u8]> {
        if self.ended {
            return None;
        }
        let end = (self.offset + CHUNK).min(self.data.len());
        let chunk = &self.data[self.offset..end];
        self.ended = chunk.is_empty();
        self.offset = end;
        Some(chunk)
    }
}

/// The data we are receiving, under a size limit.
#[derive(Debug)]
pub struct Incoming {
    data: Vec<u8>,
    limit: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    More,
    /// The empty chunk came: that was all of it.
    Done,
    /// More than the limit: give up.
    TooBig,
}

impl Incoming {
    pub fn new(limit: usize) -> Self {
        Self {
            data: Vec::new(),
            limit,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) -> Progress {
        if chunk.is_empty() {
            return Progress::Done;
        }
        if self.data.len() + chunk.len() > self.limit {
            return Progress::TooBig;
        }
        self.data.extend_from_slice(chunk);
        Progress::More
    }

    pub fn into_data(self) -> Vec<u8> {
        self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(out: &mut Outgoing) -> Vec<Vec<u8>> {
        std::iter::from_fn(|| out.next_chunk().map(<[u8]>::to_vec)).collect()
    }

    #[test]
    fn chunks_end_with_an_empty_one() {
        let data: Vec<u8> = (0..CHUNK * 2 + 10).map(|i| i as u8).collect();
        let chunks = drain(&mut Outgoing::new(data.clone()));
        let sizes: Vec<usize> = chunks.iter().map(Vec::len).collect();
        assert_eq!(sizes, [CHUNK, CHUNK, 10, 0]);
        assert_eq!(chunks.concat(), data);
    }

    #[test]
    fn a_whole_number_of_chunks_still_ends_empty() {
        let sizes: Vec<usize> = drain(&mut Outgoing::new(vec![1; CHUNK]))
            .iter()
            .map(Vec::len)
            .collect();
        assert_eq!(sizes, [CHUNK, 0]);
    }

    #[test]
    fn nothing_is_one_empty_chunk() {
        assert_eq!(drain(&mut Outgoing::new(Vec::new())), [Vec::<u8>::new()]);
    }

    #[test]
    fn receiving() {
        let mut incoming = Incoming::new(10);
        assert_eq!(incoming.push(b"hello"), Progress::More);
        assert_eq!(incoming.push(b" wor"), Progress::More);
        assert_eq!(incoming.push(b""), Progress::Done);
        assert_eq!(incoming.into_data(), b"hello wor");
    }

    #[test]
    fn receiving_stops_at_the_limit() {
        let mut incoming = Incoming::new(10);
        assert_eq!(incoming.push(&[0; 10]), Progress::More);
        assert_eq!(incoming.push(&[0]), Progress::TooBig);
    }
}

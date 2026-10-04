use std::collections::VecDeque;

use crate::header::{DatagramHeader, Flags, HEADER_LEN};

/// Splits frames into header-prefixed datagrams of at most `max_datagram` bytes.
pub struct Fragmenter {
    max_datagram: usize,
    scratch: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTooLarge {
    pub frame_len: usize,
    pub max_len: usize,
}

impl Fragmenter {
    /// `max_datagram` includes the header and must leave room for payload.
    pub fn new(max_datagram: usize) -> Self {
        assert!(
            max_datagram > HEADER_LEN,
            "datagram size must exceed the header"
        );
        Self {
            max_datagram,
            scratch: Vec::with_capacity(max_datagram),
        }
    }

    pub fn payload_capacity(&self) -> usize {
        self.max_datagram - HEADER_LEN
    }

    /// Number of datagrams a frame of `frame_len` bytes needs (at least one).
    pub fn fragment_count(&self, frame_len: usize) -> usize {
        frame_len.div_ceil(self.payload_capacity()).max(1)
    }

    /// Emits every datagram of `frame` in order. `base.frag_index` and
    /// `base.frag_count` are overwritten; everything else is copied as-is.
    /// Returns the number of datagrams emitted.
    pub fn fragment(
        &mut self,
        base: DatagramHeader,
        frame: &[u8],
        mut emit: impl FnMut(&[u8]),
    ) -> Result<u16, FrameTooLarge> {
        let count = self.fragment_count(frame.len());
        let count: u16 = count.try_into().map_err(|_| FrameTooLarge {
            frame_len: frame.len(),
            max_len: self.payload_capacity() * u16::MAX as usize,
        })?;
        let cap = self.payload_capacity();
        let mut head = [0u8; HEADER_LEN];
        for index in 0..count {
            let start = index as usize * cap;
            let end = (start + cap).min(frame.len());
            let header = DatagramHeader {
                frag_index: index,
                frag_count: count,
                ..base
            };
            header.encode(&mut head);
            self.scratch.clear();
            self.scratch.extend_from_slice(&head);
            self.scratch.extend_from_slice(&frame[start..end]);
            emit(&self.scratch);
        }
        Ok(count)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReassemblerConfig {
    /// Frames still missing fragments after this long (since their first
    /// fragment arrived) are dropped.
    pub frame_timeout_us: u64,
    /// Upper bound on partially received frames; the oldest is dropped when
    /// a new frame would exceed it.
    pub max_frames_in_flight: usize,
}

impl Default for ReassemblerConfig {
    fn default() -> Self {
        Self {
            frame_timeout_us: 100_000,
            max_frames_in_flight: 8,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedFrame {
    pub stream: u8,
    pub frame_id: u32,
    pub flags: Flags,
    pub data: Vec<u8>,
    pub frag_count: u16,
    /// Sender timestamp of the first fragment that arrived.
    pub send_ts_us: u32,
    pub first_arrival_us: u64,
    pub last_arrival_us: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    Timeout,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DroppedFrame {
    pub stream: u8,
    pub frame_id: u32,
    pub frag_count: u16,
    pub received: u16,
    pub reason: DropReason,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReassembleEvent {
    Completed(CompletedFrame),
    Dropped(DroppedFrame),
}

struct Partial {
    stream: u8,
    frame_id: u32,
    flags: Flags,
    frag_count: u16,
    received: u16,
    parts: Vec<Option<Box<[u8]>>>,
    send_ts_us: u32,
    first_arrival_us: u64,
    last_arrival_us: u64,
}

impl Partial {
    fn dropped(&self, reason: DropReason) -> DroppedFrame {
        DroppedFrame {
            stream: self.stream,
            frame_id: self.frame_id,
            frag_count: self.frag_count,
            received: self.received,
            reason,
        }
    }
}

/// Rebuilds frames from datagrams that may arrive out of order, duplicated,
/// or not at all.
pub struct Reassembler {
    config: ReassemblerConfig,
    in_flight: VecDeque<Partial>,
    /// Recently completed or dropped frames, so stragglers are ignored.
    finished: VecDeque<(u8, u32)>,
}

const FINISHED_MEMORY: usize = 64;

impl Reassembler {
    pub fn new(config: ReassemblerConfig) -> Self {
        Self {
            config,
            in_flight: VecDeque::new(),
            finished: VecDeque::new(),
        }
    }

    pub fn frames_in_flight(&self) -> usize {
        self.in_flight.len()
    }

    /// Feeds one datagram. Completed or evicted frames are appended to `events`.
    pub fn push(
        &mut self,
        now_us: u64,
        header: DatagramHeader,
        payload: &[u8],
        events: &mut Vec<ReassembleEvent>,
    ) {
        let key = (header.stream, header.frame_id);
        if self.finished.contains(&key) {
            return;
        }

        let pos = match self
            .in_flight
            .iter()
            .position(|p| (p.stream, p.frame_id) == key)
        {
            Some(pos) => pos,
            None => {
                if self.in_flight.len() >= self.config.max_frames_in_flight
                    && let Some(oldest) = self.in_flight.pop_front()
                {
                    self.remember(oldest.stream, oldest.frame_id);
                    events.push(ReassembleEvent::Dropped(
                        oldest.dropped(DropReason::Superseded),
                    ));
                }
                self.in_flight.push_back(Partial {
                    stream: header.stream,
                    frame_id: header.frame_id,
                    flags: header.flags,
                    frag_count: header.frag_count,
                    received: 0,
                    parts: vec![None; header.frag_count as usize],
                    send_ts_us: header.send_ts_us,
                    first_arrival_us: now_us,
                    last_arrival_us: now_us,
                });
                self.in_flight.len() - 1
            }
        };

        let partial = &mut self.in_flight[pos];
        // A sender that changes frag_count mid-frame is broken; ignore the datagram.
        if header.frag_count != partial.frag_count {
            return;
        }
        let slot = &mut partial.parts[header.frag_index as usize];
        if slot.is_some() {
            return;
        }
        *slot = Some(payload.into());
        partial.received += 1;
        partial.flags = Flags(partial.flags.0 | header.flags.0);
        partial.last_arrival_us = now_us;

        if partial.received == partial.frag_count {
            let done = self.in_flight.remove(pos).expect("position is valid");
            self.remember(done.stream, done.frame_id);
            let mut data = Vec::with_capacity(done.parts.iter().flatten().map(|p| p.len()).sum());
            for part in done.parts.into_iter().flatten() {
                data.extend_from_slice(&part);
            }
            events.push(ReassembleEvent::Completed(CompletedFrame {
                stream: done.stream,
                frame_id: done.frame_id,
                flags: done.flags,
                data,
                frag_count: done.frag_count,
                send_ts_us: done.send_ts_us,
                first_arrival_us: done.first_arrival_us,
                last_arrival_us: done.last_arrival_us,
            }));
        }
    }

    /// Drops frames whose first fragment is older than the configured timeout.
    pub fn expire(&mut self, now_us: u64, events: &mut Vec<ReassembleEvent>) {
        let timeout = self.config.frame_timeout_us;
        while let Some(front) = self.in_flight.front() {
            if now_us.saturating_sub(front.first_arrival_us) < timeout {
                break;
            }
            let expired = self.in_flight.pop_front().expect("front exists");
            self.remember(expired.stream, expired.frame_id);
            events.push(ReassembleEvent::Dropped(
                expired.dropped(DropReason::Timeout),
            ));
        }
    }

    fn remember(&mut self, stream: u8, frame_id: u32) {
        if self.finished.len() == FINISHED_MEMORY {
            self.finished.pop_front();
        }
        self.finished.push_back((stream, frame_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::Kind;

    fn base(frame_id: u32) -> DatagramHeader {
        DatagramHeader {
            kind: Kind::Video,
            flags: Flags::default(),
            stream: 0,
            frame_id,
            frag_index: 0,
            frag_count: 0,
            send_ts_us: 42,
        }
    }

    fn split(frame_id: u32, frame: &[u8], max_datagram: usize) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        Fragmenter::new(max_datagram)
            .fragment(base(frame_id), frame, |d| out.push(d.to_vec()))
            .unwrap();
        out
    }

    fn feed(r: &mut Reassembler, now: u64, datagram: &[u8], events: &mut Vec<ReassembleEvent>) {
        let (h, payload) = DatagramHeader::decode(datagram).unwrap();
        r.push(now, h, payload, events);
    }

    #[test]
    fn fragment_sizes() {
        let f = Fragmenter::new(HEADER_LEN + 10);
        assert_eq!(f.fragment_count(0), 1);
        assert_eq!(f.fragment_count(10), 1);
        assert_eq!(f.fragment_count(11), 2);
        let datagrams = split(1, &[7u8; 25], HEADER_LEN + 10);
        let lens: Vec<_> = datagrams.iter().map(Vec::len).collect();
        assert_eq!(lens, vec![HEADER_LEN + 10, HEADER_LEN + 10, HEADER_LEN + 5]);
    }

    #[test]
    fn reassembles_out_of_order_and_ignores_duplicates() {
        let frame: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let mut datagrams = split(9, &frame, 100);
        datagrams.reverse();
        let dup = datagrams[3].clone();
        datagrams.insert(5, dup);

        let mut r = Reassembler::new(ReassemblerConfig::default());
        let mut events = Vec::new();
        for (i, d) in datagrams.iter().enumerate() {
            feed(&mut r, 1_000 + i as u64, d, &mut events);
        }
        assert_eq!(events.len(), 1);
        let ReassembleEvent::Completed(done) = &events[0] else {
            panic!("expected completion")
        };
        assert_eq!(done.data, frame);
        assert_eq!(done.frame_id, 9);
        assert_eq!(done.first_arrival_us, 1_000);
        assert_eq!(done.frag_count as usize, datagrams.len() - 1);
        assert_eq!(r.frames_in_flight(), 0);

        // A straggler for a finished frame is ignored.
        feed(&mut r, 5_000, &datagrams[0], &mut events);
        assert_eq!(events.len(), 1);
        assert_eq!(r.frames_in_flight(), 0);
    }

    #[test]
    fn times_out_incomplete_frames() {
        let datagrams = split(1, &[0u8; 300], 116);
        let mut r = Reassembler::new(ReassemblerConfig {
            frame_timeout_us: 50,
            ..Default::default()
        });
        let mut events = Vec::new();
        feed(&mut r, 0, &datagrams[0], &mut events);
        r.expire(49, &mut events);
        assert!(events.is_empty());
        r.expire(50, &mut events);
        assert_eq!(
            events,
            vec![ReassembleEvent::Dropped(DroppedFrame {
                stream: 0,
                frame_id: 1,
                frag_count: 3,
                received: 1,
                reason: DropReason::Timeout,
            })]
        );
    }

    #[test]
    fn evicts_oldest_when_too_many_in_flight() {
        let mut r = Reassembler::new(ReassemblerConfig {
            max_frames_in_flight: 2,
            ..Default::default()
        });
        let mut events = Vec::new();
        for id in 1..=3 {
            let datagrams = split(id, &[0u8; 300], 116);
            feed(&mut r, id as u64, &datagrams[0], &mut events);
        }
        assert_eq!(r.frames_in_flight(), 2);
        assert!(matches!(
            events.as_slice(),
            [ReassembleEvent::Dropped(DroppedFrame {
                frame_id: 1,
                reason: DropReason::Superseded,
                ..
            })]
        ));
    }
}

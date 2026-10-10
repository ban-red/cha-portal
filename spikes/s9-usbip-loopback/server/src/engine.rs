//! The device end of a USB/IP connection, Sans-IO: commands in, reply bytes
//! out. It parks interrupt-IN URBs and completes them as reports arrive.

use std::collections::VecDeque;

use crate::device::{Control, Device, EP_OUT};
use crate::usbip::{
    Command, DIR_IN, ECONNRESET, EPIPE, Submit, encode_ret_submit, encode_ret_unlink,
};

/// Reports kept while no URB is parked, so a press/release pair survives.
pub const PENDING_MAX: usize = 4;

#[derive(Debug)]
struct Parked {
    seqnum: u32,
    length: u32,
}

#[derive(Debug, Default)]
pub struct Engine {
    pub device: Device,
    parked: VecDeque<Parked>,
    pending: VecDeque<Vec<u8>>,
    /// Reports dropped because the FIFO was full.
    pub dropped: u64,
    /// Interrupt OUT payloads received.
    pub out_reports: Vec<Vec<u8>>,
}

fn complete_in(out: &mut Vec<u8>, seqnum: u32, length: u32, report: &[u8]) {
    let n = report.len().min(length as usize);
    encode_ret_submit(out, seqnum, 0, n as u32, &report[..n]);
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parked(&self) -> usize {
        self.parked.len()
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Handle one command and its body; reply bytes are appended to `out`.
    pub fn on_command(&mut self, cmd: &Command, body: &[u8], out: &mut Vec<u8>) {
        match cmd {
            Command::Submit(s) if s.ep == 0 => self.control(s, body, out),
            Command::Submit(s) if s.direction == DIR_IN && s.ep == 1 => {
                if self.device.halted_in {
                    encode_ret_submit(out, s.seqnum, -EPIPE, 0, &[]);
                } else if let Some(report) = self.pending.pop_front() {
                    complete_in(out, s.seqnum, s.length, &report);
                } else {
                    self.parked.push_back(Parked {
                        seqnum: s.seqnum,
                        length: s.length,
                    });
                }
            }
            Command::Submit(s) if s.direction != DIR_IN && s.ep == u32::from(EP_OUT) => {
                let n = body.len().min(s.length as usize);
                self.out_reports.push(body[..n].to_vec());
                encode_ret_submit(out, s.seqnum, 0, n as u32, &[]);
            }
            Command::Submit(s) => encode_ret_submit(out, s.seqnum, -EPIPE, 0, &[]),
            Command::Unlink {
                seqnum,
                unlink_seqnum,
                ..
            } => {
                let found = self.parked.iter().position(|p| p.seqnum == *unlink_seqnum);
                let status = if let Some(at) = found {
                    self.parked.remove(at);
                    -ECONNRESET
                } else {
                    0
                };
                encode_ret_unlink(out, *seqnum, status);
            }
        }
    }

    fn control(&mut self, s: &Submit, body: &[u8], out: &mut Vec<u8>) {
        match self.device.control(&s.setup, body) {
            Control::Data(data) => {
                if s.setup[0] & 0x80 != 0 {
                    encode_ret_submit(out, s.seqnum, 0, data.len() as u32, &data);
                } else {
                    encode_ret_submit(out, s.seqnum, 0, s.length, &[]);
                }
            }
            Control::Stall => encode_ret_submit(out, s.seqnum, -EPIPE, 0, &[]),
        }
    }

    /// A report was produced: complete the oldest parked URB with it, or keep
    /// it (the newest `PENDING_MAX`).
    pub fn push_report(&mut self, report: &[u8], out: &mut Vec<u8>) {
        if let Some(p) = self.parked.pop_front() {
            complete_in(out, p.seqnum, p.length, report);
        } else {
            if self.pending.len() == PENDING_MAX {
                self.pending.pop_front();
                self.dropped += 1;
            }
            self.pending.push_back(report.to_vec());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usbip::{DIR_OUT, HEADER_LEN, NO_ISO, RET_SUBMIT, RET_UNLINK};

    fn submit(seqnum: u32, direction: u32, ep: u32, length: u32, setup: [u8; 8]) -> Command {
        Command::Submit(Submit {
            seqnum,
            devid: 0x10002,
            direction,
            ep,
            transfer_flags: 0,
            length,
            start_frame: 0,
            number_of_packets: NO_ISO,
            interval: 0,
            setup,
        })
    }

    fn in_urb(seqnum: u32) -> Command {
        submit(seqnum, DIR_IN, 1, 64, [0; 8])
    }

    fn word(buf: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(buf[at..at + 4].try_into().unwrap())
    }

    #[test]
    fn control_in_returns_data_with_actual_length() {
        let mut e = Engine::new();
        let mut out = Vec::new();
        let get_dev = [0x80, 6, 0, 1, 0, 0, 18, 0];
        e.on_command(&submit(1, DIR_IN, 0, 18, get_dev), &[], &mut out);
        assert_eq!(word(&out, 0), RET_SUBMIT);
        assert_eq!(word(&out, 4), 1);
        assert_eq!(word(&out, 24), 18);
        assert_eq!(out.len(), HEADER_LEN + 18);
        assert_eq!(out[HEADER_LEN], 18);
    }

    #[test]
    fn control_out_acks_with_requested_length_and_stalls_unknown() {
        let mut e = Engine::new();
        let mut out = Vec::new();
        // SET_REPORT feature 1, 4 bytes.
        let set = [0x21, 9, 1, 3, 0, 0, 4, 0];
        e.on_command(&submit(2, DIR_OUT, 0, 4, set), &[1, 2, 3, 4], &mut out);
        assert_eq!(word(&out, 20), 0);
        assert_eq!(word(&out, 24), 4);
        assert_eq!(out.len(), HEADER_LEN);
        out.clear();
        e.on_command(
            &submit(3, DIR_IN, 0, 8, [0xc0, 1, 0, 0, 0, 0, 8, 0]),
            &[],
            &mut out,
        );
        assert_eq!(word(&out, 20) as i32, -EPIPE);
    }

    #[test]
    fn report_completes_the_oldest_parked_urb() {
        let mut e = Engine::new();
        let mut out = Vec::new();
        e.on_command(&in_urb(10), &[], &mut out);
        e.on_command(&in_urb(11), &[], &mut out);
        assert!(out.is_empty());
        assert_eq!(e.parked(), 2);
        e.push_report(&[0x45, 1, 2, 3], &mut out);
        assert_eq!(word(&out, 0), RET_SUBMIT);
        assert_eq!(word(&out, 4), 10);
        assert_eq!(word(&out, 24), 4);
        assert_eq!(&out[HEADER_LEN..], &[0x45, 1, 2, 3]);
        assert_eq!(e.parked(), 1);
    }

    #[test]
    fn report_is_cut_to_the_urb_length() {
        let mut e = Engine::new();
        let mut out = Vec::new();
        e.on_command(&submit(1, DIR_IN, 1, 2, [0; 8]), &[], &mut out);
        e.push_report(&[9, 8, 7], &mut out);
        assert_eq!(word(&out, 24), 2);
        assert_eq!(out.len(), HEADER_LEN + 2);
    }

    #[test]
    fn reports_without_a_urb_wait_in_a_fifo_of_four() {
        let mut e = Engine::new();
        let mut sink = Vec::new();
        for i in 0..6u8 {
            e.push_report(&[i], &mut sink);
        }
        assert!(sink.is_empty());
        assert_eq!(e.pending(), PENDING_MAX);
        assert_eq!(e.dropped, 2);
        // The next URB gets the oldest kept one (2), then 3, 4, 5.
        for want in 2..6u8 {
            let mut out = Vec::new();
            e.on_command(&in_urb(u32::from(want)), &[], &mut out);
            assert_eq!(out[HEADER_LEN], want);
        }
        assert_eq!(e.pending(), 0);
    }

    #[test]
    fn unlink_cancels_a_parked_urb() {
        let mut e = Engine::new();
        let mut out = Vec::new();
        e.on_command(&in_urb(20), &[], &mut out);
        let unlink = |seq, target| Command::Unlink {
            seqnum: seq,
            devid: 0,
            direction: 0,
            ep: 0,
            unlink_seqnum: target,
        };
        e.on_command(&unlink(21, 20), &[], &mut out);
        assert_eq!(word(&out, 0), RET_UNLINK);
        assert_eq!(word(&out, 4), 21);
        assert_eq!(word(&out, 20) as i32, -ECONNRESET);
        assert_eq!(e.parked(), 0);
        // Nothing parked any more, and a second unlink finds nothing.
        out.clear();
        e.on_command(&unlink(22, 20), &[], &mut out);
        assert_eq!(word(&out, 20), 0);
        e.push_report(&[1], &mut out);
        assert_eq!(out.len(), HEADER_LEN);
        assert_eq!(e.pending(), 1);
    }

    #[test]
    fn interrupt_out_is_recorded() {
        let mut e = Engine::new();
        let mut out = Vec::new();
        e.on_command(&submit(5, DIR_OUT, 1, 3, [0; 8]), &[0x80, 1, 2], &mut out);
        assert_eq!(word(&out, 24), 3);
        assert_eq!(e.out_reports, vec![vec![0x80, 1, 2]]);
    }
}

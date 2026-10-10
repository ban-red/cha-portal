//! USB/IP wire format, Sans-IO: the 48-byte headers of `USBIP_CMD_SUBMIT`,
//! `USBIP_RET_SUBMIT`, `USBIP_CMD_UNLINK` and `USBIP_RET_UNLINK`. From the
//! spike `spikes/s9-usbip-loopback`; used by [`crate::usbdev`] and [`crate::vhci`].
//!
//! Written from <https://docs.kernel.org/usb/usbip_protocol.html>. All fields
//! are big-endian. A header is a 20-byte basic part (command, seqnum, devid,
//! direction, endpoint) and a 28-byte command-specific part. After a vhci
//! `attach` there is no `OP_REQ_IMPORT` exchange: the kernel starts with
//! `CMD_SUBMIT` at once.

pub const HEADER_LEN: usize = 48;

pub const CMD_SUBMIT: u32 = 0x0000_0001;
pub const CMD_UNLINK: u32 = 0x0000_0002;
pub const RET_SUBMIT: u32 = 0x0000_0003;
pub const RET_UNLINK: u32 = 0x0000_0004;

pub const DIR_OUT: u32 = 0;
pub const DIR_IN: u32 = 1;

/// `number_of_packets` of a transfer that is not isochronous.
pub const NO_ISO: u32 = 0xffff_ffff;
/// One isochronous packet descriptor.
pub const ISO_DESCRIPTOR_LEN: usize = 16;

pub const ECONNRESET: i32 = 104;
pub const EPIPE: i32 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submit {
    pub seqnum: u32,
    pub devid: u32,
    pub direction: u32,
    pub ep: u32,
    pub transfer_flags: u32,
    pub length: u32,
    pub start_frame: u32,
    pub number_of_packets: u32,
    pub interval: u32,
    pub setup: [u8; 8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Submit(Submit),
    Unlink {
        seqnum: u32,
        devid: u32,
        direction: u32,
        ep: u32,
        /// The seqnum of the `CMD_SUBMIT` to cancel.
        unlink_seqnum: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// Not a command the kernel sends to a device (`RET_*`, or unknown).
    UnknownCommand(u32),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCommand(c) => write!(f, "unknown USB/IP command {c:#x}"),
        }
    }
}

impl std::error::Error for ParseError {}

fn be(buf: &[u8; HEADER_LEN], at: usize) -> u32 {
    u32::from_be_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

fn put(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// Parse one header. The caller then reads [`body_len`] more bytes.
pub fn parse_header(buf: &[u8; HEADER_LEN]) -> Result<Command, ParseError> {
    let (seqnum, devid, direction, ep) = (be(buf, 4), be(buf, 8), be(buf, 12), be(buf, 16));
    match be(buf, 0) {
        CMD_SUBMIT => {
            let mut setup = [0; 8];
            setup.copy_from_slice(&buf[40..48]);
            Ok(Command::Submit(Submit {
                seqnum,
                devid,
                direction,
                ep,
                transfer_flags: be(buf, 20),
                length: be(buf, 24),
                start_frame: be(buf, 28),
                number_of_packets: be(buf, 32),
                interval: be(buf, 36),
                setup,
            }))
        }
        CMD_UNLINK => Ok(Command::Unlink {
            seqnum,
            devid,
            direction,
            ep,
            unlink_seqnum: be(buf, 20),
        }),
        other => Err(ParseError::UnknownCommand(other)),
    }
}

/// Bytes that follow a command's header: the OUT data, then the isochronous
/// packet descriptors (for both directions).
pub fn body_len(cmd: &Command) -> usize {
    match cmd {
        Command::Submit(s) => {
            let data = if s.direction == DIR_OUT {
                s.length as usize
            } else {
                0
            };
            let iso = if s.number_of_packets == NO_ISO {
                0
            } else {
                s.number_of_packets as usize * ISO_DESCRIPTOR_LEN
            };
            data + iso
        }
        Command::Unlink { .. } => 0,
    }
}

/// `RET_SUBMIT`: status is a negative errno or 0. The kernel ignores devid,
/// direction and endpoint in replies; they are zero.
pub fn encode_ret_submit(
    out: &mut Vec<u8>,
    seqnum: u32,
    status: i32,
    actual_length: u32,
    data: &[u8],
) {
    put(out, RET_SUBMIT);
    put(out, seqnum);
    out.extend_from_slice(&[0; 12]); // devid, direction, ep
    put(out, status as u32);
    put(out, actual_length);
    put(out, 0); // start_frame
    put(out, NO_ISO); // number_of_packets
    put(out, 0); // error_count
    out.extend_from_slice(&[0; 8]); // padding
    out.extend_from_slice(data);
}

/// `RET_UNLINK`: -ECONNRESET if the URB was cancelled, 0 if it was already
/// done.
pub fn encode_ret_unlink(out: &mut Vec<u8>, seqnum: u32, status: i32) {
    put(out, RET_UNLINK);
    put(out, seqnum);
    out.extend_from_slice(&[0; 12]);
    put(out, status as u32);
    out.extend_from_slice(&[0; 24]);
}

/// The kernel's side of the exchange, for tests.
#[cfg(test)]
pub fn encode_cmd_submit(out: &mut Vec<u8>, s: &Submit, data: &[u8]) {
    put(out, CMD_SUBMIT);
    put(out, s.seqnum);
    put(out, s.devid);
    put(out, s.direction);
    put(out, s.ep);
    put(out, s.transfer_flags);
    put(out, s.length);
    put(out, s.start_frame);
    put(out, s.number_of_packets);
    put(out, s.interval);
    out.extend_from_slice(&s.setup);
    out.extend_from_slice(data);
}

#[cfg(test)]
pub fn encode_cmd_unlink(out: &mut Vec<u8>, seqnum: u32, devid: u32, unlink_seqnum: u32) {
    put(out, CMD_UNLINK);
    put(out, seqnum);
    put(out, devid);
    put(out, DIR_OUT);
    put(out, 0);
    put(out, unlink_seqnum);
    out.extend_from_slice(&[0; 24]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit() -> Submit {
        Submit {
            seqnum: 7,
            devid: (1 << 16) | 2,
            direction: DIR_IN,
            ep: 1,
            transfer_flags: 0x200,
            length: 64,
            start_frame: 0,
            number_of_packets: NO_ISO,
            interval: 1,
            setup: [0x80, 6, 0, 1, 0, 0, 18, 0],
        }
    }

    fn header(v: &[u8]) -> [u8; HEADER_LEN] {
        v[..HEADER_LEN].try_into().unwrap()
    }

    #[test]
    fn submit_round_trips_big_endian() {
        let s = submit();
        let mut wire = Vec::new();
        encode_cmd_submit(&mut wire, &s, &[]);
        assert_eq!(wire.len(), HEADER_LEN);
        assert_eq!(&wire[..8], &[0, 0, 0, 1, 0, 0, 0, 7]);
        assert_eq!(parse_header(&header(&wire)), Ok(Command::Submit(s)));
    }

    #[test]
    fn unlink_round_trips() {
        let mut wire = Vec::new();
        encode_cmd_unlink(&mut wire, 9, 0x10002, 7);
        assert_eq!(wire.len(), HEADER_LEN);
        assert_eq!(
            parse_header(&header(&wire)),
            Ok(Command::Unlink {
                seqnum: 9,
                devid: 0x10002,
                direction: 0,
                ep: 0,
                unlink_seqnum: 7
            })
        );
    }

    #[test]
    fn replies_are_not_commands() {
        let mut wire = Vec::new();
        encode_ret_submit(&mut wire, 1, 0, 0, &[]);
        assert_eq!(
            parse_header(&header(&wire)),
            Err(ParseError::UnknownCommand(RET_SUBMIT))
        );
    }

    #[test]
    fn ret_submit_layout() {
        let mut wire = Vec::new();
        encode_ret_submit(&mut wire, 5, -EPIPE, 3, &[1, 2, 3]);
        assert_eq!(wire.len(), HEADER_LEN + 3);
        assert_eq!(&wire[0..4], &[0, 0, 0, 3]);
        assert_eq!(&wire[4..8], &[0, 0, 0, 5]);
        assert_eq!(&wire[20..24], &(-EPIPE).to_be_bytes());
        assert_eq!(&wire[24..28], &[0, 0, 0, 3]);
        assert_eq!(&wire[32..36], &[0xff; 4]);
        assert_eq!(&wire[48..], &[1, 2, 3]);
    }

    #[test]
    fn ret_unlink_layout() {
        let mut wire = Vec::new();
        encode_ret_unlink(&mut wire, 6, -ECONNRESET);
        assert_eq!(wire.len(), HEADER_LEN);
        assert_eq!(&wire[0..4], &[0, 0, 0, 4]);
        assert_eq!(&wire[20..24], &(-ECONNRESET).to_be_bytes());
        assert!(wire[24..].iter().all(|b| *b == 0));
    }

    #[test]
    fn body_follows_out_data_and_iso_descriptors() {
        let mut s = submit();
        assert_eq!(body_len(&Command::Submit(s.clone())), 0); // IN, no iso
        s.direction = DIR_OUT;
        s.length = 9;
        assert_eq!(body_len(&Command::Submit(s.clone())), 9);
        s.number_of_packets = 2;
        assert_eq!(body_len(&Command::Submit(s)), 9 + 32);
    }
}

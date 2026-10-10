//! A udev-format hotplug event for a `hidraw` node, sent into the network
//! namespace this process runs in (`--udev-event`).
//!
//! The host's kernel does announce a new pad on netlink in the app's
//! namespace, but libudev (what Steam, SDL's HIDAPI and Wine use) only
//! listens to udevd's messages there, on multicast group 2, and it joins that
//! group only if `/run/udev/control` exists (the streamer makes the file).
//! No udevd runs in the app, so the node agent runs this in a throwaway
//! container that shares the app's network namespace, with `CAP_NET_ADMIN`
//! (sending to a uevent group takes it) as root (libudev ignores other
//! senders), once per node it puts into the app's `/dev`.
//!
//! The format is libudev's `udev_monitor_netlink_header` and a list of
//! `KEY=value` strings; our own code, written from the format's description
//! (ADR 0004).

use anyhow::{Context, Result, bail};
use std::io;

/// The kernel's netlink protocol for uevents.
const NETLINK_KOBJECT_UEVENT: libc::c_int = 15;
/// The group udevd's messages go to.
const UDEV_GROUP: u32 = 2;
/// Marks a libudev message (`htonl(0xfeedcafe)`).
const MAGIC: u32 = 0xfeed_cafe;
/// The header's size: the 8-byte prefix and eight 32-bit fields.
const HEADER_LEN: u32 = 40;

/// libudev's string hash (MurmurHash2, seed 0): a receiver's filter on the
/// subsystem compares it with the header's.
fn string_hash32(text: &str) -> u32 {
    const M: u32 = 0x5bd1_e995;
    let data = text.as_bytes();
    let mut hash = data.len() as u32;
    let mut chunks = data.chunks_exact(4);
    for chunk in &mut chunks {
        let mut k = u32::from_le_bytes(chunk.try_into().expect("4 bytes"));
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        k = k.wrapping_mul(M);
        hash = hash.wrapping_mul(M) ^ k;
    }
    let rest = chunks.remainder();
    if !rest.is_empty() {
        for (i, byte) in rest.iter().enumerate().rev() {
            hash ^= u32::from(*byte) << (8 * i);
        }
        hash = hash.wrapping_mul(M);
    }
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(M);
    hash ^ (hash >> 15)
}

/// What the event is about: a `hidraw` node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidrawEvent {
    /// `add` or `remove`.
    pub action: String,
    /// The device's sysfs path without `/sys`:
    /// `/devices/virtual/misc/uhid/0005:28DE:1303.0028/hidraw/hidraw2`.
    pub devpath: String,
    /// `hidraw2`.
    pub name: String,
    pub major: u32,
    pub minor: u32,
}

impl HidrawEvent {
    /// Reads the command line's four values (`--udev-event`).
    pub fn from_args(args: &[String]) -> Result<Self> {
        let [action, devpath, name, dev] = args else {
            bail!("--udev-event takes an action, a devpath, a node name and MAJOR:MINOR");
        };
        if !matches!(action.as_str(), "add" | "remove") {
            bail!("the action is add or remove, not {action}");
        }
        if !devpath.starts_with("/devices/")
            || devpath.contains("..")
            || devpath.bytes().any(|b| b == 0 || b == b'=')
        {
            bail!("not a sysfs device path: {devpath}");
        }
        let valid_name = name
            .strip_prefix("hidraw")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !valid_name {
            bail!("not a hidraw node name: {name}");
        }
        let (major, minor) = dev
            .split_once(':')
            .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            .with_context(|| format!("not MAJOR:MINOR: {dev}"))?;
        Ok(Self {
            action: action.clone(),
            devpath: devpath.clone(),
            name: name.clone(),
            major,
            minor,
        })
    }

    /// The datagram: the header, then the properties, each ended by a NUL.
    pub fn message(&self, seqnum: u64, initialized_us: u64) -> Vec<u8> {
        let mut props = vec![
            ("ACTION", self.action.clone()),
            ("DEVPATH", self.devpath.clone()),
            ("SUBSYSTEM", "hidraw".into()),
            ("DEVNAME", format!("/dev/{}", self.name)),
            ("MAJOR", self.major.to_string()),
            ("MINOR", self.minor.to_string()),
            ("SEQNUM", seqnum.to_string()),
        ];
        if self.action == "add" {
            props.push(("USEC_INITIALIZED", initialized_us.to_string()));
        }
        let mut body = Vec::new();
        for (key, value) in props {
            body.extend_from_slice(key.as_bytes());
            body.push(b'=');
            body.extend_from_slice(value.as_bytes());
            body.push(0);
        }
        let mut message = Vec::with_capacity(HEADER_LEN as usize + body.len());
        message.extend_from_slice(b"libudev\0");
        message.extend_from_slice(&MAGIC.to_be_bytes());
        // Host-order fields: the header's and the properties' sizes.
        message.extend_from_slice(&HEADER_LEN.to_ne_bytes());
        message.extend_from_slice(&HEADER_LEN.to_ne_bytes());
        message.extend_from_slice(&(body.len() as u32).to_ne_bytes());
        // The receiver's kernel-side filter compares these in network order;
        // no devtype, no tags.
        message.extend_from_slice(&string_hash32("hidraw").to_be_bytes());
        message.extend_from_slice(&[0; 12]);
        message.extend_from_slice(&body);
        message
    }

    /// Sends the event to udev's group in this network namespace.
    pub fn send(&self) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let message = self.message(now.as_secs(), now.as_micros() as u64);
        // SAFETY: plain socket calls with a sockaddr_nl of the right size and
        // a buffer of the length passed; the descriptor is closed at the end.
        unsafe {
            let fd = libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                NETLINK_KOBJECT_UEVENT,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error()).context("opening a uevent socket");
            }
            let mut addr: libc::sockaddr_nl = std::mem::zeroed();
            addr.nl_family = libc::AF_NETLINK as libc::sa_family_t;
            let size = std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t;
            let result = if libc::bind(fd, (&raw const addr).cast(), size) < 0 {
                Err(io::Error::last_os_error()).context("binding the uevent socket")
            } else {
                addr.nl_groups = UDEV_GROUP;
                let sent = libc::sendto(
                    fd,
                    message.as_ptr().cast(),
                    message.len(),
                    0,
                    (&raw const addr).cast(),
                    size,
                );
                if sent < 0 {
                    Err(io::Error::last_os_error())
                        .context("sending the uevent (does it have CAP_NET_ADMIN?)")
                } else {
                    Ok(())
                }
            };
            libc::close(fd);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(action: &str) -> HidrawEvent {
        HidrawEvent {
            action: action.into(),
            devpath: "/devices/virtual/misc/uhid/0005:28DE:1303.0028/hidraw/hidraw2".into(),
            name: "hidraw2".into(),
            major: 238,
            minor: 2,
        }
    }

    #[test]
    fn the_hash_is_libudevs() {
        assert_eq!(string_hash32(""), 0);
        // One block and a tail of 0, 1, 2 and 3 bytes all go through.
        for text in ["abcd", "abcde", "abcdef", "abcdefg", "hidraw", "usb_device"] {
            assert_ne!(string_hash32(text), 0, "{text}");
        }
        assert_ne!(string_hash32("hidraw"), string_hash32("input"));
        // MurmurHash2 with seed 0, as the reference algorithm gives them.
        assert_eq!(string_hash32("hello"), 0xe561_29cb);
        assert_eq!(string_hash32("hidraw"), 0xc2ca_f397);
    }

    #[test]
    fn the_message_has_libudevs_header_and_properties() {
        let message = event("add").message(7, 1_000_000);
        assert_eq!(&message[..8], b"libudev\0");
        assert_eq!(
            u32::from_be_bytes(message[8..12].try_into().unwrap()),
            MAGIC
        );
        let header = u32::from_ne_bytes(message[12..16].try_into().unwrap());
        let off = u32::from_ne_bytes(message[16..20].try_into().unwrap());
        let len = u32::from_ne_bytes(message[20..24].try_into().unwrap());
        assert_eq!((header, off), (40, 40));
        assert_eq!(message.len(), 40 + len as usize);
        assert_eq!(
            u32::from_be_bytes(message[24..28].try_into().unwrap()),
            string_hash32("hidraw")
        );
        assert_eq!(&message[28..40], &[0; 12]);
        let body = String::from_utf8(message[40..].to_vec()).unwrap();
        let props: Vec<&str> = body.split('\0').filter(|p| !p.is_empty()).collect();
        assert_eq!(
            props,
            [
                "ACTION=add",
                "DEVPATH=/devices/virtual/misc/uhid/0005:28DE:1303.0028/hidraw/hidraw2",
                "SUBSYSTEM=hidraw",
                "DEVNAME=/dev/hidraw2",
                "MAJOR=238",
                "MINOR=2",
                "SEQNUM=7",
                "USEC_INITIALIZED=1000000",
            ]
        );
        assert!(body.ends_with('\0'));
    }

    #[test]
    fn a_remove_is_not_initialized() {
        let message = event("remove").message(8, 5);
        let body = String::from_utf8(message[40..].to_vec()).unwrap();
        assert!(body.contains("ACTION=remove\0"));
        assert!(!body.contains("USEC_INITIALIZED"));
    }

    #[test]
    fn the_arguments_are_checked() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let good = args(&[
            "add",
            "/devices/virtual/misc/uhid/x/hidraw/hidraw2",
            "hidraw2",
            "238:2",
        ]);
        assert_eq!(
            HidrawEvent::from_args(&good).unwrap().devpath,
            "/devices/virtual/misc/uhid/x/hidraw/hidraw2"
        );
        for bad in [
            args(&["add"]),
            args(&["bind", "/devices/x", "hidraw2", "238:2"]),
            args(&["add", "/sys/devices/x", "hidraw2", "238:2"]),
            args(&["add", "/devices/../x", "hidraw2", "238:2"]),
            args(&["add", "/devices/x=y", "hidraw2", "238:2"]),
            args(&["add", "/devices/x", "event2", "238:2"]),
            args(&["add", "/devices/x", "hidraw", "238:2"]),
            args(&["add", "/devices/x", "hidraw2", "238"]),
            args(&["add", "/devices/x", "hidraw2", "a:b"]),
        ] {
            assert!(HidrawEvent::from_args(&bad).is_err(), "{bad:?}");
        }
    }
}

//! The client's `PING`s on the video and audio ports, and who may send them.
//!
//! The host sends media to the address a `PING` came from, so a `PING` from
//! anywhere else would redirect the stream. A datagram counts only when it
//! comes from the IP of the client whose session this is, and is either the
//! legacy `PING` or carries the session's ping payload.

use std::net::{IpAddr, SocketAddr};

use crate::net::same_ip;

/// The payload (16 bytes) and sequence number (4 bytes, big-endian) of a
/// Sunshine-style ping.
const SUNSHINE_PING_LEN: usize = 20;

/// Whether `datagram` from `from` is a ping from the session's client.
pub(crate) fn is_client_ping(
    datagram: &[u8],
    from: SocketAddr,
    client_ip: IpAddr,
    payload: &[u8; 16],
) -> bool {
    same_ip(from.ip(), client_ip)
        && (datagram == b"PING"
            || (datagram.len() == SUNSHINE_PING_LEN && &datagram[..16] == payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAYLOAD: [u8; 16] = *b"0123456789abcdef";

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_the_sessions_client_may_ping() {
        let client: IpAddr = "192.168.1.20".parse().unwrap();
        assert!(is_client_ping(
            b"PING",
            addr("192.168.1.20:5000"),
            client,
            &PAYLOAD
        ));
        assert!(!is_client_ping(
            b"PING",
            addr("192.168.1.21:5000"),
            client,
            &PAYLOAD
        ));
        assert!(is_client_ping(
            b"PING",
            addr("[::ffff:192.168.1.20]:5000"),
            client,
            &PAYLOAD
        ));
    }

    #[test]
    fn sunshine_pings_need_the_payload() {
        let client: IpAddr = "10.0.0.2".parse().unwrap();
        let mut good = PAYLOAD.to_vec();
        good.extend(7u32.to_be_bytes());
        assert!(is_client_ping(&good, addr("10.0.0.2:1"), client, &PAYLOAD));
        let mut bad = b"yyyyyyyyyyyyyyyy".to_vec();
        bad.extend(7u32.to_be_bytes());
        assert!(!is_client_ping(&bad, addr("10.0.0.2:1"), client, &PAYLOAD));
        assert!(!is_client_ping(
            &PAYLOAD,
            addr("10.0.0.2:1"),
            client,
            &PAYLOAD
        ));
        assert!(!is_client_ping(b"", addr("10.0.0.2:1"), client, &PAYLOAD));
    }
}

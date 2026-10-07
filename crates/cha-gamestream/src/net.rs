//! Small address helpers.

use std::net::IpAddr;

/// An IPv4-mapped IPv6 address as its IPv4; other addresses as they are.
/// A dual-stack socket reports IPv4 peers mapped.
pub(crate) fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

/// Equal addresses, mapped or not.
pub(crate) fn same_ip(a: IpAddr, b: IpAddr) -> bool {
    unmap(a) == unmap(b)
}

/// Resolves when the flag is set, or its sender is gone. (`wait_for` itself
/// returns a lock guard, which can't be held across the awaits of a `select!`
/// arm that must be `Send`.)
pub(crate) async fn flagged(rx: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|set| *set).await;
}

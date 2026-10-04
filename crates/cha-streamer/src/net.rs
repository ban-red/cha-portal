//! The node's addresses for WebRTC host candidates: every IPv4 address on an
//! interface that is up, except loopback, link-local and container bridges.
//! A Tailscale or WireGuard address among them lets mesh clients reach the
//! node directly, with no subnet route.

use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr};

pub fn host_addresses() -> Vec<IpAddr> {
    let mut found = Vec::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a list that freeifaddrs releases below.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return found;
    }
    let mut cursor = list;
    while !cursor.is_null() {
        // SAFETY: a node of the list getifaddrs returned.
        let ifa = unsafe { &*cursor };
        cursor = ifa.ifa_next;
        if ifa.ifa_addr.is_null()
            || ifa.ifa_flags & libc::IFF_UP as u32 == 0
            || ifa.ifa_flags & libc::IFF_LOOPBACK as u32 != 0
        {
            continue;
        }
        // SAFETY: interface names are NUL-terminated.
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }.to_string_lossy();
        // SAFETY: ifa_addr is non-null; its family says how to read it.
        if is_bridge(&name) || i32::from(unsafe { (*ifa.ifa_addr).sa_family }) != libc::AF_INET {
            continue;
        }
        // SAFETY: an AF_INET address is a sockaddr_in.
        let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
        let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        if !ip.is_link_local() && !found.contains(&IpAddr::V4(ip)) {
            found.push(IpAddr::V4(ip));
        }
    }
    // SAFETY: the list from getifaddrs, freed once.
    unsafe { libc::freeifaddrs(list) };
    found
}

/// Interfaces that only lead into containers or VMs on this machine.
fn is_bridge(name: &str) -> bool {
    [
        "docker", "br-", "veth", "virbr", "cni", "flannel", "cali", "vxlan", "lxc", "podman",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_container_bridges() {
        assert!(is_bridge("docker0"));
        assert!(is_bridge("br-9fbbb54b5717"));
        assert!(!is_bridge("ens18"));
        assert!(!is_bridge("tailscale0"));
        assert!(!is_bridge("wg0"));
    }

    #[test]
    fn finds_no_loopback() {
        assert!(host_addresses().iter().all(|ip| !ip.is_loopback()));
    }
}

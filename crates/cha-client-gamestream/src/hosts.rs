//! The host list: what the LAN browse and probes found, what was added by
//! hand and what was paired. Pure bookkeeping with the clock passed in, so
//! the aging is testable.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use cha_client::Host;

use crate::store::SavedHost;

/// A host nobody has answered for this long is dropped, unless it is paired
/// or was added by address.
pub const GONE_AFTER: Duration = Duration::from_secs(120);

/// Where to reach a host's HTTP API.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Endpoint {
    /// As the library takes it: a name, an IPv4 address or a bracketed IPv6 one.
    pub address: String,
    pub port: u16,
}

impl Endpoint {
    /// `192.168.1.20`, `host:47989`, `[fe80::1]:47989` or `::1`; the default
    /// port is GameStream's 47989.
    pub fn parse(text: &str) -> Option<Self> {
        const DEFAULT_PORT: u16 = 47989;
        let text = text.trim();
        if text.is_empty() || text.contains(char::is_whitespace) || text.contains('/') {
            return None;
        }
        if let Some(rest) = text.strip_prefix('[') {
            let (ip, after) = rest.split_once(']')?;
            ip.parse::<std::net::Ipv6Addr>().ok()?;
            let port = match after {
                "" => DEFAULT_PORT,
                _ => after.strip_prefix(':')?.parse().ok()?,
            };
            return Some(Self {
                address: format!("[{ip}]"),
                port,
            });
        }
        // A bare IPv6 address has colons of its own.
        if text.parse::<std::net::Ipv6Addr>().is_ok() {
            return Some(Self {
                address: format!("[{text}]"),
                port: DEFAULT_PORT,
            });
        }
        let (address, port) = match text.split_once(':') {
            Some((address, port)) => (address, port.parse().ok()?),
            None => (text, DEFAULT_PORT),
        };
        if address.is_empty() || port == 0 {
            return None;
        }
        Some(Self {
            address: address.to_string(),
            port,
        })
    }

    /// For showing: `192.168.1.20:47989`.
    pub fn display(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }

    /// An address the browse found.
    pub fn from_ip(ip: std::net::IpAddr, port: u16) -> Self {
        let address = match ip {
            std::net::IpAddr::V4(v4) => v4.to_string(),
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
        };
        Self { address, port }
    }
}

/// What a probe of `/serverinfo` learned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probed {
    pub id: String,
    pub name: String,
    pub paired: bool,
    pub running_app: Option<u32>,
}

struct Entry {
    host: Host,
    endpoint: Endpoint,
    /// Last time the host answered (or, for one loaded from disk, when we started).
    last_seen: Instant,
    added: bool,
}

impl Entry {
    /// Kept however long it stays silent.
    fn pinned(&self) -> bool {
        self.added || self.host.paired
    }
}

#[derive(Default)]
pub struct HostList {
    entries: HashMap<String, Entry>,
}

impl HostList {
    /// The list as last saved; each host is taken to have just been seen.
    pub fn from_saved(saved: Vec<SavedHost>, now: Instant) -> Self {
        let mut list = Self::default();
        for s in saved {
            let endpoint = Endpoint {
                address: s.address,
                port: s.port,
            };
            list.entries.insert(
                s.id.clone(),
                Entry {
                    host: Host {
                        id: s.id,
                        name: s.name,
                        address: endpoint.display(),
                        paired: s.paired,
                        running_app: None,
                    },
                    endpoint,
                    last_seen: now,
                    added: s.added,
                },
            );
        }
        list
    }

    /// A host answered at `endpoint`. `added` marks one the user asked for.
    pub fn seen(&mut self, probed: Probed, endpoint: Endpoint, added: bool, now: Instant) {
        let added = added || self.entries.get(&probed.id).is_some_and(|e| e.added);
        let host = Host {
            id: probed.id.clone(),
            name: probed.name,
            address: endpoint.display(),
            paired: probed.paired,
            running_app: probed.running_app,
        };
        self.entries.insert(
            probed.id,
            Entry {
                host,
                endpoint,
                last_seen: now,
                added,
            },
        );
    }

    /// Drops the hosts gone silent; true if any went.
    pub fn age(&mut self, now: Instant) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|_, e| e.pinned() || now.duration_since(e.last_seen) < GONE_AFTER);
        self.entries.len() != before
    }

    /// The hosts to show, by name.
    pub fn hosts(&self) -> Vec<Host> {
        let mut hosts: Vec<Host> = self.entries.values().map(|e| e.host.clone()).collect();
        hosts.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        hosts
    }

    pub fn get(&self, id: &str) -> Option<(&Host, &Endpoint)> {
        self.entries.get(id).map(|e| (&e.host, &e.endpoint))
    }

    /// Where to look again.
    pub fn endpoints(&self) -> Vec<Endpoint> {
        self.entries.values().map(|e| e.endpoint.clone()).collect()
    }

    /// What to keep across restarts: the hosts that never age out.
    pub fn saved(&self) -> Vec<SavedHost> {
        let mut saved: Vec<SavedHost> = self
            .entries
            .values()
            .filter(|e| e.pinned())
            .map(|e| SavedHost {
                id: e.host.id.clone(),
                name: e.host.name.clone(),
                address: e.endpoint.address.clone(),
                port: e.endpoint.port,
                added: e.added,
                paired: e.host.paired,
            })
            .collect();
        saved.sort_by(|a, b| a.id.cmp(&b.id));
        saved
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probed(id: &str, paired: bool) -> Probed {
        Probed {
            id: id.into(),
            name: format!("host {id}"),
            paired,
            running_app: None,
        }
    }

    fn at(last: u8) -> Endpoint {
        Endpoint {
            address: format!("192.168.1.{last}"),
            port: 47989,
        }
    }

    #[test]
    fn a_silent_found_host_drops_after_two_minutes_but_not_before() {
        let t0 = Instant::now();
        let mut list = HostList::default();
        list.seen(probed("A1", false), at(20), false, t0);
        assert!(!list.age(t0 + GONE_AFTER - Duration::from_secs(1)));
        assert_eq!(list.hosts().len(), 1);
        assert!(list.age(t0 + GONE_AFTER));
        assert!(list.hosts().is_empty());
    }

    #[test]
    fn an_answer_renews_the_host() {
        let t0 = Instant::now();
        let mut list = HostList::default();
        list.seen(probed("A1", false), at(20), false, t0);
        list.seen(
            probed("A1", false),
            at(20),
            false,
            t0 + Duration::from_secs(100),
        );
        assert!(!list.age(t0 + Duration::from_secs(200)));
        assert!(list.age(t0 + Duration::from_secs(221)));
    }

    #[test]
    fn paired_and_added_hosts_stay_however_long_they_are_silent() {
        let t0 = Instant::now();
        let mut list = HostList::default();
        list.seen(probed("A1", true), at(20), false, t0);
        list.seen(probed("B2", false), at(21), true, t0);
        list.seen(probed("C3", false), at(22), false, t0);
        assert!(list.age(t0 + Duration::from_secs(3600)));
        let ids: Vec<String> = list.hosts().into_iter().map(|h| h.id).collect();
        assert_eq!(ids, ["A1", "B2"]);
        // Added stays added when a later sighting is only the browse's.
        list.seen(probed("B2", false), at(21), false, t0);
        assert!(!list.age(t0 + Duration::from_secs(7200)));
    }

    #[test]
    fn a_host_that_moves_keeps_one_entry() {
        let t0 = Instant::now();
        let mut list = HostList::default();
        list.seen(probed("A1", false), at(20), false, t0);
        list.seen(probed("A1", false), at(30), false, t0);
        let hosts = list.hosts();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].address, "192.168.1.30:47989");
        assert_eq!(list.get("A1").unwrap().1, &at(30));
    }

    #[test]
    fn only_the_lasting_hosts_are_saved_and_come_back() {
        let t0 = Instant::now();
        let mut list = HostList::default();
        list.seen(probed("A1", true), at(20), false, t0);
        list.seen(probed("C3", false), at(22), false, t0);
        let saved = list.saved();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, "A1");
        let back = HostList::from_saved(saved, t0);
        let hosts = back.hosts();
        assert_eq!(hosts.len(), 1);
        assert!(hosts[0].paired);
        assert_eq!(hosts[0].address, "192.168.1.20:47989");
        // Loaded hosts are not instantly aged out: they are paired, so never.
        let mut back = back;
        assert!(!back.age(t0 + Duration::from_secs(3600)));
    }

    #[test]
    fn addresses_parse_with_and_without_a_port() {
        let e = |a: &str, p| Endpoint {
            address: a.into(),
            port: p,
        };
        assert_eq!(
            Endpoint::parse("192.168.1.20"),
            Some(e("192.168.1.20", 47989))
        );
        assert_eq!(Endpoint::parse(" pc.lan:48989 "), Some(e("pc.lan", 48989)));
        assert_eq!(
            Endpoint::parse("[fe80::1]:47990"),
            Some(e("[fe80::1]", 47990))
        );
        assert_eq!(Endpoint::parse("::1"), Some(e("[::1]", 47989)));
        for bad in [
            "",
            "host:",
            "host:0",
            "host:99999",
            ":47989",
            "a b",
            "http://x",
            "[::1",
        ] {
            assert_eq!(Endpoint::parse(bad), None, "{bad:?}");
        }
        assert_eq!(e("[::1]", 47989).display(), "[::1]:47989");
    }
}

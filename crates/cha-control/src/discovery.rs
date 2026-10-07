//! Unclaimed nodes found on the LAN (ADR 0007).
//!
//! Nodes with no identity advertise `_cha-node._tcp.local.` over mDNS. The
//! portal browses for it and keeps what it saw in memory: [`Discovered`] is
//! the shared list, filled by [`browse`] in production and directly in tests.
//! An entry nobody has refreshed for [`STALE`] is dropped.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use cha_wire::claim::MDNS_SERVICE;
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use serde::Serialize;
use tracing::{debug, info, warn};

/// How long a found node stays listed without being seen again.
const STALE_MS: i64 = 60_000;

/// One advertising node, as the admin API shows it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredNode {
    /// Stable for the service: its mDNS full name.
    pub id: String,
    pub name: String,
    pub gpu: String,
    /// The first 16 hex characters of SHA-256 of the node's public key.
    pub fingerprint: String,
    pub addresses: Vec<String>,
    pub port: u16,
    /// Unix milliseconds.
    pub last_seen: i64,
}

/// The nodes found, by id.
#[derive(Default)]
pub struct Discovered {
    nodes: Mutex<HashMap<String, DiscoveredNode>>,
}

pub fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Discovered {
    /// Adds a node, or refreshes it.
    pub fn insert(&self, node: DiscoveredNode) {
        self.nodes
            .lock()
            .expect("discovery lock")
            .insert(node.id.clone(), node);
    }

    pub fn remove(&self, id: &str) {
        self.nodes.lock().expect("discovery lock").remove(id);
    }

    /// The nodes seen recently, by name.
    pub fn list(&self) -> Vec<DiscoveredNode> {
        self.list_at(unix_ms())
    }

    fn list_at(&self, now_ms: i64) -> Vec<DiscoveredNode> {
        let mut nodes = self.nodes.lock().expect("discovery lock");
        nodes.retain(|_, n| now_ms - n.last_seen <= STALE_MS);
        let mut list: Vec<_> = nodes.values().cloned().collect();
        list.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        list
    }

    pub fn get(&self, id: &str) -> Option<DiscoveredNode> {
        self.list().into_iter().find(|n| n.id == id)
    }
}

/// A resolved service as an entry; `None` if it lacks what a claim needs.
fn node_from(service: &ResolvedService) -> Option<DiscoveredNode> {
    let txt = |key: &str| service.get_property_val_str(key).map(str::to_string);
    let fingerprint = txt("fp").filter(|fp| !fp.is_empty())?;
    let mut addresses: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(|a| a.to_ip_addr())
        .collect();
    // IPv4 first: link-local IPv6 isn't reachable without a scope.
    addresses.sort_by_key(|a| (a.is_ipv6(), *a));
    if addresses.is_empty() {
        return None;
    }
    let instance = service
        .get_fullname()
        .strip_suffix(&format!(".{MDNS_SERVICE}"))
        .unwrap_or(service.get_fullname());
    Some(DiscoveredNode {
        id: service.get_fullname().to_string(),
        name: txt("name").unwrap_or_else(|| instance.to_string()),
        gpu: txt("gpu").unwrap_or_default(),
        fingerprint,
        addresses: addresses.iter().map(IpAddr::to_string).collect(),
        port: service.get_port(),
        last_seen: unix_ms(),
    })
}

/// Browses for unclaimed nodes until the process ends, filling `found`.
/// Returns once the browse is running; the daemon keeps going in the
/// background.
pub fn browse(found: Arc<Discovered>) -> Result<()> {
    let daemon = ServiceDaemon::new()?;
    let events = daemon.browse(MDNS_SERVICE)?;
    info!("looking for unclaimed nodes on the LAN ({MDNS_SERVICE})");
    tokio::spawn(async move {
        // The daemon stops when this is dropped, which is when the task ends.
        let _daemon = daemon;
        while let Ok(event) = events.recv_async().await {
            match event {
                ServiceEvent::ServiceResolved(service) => match node_from(&service) {
                    Some(node) => found.insert(node),
                    None => debug!(name = service.get_fullname(), "ignoring an incomplete node"),
                },
                ServiceEvent::ServiceRemoved(_, fullname) => found.remove(&fullname),
                _ => {}
            }
        }
        warn!("the LAN browse stopped; nodes won't be found until the portal restarts");
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, name: &str, last_seen: i64) -> DiscoveredNode {
        DiscoveredNode {
            id: id.into(),
            name: name.into(),
            gpu: String::new(),
            fingerprint: "0011223344556677".into(),
            addresses: vec!["192.168.1.5".into()],
            port: 7679,
            last_seen,
        }
    }

    #[test]
    fn stale_entries_drop_out_and_removals_apply() {
        let found = Discovered::default();
        found.insert(node("b", "beta", 1_000));
        found.insert(node("a", "alpha", 50_000));
        let names = |at| {
            found
                .list_at(at)
                .into_iter()
                .map(|n| n.name)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(55_000), ["alpha", "beta"]);
        assert_eq!(names(62_000), ["alpha"], "beta was last seen over 60 s ago");
        found.remove("a");
        assert!(names(62_000).is_empty());
    }
}

//! mDNS browse for `_nvstream._tcp.local.`, which Sunshine, Apollo and our
//! nodes advertise.

use std::net::IpAddr;

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use tokio::sync::mpsc;
use tracing::{info, warn};

/// What the hosts advertise.
pub const SERVICE: &str = "_nvstream._tcp.local.";

/// A service the browse resolved, or lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    Resolved {
        /// Stable for the service: its mDNS full name.
        name: String,
        address: IpAddr,
        port: u16,
    },
    Removed {
        name: String,
    },
}

/// Starts the browse; the returned channel ends when the browse does. The
/// daemon runs for as long as the forwarding task does, which is as long as
/// the receiver is held.
pub fn browse() -> Result<mpsc::Receiver<Seen>> {
    let (tx, rx) = mpsc::channel(64);
    let daemon = ServiceDaemon::new()?;
    let events = daemon.browse(SERVICE)?;
    info!("looking for Moonlight hosts on the LAN ({SERVICE})");
    tokio::spawn(async move {
        let _daemon = daemon;
        while let Ok(event) = events.recv_async().await {
            let seen = match event {
                ServiceEvent::ServiceResolved(service) => {
                    let mut addresses: Vec<IpAddr> = service
                        .get_addresses()
                        .iter()
                        .map(|a| a.to_ip_addr())
                        .collect();
                    // IPv4 first: link-local IPv6 isn't reachable without a scope.
                    addresses.sort_by_key(|a| (a.is_ipv6(), *a));
                    match addresses.first() {
                        Some(&address) => Seen::Resolved {
                            name: service.get_fullname().to_string(),
                            address,
                            port: service.get_port(),
                        },
                        None => continue,
                    }
                }
                ServiceEvent::ServiceRemoved(_, name) => Seen::Removed { name },
                _ => continue,
            };
            if tx.send(seen).await.is_err() {
                return;
            }
        }
        warn!("the Moonlight browse stopped; hosts won't be found until the app restarts");
    });
    Ok(rx)
}

// Ported from Moonshine (https://github.com/hgaiser/moonshine), BSD-2-Clause, © 2024 Hans Gaiser; see LICENSE-MOONSHINE.
// Changed: no network-interface or gethostname dependency (the responder's host name derives from the host's
// name, addresses are tracked by the daemon), failures are returned to the caller.

//! mDNS advertisement of `_nvstream._tcp`, which is how Moonlight finds a host
//! on its network.

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{ServiceDaemon, ServiceInfo};

const SERVICE_TYPE: &str = "_nvstream._tcp.local.";

/// Advertises the host until dropped.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

/// A DNS-safe host label from the host's display name.
fn label(name: &str) -> String {
    let mut label: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    label = label.trim_matches('-').to_owned();
    if label.is_empty() {
        label = "host".into();
    }
    label.truncate(40);
    label
}

impl Advertisement {
    /// Starts advertising `name` on `port`. When `bind` is a specific address
    /// that is the one advertised; for an unspecified address the daemon
    /// advertises every address the machine has, and follows changes.
    pub fn start(name: &str, bind: IpAddr, port: u16) -> Result<Self, mdns_sd::Error> {
        let daemon = ServiceDaemon::new()?;
        // A name distinct from the machine's own, so a system responder
        // (avahi) never sees conflicting records and renames itself.
        let hostname = format!("{}-gamestream.local.", label(name));
        let props: HashMap<String, String> = HashMap::new();
        let info = if bind.is_unspecified() {
            ServiceInfo::new(SERVICE_TYPE, name, &hostname, "", port, props)?.enable_addr_auto()
        } else {
            ServiceInfo::new(SERVICE_TYPE, name, &hostname, bind, port, props)?
        };
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;
        tracing::debug!("advertising {fullname} as {hostname}");
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        // Unregister first so goodbye packets go out, then stop the daemon.
        if let Ok(done) = self.daemon.unregister(&self.fullname) {
            let _ = done.recv_timeout(std::time::Duration::from_secs(1));
        }
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_dns_safe() {
        assert_eq!(label("Cha Node #1"), "cha-node--1");
        assert_eq!(label("!!!"), "host");
        assert_eq!(label(&"x".repeat(100)).len(), 40);
    }
}

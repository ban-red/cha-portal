//! What the front is told about the host it runs.

use std::net::IpAddr;
use std::time::Duration;

use crate::handoff::Capabilities;

/// The front's ports. Where the media is served is the [`Directory`]'s
/// business and comes with each launch.
///
/// [`Directory`]: crate::directory::Directory
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ports {
    pub http: u16,
    pub https: u16,
    pub rtsp: u16,
}

impl Default for Ports {
    /// What GameStream hosts use.
    fn default() -> Self {
        Self {
            http: 47989,
            https: 47984,
            rtsp: 48010,
        }
    }
}

#[derive(Clone, Debug)]
pub struct HostConfig {
    /// What clients call the host.
    pub name: String,
    /// The host's id: a UUID (32 hex digits, hyphens optional; clients such as
    /// moonlight-common-rust refuse anything else). Clients remember a host by
    /// it, so keep it across restarts; [`generate_unique_id`] makes one.
    pub unique_id: String,
    /// Address to listen on.
    pub bind: IpAddr,
    pub ports: Ports,
    /// What the media side can encode; `serverinfo` and RTSP `DESCRIBE` say so.
    pub capabilities: Capabilities,
    /// Parity packets per hundred data packets, for every stream.
    pub fec_percent: u8,
    /// Upper bound on the client's video packet size; 0 for none. The wire
    /// size is this plus 16 (plus 32 when video is encrypted).
    pub max_packet_size: usize,
    /// Let clients encrypt video (they ask for it; Moonlight leaves it off
    /// on a LAN by default). Control and audio encryption need no setting.
    pub video_encryption: bool,
    /// The MAC address `serverinfo` reports (for Wake-on-LAN); unknown if `None`.
    pub mac: Option<String>,
    /// Advertise on mDNS so clients find the host.
    pub mdns: bool,
    /// How long a pairing request waits for its PIN.
    pub pin_timeout: Duration,
    /// How long a client may take to send an HTTP or RTSP request.
    pub request_timeout: Duration,
    /// Connections served at once on each of nvhttp (HTTP and HTTPS) and RTSP.
    pub max_connections: usize,
    /// Moonlight's "Unpair" sends `/unpair?uniqueid=...` over plain HTTP, where
    /// the host cannot tell who asks: with this on, anyone on the network who
    /// knows a paired client's `uniqueid` (many clients share one, such as
    /// `0123456789ABCDEF`) can unpair it and end its session. Off (the
    /// default), only a client's own HTTPS request unpairs it, and devices are
    /// otherwise removed where they were paired (the portal).
    pub unauthenticated_unpair: bool,
}

/// A fresh host id: 32 random hex digits.
pub fn generate_unique_id() -> String {
    use aws_lc_rs::rand::{SecureRandom, SystemRandom};
    let mut raw = [0u8; 16];
    SystemRandom::new()
        .fill(&mut raw)
        .expect("the system has randomness");
    hex::encode_upper(raw)
}

impl HostConfig {
    pub fn new(name: impl Into<String>, unique_id: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            unique_id: unique_id.into(),
            bind: IpAddr::from([0, 0, 0, 0]),
            ports: Ports::default(),
            capabilities: Capabilities::default(),
            fec_percent: 20,
            max_packet_size: 0,
            video_encryption: false,
            mac: None,
            mdns: true,
            pin_timeout: Duration::from_secs(300),
            request_timeout: Duration::from_secs(10),
            max_connections: 64,
            unauthenticated_unpair: false,
        }
    }
}

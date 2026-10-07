//! Portal addresses: what the user types, and the normalised origin a token
//! is bound to.

use anyhow::{Result, bail};
use url::{Host, Url};

/// Set to `true` to allow a portal over plain `http://` that isn't on this
/// machine (a dev portal on the LAN).
pub const ALLOW_INSECURE_ENV: &str = "CHA_ALLOW_INSECURE_PORTAL";

/// Whether the environment allows plain-http portals anywhere.
pub fn insecure_allowed() -> bool {
    std::env::var(ALLOW_INSECURE_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("true"))
}

/// A portal address as the user types it (`https://portal.example`,
/// `portal.example:7676`; https unless said otherwise) as an origin:
/// `scheme://host[:port]`, lower-case, default port dropped, no path.
pub fn normalize_origin(input: &str) -> Result<String> {
    normalize_origin_with(input, insecure_allowed())
}

/// As [`normalize_origin`] with the insecure rule given rather than read.
pub fn normalize_origin_with(input: &str, allow_insecure: bool) -> Result<String> {
    let input = input.trim();
    if input.is_empty() {
        bail!("enter the portal's address");
    }
    let text = if input.contains("://") {
        input.to_string()
    } else {
        format!("https://{input}")
    };
    origin_of(&text, allow_insecure)
}

/// The origin of a URL that must name its scheme (a `cha://` link's `portal`).
pub(crate) fn strict_origin(input: &str, allow_insecure: bool) -> Result<String> {
    let input = input.trim();
    if !input.contains("://") {
        bail!("the portal address {input:?} has no scheme");
    }
    origin_of(input, allow_insecure)
}

fn origin_of(text: &str, allow_insecure: bool) -> Result<String> {
    let url =
        Url::parse(text).map_err(|e| anyhow::anyhow!("{text:?} isn't a portal address: {e}"))?;
    let scheme = url.scheme();
    if scheme != "http" && scheme != "https" {
        bail!("a portal address is http or https, not {scheme}");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("a portal address has no user name or password in it");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("a portal address is just the host and port");
    }
    let Some(host) = url.host() else {
        bail!("{text:?} has no host");
    };
    if scheme == "http" && !allow_insecure && !is_loopback(&host) {
        bail!(
            "refusing a portal over plain http; use https, or set {ALLOW_INSECURE_ENV}=true for a dev portal"
        );
    }
    Ok(url.origin().ascii_serialization())
}

fn is_loopback(host: &Host<&str>) -> bool {
    match host {
        Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => ip.is_loopback(),
    }
}

/// Just the host (and port), for naming a portal in the UI.
pub fn host_label(origin: &str) -> String {
    origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> Result<String> {
        normalize_origin_with(s, false)
    }

    #[test]
    fn addresses_normalise_to_an_origin() {
        assert_eq!(n("portal.example").unwrap(), "https://portal.example");
        assert_eq!(n(" Portal.Example/ ").unwrap(), "https://portal.example");
        assert_eq!(
            n("portal.example:7676").unwrap(),
            "https://portal.example:7676"
        );
        assert_eq!(
            n("https://portal.example:443/x/y").unwrap(),
            "https://portal.example"
        );
        assert_eq!(n("localhost:7676").unwrap(), "https://localhost:7676");
    }

    #[test]
    fn plain_http_only_for_this_machine() {
        assert_eq!(n("http://localhost:8090").unwrap(), "http://localhost:8090");
        assert_eq!(n("http://127.0.0.1:8090").unwrap(), "http://127.0.0.1:8090");
        assert_eq!(n("http://[::1]:8090").unwrap(), "http://[::1]:8090");
        assert!(n("http://192.168.1.5:8090").is_err());
        assert!(n("http://portal.example").is_err());
        assert_eq!(
            normalize_origin_with("http://192.168.1.5:8090", true).unwrap(),
            "http://192.168.1.5:8090"
        );
    }

    #[test]
    fn odd_addresses_are_refused() {
        assert!(n("").is_err());
        assert!(n("ftp://portal.example").is_err());
        assert!(n("https://user:pw@portal.example").is_err());
        assert!(n("https://portal.example/?x=1").is_err());
        assert!(n("https://").is_err());
    }
}

//! `cha://connect?portal=…&ticket=…[&launch=…]`, the link the portal's
//! "Open in Cha Player" makes. Parsed strictly: this comes from outside the
//! app, so nothing but this one shape is accepted.

use anyhow::{Result, bail};
use url::Url;

use crate::origin::{insecure_allowed, strict_origin};

/// A parsed connect link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectLink {
    /// The portal's origin (normalised).
    pub portal: String,
    /// The one-use ticket to swap for a device token.
    pub ticket: String,
    /// A catalog template id to launch once signed in.
    pub launch: Option<String>,
}

/// Parses a `cha://` link (the insecure-portal rule comes from the environment).
pub fn parse_connect_link(link: &str) -> Result<ConnectLink> {
    parse_connect_link_with(link, insecure_allowed())
}

pub fn parse_connect_link_with(link: &str, allow_insecure: bool) -> Result<ConnectLink> {
    let url = Url::parse(link.trim()).map_err(|e| anyhow::anyhow!("not a cha:// link: {e}"))?;
    if url.scheme() != "cha" {
        bail!("not a cha:// link");
    }
    if url.host_str() != Some("connect") {
        bail!("unknown cha:// action {:?}", url.host_str().unwrap_or(""));
    }
    if !matches!(url.path(), "" | "/")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.fragment().is_some()
    {
        bail!("unexpected parts in the cha:// link");
    }
    let (mut portal, mut ticket, mut launch) = (None, None, None);
    for (key, value) in url.query_pairs() {
        let slot = match key.as_ref() {
            "portal" => &mut portal,
            "ticket" => &mut ticket,
            "launch" => &mut launch,
            other => bail!("unexpected {other:?} in the cha:// link"),
        };
        if slot.replace(value.into_owned()).is_some() {
            bail!("{key:?} appears twice in the cha:// link");
        }
    }
    let Some(portal) = portal else {
        bail!("the cha:// link names no portal");
    };
    let Some(ticket) = ticket else {
        bail!("the cha:// link has no ticket");
    };
    if ticket.is_empty()
        || ticket.len() > 200
        || !ticket
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("the cha:// link's ticket is malformed");
    }
    if let Some(l) = &launch
        && (l.is_empty()
            || l.len() > 64
            || !l
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
    {
        bail!("the cha:// link's launch id is malformed");
    }
    Ok(ConnectLink {
        portal: strict_origin(&portal, allow_insecure)?,
        ticket,
        launch,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<ConnectLink> {
        parse_connect_link_with(s, false)
    }

    #[test]
    fn a_connect_link_parses() {
        let l =
            p("cha://connect?portal=https%3A%2F%2Fportal.example&ticket=abc_DEF-123&launch=chrome")
                .unwrap();
        assert_eq!(
            l,
            ConnectLink {
                portal: "https://portal.example".into(),
                ticket: "abc_DEF-123".into(),
                launch: Some("chrome".into()),
            }
        );
        let l = p("cha://connect/?portal=https%3A%2F%2Fportal.example%3A7676%2F&ticket=t").unwrap();
        assert_eq!(l.portal, "https://portal.example:7676");
        assert_eq!(l.launch, None);
    }

    #[test]
    fn only_the_connect_action_with_known_parameters() {
        let ok = "portal=https%3A%2F%2Fp.example&ticket=t";
        assert!(p(&format!("cha://signin?{ok}")).is_err());
        assert!(p(&format!("chax://connect?{ok}")).is_err());
        assert!(p(&format!("https://connect?{ok}")).is_err());
        assert!(p(&format!("cha://connect/extra?{ok}")).is_err());
        assert!(p(&format!("cha://connect?{ok}&x=1")).is_err());
        assert!(p(&format!("cha://connect?{ok}&ticket=u")).is_err());
        assert!(p(&format!("cha://connect?{ok}#frag")).is_err());
        assert!(p("cha://connect?ticket=t").is_err());
        assert!(p("cha://connect?portal=https%3A%2F%2Fp.example").is_err());
    }

    #[test]
    fn the_portal_must_be_an_http_origin() {
        let t = |portal: &str| p(&format!("cha://connect?portal={portal}&ticket=t"));
        assert!(t("file%3A%2F%2F%2Fetc").is_err());
        assert!(t("javascript%3Aalert(1)").is_err());
        assert!(t("portal.example").is_err());
        assert!(t("http%3A%2F%2Fportal.example").is_err());
        assert!(t("https%3A%2F%2Fuser%3Apw%40portal.example").is_err());
        assert!(t("http%3A%2F%2Flocalhost%3A8090").is_ok());
    }

    #[test]
    fn tickets_and_launch_ids_are_plain() {
        let t = |ticket: &str, launch: &str| {
            p(&format!(
                "cha://connect?portal=https%3A%2F%2Fp.example&ticket={ticket}&launch={launch}"
            ))
        };
        assert!(t("a%20b", "x").is_err());
        assert!(t("t", "..%2F..").is_err());
        assert!(t("t", "").is_err());
        assert!(t(&"a".repeat(201), "x").is_err());
    }
}

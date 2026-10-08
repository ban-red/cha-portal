//! What this node's owner lets custom environments ask for (ADR 0021): the
//! mode (`CHA_HOST_OPTIONS`) and the allowlist (`CHA_HOST_MOUNTS`,
//! `CHA_HOST_PORTS`, `CHA_HOST_CAPS`, `CHA_HOST_DEVICES`).
//!
//! The configuration is read once at start; a value that doesn't parse stops
//! the agent with the variable's name in the message, like the other `CHA_*`
//! settings. What goes to the portal is [`HostPolicy`] (names and read-only
//! flags, never host paths); the paths stay here.

use std::path::PathBuf;

use anyhow::{Result, bail};
use cha_wire::{
    AllowedMount, HostOptionsMode, HostPolicy, PortRange, Protocol, valid_abs_path, valid_cap_name,
    valid_mount_name,
};

/// A mount the owner named: `media=/mnt/media:ro`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredMount {
    pub name: String,
    /// On the host, and as Docker is given it.
    pub path: PathBuf,
    /// `:ro`: a ceiling, whatever a custom environment asks.
    pub read_only: bool,
}

/// `CHA_HOST_OPTIONS` and the lists beside it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostOptionsConfig {
    pub mode: HostOptionsMode,
    pub mounts: Vec<ConfiguredMount>,
    pub ports: Vec<PortRange>,
    pub caps: Vec<String>,
    pub devices: Vec<String>,
}

impl HostOptionsConfig {
    /// Reads the five settings; empty ones are unset (compose passes unset
    /// variables as empty strings).
    pub fn from_settings(
        mode: &str,
        mounts: &str,
        ports: &str,
        caps: &str,
        devices: &str,
    ) -> Result<Self> {
        Ok(Self {
            mode: parse_mode(mode)?,
            mounts: parse_mounts(mounts)?,
            ports: parse_ports(ports)?,
            caps: parse_caps(caps)?,
            devices: parse_devices(devices)?,
        })
    }

    /// Whether any of the lists is set.
    pub fn has_lists(&self) -> bool {
        !(self.mounts.is_empty()
            && self.ports.is_empty()
            && self.caps.is_empty()
            && self.devices.is_empty())
    }

    /// What the inventory tells the portal. Nothing listed while the mode is
    /// `off`, whatever the lists say.
    pub fn policy(&self) -> HostPolicy {
        if self.mode == HostOptionsMode::Off {
            return HostPolicy::default();
        }
        HostPolicy {
            mode: self.mode,
            mounts: self
                .mounts
                .iter()
                .map(|m| AllowedMount {
                    name: m.name.clone(),
                    read_only: m.read_only,
                })
                .collect(),
            ports: self.ports.clone(),
            caps: self.caps.clone(),
            devices: self.devices.clone(),
        }
    }

    pub fn mount(&self, name: &str) -> Option<&ConfiguredMount> {
        self.mounts.iter().find(|m| m.name == name)
    }
}

fn entries(value: &str) -> impl Iterator<Item = &str> {
    value.split(',').map(str::trim).filter(|e| !e.is_empty())
}

/// `off`, `allowlist` or `full`; empty is `off`.
pub fn parse_mode(value: &str) -> Result<HostOptionsMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "off" => Ok(HostOptionsMode::Off),
        "allowlist" => Ok(HostOptionsMode::Allowlist),
        "full" => Ok(HostOptionsMode::Full),
        other => bail!("CHA_HOST_OPTIONS must be off, allowlist or full, not {other:?}"),
    }
}

/// `media=/mnt/media:ro,roms=/srv/roms`.
pub fn parse_mounts(value: &str) -> Result<Vec<ConfiguredMount>> {
    let mut out: Vec<ConfiguredMount> = Vec::new();
    for entry in entries(value) {
        let Some((name, rest)) = entry.split_once('=') else {
            bail!("CHA_HOST_MOUNTS: {entry:?} isn't name=/host/path[:ro]");
        };
        let name = name.trim();
        if !valid_mount_name(name) {
            bail!(
                "CHA_HOST_MOUNTS: {name:?} isn't a mount name (lowercase letters, digits, - and _, at most 32)"
            );
        }
        let rest = rest.trim();
        let (path, read_only) = match rest.strip_suffix(":ro") {
            Some(path) => (path, true),
            None => (rest, false),
        };
        if !valid_abs_path(path) || path == "/" {
            bail!(
                "CHA_HOST_MOUNTS: {path:?} (for {name}) must be an absolute path, not /, with no ., .. or doubled slashes"
            );
        }
        if out.iter().any(|m| m.name == name) {
            bail!("CHA_HOST_MOUNTS: {name} is given twice");
        }
        out.push(ConfiguredMount {
            name: name.to_string(),
            path: PathBuf::from(path),
            read_only,
        });
    }
    Ok(out)
}

/// `27015-27030/udp,25565/tcp`.
pub fn parse_ports(value: &str) -> Result<Vec<PortRange>> {
    let mut out = Vec::new();
    for entry in entries(value) {
        let Some((ports, protocol)) = entry.split_once('/') else {
            bail!("CHA_HOST_PORTS: {entry:?} needs a protocol (27015-27030/udp or 25565/tcp)");
        };
        let protocol = match protocol.trim().to_ascii_lowercase().as_str() {
            "tcp" => Protocol::Tcp,
            "udp" => Protocol::Udp,
            other => bail!("CHA_HOST_PORTS: {entry:?}: the protocol is tcp or udp, not {other:?}"),
        };
        let number = |s: &str| -> Result<u16> {
            match s.trim().parse::<u16>() {
                Ok(n) if n > 0 => Ok(n),
                _ => bail!("CHA_HOST_PORTS: {entry:?}: {s:?} isn't a port (1-65535)"),
            }
        };
        let (start, end) = match ports.split_once('-') {
            Some((a, b)) => (number(a)?, number(b)?),
            None => {
                let n = number(ports)?;
                (n, n)
            }
        };
        if start > end {
            bail!("CHA_HOST_PORTS: {entry:?}: the range runs backwards");
        }
        out.push(PortRange {
            start,
            end,
            protocol,
        });
    }
    Ok(out)
}

/// `SYS_NICE,NET_RAW`.
pub fn parse_caps(value: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for cap in entries(value) {
        if !valid_cap_name(cap) {
            bail!("CHA_HOST_CAPS: {cap:?} isn't a capability (SYS_NICE, upper case, without CAP_)");
        }
        if !out.iter().any(|c| c == cap) {
            out.push(cap.to_string());
        }
    }
    Ok(out)
}

/// `/dev/dri/card1`.
pub fn parse_devices(value: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for dev in entries(value) {
        if !valid_abs_path(dev) || !dev.starts_with("/dev/") || dev == "/dev/" {
            bail!("CHA_HOST_DEVICES: {dev:?} isn't a device path under /dev");
        }
        if !out.iter().any(|d| d == dev) {
            out.push(dev.to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_mode() {
        assert_eq!(parse_mode("").unwrap(), HostOptionsMode::Off);
        assert_eq!(parse_mode(" Off ").unwrap(), HostOptionsMode::Off);
        assert_eq!(parse_mode("allowlist").unwrap(), HostOptionsMode::Allowlist);
        assert_eq!(parse_mode("FULL").unwrap(), HostOptionsMode::Full);
        let err = parse_mode("yes").unwrap_err().to_string();
        assert!(err.contains("CHA_HOST_OPTIONS"), "{err}");
    }

    #[test]
    fn reads_mounts_with_a_read_only_ceiling() {
        let mounts = parse_mounts("media=/mnt/media:ro, roms=/srv/roms ,").unwrap();
        assert_eq!(
            mounts,
            vec![
                ConfiguredMount {
                    name: "media".into(),
                    path: "/mnt/media".into(),
                    read_only: true,
                },
                ConfiguredMount {
                    name: "roms".into(),
                    path: "/srv/roms".into(),
                    read_only: false,
                },
            ]
        );
        assert!(parse_mounts("").unwrap().is_empty());
        for bad in [
            "media",
            "Media=/x",
            "=/x",
            "media=rel/path",
            "media=/",
            "media=/a/../b",
            "media=/a:rw",
            "media=",
            "a=/x,a=/y",
        ] {
            let err = parse_mounts(bad).unwrap_err().to_string();
            assert!(err.contains("CHA_HOST_MOUNTS"), "{bad}: {err}");
        }
    }

    #[test]
    fn reads_ports_and_ranges() {
        let ports = parse_ports("27015-27030/udp, 25565/tcp").unwrap();
        assert_eq!(
            ports,
            vec![
                PortRange {
                    start: 27015,
                    end: 27030,
                    protocol: Protocol::Udp,
                },
                PortRange {
                    start: 25565,
                    end: 25565,
                    protocol: Protocol::Tcp,
                },
            ]
        );
        for bad in [
            "25565",
            "25565/sctp",
            "0/tcp",
            "70000/tcp",
            "30-20/udp",
            "a-b/tcp",
            "-5/tcp",
        ] {
            let err = parse_ports(bad).unwrap_err().to_string();
            assert!(err.contains("CHA_HOST_PORTS"), "{bad}: {err}");
        }
    }

    #[test]
    fn reads_capabilities_and_devices() {
        assert_eq!(
            parse_caps("SYS_NICE, NET_RAW,SYS_NICE").unwrap(),
            vec!["SYS_NICE", "NET_RAW"]
        );
        for bad in ["sys_nice", "CAP_SYS_NICE", "NET ADMIN"] {
            assert!(parse_caps(bad).is_err(), "{bad}");
        }
        assert_eq!(
            parse_devices("/dev/dri/card1").unwrap(),
            vec!["/dev/dri/card1"]
        );
        for bad in ["dri/card1", "/etc/passwd", "/dev/../etc", "/dev/"] {
            assert!(parse_devices(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_policy_names_but_never_locates() {
        let config = HostOptionsConfig::from_settings(
            "allowlist",
            "media=/mnt/media:ro,roms=/srv/roms",
            "27015-27030/udp",
            "SYS_NICE",
            "/dev/dri/card1",
        )
        .unwrap();
        let policy = config.policy();
        assert_eq!(policy.mode, HostOptionsMode::Allowlist);
        assert_eq!(
            policy.mounts,
            vec![
                AllowedMount {
                    name: "media".into(),
                    read_only: true,
                },
                AllowedMount {
                    name: "roms".into(),
                    read_only: false,
                },
            ]
        );
        assert!(!serde_json::to_string(&policy).unwrap().contains("/mnt"));
        // Off reports nothing, whatever is listed.
        let off = HostOptionsConfig {
            mode: HostOptionsMode::Off,
            ..config
        };
        assert_eq!(off.policy(), HostPolicy::default());
        assert!(off.has_lists());
    }

    #[test]
    fn a_bad_value_names_its_variable() {
        let err = HostOptionsConfig::from_settings("full", "", "80", "", "")
            .unwrap_err()
            .to_string();
        assert!(err.contains("CHA_HOST_PORTS"), "{err}");
    }
}

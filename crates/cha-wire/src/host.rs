//! What a custom environment may ask of its node beyond its template (ADR
//! 0021): extra variables, and host options (mounts, ports, capabilities,
//! devices), which each node allows or not from its own configuration.
//!
//! The portal never grants a node more than its owner configured: the node
//! reports its policy ([`HostPolicy`]) in its inventory, the portal places a
//! spec only on a node whose policy [`allows`](HostPolicy::allows) it, and the
//! node checks again before it starts anything. Both sides run the same
//! checks, from here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// [`Inventory::spec_features`](crate::Inventory::spec_features): the agent
/// adds [`EnvironmentSpec::env`](crate::EnvironmentSpec::env) to the app.
pub const SPEC_FEATURE_ENV: &str = "env";
/// The agent keeps an environment's data under
/// [`EnvironmentSpec::data_template`](crate::EnvironmentSpec::data_template).
pub const SPEC_FEATURE_DATA_TEMPLATE: &str = "data-template";
/// The agent reads [`EnvironmentSpec::host`](crate::EnvironmentSpec::host).
pub const SPEC_FEATURE_HOST_OPTIONS: &str = "host-options";

/// At most this many extra variables, and this many bytes of them in all.
pub const MAX_ENV_VARS: usize = 64;
pub const MAX_ENV_BYTES: usize = 16 * 1024;

/// Variables the node sets for the app; a custom environment may not.
/// Anything starting with `CHA_` is the node's too.
pub const RESERVED_ENV: &[&str] = &[
    "HOME",
    "USER",
    "XDG_RUNTIME_DIR",
    "WAYLAND_DISPLAY",
    "PULSE_SERVER",
    "DISPLAY",
];

/// Paths in the app's container the agent mounts itself; nothing may be
/// mounted at or under them, nor over a parent of one.
pub const RESERVED_TARGETS: &[&str] = &[
    "/run/cha",
    "/dev/input",
    "/home/cha",
    crate::SHARED_MOUNT_ROOT,
];

/// Why a set of extra variables is refused, if it is.
pub fn check_env(env: &BTreeMap<String, String>) -> Result<(), String> {
    if env.len() > MAX_ENV_VARS {
        return Err(format!("at most {MAX_ENV_VARS} variables"));
    }
    let bytes: usize = env.iter().map(|(k, v)| k.len() + v.len() + 1).sum();
    if bytes > MAX_ENV_BYTES {
        return Err(format!("at most {} KiB of variables", MAX_ENV_BYTES / 1024));
    }
    for (name, value) in env {
        if !valid_env_name(name) {
            return Err(format!(
                "{name:?} isn't a variable name (letters, digits and _, not starting with a digit)"
            ));
        }
        if name.starts_with("CHA_") || RESERVED_ENV.contains(&name.as_str()) {
            return Err(format!("{name} is set by the node"));
        }
        if value.contains('\0') {
            return Err(format!("{name} holds a NUL byte"));
        }
    }
    Ok(())
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.len() <= 128
}

/// What a node lets custom environments ask for (`CHA_HOST_OPTIONS`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostOptionsMode {
    /// Nothing: the default.
    #[default]
    Off,
    /// What the owner listed, by name.
    Allowlist,
    /// Anything [`HostOptions`] can say, host paths and privilege included.
    Full,
}

/// One host option request: what the app's container gets besides what the
/// agent gives every app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostOptions {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<HostMount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<HostPort>,
    /// Capability names without `CAP_` (`SYS_NICE`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_add: Vec<String>,
    /// Host device paths, passed read-write-mknod as Docker's `Devices`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<String>,
    /// `full` only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub privileged: bool,
    /// `full` only: the host's network instead of Docker's bridge. Ports are
    /// then meaningless and refused.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub network_host: bool,
    /// `full` only: Docker `SecurityOpt` entries, added to the profile's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_opt: Vec<String>,
}

impl HostOptions {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostMount {
    pub source: MountSource,
    /// Absolute path in the app's container.
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
}

/// Where a mount comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MountSource {
    /// A mount the node's owner named (`CHA_HOST_MOUNTS=media=/mnt/media`).
    Named { name: String },
    /// `full` only: a host path.
    Path { path: String },
    /// `full` only: a network share, as a volume of Docker's `local` driver
    /// made for the launch and removed with the environment.
    #[serde(rename_all = "camelCase")]
    Network {
        fs_type: NetworkFs,
        /// `:/export/media` for NFS, `//server/share` for CIFS.
        device: String,
        /// The driver's `o`: `addr=10.0.0.5,nfsvers=4` or `username=…`.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        options: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkFs {
    Nfs,
    Nfs4,
    Cifs,
}

impl NetworkFs {
    pub fn as_str(self) -> &'static str {
        match self {
            NetworkFs::Nfs => "nfs",
            NetworkFs::Nfs4 => "nfs4",
            NetworkFs::Cifs => "cifs",
        }
    }

    /// The `type` and `o` options of Docker's `local` driver for a share
    /// with these `options`. The driver hands them to the kernel's mount
    /// call, which doesn't negotiate a version: `type=nfs4` and `nfsvers=4`
    /// fail with "protocol not supported" against a 4.2 server (Unraid,
    /// measured on iolinux). So NFSv4 is `type=nfs` with `vers=4.2`, unless
    /// the options name a version themselves.
    pub fn driver_options(self, options: &str) -> (&'static str, String) {
        match self {
            NetworkFs::Nfs4 => {
                let versioned = options
                    .split(',')
                    .any(|o| o.starts_with("vers=") || o.starts_with("nfsvers="));
                let o = match (versioned, options.is_empty()) {
                    (true, _) => options.to_string(),
                    (false, true) => "vers=4.2".to_string(),
                    (false, false) => format!("{options},vers=4.2"),
                };
                ("nfs", o)
            }
            other => (other.as_str(), options.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPort {
    /// The port the app listens on in its container.
    pub container: u16,
    pub protocol: Protocol,
    /// The node's port. Absent: the node picks a free one from its allowed
    /// ranges (any free port in `full` mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Udp => "udp",
        }
    }
}

/// What the node allows, as its inventory reports it. Host paths behind
/// named mounts are not in it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPolicy {
    pub mode: HostOptionsMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<AllowedMount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PortRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub caps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AllowedMount {
    pub name: String,
    /// The owner allows it read-only: a request for read-write gets
    /// read-only.
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
    pub protocol: Protocol,
}

impl PortRange {
    pub fn contains(&self, port: u16, protocol: Protocol) -> bool {
        self.protocol == protocol && (self.start..=self.end).contains(&port)
    }
}

/// Mount names: what `CHA_HOST_MOUNTS` may call one.
pub fn valid_mount_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Capability names as Docker takes them, without `CAP_`.
pub fn valid_cap_name(cap: &str) -> bool {
    !cap.is_empty()
        && cap.len() <= 32
        && !cap.starts_with("CAP_")
        && cap
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// An absolute, normalised path: starts with `/`, no `.` or `..` parts, no
/// empty parts, no trailing `/` (except `/` itself), no NUL or `,`/`:`
/// (Docker's mount syntax).
pub fn valid_abs_path(path: &str) -> bool {
    path.starts_with('/')
        && path.len() <= 1024
        && !path.contains(['\0', ',', ':'])
        && (path == "/"
            || path[1..]
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."))
}

/// `a` is `b` or under it.
fn under(a: &str, b: &str) -> bool {
    b == "/" || a == b || a.strip_prefix(b).is_some_and(|r| r.starts_with('/'))
}

/// Paths that are the node's Docker socket in any spelling a request might
/// use; the node also refuses its own configured socket.
const DOCKER_SOCKETS: &[&str] = &["/var/run/docker.sock", "/run/docker.sock"];

impl HostOptions {
    /// Whether the request is well-formed, whatever any node allows: paths,
    /// names, reserved targets, duplicates. Both sides run it.
    pub fn check_shape(&self) -> Result<(), String> {
        let mut targets: Vec<&str> = Vec::new();
        for m in &self.mounts {
            if !valid_abs_path(&m.target) || m.target == "/" {
                return Err(format!("{:?} isn't an absolute path to mount at", m.target));
            }
            if let Some(r) = RESERVED_TARGETS
                .iter()
                .find(|r| under(&m.target, r) || under(r, &m.target))
            {
                return Err(format!(
                    "{} would cover {r}, which the node mounts itself",
                    m.target
                ));
            }
            if m.target.starts_with("/dev/hidraw") {
                return Err(format!("{} is where the node puts controllers", m.target));
            }
            if let Some(t) = targets
                .iter()
                .find(|t| under(&m.target, t) || under(t, &m.target))
            {
                return Err(format!("{} and {t} overlap", m.target));
            }
            targets.push(&m.target);
            match &m.source {
                MountSource::Named { name } => {
                    if !valid_mount_name(name) {
                        return Err(format!("{name:?} isn't a mount name"));
                    }
                }
                MountSource::Path { path } => {
                    if !valid_abs_path(path) {
                        return Err(format!("{path:?} isn't an absolute host path"));
                    }
                    if DOCKER_SOCKETS.iter().any(|s| under(s, path)) {
                        return Err(format!("{path} holds the node's Docker socket"));
                    }
                }
                MountSource::Network {
                    device, options, ..
                } => {
                    if device.is_empty() || device.contains(['\0', ',']) {
                        return Err(format!("{device:?} isn't a share to mount"));
                    }
                    if options.contains('\0') {
                        return Err("the share's options hold a NUL byte".into());
                    }
                }
            }
        }
        let mut seen = Vec::new();
        for p in &self.ports {
            if p.container == 0 || p.host == Some(0) {
                return Err("port 0 isn't a port".into());
            }
            if seen.contains(&(p.container, p.protocol)) {
                return Err(format!(
                    "{}/{} is listed twice",
                    p.container,
                    p.protocol.as_str()
                ));
            }
            seen.push((p.container, p.protocol));
        }
        if self.network_host && !self.ports.is_empty() {
            return Err("ports mean nothing on the host's network".into());
        }
        for cap in &self.cap_add {
            if !valid_cap_name(cap) {
                return Err(format!(
                    "{cap:?} isn't a capability (SYS_NICE, without CAP_)"
                ));
            }
        }
        for dev in &self.devices {
            if !valid_abs_path(dev) || !dev.starts_with("/dev/") {
                return Err(format!("{dev:?} isn't a device under /dev"));
            }
        }
        for opt in &self.security_opt {
            if opt.is_empty() || opt.contains('\0') {
                return Err(format!("{opt:?} isn't a security option"));
            }
        }
        Ok(())
    }
}

impl HostPolicy {
    /// Every reason this node refuses the request; empty when it allows it.
    /// Read-only mounts asked for read-write are not a reason: the node
    /// mounts them read-only.
    pub fn refusals(&self, req: &HostOptions) -> Vec<String> {
        if req.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        match self.mode {
            HostOptionsMode::Off => out.push("allows no host options".into()),
            HostOptionsMode::Full => {}
            HostOptionsMode::Allowlist => {
                for m in &req.mounts {
                    match &m.source {
                        MountSource::Named { name } => {
                            if !self.mounts.iter().any(|a| a.name == *name) {
                                out.push(format!("has no mount named {name}"));
                            }
                        }
                        MountSource::Path { path } => {
                            out.push(format!("allows host paths ({path}) only in full mode"))
                        }
                        MountSource::Network { device, .. } => out.push(format!(
                            "allows network shares ({device}) only in full mode"
                        )),
                    }
                }
                for p in &req.ports {
                    let ok = match p.host {
                        Some(h) => self.ports.iter().any(|r| r.contains(h, p.protocol)),
                        None => self.ports.iter().any(|r| r.protocol == p.protocol),
                    };
                    if !ok {
                        let what = match p.host {
                            Some(h) => format!("{h}/{}", p.protocol.as_str()),
                            None => format!("{} ports", p.protocol.as_str()),
                        };
                        out.push(format!("doesn't allow {what}"));
                    }
                }
                for cap in &req.cap_add {
                    if !self.caps.contains(cap) {
                        out.push(format!("doesn't allow {cap}"));
                    }
                }
                for dev in &req.devices {
                    if !self.devices.contains(dev) {
                        out.push(format!("doesn't allow {dev}"));
                    }
                }
                if req.privileged {
                    out.push("allows privileged only in full mode".into());
                }
                if req.network_host {
                    out.push("allows the host's network only in full mode".into());
                }
                if !req.security_opt.is_empty() {
                    out.push("allows security options only in full mode".into());
                }
            }
        }
        out
    }

    pub fn allows(&self, req: &HostOptions) -> bool {
        self.refusals(req).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str, target: &str) -> HostMount {
        HostMount {
            source: MountSource::Named { name: name.into() },
            target: target.into(),
            read_only: false,
        }
    }

    #[test]
    fn env_names_and_reserved() {
        let mut env = BTreeMap::new();
        env.insert("PROTON_LOG".to_string(), "1".to_string());
        assert!(check_env(&env).is_ok());
        env.insert("CHA_WIDTH".into(), "1".into());
        assert!(check_env(&env).unwrap_err().contains("set by the node"));
        env.remove("CHA_WIDTH");
        env.insert("HOME".into(), "/x".into());
        assert!(check_env(&env).is_err());
        env.remove("HOME");
        env.insert("1BAD".into(), "x".into());
        assert!(check_env(&env).is_err());
    }

    #[test]
    fn shape_refuses_reserved_and_overlapping_targets() {
        let ok = HostOptions {
            mounts: vec![named("media", "/mnt/media")],
            ..Default::default()
        };
        assert!(ok.check_shape().is_ok());
        for bad in [
            "/home/cha/x",
            "/home",
            "/run/cha",
            "/dev/input/event0",
            "/",
            "rel",
            "/a/../b",
        ] {
            let req = HostOptions {
                mounts: vec![named("media", bad)],
                ..Default::default()
            };
            assert!(req.check_shape().is_err(), "{bad}");
        }
        let overlap = HostOptions {
            mounts: vec![named("a", "/mnt"), named("b", "/mnt/b")],
            ..Default::default()
        };
        assert!(overlap.check_shape().is_err());
        let socket = HostOptions {
            mounts: vec![HostMount {
                source: MountSource::Path {
                    path: "/var/run".into(),
                },
                target: "/x".into(),
                read_only: true,
            }],
            ..Default::default()
        };
        assert!(socket.check_shape().unwrap_err().contains("Docker socket"));
    }

    #[test]
    fn allowlist_names_what_it_lacks() {
        let policy = HostPolicy {
            mode: HostOptionsMode::Allowlist,
            mounts: vec![AllowedMount {
                name: "media".into(),
                read_only: true,
            }],
            ports: vec![PortRange {
                start: 27015,
                end: 27030,
                protocol: Protocol::Udp,
            }],
            caps: vec!["SYS_NICE".into()],
            devices: vec![],
        };
        let req = HostOptions {
            mounts: vec![named("media", "/mnt/media")],
            ports: vec![HostPort {
                container: 27015,
                protocol: Protocol::Udp,
                host: Some(27016),
            }],
            cap_add: vec!["SYS_NICE".into()],
            ..Default::default()
        };
        assert!(policy.allows(&req));
        let wider = HostOptions {
            ports: vec![HostPort {
                container: 80,
                protocol: Protocol::Tcp,
                host: None,
            }],
            cap_add: vec!["NET_ADMIN".into()],
            privileged: true,
            ..req.clone()
        };
        assert_eq!(policy.refusals(&wider).len(), 3);
        let off = HostPolicy::default();
        assert!(off.allows(&HostOptions::default()));
        assert!(!off.allows(&req));
        let full = HostPolicy {
            mode: HostOptionsMode::Full,
            ..Default::default()
        };
        assert!(full.allows(&wider));
    }

    #[test]
    fn nfs4_is_nfs_with_a_version() {
        assert_eq!(
            NetworkFs::Nfs4.driver_options("addr=10.0.0.5,ro"),
            ("nfs", "addr=10.0.0.5,ro,vers=4.2".to_string())
        );
        assert_eq!(
            NetworkFs::Nfs4.driver_options("addr=10.0.0.5,vers=4.1"),
            ("nfs", "addr=10.0.0.5,vers=4.1".to_string())
        );
        assert_eq!(
            NetworkFs::Cifs.driver_options("username=a"),
            ("cifs", "username=a".to_string())
        );
    }

    #[test]
    fn mount_sources_read_as_tagged_json() {
        let m: HostMount = serde_json::from_str(
            r#"{"source":{"kind":"network","fsType":"nfs","device":":/export","options":"addr=10.0.0.5"},"target":"/mnt/nas"}"#,
        )
        .unwrap();
        assert!(matches!(
            m.source,
            MountSource::Network {
                fs_type: NetworkFs::Nfs,
                ..
            }
        ));
        assert!(!m.read_only);
    }
}

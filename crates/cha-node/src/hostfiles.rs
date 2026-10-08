//! The files the node's owner installs on the host (udev rules, the AppArmor
//! profile, the module list), compared with the copies this agent was built
//! with. The agent never writes them: `deploy/node/host/install.sh`, run with
//! sudo, does. The agent only sees the host's directories through read-only
//! binds (`deploy/node/compose.yaml`), under `/run/host/etc`.

use std::path::Path;

use tracing::warn;

/// Where the compose file binds the host's `/etc` directories.
pub const HOST_ETC: &str = "/run/host/etc";

/// The command that brings the host's files up to date.
pub const FIX: &str = "sudo deploy/node/host/install.sh";

struct HostFile {
    /// Under the host's `/etc`, where install.sh puts it.
    path: &'static str,
    /// The copy this build carries.
    content: &'static str,
    /// Only meaningful on a host with AppArmor.
    apparmor: bool,
}

// Keep in step with install.sh, which installs every file in these places.
const FILES: [HostFile; 3] = [
    HostFile {
        path: "udev/rules.d/72-cha-virtual-pads.rules",
        content: include_str!("../../../deploy/node/host/72-cha-virtual-pads.rules"),
        apparmor: false,
    },
    HostFile {
        path: "apparmor.d/cha-sandbox",
        content: include_str!("../../../deploy/node/host/apparmor/cha-sandbox"),
        apparmor: true,
    },
    HostFile {
        path: "modules-load.d/cha.conf",
        content: include_str!("../../../deploy/node/host/modules-load.d/cha.conf"),
        apparmor: false,
    },
];

/// How the host's files compare with ours. Paths are as on the host
/// (`/etc/...`).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub missing: Vec<String>,
    pub outdated: Vec<String>,
    /// Files whose directory isn't bound into the agent: not known either way.
    pub unknown: Vec<String>,
    /// The engine has no AppArmor, so its profiles weren't looked for.
    pub apparmor_skipped: bool,
}

impl Report {
    /// Something on the host needs installing or updating.
    pub fn stale(&self) -> bool {
        !self.missing.is_empty() || !self.outdated.is_empty()
    }

    /// Which files, and why, in one phrase.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.missing.is_empty() {
            parts.push(format!("missing: {}", self.missing.join(", ")));
        }
        if !self.outdated.is_empty() {
            parts.push(format!("outdated: {}", self.outdated.join(", ")));
        }
        parts.join("; ")
    }
}

/// Compares each file under `etc` (the host's `/etc` as bound in) with ours.
/// `apparmor` is whether the Docker engine has AppArmor: when it hasn't (an
/// LXC container, say), the profile can't be loaded and isn't needed, so it
/// is left out.
pub fn compare(etc: &Path, apparmor: bool) -> Report {
    let mut report = Report {
        apparmor_skipped: !apparmor,
        ..Report::default()
    };
    for file in &FILES {
        if file.apparmor && !apparmor {
            continue;
        }
        let shown = format!("/etc/{}", file.path);
        let dir = Path::new(file.path).parent().unwrap_or(Path::new(""));
        let Ok(mut entries) = std::fs::read_dir(etc.join(dir)) else {
            report.unknown.push(shown);
            continue;
        };
        // The bind of a directory the host doesn't have is an empty one that
        // Docker makes: real AppArmor hosts always have files in /etc/apparmor.d.
        if file.apparmor && entries.next().is_none() {
            continue;
        }
        match std::fs::read(etc.join(file.path)) {
            Ok(bytes) if bytes == file.content.as_bytes() => {}
            Ok(_) => report.outdated.push(shown),
            Err(_) => report.missing.push(shown),
        }
    }
    report
}

/// At startup: one warning if the host's files are missing or outdated.
/// Doesn't stop the agent, and says nothing when the binds aren't there.
pub fn warn_if_stale(apparmor: bool) {
    let report = compare(Path::new(HOST_ETC), apparmor);
    if report.stale() {
        warn!("host files {} (run `{FIX}` on the node)", report.describe());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host `/etc` with every file as built, or only the directories.
    fn etc(with_files: bool) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for file in &FILES {
            let path = tmp.path().join(file.path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            if with_files {
                std::fs::write(&path, file.content).unwrap();
            }
        }
        // An AppArmor host's directory has other things in it.
        std::fs::write(tmp.path().join("apparmor.d/docker-default"), "").unwrap();
        tmp
    }

    #[test]
    fn current_files_report_nothing() {
        let tmp = etc(true);
        let report = compare(tmp.path(), true);
        assert_eq!(report, Report::default());
        assert!(!report.stale());
    }

    #[test]
    fn files_that_were_never_installed_are_missing() {
        let tmp = etc(false);
        let report = compare(tmp.path(), true);
        assert_eq!(report.missing.len(), 3);
        assert!(report.stale());
        assert!(
            report
                .describe()
                .starts_with("missing: /etc/udev/rules.d/72-cha-virtual-pads.rules")
        );
    }

    #[test]
    fn a_file_that_differs_is_outdated() {
        let tmp = etc(true);
        std::fs::write(
            tmp.path().join("udev/rules.d/72-cha-virtual-pads.rules"),
            "old\n",
        )
        .unwrap();
        let report = compare(tmp.path(), true);
        assert_eq!(
            report.outdated,
            ["/etc/udev/rules.d/72-cha-virtual-pads.rules"]
        );
        assert!(report.missing.is_empty());
        assert!(report.describe().starts_with("outdated: "));
    }

    #[test]
    fn without_the_binds_nothing_is_known() {
        let tmp = tempfile::tempdir().unwrap();
        let report = compare(tmp.path(), true);
        assert_eq!(report.unknown.len(), 3);
        assert!(!report.stale());
    }

    #[test]
    fn a_host_without_apparmor_isnt_asked_for_the_profile() {
        let tmp = etc(true);
        std::fs::remove_file(tmp.path().join("apparmor.d/cha-sandbox")).unwrap();
        std::fs::remove_file(tmp.path().join("apparmor.d/docker-default")).unwrap();
        assert_eq!(compare(tmp.path(), true), Report::default());
    }

    #[test]
    fn an_engine_without_apparmor_isnt_asked_for_the_profile() {
        let tmp = etc(false);
        let report = compare(tmp.path(), false);
        assert_eq!(report.missing.len(), 2);
        assert!(!report.missing.iter().any(|f| f.contains("apparmor")));
        assert!(report.apparmor_skipped);
        // The other files are still compared.
        let tmp = etc(true);
        let report = compare(tmp.path(), false);
        assert!(!report.stale());
    }
}

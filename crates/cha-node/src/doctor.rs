//! `cha-node --doctor`: whether this machine can run environments, and how to
//! fix what it can't. It changes nothing. Fixes that need root on the host
//! (kernel modules, CDI, udev) are printed for the owner to run.
//!
//! In the agent's container (`docker compose run --rm agent --doctor`) it
//! sees the host through the Docker engine: GPU and device checks run in
//! short-lived probe containers configured like a streamer.

use std::path::Path;
use std::time::{Duration, SystemTime};

use cha_wire::{HOME_VOLUME_PREFIX, parse_home_volume_name};
use serde_json::json;

use crate::Identity;
use crate::docker::{Docker, names_registry};
use crate::environments::{
    APP_UID, DockerConfig, SANDBOX_APPARMOR, browser_seccomp, catalog_images, catalog_per_user,
    nvidia_present,
};
use crate::hostfiles;
use crate::storage::DataRoot;

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Below this much free space under the data root, the doctor warns.
const LOW_SPACE_BYTES: u64 = 20 << 30;
/// Media tokens live 60 s: a clock this far off breaks connecting.
const SKEW_FAIL_SECS: f64 = 30.0;
const SKEW_WARN_SECS: f64 = 5.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

struct Check {
    level: Level,
    name: String,
    detail: String,
    fix: Option<String>,
}

fn check(level: Level, name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        level,
        name: name.into(),
        detail: detail.into(),
        fix: None,
    }
}

impl Check {
    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

/// Runs every check, prints the report; true when nothing failed.
pub async fn run(
    docker: &Docker,
    config: &DockerConfig,
    identity: Option<&Identity>,
    state_dir: &Path,
    moonlight: bool,
) -> bool {
    let mut checks = Vec::new();
    let engine = docker.version().await;
    checks.push(match &engine {
        Ok(version) => check(Level::Ok, "Docker engine", format!("Docker {version}")),
        Err(err) => check(Level::Fail, "Docker engine", format!("{err:#}"))
            .fix("mount the engine's socket into the agent (/var/run/docker.sock), or pass --docker-socket"),
    });
    if engine.is_ok() {
        checks.push(images(docker, config).await);
        checks.push(gpu(docker, config).await);
        checks.extend(devices(docker, config).await);
        checks.push(pyrowave(docker, config).await);
        checks.push(gamepads(docker, config).await);
        checks.push(uhid_pads(docker, config).await);
        checks.push(sandboxes(docker, config).await);
        checks.push(nvidia_wine(docker, config).await);
    }
    checks.push(storage(docker, config, engine.is_ok()).await);
    for (template, dir) in &config.shared_dirs {
        checks.push(external_shared(template, dir).await);
    }
    if engine.is_ok() {
        checks.push(legacy_homes(docker, config).await);
    }
    checks.push(moonlight_hosts(docker, config, engine.is_ok(), moonlight).await);
    checks.push(render_node(&config.render_node));
    checks.push(pad_modules(&PAD_MODULES));
    checks.push(user_namespaces());
    checks.push(match identity {
        Some(identity) => clock(&identity.portal_url).await,
        None => unclaimed(crate::claim::read_code(state_dir).as_deref()),
    });
    checks.push(ports(config));
    checks.push(host_files(Path::new(hostfiles::HOST_ETC)));

    println!("cha-node doctor\n");
    for c in &checks {
        let tag = match c.level {
            Level::Ok => "ok  ",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        };
        println!("  {tag}  {:<18} {}", c.name, c.detail);
        if let Some(fix) = &c.fix
            && c.level != Level::Ok
        {
            println!("        {:<18} → {fix}", "");
        }
    }
    let worst = checks.iter().map(|c| c.level).max().unwrap_or(Level::Ok);
    println!();
    println!(
        "{}",
        match worst {
            Level::Fail => "Something needs fixing before environments can run.",
            Level::Warn => "Environments can run; see the warnings.",
            _ => "Ready to run environments.",
        }
    );
    worst != Level::Fail
}

async fn images(docker: &Docker, config: &DockerConfig) -> Check {
    let mut missing = Vec::new();
    let mut wanted = vec![config.streamer_image.clone()];
    wanted.extend(catalog_images().iter().map(|i| config.app_image(i)));
    for image in &wanted {
        if !docker.image_exists(image).await.unwrap_or(false) {
            missing.push(image.clone());
        }
    }
    if missing.is_empty() {
        check(
            Level::Ok,
            "Images",
            format!("the streamer and {} environment images", wanted.len() - 1),
        )
    } else {
        let level = if missing.contains(&config.streamer_image) {
            Level::Fail
        } else {
            Level::Warn
        };
        let mut fixes = Vec::new();
        if missing.contains(&config.streamer_image) {
            fixes.push(if names_registry(&config.streamer_image) {
                format!("docker pull {}", config.streamer_image)
            } else {
                "docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev ."
                    .to_string()
            });
        }
        let apps: Vec<&String> = missing
            .iter()
            .filter(|i| **i != config.streamer_image)
            .collect();
        if !apps.is_empty() {
            fixes.push(if config.app_images.is_some() {
                apps.iter()
                    .map(|i| format!("docker pull {i}"))
                    .collect::<Vec<_>>()
                    .join(" && ")
            } else {
                "docker compose -f images/compose.yaml build".to_string()
            });
        }
        check(level, "Images", format!("missing {}", missing.join(", "))).fix(fixes.join(" && "))
    }
}

/// Moonlight hosts on the LAN (ADR 0008): how many this node sees and is
/// paired with, and whether the gateway image is here.
async fn moonlight_hosts(
    docker: &Docker,
    config: &DockerConfig,
    engine: bool,
    enabled: bool,
) -> Check {
    if !enabled {
        return check(Level::Info, "Moonlight", "off (CHA_MOONLIGHT=false)");
    }
    let dir = crate::moonlight::dir(&config.data_root);
    let moonlight = match crate::moonlight::Moonlight::spawn(dir) {
        Ok(moonlight) => moonlight,
        Err(err) => {
            return check(
                Level::Warn,
                "Moonlight",
                format!("can't browse the LAN: {err:#}"),
            );
        }
    };
    tokio::time::sleep(Duration::from_secs(4)).await;
    let found = crate::moonlight::Control::hosts(&*moonlight)
        .borrow()
        .clone();
    let paired = found.iter().filter(|h| h.paired).count();
    let image = config.app_image(&config.gateway_image);
    let have_image = engine && docker.image_exists(&image).await.unwrap_or(false);
    let detail = format!(
        "{} host(s) found, {paired} paired; gateway image {image} {}",
        found.len(),
        if have_image { "present" } else { "missing" }
    );
    if have_image || found.is_empty() {
        check(Level::Ok, "Moonlight", detail)
    } else {
        let fix = if config.app_images.is_some() {
            format!("docker pull {image}")
        } else {
            "docker build -f crates/cha-gateway/Dockerfile -t cha/gateway:dev .".to_string()
        };
        check(Level::Warn, "Moonlight", detail).fix(fix)
    }
}

/// The GPU as a streamer gets it (CDI), named by the driver's own tool.
async fn gpu(docker: &Docker, config: &DockerConfig) -> Check {
    let probe = json!({
        "Image": config.streamer_image,
        "Entrypoint": ["nvidia-smi", "--query-gpu=name,driver_version", "--format=csv,noheader"],
        "HostConfig": {
            "DeviceRequests": [{ "Driver": "cdi", "DeviceIDs": [config.gpu_device] }],
            "NetworkMode": "none",
        },
    });
    match docker.run("cha-doctor-gpu", &probe, PROBE_TIMEOUT).await {
        Ok((0, out)) => check(
            Level::Ok,
            "GPU (CDI)",
            out.lines().next().unwrap_or("").replace(", ", ", driver "),
        ),
        Ok((code, out)) => check(
            Level::Fail,
            "GPU (CDI)",
            format!("nvidia-smi exited {code}: {}", out.trim()),
        ),
        Err(err) => check(Level::Fail, "GPU (CDI)", format!("{err:#}")),
    }
    .fix(format!(
        "install the NVIDIA Container Toolkit and generate the CDI spec: \
         sudo nvidia-ctk cdi generate --output=/etc/cdi/nvidia.yaml (device {})",
        config.gpu_device
    ))
}

/// The devices environments can run on (`docs/devices.md`), one line each:
/// NVIDIA from the inventory, VA-API ones as the streamer image probes them,
/// and the CPU (its codecs probed the same way). A render node that isn't NVIDIA's and can't encode says why.
async fn devices(docker: &Docker, config: &DockerConfig) -> Vec<Check> {
    let mut checks = Vec::new();
    let mut inventory = crate::inventory::collect();
    // The CPU's codecs are the image's to say (AV1 needs SVT-AV1 in it).
    if let Ok(codecs) = crate::devices::probe_cpu(docker, &config.streamer_image).await {
        crate::inventory::set_cpu_codecs(&mut inventory, codecs);
    }
    let line = |d: &cha_wire::Device| {
        let what = match (d.kind, d.cores) {
            (cha_wire::DeviceKind::Cpu, Some(cores)) => format!("{}, {cores} cores", d.name),
            _ => d.name.clone(),
        };
        format!(
            "{}: {what} ({})",
            d.kind.as_str(),
            if d.codecs.is_empty() {
                "no encoder".to_string()
            } else {
                d.codecs.join(", ")
            }
        )
    };
    for device in inventory
        .devices
        .iter()
        .flatten()
        .filter(|d| d.kind == cha_wire::DeviceKind::Nvidia)
    {
        checks.push(check(Level::Ok, "Device", line(device)));
    }
    for node in crate::inventory::vaapi_candidates() {
        match crate::devices::probe(docker, &config.streamer_image, &node).await {
            Ok(Some(device)) => checks.push(check(Level::Ok, "Device", line(&device))),
            Ok(None) => {}
            Err(err) => checks.push(
                check(
                    Level::Info,
                    "Device",
                    format!("{node}: no VA-API ({err:#})"),
                )
                .fix(
                    "an Intel or AMD GPU needs the host's VA-API driver in the streamer image \
                     (intel-media-va-driver, or Mesa's for AMD) and a streamer image that knows \
                     --probe-device; rebuild it, or ignore this if the GPU isn't for streaming",
                ),
            ),
        }
    }
    for device in inventory
        .devices
        .iter()
        .flatten()
        .filter(|d| d.kind == cha_wire::DeviceKind::Cpu)
    {
        checks.push(check(Level::Ok, "Device", line(device)));
    }
    checks
}

/// NVIDIA's Wine DLLs (`nvngx.dll`) on the host, which Proton games need for
/// DLSS and the CDI spec leaves out; the agent binds them into apps.
async fn nvidia_wine(docker: &Docker, config: &DockerConfig) -> Check {
    let Some(dir) = &config.nvidia_wine_dir else {
        return check(
            Level::Info,
            "NVIDIA Wine DLLs",
            "off (CHA_NVIDIA_WINE_DIR is empty)",
        );
    };
    let shown = dir.display();
    match docker.host_path_exists(&config.streamer_image, dir).await {
        Ok(true) => check(
            Level::Ok,
            "NVIDIA Wine DLLs",
            format!("{shown} goes into apps (DLSS under Proton)"),
        ),
        Ok(false) if nvidia_present() => check(
            Level::Warn,
            "NVIDIA Wine DLLs",
            format!("{shown} is missing, so Proton games get no DLSS"),
        )
        .fix(
            "install the driver package that ships nvngx.dll (Ubuntu: libnvidia-gl-<version>; \
             NVIDIA's .run installer includes it), or point CHA_NVIDIA_WINE_DIR at where it is",
        ),
        Ok(false) => check(
            Level::Info,
            "NVIDIA Wine DLLs",
            format!("{shown} is missing; only NVIDIA's driver ships it"),
        ),
        Err(err) => check(Level::Warn, "NVIDIA Wine DLLs", format!("{err:#}")),
    }
}

/// PyroWave's Vulkan device, made the way a streamer makes it. Optional:
/// without it sessions use the hardware codecs only.
async fn pyrowave(docker: &Docker, config: &DockerConfig) -> Check {
    let probe = json!({
        "Image": config.streamer_image,
        "Entrypoint": ["cha-streamer", "--probe-pyrowave", "--render-node", config.render_node],
        "HostConfig": {
            "DeviceRequests": [{ "Driver": "cdi", "DeviceIDs": [config.gpu_device] }],
            "NetworkMode": "none",
        },
    });
    match docker.run("cha-doctor-pyrowave", &probe, PROBE_TIMEOUT).await {
        Ok((0, out)) => check(Level::Ok, "PyroWave", out.lines().last().unwrap_or("").to_string()),
        Ok((code, out)) => check(
            Level::Warn,
            "PyroWave",
            format!("off, the probe exited {code}: {}", out.trim().lines().last().unwrap_or("")),
        ),
        Err(err) => check(Level::Warn, "PyroWave", format!("{err:#}")),
    }
    .fix(
        "rebuild the streamer image (it carries libpyrowave and the libraries NVIDIA's Vulkan driver \
         needs); VK_LOADER_DEBUG=error,driver in the probe says what the driver is missing",
    )
}

/// `/dev/uinput` as a streamer gets it.
async fn gamepads(docker: &Docker, config: &DockerConfig) -> Check {
    let Some(uinput) = &config.uinput else {
        return check(Level::Info, "Gamepads", "off (CHA_UINPUT is empty)");
    };
    let probe = json!({
        "Image": config.streamer_image,
        "Entrypoint": ["sh", "-c", "test -c /dev/uinput"],
        "HostConfig": {
            "Devices": [{ "PathOnHost": uinput, "PathInContainer": "/dev/uinput", "CgroupPermissions": "rw" }],
            "NetworkMode": "none",
        },
    });
    match docker.run("cha-doctor-uinput", &probe, PROBE_TIMEOUT).await {
        Ok((0, _)) => check(Level::Ok, "Gamepads", format!("{uinput} reaches streamers")),
        Ok((code, out)) => check(
            Level::Fail,
            "Gamepads",
            format!("probe exited {code}: {}", out.trim()),
        ),
        Err(err) => check(Level::Fail, "Gamepads", format!("{err:#}")),
    }
    .fix(format!(
        "load the module: {} (or run the agent with CHA_UINPUT= to go without gamepads)",
        hostfiles::FIX
    ))
}

/// `/dev/uhid` as a streamer gets it: what the DualSense and Steam Controller
/// are made with. Without it those fall back to an Xbox 360 pad, so this
/// warns rather than fails.
async fn uhid_pads(docker: &Docker, config: &DockerConfig) -> Check {
    let name = "DualSense/Steam";
    let Some(uhid) = &config.uhid else {
        return check(
            Level::Info,
            name,
            "off (CHA_UHID is empty): they fall back to an Xbox 360 pad",
        );
    };
    let probe = json!({
        "Image": config.streamer_image,
        "Entrypoint": ["sh", "-c", "test -c /dev/uhid"],
        "HostConfig": {
            "Devices": [{ "PathOnHost": uhid, "PathInContainer": "/dev/uhid", "CgroupPermissions": "rw" }],
            "NetworkMode": "none",
        },
    });
    match docker.run("cha-doctor-uhid", &probe, PROBE_TIMEOUT).await {
        Ok((0, _)) => check(Level::Ok, name, format!("{uhid} reaches streamers")),
        Ok((code, out)) => check(
            Level::Warn,
            name,
            format!("probe exited {code}: {}", out.trim()),
        ),
        Err(err) => check(Level::Warn, name, format!("{err:#}")),
    }
    .fix(format!(
        "load the module: {} (or run the agent with CHA_UHID= to go without)",
        hostfiles::FIX
    ))
}

/// The host kernel modules that turn a virtual DualSense and Steam Controller
/// into the evdev devices apps read: (module, what for).
const PAD_MODULES: [(&str, &str); 3] = [
    ("uhid", "virtual DualSense and Steam Controller"),
    (
        "hid_playstation",
        "the DualSense's gamepad, touchpad and motion nodes",
    ),
    ("hid_steam", "the Steam Controller's input nodes"),
];

/// Whether each of `modules` is in this kernel. The agent's container shares
/// the host's kernel, so `/proc/modules` and `/sys/module` (loaded or built
/// in) are the host's; the modules on disk (`/lib/modules`) aren't visible
/// from there, so one that isn't loaded can't be told from one that is
/// missing. The kernel loads these when the first virtual device appears
/// (`uhid` itself when `/dev/uhid` is opened, if the host's udev makes the
/// device), so that is fine; a node that wants them there from boot lists
/// them in `/etc/modules-load.d`.
fn pad_modules(modules: &[(&str, &str)]) -> Check {
    let loaded = std::fs::read_to_string("/proc/modules").unwrap_or_default();
    let present =
        |name: &str| module_loaded(&loaded, name) || Path::new("/sys/module").join(name).exists();
    let (up, down): (Vec<_>, Vec<_>) = modules.iter().partition(|(name, _)| present(name));
    let names = |list: &[&(&str, &str)]| {
        list.iter()
            .map(|(name, _)| name.replace('_', "-"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if down.is_empty() {
        return check(Level::Ok, "Pad modules", format!("{} loaded", names(&up)));
    }
    let detail = if up.is_empty() {
        format!(
            "{} aren't loaded; the kernel loads them on demand",
            names(&down)
        )
    } else {
        format!(
            "{} loaded; {} not yet, the kernel loads them on demand",
            names(&up),
            names(&down)
        )
    };
    check(Level::Info, "Pad modules", detail).fix(format!(
        "to have uhid from boot: {} (a kernel without hid-playstation or hid-steam can't make \
         those pads: apps get the Xbox 360 pad)",
        hostfiles::FIX
    ))
}

/// Whether `/proc/modules`' text lists the module `name` (underscores and
/// hyphens are the same in module names).
fn module_loaded(proc_modules: &str, name: &str) -> bool {
    let want = name.replace('-', "_");
    proc_modules
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .any(|m| m.replace('-', "_") == want)
}

/// The host files only the owner can install (udev rules that keep virtual
/// pads off the desktop's seat, the Steam sandbox's AppArmor profile, the
/// module list), against the copies this agent was built with. Never a
/// failure: the runtime checks above say whether things work.
fn host_files(etc: &Path) -> Check {
    const NAME: &str = "Host files";
    let report = hostfiles::compare(etc);
    if report.stale() {
        return check(Level::Warn, NAME, report.describe()).fix(hostfiles::FIX);
    }
    if !report.unknown.is_empty() {
        return check(
            Level::Info,
            NAME,
            "can't tell: the host's /etc/udev/rules.d, /etc/apparmor.d and /etc/modules-load.d \
             aren't bound into the agent",
        )
        .fix(format!(
            "to check from here, bind them in (deploy/node/compose.yaml, under /run/host/etc) and \
             recreate the agent; to install: {}",
            hostfiles::FIX
        ));
    }
    check(
        Level::Ok,
        NAME,
        "udev rules, AppArmor profile and module list are current",
    )
}

/// What Steam's pressure-vessel does: mount inside its own user namespace,
/// under the `steam` profile's seccomp and AppArmor profiles.
async fn sandboxes(docker: &Docker, config: &DockerConfig) -> Check {
    let probe = json!({
        "Image": config.streamer_image,
        "User": "1000:1000",
        "Entrypoint": ["unshare", "--user", "--map-root-user", "--mount", "sh", "-c",
                       "mount -t tmpfs none /mnt && echo mounted"],
        "HostConfig": {
            "CapDrop": ["ALL"],
            "SecurityOpt": ["no-new-privileges", browser_seccomp(), format!("apparmor={SANDBOX_APPARMOR}")],
            "NetworkMode": "none",
        },
    });
    let fix = format!("load the profile on the node: {}", hostfiles::FIX);
    match docker
        .run("cha-doctor-sandbox", &probe, PROBE_TIMEOUT)
        .await
    {
        Ok((0, out)) if out.contains("mounted") => check(
            Level::Ok,
            "Sandboxes",
            "apps can build their own (bubblewrap, pressure-vessel)",
        ),
        Ok((code, out)) => check(
            Level::Warn,
            "Sandboxes",
            format!(
                "Steam environments won't work: a sandbox mount failed ({code}: {})",
                out.trim()
            ),
        )
        .fix(fix),
        Err(err) => check(
            Level::Warn,
            "Sandboxes",
            format!("Steam environments won't start: {err:#}"),
        )
        .fix(fix),
    }
}

/// The data root: where users' app data and shared data live. The agent must
/// write it, and Docker must find the same directory at the path the agent
/// names (it is given host paths): a root that is only inside the agent's
/// container would take users' data to places nothing keeps.
async fn storage(docker: &Docker, config: &DockerConfig, engine: bool) -> Check {
    let fix_missing = format!(
        "make it on the host (sudo mkdir -p {0}) and mount it into the agent at the same path \
         (CHA_DATA_ROOT, deploy/node/compose.yaml)",
        config.data_root.display()
    );
    let data = match DataRoot::new(&config.data_root, 1000, 1000) {
        Ok(data) => data,
        Err(err) => return check(Level::Fail, "Storage", format!("{err:#}")),
    };
    let root = data.path().display().to_string();
    let name = {
        let data = data.clone();
        tokio::task::spawn_blocking(move || data.create_probe())
            .await
            .map_err(anyhow::Error::from)
            .and_then(|r| r)
    };
    let name = match name {
        Ok(name) => name,
        Err(err) => {
            return check(Level::Fail, "Storage", format!("{err:#}")).fix(fix_missing);
        }
    };
    // The same directory, as Docker sees it.
    // (Without the engine, or the streamer image the probe runs from, there is
    // nothing to ask Docker.)
    let can_ask = engine
        && docker
            .image_exists(&config.streamer_image)
            .await
            .unwrap_or(false);
    let seen = if !can_ask {
        None
    } else {
        let probe = json!({
            "Image": config.streamer_image,
            "Entrypoint": ["test", "-e", format!("/probe/{name}")],
            "HostConfig": {
                "Mounts": [{
                    "Type": "bind",
                    "Source": root,
                    "Target": "/probe",
                    "ReadOnly": true,
                    "BindOptions": { "CreateMountpoint": false },
                }],
                "NetworkMode": "none",
            },
        });
        Some(
            docker
                .run("cha-doctor-storage", &probe, PROBE_TIMEOUT)
                .await,
        )
    };
    {
        let data = data.clone();
        let name = name.clone();
        let _ = tokio::task::spawn_blocking(move || data.remove_probe(&name)).await;
    }
    match seen {
        Some(Ok((0, _))) | None => {}
        Some(Ok((_, _))) => {
            return check(
                Level::Fail,
                "Storage",
                format!(
                    "{root} is writable here, but it isn't the same directory on the host: users' \
                     data would land in the agent's container"
                ),
            )
            .fix(format!(
                "mount the host's {root} into the agent at {root} (CHA_DATA_ROOT, \
                 deploy/node/compose.yaml)"
            ));
        }
        Some(Err(err)) => {
            return check(
                Level::Fail,
                "Storage",
                format!("Docker can't mount {root}: {err:#}"),
            )
            .fix(fix_missing);
        }
    }
    let stats = {
        let data = data.clone();
        tokio::task::spawn_blocking(move || data.stats())
            .await
            .unwrap_or_default()
    };
    let free = stats.free_bytes.unwrap_or(0);
    let detail = format!(
        "{root}: writable, {} free; {} app directories for {} users, {} shared{}",
        gigabytes(free),
        stats.user_dirs,
        stats.users,
        stats.shared_dirs,
        if seen.is_none() {
            " (not checked from Docker's side)"
        } else {
            ""
        },
    );
    if stats.free_bytes.is_some() && free < LOW_SPACE_BYTES {
        check(Level::Warn, "Storage", detail).fix(format!(
            "free up space on the disk holding {root}: games and users' homes live there"
        ))
    } else {
        check(Level::Ok, "Storage", detail)
    }
}

/// A shared directory the owner keeps outside the data root (a NAS share).
/// The agent only looks at it, from its own read-only bind of it, and a launch
/// whose directory isn't usable goes without it, so problems here are
/// warnings. Looking at an NFS share that is away can block, so it gives up.
async fn external_shared(template: &str, dir: &Path) -> Check {
    let name = format!("Shared ({template})");
    let per_user = catalog_per_user(template);
    let looked = {
        let (dir, per_user) = (dir.to_path_buf(), per_user.clone());
        tokio::time::timeout(
            Duration::from_secs(10),
            tokio::task::spawn_blocking(move || inspect_shared(&dir, &per_user)),
        )
        .await
    };
    let Ok(Ok(looked)) = looked else {
        return check(
            Level::Warn,
            name,
            format!("{} didn't answer within 10 s", dir.display()),
        )
        .fix(
            "a hard NFS mount makes reads wait for the server: check that it is up; \
             launches go without this directory until then",
        );
    };
    describe_shared(name, dir, &per_user, &looked)
}

/// What looking at a shared directory found.
#[derive(Debug, Default)]
struct SharedLook {
    /// `Err` is why it can't be read.
    stat: Option<Result<SharedStat, String>>,
    /// The filesystem it is on: type and source, from the mount table.
    fs: Option<(String, String)>,
    /// Per-user places it doesn't have as real directories.
    missing: Vec<String>,
    /// Empty, and on the same device as its parent: likely a mountpoint that
    /// nothing is mounted on.
    looks_unmounted: bool,
}

#[derive(Debug)]
struct SharedStat {
    mode: u32,
    uid: u32,
    gid: u32,
}

fn inspect_shared(dir: &Path, per_user: &[String]) -> SharedLook {
    use std::os::unix::fs::MetadataExt;
    let mut look = SharedLook::default();
    let meta = match std::fs::metadata(dir) {
        Ok(m) if m.is_dir() => m,
        Ok(_) => {
            look.stat = Some(Err("not a directory".into()));
            return look;
        }
        Err(e) => {
            look.stat = Some(Err(e.to_string()));
            return look;
        }
    };
    look.stat = Some(Ok(SharedStat {
        mode: meta.mode() & 0o7777,
        uid: meta.uid(),
        gid: meta.gid(),
    }));
    look.fs = std::fs::read_to_string("/proc/self/mountinfo")
        .ok()
        .and_then(|table| mount_for(&table, dir));
    look.missing = crate::storage::missing_per_user(dir, per_user);
    let empty = std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_none());
    let same_device = dir
        .parent()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|parent| parent.dev() == meta.dev());
    look.looks_unmounted = empty && same_device;
    look
}

fn describe_shared(name: String, dir: &Path, per_user: &[String], look: &SharedLook) -> Check {
    let path = dir.display();
    let bind = format!(
        "mount the share on the node and bind it into the agent read-only at the same path \
         ({path}:{path}:ro, deploy/node/compose.yaml)"
    );
    let stat = match &look.stat {
        Some(Ok(stat)) => stat,
        Some(Err(why)) => {
            return check(
                Level::Warn,
                name,
                format!("{path}: {why}; launches go without the shared directory"),
            )
            .fix(bind);
        }
        None => return check(Level::Warn, name, format!("{path}: not looked at")),
    };
    let mut problems = Vec::new();
    let mut notes = Vec::new();
    let mut fixes = Vec::new();
    if look.looks_unmounted {
        problems
            .push("empty, on the same device as its parent: the share looks unmounted".to_string());
        fixes.push(bind);
    }
    if !look.missing.is_empty() {
        problems.push(format!("missing {}", look.missing.join(", ")));
        let dirs: Vec<String> = look
            .missing
            .iter()
            .map(|m| dir.join(m).display().to_string())
            .collect();
        fixes.push(format!(
            "as a user who can write there: mkdir -p {} (launches go without the shared \
             directory until they exist)",
            dirs.join(" ")
        ));
    }
    let owner = format!("{:o} {}:{}", stat.mode, stat.uid, stat.gid);
    if can_write(stat, APP_UID, APP_UID) {
        notes.push(format!("uid {APP_UID} can write ({owner})"));
        if stat.mode & 0o002 != 0 {
            notes.push(
                "world-writable: fine on a NAS that maps every user to one, otherwise any local user can write it"
                    .into(),
            );
        }
    } else {
        problems.push(format!("uid {APP_UID} can't write it ({owner})"));
        fixes.push(format!(
            "give uid {APP_UID} write access on the server (apps write installs and manifests there)"
        ));
    }
    let on = match &look.fs {
        Some((fs, source)) => format!("{fs} ({source})"),
        None => "filesystem unknown".into(),
    };
    let per_user_note = if look.missing.is_empty() && !per_user.is_empty() {
        format!("{} per-user places present", per_user.len())
    } else {
        String::new()
    };
    let mut detail = vec![format!("{path}: {on}")];
    detail.extend(Some(per_user_note).filter(|n| !n.is_empty()));
    detail.extend(notes);
    let (level, detail) = if problems.is_empty() {
        (Level::Ok, detail.join("; "))
    } else {
        detail.extend(problems);
        (Level::Warn, detail.join("; "))
    };
    let result = check(level, name, detail);
    if fixes.is_empty() {
        result
    } else {
        result.fix(fixes.join("; "))
    }
}

/// Whether the user `uid`:`gid` can write a directory by its mode bits (a NAS
/// may map users, so this is what the permissions say, not a test).
fn can_write(stat: &SharedStat, uid: u32, gid: u32) -> bool {
    if stat.uid == uid {
        stat.mode & 0o200 != 0
    } else if stat.gid == gid {
        stat.mode & 0o020 != 0
    } else {
        stat.mode & 0o002 != 0
    }
}

/// The filesystem `path` is on, from a mount table (`/proc/self/mountinfo`):
/// its type and source, from the mount that covers it most closely. Each line
/// is `id parent major:minor root mount-point options [fields] - type source
/// super-options`, with spaces in the mount point written `\040`.
fn mount_for(table: &str, path: &Path) -> Option<(String, String)> {
    let mut best: Option<(usize, String, String)> = None;
    for line in table.lines() {
        let Some((head, tail)) = line.split_once(" - ") else {
            continue;
        };
        let Some(point) = head.split_whitespace().nth(4) else {
            continue;
        };
        let point = point.replace("\\040", " ");
        let mut tail = tail.split_whitespace();
        let (Some(fs), Some(source)) = (tail.next(), tail.next()) else {
            continue;
        };
        if path.starts_with(&point) && best.as_ref().is_none_or(|(len, ..)| point.len() >= *len) {
            best = Some((point.len(), fs.to_string(), source.replace("\\040", " ")));
        }
    }
    best.map(|(_, fs, source)| (fs, source))
}

fn gigabytes(bytes: u64) -> String {
    format!("{:.0} GB", bytes as f64 / (1u64 << 30) as f64)
}

/// The home volumes from before app data moved under the data root (one per
/// user and persistent template). They are copied in on a launch and never
/// removed by the agent: the owner removes them once the copy is trusted.
async fn legacy_homes(docker: &Docker, config: &DockerConfig) -> Check {
    let names = match docker.volumes_named(HOME_VOLUME_PREFIX).await {
        Ok(names) => names,
        Err(err) => return check(Level::Warn, "Legacy homes", format!("{err:#}")),
    };
    if names.is_empty() {
        return check(
            Level::Ok,
            "Legacy homes",
            "no home volumes left from before app data moved under the data root",
        );
    }
    let migrated = DataRoot::new(&config.data_root, 1000, 1000)
        .ok()
        .map(|data| data.stats().migrated)
        .unwrap_or_default();
    let (copied, waiting): (Vec<&String>, Vec<&String>) = names.iter().partition(|name| {
        parse_home_volume_name(name)
            .is_some_and(|(user, template)| migrated.contains(&(user.into(), template.into())))
    });
    let fix = if copied.is_empty() {
        "they are copied under the data root when their user next launches the app".to_string()
    } else {
        format!(
            "the copied ones are safe to remove once you trust the new directories: docker volume rm {}",
            copied
                .iter()
                .map(|n| n.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    check(
        Level::Info,
        "Legacy homes",
        format!(
            "{} left from before app data moved under {}: {} copied, {} not yet",
            names.len(),
            config.data_root.display(),
            copied.len(),
            waiting.len()
        ),
    )
    .fix(fix)
}

fn render_node(path: &str) -> Check {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    match std::fs::metadata(path) {
        Ok(m) if m.file_type().is_char_device() => {
            check(Level::Ok, "Render node", format!("{path}, group {}", m.gid()))
        }
        Ok(_) => check(Level::Fail, "Render node", format!("{path} isn't a device")),
        Err(err) => check(Level::Fail, "Render node", format!("{path}: {err}")).fix(
            "give the agent the GPU (the CDI device in deploy/node/compose.yaml), or pass --render-node",
        ),
    }
}

/// Ubuntu's restriction on unprivileged user namespaces (AppArmor).
fn user_namespaces() -> Check {
    let path = Path::new("/proc/sys/kernel/apparmor_restrict_unprivileged_userns");
    match std::fs::read_to_string(path).map(|v| v.trim() == "1") {
        Ok(true) => check(
            Level::Info,
            "User namespaces",
            "restricted by AppArmor: browsers' sandboxes work, and Steam environments run \
             under cha-sandbox (above); elsewhere bubblewrap (GTK's image loaders) can't build \
             its own, so the base image's stand-in runs those loaders inside the container",
        )
        .fix("nothing to do; a cha-browser AppArmor profile will lift this (images/README.md)"),
        Ok(false) => check(Level::Ok, "User namespaces", "unrestricted"),
        Err(_) => check(Level::Ok, "User namespaces", "no AppArmor restriction"),
    }
}

/// The portal's clock against ours, from its `Date` header.
async fn clock(portal_url: &str) -> Check {
    let url = format!("{}/api/health", portal_url.trim_end_matches('/'));
    let sent = SystemTime::now();
    let response = match reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(err) => {
            return check(Level::Fail, "Portal", format!("{url}: {err}"))
                .fix("check the portal URL (CHA_PORTAL_URL) and that this machine can reach it");
        }
    };
    let received = SystemTime::now();
    let Some(date) = response
        .headers()
        .get("date")
        .and_then(|d| d.to_str().ok())
        .and_then(|d| httpdate::parse_http_date(d).ok())
    else {
        return check(
            Level::Ok,
            "Portal",
            format!("{url} answers ({})", response.status()),
        );
    };
    // The header has whole seconds: compare with the request's midpoint.
    let midpoint = sent + received.duration_since(sent).unwrap_or_default() / 2;
    let skew = match midpoint.duration_since(date) {
        Ok(ahead) => ahead.as_secs_f64(),
        Err(behind) => -behind.duration().as_secs_f64(),
    };
    let detail = format!("{url} answers; this clock is {skew:+.1} s from the portal's");
    let level = if skew.abs() > SKEW_FAIL_SECS {
        Level::Fail
    } else if skew.abs() > SKEW_WARN_SECS {
        Level::Warn
    } else {
        Level::Ok
    };
    check(level, "Portal", detail)
        .fix("sync the clocks (NTP); media tokens are only valid for 60 s")
}

/// The first environment's ports, if nothing holds them.
/// A node with no identity: waiting to be claimed, if it shows a code.
fn unclaimed(code: Option<&str>) -> Check {
    match code {
        Some(code) => check(
            Level::Info,
            "Portal",
            format!("not claimed yet: pairing code {code}"),
        )
        .fix("in the portal, open Admin → Nodes, pick this node under Found on your network and enter the code"),
        None => check(Level::Info, "Portal", "not enrolled yet").fix(
            "run the agent once with CHA_PORTAL_URL and a join token from the portal's Nodes page",
        ),
    }
}

fn ports(config: &DockerConfig) -> Check {
    let http = config.port_base;
    let webrtc = http + 1;
    let tcp = std::net::TcpListener::bind(("0.0.0.0", http)).is_ok();
    let udp = std::net::UdpSocket::bind(("0.0.0.0", webrtc)).is_ok();
    let last = config.port_base + 3 * config.max_environments - 1;
    if tcp && udp {
        check(
            Level::Ok,
            "Ports",
            format!(
                "{http}-{last} for streamers (TCP on localhost, UDP for WebRTC and WebTransport)"
            ),
        )
    } else {
        check(
            Level::Warn,
            "Ports",
            format!("{http} (TCP) or {webrtc} (UDP) is taken: by a running environment, or something else"),
        )
        .fix("pick another range with CHA_PORT_BASE")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unclaimed_node_shows_its_code() {
        let shown = unclaimed(Some("4821-9375"));
        assert!(shown.detail.contains("4821-9375"));
        assert!(unclaimed(None).detail.contains("not enrolled yet"));
    }

    #[test]
    fn modules_are_found_by_name_with_either_dash() {
        let proc_modules = "\
hid_playstation 36864 0 - Live 0x0000000000000000
uhid 24576 2 - Live 0x0000000000000000
ff_memless 20480 1 hid_playstation, Live 0x0000000000000000
";
        assert!(module_loaded(proc_modules, "uhid"));
        assert!(module_loaded(proc_modules, "hid_playstation"));
        assert!(module_loaded(proc_modules, "hid-playstation"));
        // Only the name column counts, not a module that lists it as a user.
        assert!(!module_loaded(proc_modules, "hid_steam"));
        assert!(!module_loaded(proc_modules, "hid"));
        assert!(!module_loaded("", "uhid"));
    }

    /// The agent's view of the node: the container's root, and `/mnt/games/steam`
    /// bound in from the host's NFS mount of `/mnt/games`.
    const MOUNTINFO: &str = "\
1011 987 0:61 / / rw,relatime master:1 - overlay overlay rw,lowerdir=/x
1020 1011 0:62 / /proc rw,nosuid,nodev,noexec,relatime - proc proc rw
1030 1011 259:2 /srv/cha-portal /srv/cha-portal rw,relatime - ext4 /dev/nvme0n1p2 rw
1040 1011 0:53 /steam /mnt/games/steam ro,relatime shared:200 - nfs4 192.168.1.50:/mnt/user/games ro,vers=4.2,hard,nconnect=4
1041 1011 0:54 / /mnt/with\\040space rw - cifs //nas/share rw
";

    #[test]
    fn the_filesystem_comes_from_the_closest_mount() {
        let nfs = mount_for(MOUNTINFO, Path::new("/mnt/games/steam"));
        assert_eq!(
            nfs,
            Some(("nfs4".into(), "192.168.1.50:/mnt/user/games".into()))
        );
        assert_eq!(
            mount_for(
                MOUNTINFO,
                Path::new("/mnt/games/steam/steamapps/compatdata")
            )
            .map(|m| m.0),
            Some("nfs4".into())
        );
        assert_eq!(
            mount_for(MOUNTINFO, Path::new("/srv/cha-portal/users")).map(|m| m.0),
            Some("ext4".into())
        );
        // Not under any mount of its own: the root's.
        assert_eq!(
            mount_for(MOUNTINFO, Path::new("/mnt/other")).map(|m| m.0),
            Some("overlay".into())
        );
        assert_eq!(
            mount_for(MOUNTINFO, Path::new("/mnt/with space/x")).map(|m| m.0),
            Some("cifs".into())
        );
        assert_eq!(mount_for("", Path::new("/mnt")), None);
        assert_eq!(
            mount_for("garbage\nno dash here\n", Path::new("/mnt")),
            None
        );
    }

    fn stat(mode: u32, uid: u32, gid: u32) -> SharedStat {
        SharedStat { mode, uid, gid }
    }

    #[test]
    fn write_access_follows_the_mode_bits_for_the_apps_user() {
        // A NAS that maps everyone to nobody:users, 0777.
        assert!(can_write(&stat(0o777, 99, 100), 1000, 1000));
        assert!(!can_write(&stat(0o755, 99, 100), 1000, 1000));
        assert!(
            !can_write(&stat(0o775, 99, 100), 1000, 1000),
            "group isn't ours"
        );
        assert!(can_write(&stat(0o775, 99, 1000), 1000, 1000));
        assert!(can_write(&stat(0o700, 1000, 1000), 1000, 1000));
        // The owner's own bits decide, even if others could.
        assert!(!can_write(&stat(0o577, 1000, 1000), 1000, 1000));
    }

    fn healthy() -> SharedLook {
        SharedLook {
            stat: Some(Ok(stat(0o777, 99, 100))),
            fs: Some(("nfs4".into(), "192.168.1.50:/mnt/user/games".into())),
            missing: vec![],
            looks_unmounted: false,
        }
    }

    fn per_user() -> Vec<String> {
        vec![
            "steamapps/compatdata".into(),
            "steamapps/shadercache".into(),
        ]
    }

    #[test]
    fn a_healthy_share_says_what_it_is() {
        let dir = Path::new("/mnt/games/steam");
        let c = describe_shared("Shared (steam)".into(), dir, &per_user(), &healthy());
        assert!(c.level == Level::Ok);
        assert!(
            c.detail.contains("nfs4 (192.168.1.50:/mnt/user/games)"),
            "{}",
            c.detail
        );
        assert!(c.detail.contains("2 per-user places present"));
        assert!(c.detail.contains("uid 1000 can write (777 99:100)"));
        assert!(c.detail.contains("world-writable"));
        assert!(c.fix.is_none());
    }

    #[test]
    fn missing_places_come_with_the_exact_mkdir() {
        let dir = Path::new("/mnt/games/steam");
        let look = SharedLook {
            missing: per_user(),
            ..healthy()
        };
        let c = describe_shared("Shared (steam)".into(), dir, &per_user(), &look);
        assert!(c.level == Level::Warn);
        let fix = c.fix.unwrap();
        assert!(
            fix.contains("mkdir -p /mnt/games/steam/steamapps/compatdata /mnt/games/steam/steamapps/shadercache"),
            "{fix}"
        );
    }

    #[test]
    fn a_share_that_looks_unmounted_or_unwritable_or_absent_warns() {
        let dir = Path::new("/mnt/games/steam");
        let unmounted = SharedLook {
            looks_unmounted: true,
            missing: per_user(),
            ..healthy()
        };
        let c = describe_shared("Shared (steam)".into(), dir, &per_user(), &unmounted);
        assert!(c.level == Level::Warn);
        assert!(c.detail.contains("looks unmounted"));
        assert!(
            c.fix
                .unwrap()
                .contains("/mnt/games/steam:/mnt/games/steam:ro")
        );

        let read_only = SharedLook {
            stat: Some(Ok(stat(0o755, 99, 100))),
            ..healthy()
        };
        let c = describe_shared("Shared (steam)".into(), dir, &per_user(), &read_only);
        assert!(c.level == Level::Warn);
        assert!(c.detail.contains("uid 1000 can't write"));

        let absent = SharedLook {
            stat: Some(Err("No such file or directory (os error 2)".into())),
            ..SharedLook::default()
        };
        let c = describe_shared("Shared (steam)".into(), dir, &per_user(), &absent);
        assert!(c.level == Level::Warn);
        assert!(c.detail.contains("go without the shared directory"));
    }

    #[test]
    fn looking_at_a_local_directory_finds_what_is_missing_and_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let share = tmp.path().join("steam");
        std::fs::create_dir(&share).unwrap();
        // Empty, beside its parent's own device.
        let look = inspect_shared(&share, &per_user());
        assert!(look.looks_unmounted);
        assert_eq!(look.missing, per_user());
        std::fs::create_dir_all(share.join("steamapps/compatdata")).unwrap();
        std::fs::create_dir_all(share.join("steamapps/shadercache")).unwrap();
        let look = inspect_shared(&share, &per_user());
        assert!(!look.looks_unmounted);
        assert!(look.missing.is_empty());
        assert!(matches!(look.stat, Some(Ok(_))));
        let gone = inspect_shared(&tmp.path().join("nope"), &per_user());
        assert!(matches!(gone.stat, Some(Err(_))));
    }
}

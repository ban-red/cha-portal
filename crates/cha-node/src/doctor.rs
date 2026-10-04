//! `cha-node --doctor`: whether this machine can run environments, and how to
//! fix what it can't. It changes nothing. Fixes that need root on the host
//! (kernel modules, CDI, udev) are printed for the owner to run.
//!
//! In the agent's container (`docker compose run --rm agent --doctor`) it
//! sees the host through the Docker engine: GPU and device checks run in
//! short-lived probe containers configured like a streamer.

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::json;

use crate::Identity;
use crate::docker::Docker;
use crate::environments::{DockerConfig, SANDBOX_APPARMOR, browser_seccomp, catalog_images};

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
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
    name: &'static str,
    detail: String,
    fix: Option<String>,
}

fn check(level: Level, name: &'static str, detail: impl Into<String>) -> Check {
    Check {
        level,
        name,
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
pub async fn run(docker: &Docker, config: &DockerConfig, identity: Option<&Identity>) -> bool {
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
        checks.push(pyrowave(docker, config).await);
        checks.push(gamepads(docker, config).await);
        checks.push(sandboxes(docker, config).await);
    }
    checks.push(render_node(&config.render_node));
    checks.push(user_namespaces());
    checks.push(match identity {
        Some(identity) => clock(&identity.portal_url).await,
        None => check(Level::Info, "Portal", "not enrolled yet").fix(
            "run the agent once with CHA_PORTAL_URL and a join token from the portal's Nodes page",
        ),
    });
    checks.push(ports(config));
    checks.push(
        check(
            Level::Info,
            "Host input",
            "virtual gamepads also appear on this machine's own input stack",
        )
        .fix(
            "on a desktop host, keep them out of your session: sudo install -m 644 \
             deploy/node/host/72-cha-virtual-pads.rules /etc/udev/rules.d/ && sudo udevadm control --reload",
        ),
    );

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
    wanted.extend(catalog_images());
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
        check(level, "Images", format!("missing {}", missing.join(", "))).fix(
            "docker build -f deploy/streamer/Dockerfile --target runtime -t cha/streamer:dev . && \
             docker compose -f images/compose.yaml build",
        )
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
        Ok((code, out)) => check(Level::Fail, "Gamepads", format!("probe exited {code}: {}", out.trim())),
        Err(err) => check(Level::Fail, "Gamepads", format!("{err:#}")),
    }
    .fix(
        "load the module: sudo modprobe uinput && echo uinput | sudo tee /etc/modules-load.d/uinput.conf \
         (or run the agent with CHA_UINPUT= to go without gamepads)",
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
    let fix = format!(
        "load the profile on the node: sudo install -m 644 deploy/node/host/apparmor/{SANDBOX_APPARMOR} \
         /etc/apparmor.d/ && sudo apparmor_parser -r -W /etc/apparmor.d/{SANDBOX_APPARMOR}"
    );
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
            Level::Warn,
            "User namespaces",
            "restricted by AppArmor: browsers' sandboxes work, but bubblewrap (GTK's image \
             loaders, later Steam) can't build its own; the base image's stand-in runs those \
             loaders inside the container instead",
        )
        .fix("nothing to do yet; a cha-browser AppArmor profile will lift this (images/README.md)"),
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

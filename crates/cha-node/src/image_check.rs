//! `cha-node --check-image <image> [--profile standard|browser|steam]`: tells
//! an image's author, or a node's owner about to add a catalog, whether the
//! image will run as a Cha environment on this node, before anyone launches
//! it.
//!
//! The static checks read the image's configuration. The dynamic ones run it
//! the way a launch does: the app container comes from the same confinement
//! as a launch's ([`app_host_config`]: user, capabilities, seccomp, AppArmor,
//! shared memory, init), with no GPU and no input devices, and the
//! compositor is a real `cha-streamer` on its CPU device. Every container the
//! check makes carries [`LABEL_CHECK`] (never `sh.cha.env`, so a running
//! agent doesn't adopt them) and is removed at the end, on an error and on
//! Ctrl-C too.

use std::fmt::Write as _;
use std::net::TcpListener;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use cha_wire::{NodeKey, SecurityProfile};
use serde_json::{Value, json};

use crate::docker::{Docker, PullProgress, names_registry};
use crate::environments::{
    AGENT_VERSION, APP_HOME, APP_UID, DEFAULT_VM_MEMORY_MB, RUNTIME_DIR, SANDBOX_APPARMOR, app_env,
    app_host_config, app_user,
};

/// Marks every container and volume the check makes; its value is this run's id.
pub const LABEL_CHECK: &str = "sh.cha.image-check";

/// The sound server's socket, which the streamer serves and `cha-env-base` points apps at.
const PULSE_SERVER: &str = "unix:/run/cha/pulse/native";
const WAYLAND_SOCKET: &str = "/run/cha/wayland-0";
/// An app that needs a compositor must still be running after this long
/// without one (it waits, as `cha-run` does).
const NO_COMPOSITOR_WAIT: Duration = Duration::from_secs(3);
/// The streamer has this long to open its Wayland socket.
const STREAMER_WAIT: Duration = Duration::from_secs(30);
/// The app has this long, with the socket there, to connect to it.
const CONNECT_WAIT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(400);
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// First port tried for the check's streamer (HTTP; WebRTC and WebTransport
/// take the next two). Clear of the agent's own (7600 up) and of the
/// ephemeral range.
const STREAMER_PORT_FROM: u16 = 17600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Info,
    Warn,
    Fail,
}

impl Level {
    fn word(self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
        }
    }
}

/// One line of the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub level: Level,
    pub name: &'static str,
    pub detail: String,
}

fn line(level: Level, name: &'static str, detail: impl Into<String>) -> Line {
    Line {
        level,
        name,
        detail: detail.into(),
    }
}

impl std::fmt::Display for Line {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:<5} {:<14} {}",
            self.level.word(),
            self.name,
            self.detail
        )
    }
}

/// Whether every MUST check passed (nothing is a `FAIL`).
pub fn passed(lines: &[Line]) -> bool {
    lines.iter().all(|l| l.level != Level::Fail)
}

pub fn parse_profile(name: &str) -> Result<SecurityProfile> {
    match name {
        "standard" => Ok(SecurityProfile::Standard),
        "browser" => Ok(SecurityProfile::Browser),
        "steam" => Ok(SecurityProfile::Steam),
        "vm" => Ok(SecurityProfile::Vm),
        other => bail!("unknown profile {other}: use standard, browser, steam or vm"),
    }
}

fn profile_name(profile: SecurityProfile) -> &'static str {
    match profile {
        SecurityProfile::Standard => "standard",
        SecurityProfile::Browser => "browser",
        SecurityProfile::Steam => "steam",
        SecurityProfile::Vm => "vm",
    }
}

/// `KEY=value` entries of an image's `Config.Env`, as a lookup.
fn env_value<'a>(env: &'a [String], key: &str) -> Option<&'a str> {
    env.iter()
        .rev()
        .find_map(|e| e.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn megabytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MB", bytes >> 20)
    }
}

/// The checks that need only the image's configuration (`GET /images/{name}/json`).
pub fn static_checks(image: &Value) -> Vec<Line> {
    let mut out = Vec::new();
    let os = image["Os"].as_str().unwrap_or("?");
    let arch = image["Architecture"].as_str().unwrap_or("?");
    if os == "linux" && arch == "amd64" {
        out.push(line(Level::Ok, "platform", "linux/amd64"));
    } else {
        out.push(line(
            Level::Fail,
            "platform",
            format!(
                "{os}/{arch}: nodes are linux/amd64, so this image can't run here; build it for linux/amd64"
            ),
        ));
    }

    let config = &image["Config"];
    let entrypoint = strings(&config["Entrypoint"]);
    let cmd = strings(&config["Cmd"]);
    if entrypoint.is_empty() && cmd.is_empty() {
        out.push(line(
            Level::Fail,
            "start command",
            "the image has no ENTRYPOINT or CMD, so a launch has nothing to run",
        ));
    } else {
        let shown: Vec<&str> = entrypoint.iter().chain(&cmd).map(String::as_str).collect();
        out.push(line(Level::Ok, "start command", shown.join(" ")));
    }

    let env = strings(&config["Env"]);
    match env_value(&env, "HOME") {
        Some(home) if home == APP_HOME => out.push(line(Level::Ok, "HOME", home)),
        Some(home) => out.push(line(
            Level::Warn,
            "HOME",
            format!(
                "{home}, not {APP_HOME}: the node runs every app as {}, and keeps an app's data at {APP_HOME}",
                app_user()
            ),
        )),
        None => out.push(line(
            Level::Warn,
            "HOME",
            format!("not set in the image: set ENV HOME={APP_HOME}, where a launch mounts the app's data"),
        )),
    }
    match env_value(&env, "PULSE_SERVER") {
        Some(PULSE_SERVER) => out.push(line(Level::Ok, "sound", format!("PULSE_SERVER={PULSE_SERVER}"))),
        other => out.push(line(
            Level::Warn,
            "sound",
            format!(
                "no sound: set ENV PULSE_SERVER={PULSE_SERVER} so the app finds the streamer's audio{}",
                other.map(|v| format!(" (it is {v})")).unwrap_or_default()
            ),
        )),
    }
    if env_value(&env, "SDL_JOYSTICK_DISABLE_UDEV") == Some("1") {
        out.push(line(Level::Ok, "gamepads", "SDL_JOYSTICK_DISABLE_UDEV=1"));
    } else {
        out.push(line(
            Level::Warn,
            "gamepads",
            "hot-plugged gamepads go unseen by SDL apps: set ENV SDL_JOYSTICK_DISABLE_UDEV=1",
        ));
    }

    let uses_run = entrypoint
        .first()
        .is_some_and(|e| e.rsplit('/').next() == Some("cha-run"));
    out.push(line(
        Level::Info,
        "base",
        if uses_run {
            "starts through cha-run (cha-env-base): it waits for the compositor"
        } else {
            "doesn't start through cha-run (cha-env-base): the app itself must wait for the \
             compositor's socket in /run/cha"
        },
    ));
    if let Some(size) = image["Size"].as_u64() {
        out.push(line(Level::Info, "size", megabytes(size)));
    }
    out
}

/// A container's output, for the end of a failure line: the last lines, each
/// without the engine's timestamp.
async fn tail(docker: &Docker, container: &str) -> String {
    let Ok(lines) = docker.logs_tail(container, 4).await else {
        return String::new();
    };
    let text: Vec<String> = lines
        .iter()
        .map(|l| {
            l.split_once(' ')
                .map_or(l.as_str(), |(_, rest)| rest)
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .collect();
    if text.is_empty() {
        " (it printed nothing)".into()
    } else {
        let joined = text.join(" | ");
        let cut: String = joined.chars().take(400).collect();
        format!("; it said: {cut}")
    }
}

/// How the app's container is made for the check: a launch's own user,
/// environment and confinement, `/run/cha` as `run` says, no GPU, no devices.
fn app_config(
    image: &str,
    profile: SecurityProfile,
    check: &str,
    run: Run,
    entrypoint: Option<&str>,
) -> Value {
    let mut host = app_host_config(profile, 1024, DEFAULT_VM_MEMORY_MB);
    match run {
        // Not shared with a streamer: an empty directory the app owns, as a
        // launch's is once the streamer has made it over.
        Run::Alone => {
            host["Tmpfs"] =
                json!({ RUNTIME_DIR: format!("uid={APP_UID},gid={APP_UID},mode=0755") });
        }
        Run::Beside => {
            host["Mounts"] =
                json!([{ "Type": "volume", "Source": volume(check), "Target": RUNTIME_DIR }]);
        }
    }
    let mut config = json!({
        "Image": image,
        "User": app_user(),
        "Env": app_env(1920, 1080, 60),
        "Labels": { LABEL_CHECK: check },
        "HostConfig": host,
    });
    if let Some(script) = entrypoint {
        config["Entrypoint"] = json!(["sh", "-c", script]);
        config["Cmd"] = json!([]);
    }
    config
}

#[derive(Clone, Copy)]
enum Run {
    /// `/run/cha` is a private tmpfs: no compositor.
    Alone,
    /// `/run/cha` is the volume the check's streamer also mounts.
    Beside,
}

fn volume(check: &str) -> String {
    format!("cha-check-{check}-run")
}

fn name(check: &str, role: &str) -> String {
    format!("cha-check-{check}-{role}")
}

fn is_apparmor(err: &anyhow::Error) -> bool {
    format!("{err:#}").to_lowercase().contains("apparmor")
}

fn is_missing_program(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("executable file not found") || text.contains("no such file or directory")
}

/// What the probe container (`sh` run as the app) printed, as verdicts.
pub fn probe_verdicts(code: i64, output: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let find = |key: &str| {
        output
            .lines()
            .find_map(|l| l.trim().strip_prefix(key))
            .map(str::trim)
    };
    if code != 0 && find("uid=").is_none() {
        out.push(line(
            Level::Warn,
            "runs as 1000",
            format!(
                "can't tell: the image has no working `sh` to ask (exit {code}); the node runs it as {}",
                app_user()
            ),
        ));
        return out;
    }
    let uid = find("uid=").unwrap_or("?");
    let home = find("home=").unwrap_or("?");
    let writable = find("home-writable=") == Some("yes");
    if uid != APP_UID.to_string() {
        out.push(line(
            Level::Fail,
            "runs as 1000",
            format!(
                "runs as uid {uid}: the node runs every app as {}",
                app_user()
            ),
        ));
    } else {
        out.push(line(
            Level::Ok,
            "runs as 1000",
            "uid 1000 with no capabilities and no privilege gain",
        ));
    }
    if writable {
        out.push(line(
            Level::Ok,
            "home writable",
            format!("{home} is writable by {}", app_user()),
        ));
    } else {
        out.push(line(
            Level::Fail,
            "home writable",
            format!(
                "{home} isn't writable by {}: the app can't save anything. Make the home directory \
                 yours (`chown 1000:1000 {APP_HOME}`) and set ENV HOME={APP_HOME}",
                app_user()
            ),
        ));
    }
    out
}

/// Everything the dynamic checks need to clean up after.
struct Check<'a> {
    docker: &'a Docker,
    id: String,
}

impl Check<'_> {
    /// Removes every container and volume this run made.
    async fn cleanup(&self) {
        let label = format!("{LABEL_CHECK}={}", self.id);
        match self.docker.list(&label).await {
            Ok(found) => {
                for c in found {
                    let _ = self.docker.remove(&c.id, 0).await;
                }
            }
            Err(err) => eprintln!("cleaning up: can't list the check's containers: {err:#}"),
        }
        let _ = self.docker.remove_volume(&volume(&self.id)).await;
    }

    /// Runs the probe under `profile`: `Ok(None)` when the AppArmor profile
    /// refused it.
    async fn probe(&self, image: &str, profile: SecurityProfile) -> Result<Option<(i64, String)>> {
        let script = "echo uid=$(id -u); echo home=$HOME; \
                      if [ -w \"$HOME\" ]; then echo home-writable=yes; else echo home-writable=no; fi";
        let config = app_config(image, profile, &self.id, Run::Alone, Some(script));
        match self
            .docker
            .run(&name(&self.id, "probe"), &config, PROBE_TIMEOUT)
            .await
        {
            Ok(found) => Ok(Some(found)),
            Err(err) if is_apparmor(&err) => Ok(None),
            Err(err) => Err(err),
        }
    }

    /// Starts the app with its real entrypoint and no compositor.
    async fn without_compositor(&self, image: &str, profile: SecurityProfile) -> Result<Line> {
        let container = name(&self.id, "alone");
        let config = app_config(image, profile, &self.id, Run::Alone, None);
        let _ = self.docker.remove(&container, 0).await;
        let id = self.docker.create(&container, &config).await?;
        self.docker.start(&id).await?;
        let started = Instant::now();
        let line = loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let state = self.docker.inspect_container(&id).await?;
            if state["State"]["Running"].as_bool() != Some(true) {
                let code = state["State"]["ExitCode"].as_i64().unwrap_or(-1);
                let said = tail(self.docker, &id).await;
                if wants_kvm(profile, &said) {
                    break line(Level::Warn, "no compositor", kvm_missing(code, &said));
                }
                break line(
                    Level::Fail,
                    "no compositor",
                    format!(
                        "exited with code {code} after {:.1} s with no Wayland socket: an app must wait for \
                         the compositor (start through cha-run, or retry until /run/cha/wayland-0 exists){}",
                        started.elapsed().as_secs_f32(),
                        said
                    ),
                );
            }
            if started.elapsed() >= NO_COMPOSITOR_WAIT {
                break line(
                    Level::Ok,
                    "no compositor",
                    format!(
                        "still running after {} s with no Wayland socket (it waits)",
                        NO_COMPOSITOR_WAIT.as_secs()
                    ),
                );
            }
        };
        let _ = self.docker.remove(&id, 0).await;
        Ok(line)
    }

    /// Starts a real streamer on its CPU device and then the app beside it;
    /// the app must connect to the compositor. The streamer has no
    /// client list in `/info`, so the evidence is the kernel's: the
    /// streamer's `/proc/net/unix` shows a connected socket on its Wayland
    /// socket's path for each client.
    async fn with_streamer(
        &self,
        streamer_image: &str,
        image: &str,
        profile: SecurityProfile,
    ) -> Result<Line> {
        let port = free_ports().context("no free port range for the check's streamer")?;
        let key = NodeKey::from_secret([7; 32]).public_b64();
        let streamer_config = json!({
            "Image": streamer_image,
            "Cmd": [
                "--width", "1280", "--height", "720", "--fps", "60",
                "--app-uid", APP_UID.to_string(),
                "--listen", "127.0.0.1",
                "--http-port", port.to_string(),
                "--webrtc-port", (port + 1).to_string(),
                "--wt-port", (port + 2).to_string(),
                "--portal-key", key,
                "--environment-id", "image-check",
                "--device", "cpu",
            ],
            "Env": [
                format!("XDG_RUNTIME_DIR={RUNTIME_DIR}"),
                "RUST_LOG=info,smithay=warn,str0m=warn",
            ],
            "Labels": { LABEL_CHECK: &self.id },
            "HostConfig": {
                "NetworkMode": "host",
                "Mounts": [{ "Type": "volume", "Source": volume(&self.id), "Target": RUNTIME_DIR }],
                "RestartPolicy": { "Name": "no" },
                "Init": true,
            },
        });
        let streamer = self
            .docker
            .create(&name(&self.id, "streamer"), &streamer_config)
            .await?;
        self.docker.start(&streamer).await?;
        // The compositor's socket.
        let started = Instant::now();
        loop {
            tokio::time::sleep(POLL).await;
            let state = self.docker.inspect_container(&streamer).await?;
            if state["State"]["Running"].as_bool() != Some(true) {
                return Ok(line(
                    Level::Warn,
                    "compositor",
                    format!(
                        "couldn't test the connection: the check's streamer exited{}",
                        tail(self.docker, &streamer).await
                    ),
                ));
            }
            if let Ok((0, _)) = self
                .docker
                .exec(&streamer, &["test", "-S", WAYLAND_SOCKET])
                .await
            {
                break;
            }
            if started.elapsed() >= STREAMER_WAIT {
                return Ok(line(
                    Level::Warn,
                    "compositor",
                    "couldn't test the connection: the check's streamer opened no Wayland socket in 30 s",
                ));
            }
        }
        let config = app_config(image, profile, &self.id, Run::Beside, None);
        let app = self.docker.create(&name(&self.id, "app"), &config).await?;
        self.docker.start(&app).await?;
        let started = Instant::now();
        loop {
            tokio::time::sleep(POLL).await;
            let state = self.docker.inspect_container(&app).await?;
            let running = state["State"]["Running"].as_bool() == Some(true);
            // The connected socket belongs to the connecting side's network
            // namespace (the app's, not the host-network streamer's), so it is
            // read from the app. Without `cat` there, the streamer's log of the
            // surface the app opened is the evidence.
            let clients = match self.docker.exec(&app, &["cat", "/proc/net/unix"]).await {
                Ok((0, table)) if running => wayland_clients(&table, WAYLAND_SOCKET),
                _ => 0,
            };
            let opened_window = clients == 0
                && self
                    .docker
                    .logs_tail(&streamer, 200)
                    .await
                    .is_ok_and(|l| l.iter().any(|l| l.contains("handlers: new window")));
            let clients = clients.max(usize::from(opened_window));
            if clients > 0 {
                return Ok(line(
                    Level::Ok,
                    "compositor",
                    format!(
                        "connected to the compositor {:.1} s after starting",
                        started.elapsed().as_secs_f32()
                    ),
                ));
            }
            if !running {
                let code = state["State"]["ExitCode"].as_i64().unwrap_or(-1);
                let said = tail(self.docker, &app).await;
                // The check's compositor has no GPU; an app that needs one
                // (Steam's gamescope) can't be judged here.
                let lower = said.to_lowercase();
                if wants_kvm(profile, &said) {
                    return Ok(line(Level::Warn, "compositor", kvm_missing(code, &said)));
                }
                if lower.contains("vulkan") || lower.contains("no gpu") {
                    return Ok(line(
                        Level::Warn,
                        "compositor",
                        format!(
                            "couldn't test the connection: the app wants a GPU and the check runs the \
                             compositor on the CPU (exit {code}); try a real launch{said}"
                        ),
                    ));
                }
                return Ok(line(
                    Level::Fail,
                    "compositor",
                    format!("exited with code {code} without connecting to the compositor{said}"),
                ));
            }
            if started.elapsed() >= CONNECT_WAIT {
                return Ok(line(
                    Level::Fail,
                    "compositor",
                    format!(
                        "still running {} s after the Wayland socket appeared, but never connected to it: \
                         the app must be a Wayland client (or use Xwayland){}",
                        CONNECT_WAIT.as_secs(),
                        tail(self.docker, &app).await
                    ),
                ));
            }
        }
    }
}

/// Whether a `vm` app's output says it stopped for want of KVM: the check
/// gives it no `/dev/kvm`, so such an image can't be judged here.
fn wants_kvm(profile: SecurityProfile, said: &str) -> bool {
    profile == SecurityProfile::Vm && said.to_lowercase().contains("kvm")
}

fn kvm_missing(code: i64, said: &str) -> String {
    format!(
        "couldn't test it: the app wants /dev/kvm and the check doesn't pass it (exit {code}); \
         try a real launch on a node with KVM{said}"
    )
}

/// How many clients are connected to the Wayland socket at `path`, from the
/// streamer's `/proc/net/unix`: a socket the compositor accepted shows the
/// listener's path, in state 03 and without the listening flag.
pub fn wayland_clients(table: &str, path: &str) -> usize {
    table
        .lines()
        .filter(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            // Num RefCount Protocol Flags Type St Inode Path
            f.len() >= 8 && f[3] == "00000000" && f[5] == "03" && f[7] == path
        })
        .count()
}

/// Three consecutive ports that are free here, for the streamer.
fn free_ports() -> Option<u16> {
    (STREAMER_PORT_FROM..STREAMER_PORT_FROM + 300)
        .step_by(3)
        .find(|p| TcpListener::bind(("127.0.0.1", *p)).is_ok())
}

/// A pull's progress, as one line on stderr.
fn print_pull(image: &str, p: PullProgress) {
    if p.total == 0 {
        eprintln!("pulling {image} ...");
    } else {
        eprintln!(
            "pulling {image}: {} of {}",
            megabytes(p.done),
            megabytes(p.total)
        );
    }
}

/// Runs the whole check for `image` and prints the report on stdout; true
/// when every MUST check passed. `streamer_image` is the node's configured
/// streamer, whose published copy is the fallback.
pub async fn run(
    docker: &Docker,
    streamer_image: &str,
    image: &str,
    profile: SecurityProfile,
) -> Result<bool> {
    docker
        .ping()
        .await
        .context("the Docker engine isn't reachable (mount its socket, or set --docker-socket)")?;
    println!(
        "checking {image} as a {} environment",
        profile_name(profile)
    );
    let config = match docker.inspect_image(image).await? {
        Some(config) => config,
        None if names_registry(image) => {
            docker
                .pull_with(image, |p| print_pull(image, p))
                .await
                .with_context(|| format!("{image} isn't on this node and couldn't be pulled"))?;
            docker
                .inspect_image(image)
                .await?
                .ok_or_else(|| anyhow!("{image} was pulled but the engine doesn't list it"))?
        }
        None => bail!(
            "{image} isn't on this node, and a bare name is never pulled (it would come from \
             whoever owns that name on Docker Hub): build it here, or name its registry"
        ),
    };

    let mut lines = static_checks(&config);
    for l in &lines {
        println!("{l}");
    }
    // Only an image that can run at all is worth starting.
    let runnable = !lines
        .iter()
        .any(|l| l.level == Level::Fail && matches!(l.name, "platform" | "start command"));
    if runnable {
        let mut id = [0u8; 4];
        getrandom::fill(&mut id).map_err(|e| anyhow!("random source: {e}"))?;
        let check = Check {
            docker,
            id: id.iter().map(|b| format!("{b:02x}")).collect(),
        };
        let dynamic = tokio::select! {
            result = dynamic(&check, streamer_image, image, profile) => result,
            _ = tokio::signal::ctrl_c() => Err(anyhow!("interrupted")),
        };
        check.cleanup().await;
        match dynamic {
            Ok(found) => {
                for l in &found {
                    println!("{l}");
                }
                lines.extend(found);
            }
            Err(err) => {
                let l = line(Level::Fail, "run", format!("{err:#}"));
                println!("{l}");
                lines.push(l);
            }
        }
    } else {
        println!("skipped running it: it can't start here");
    }
    let failed = lines.iter().filter(|l| l.level == Level::Fail).count();
    let warned = lines.iter().filter(|l| l.level == Level::Warn).count();
    let mut summary = String::new();
    if failed == 0 {
        write!(summary, "{image} should run as a Cha environment")?;
        if warned > 0 {
            write!(
                summary,
                " ({warned} warning{})",
                if warned == 1 { "" } else { "s" }
            )?;
        }
    } else {
        write!(
            summary,
            "{image} will not run as a Cha environment: {failed} check{} failed",
            if failed == 1 { "" } else { "s" }
        )?;
    }
    println!("{summary}");
    Ok(passed(&lines))
}

/// The checks that start containers.
async fn dynamic(
    check: &Check<'_>,
    streamer_image: &str,
    image: &str,
    profile: SecurityProfile,
) -> Result<Vec<Line>> {
    let mut out = Vec::new();
    let mut profile = profile;
    let found = match check.probe(image, profile).await? {
        Some(found) => found,
        None => {
            out.push(line(
                Level::Warn,
                "apparmor",
                format!(
                    "this node hasn't loaded the {SANDBOX_APPARMOR} profile that the steam profile needs, so \
                     Steam environments won't start here (`cha-node --doctor` says how); the rest ran as browser"
                ),
            ));
            profile = SecurityProfile::Browser;
            check
                .probe(image, profile)
                .await?
                .ok_or_else(|| anyhow!("the engine refused the app's AppArmor profile"))?
        }
    };
    let (code, text) = &found;
    if *code != 0 && is_missing_program(text) {
        out.push(line(
            Level::Warn,
            "runs as 1000",
            "can't tell: the image has no `sh` to ask with (the check overrides the entrypoint for this one)",
        ));
    } else {
        out.extend(probe_verdicts(*code, text));
    }
    out.push(check.without_compositor(image, profile).await?);

    match streamer(check.docker, streamer_image).await {
        Ok(streamer_image) => out.push(check.with_streamer(&streamer_image, image, profile).await?),
        Err(err) => out.push(line(
            Level::Warn,
            "compositor",
            format!("couldn't test the connection: no streamer image to run ({err:#})"),
        )),
    }
    Ok(out)
}

/// The streamer image the check runs the compositor in: the node's own, else
/// its published copy (pulled, with progress on stderr).
async fn streamer(docker: &Docker, configured: &str) -> Result<String> {
    let candidates = crate::environments::streamer_candidates(configured, AGENT_VERSION);
    for image in &candidates {
        if docker.image_exists(image).await? {
            return Ok(image.clone());
        }
    }
    let image = candidates
        .iter()
        .find(|i| names_registry(i))
        .ok_or_else(|| anyhow!("none of {} is on this node", candidates.join(", ")))?;
    docker.pull_with(image, |p| print_pull(image, p)).await?;
    Ok(image.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cha_image() -> Value {
        json!({
            "Os": "linux",
            "Architecture": "amd64",
            "Size": 900_000_000u64,
            "Config": {
                "Entrypoint": ["cha-run"],
                "Cmd": ["google-chrome"],
                "Env": [
                    "PATH=/usr/bin",
                    "HOME=/home/cha",
                    "PULSE_SERVER=unix:/run/cha/pulse/native",
                    "SDL_JOYSTICK_DISABLE_UDEV=1",
                ],
            },
        })
    }

    fn level_of(lines: &[Line], name: &str) -> Level {
        lines.iter().find(|l| l.name == name).unwrap().level
    }

    #[test]
    fn a_conforming_image_has_nothing_to_say() {
        let lines = static_checks(&cha_image());
        assert!(passed(&lines));
        assert!(
            lines
                .iter()
                .all(|l| matches!(l.level, Level::Ok | Level::Info)),
            "{lines:#?}"
        );
        let base = lines.iter().find(|l| l.name == "base").unwrap();
        assert!(base.detail.starts_with("starts through cha-run"));
    }

    #[test]
    fn the_wrong_platform_or_no_command_fails() {
        let mut image = cha_image();
        image["Architecture"] = json!("arm64");
        let lines = static_checks(&image);
        assert_eq!(level_of(&lines, "platform"), Level::Fail);
        assert!(!passed(&lines));

        let mut image = cha_image();
        image["Config"]["Entrypoint"] = Value::Null;
        image["Config"]["Cmd"] = Value::Null;
        let lines = static_checks(&image);
        assert_eq!(level_of(&lines, "start command"), Level::Fail);
        assert!(!passed(&lines));

        // Either alone is a command.
        let mut image = cha_image();
        image["Config"]["Entrypoint"] = Value::Null;
        assert_eq!(level_of(&static_checks(&image), "start command"), Level::Ok);
    }

    #[test]
    fn missing_environment_only_warns() {
        let image = json!({
            "Os": "linux",
            "Architecture": "amd64",
            "Config": { "Cmd": ["nginx"], "Env": ["HOME=/root"] },
        });
        let lines = static_checks(&image);
        assert!(passed(&lines));
        for name in ["HOME", "sound", "gamepads"] {
            assert_eq!(level_of(&lines, name), Level::Warn, "{name}");
        }
        let base = lines.iter().find(|l| l.name == "base").unwrap();
        assert!(base.detail.contains("doesn't start through cha-run"));

        let mut image = cha_image();
        image["Config"]["Env"] = json!(["PULSE_SERVER=tcp:somewhere"]);
        let lines = static_checks(&image);
        assert_eq!(level_of(&lines, "HOME"), Level::Warn);
        let sound = lines.iter().find(|l| l.name == "sound").unwrap();
        assert!(sound.detail.contains("tcp:somewhere"));
    }

    #[test]
    fn the_probes_answers_become_verdicts() {
        let good = probe_verdicts(0, "uid=1000\nhome=/home/cha\nhome-writable=yes\n");
        assert!(passed(&good));
        assert_eq!(good.len(), 2);

        let readonly = probe_verdicts(0, "uid=1000\nhome=/\nhome-writable=no\n");
        assert_eq!(level_of(&readonly, "home writable"), Level::Fail);
        assert!(readonly[1].detail.contains("chown 1000:1000 /home/cha"));

        let root = probe_verdicts(0, "uid=0\nhome=/root\nhome-writable=yes\n");
        assert_eq!(level_of(&root, "runs as 1000"), Level::Fail);

        // No shell to ask: unknown, not a failure.
        let nothing = probe_verdicts(127, "sh: not found");
        assert_eq!(nothing.len(), 1);
        assert_eq!(nothing[0].level, Level::Warn);
    }

    #[test]
    fn clients_are_the_connected_sockets_on_the_wayland_path() {
        let table = "Num       RefCount Protocol Flags    Type St Inode Path\n\
            0000: 00000002 00000000 00010000 0001 01 111 /run/cha/wayland-0\n\
            0000: 00000003 00000000 00000000 0001 03 222 /run/cha/wayland-0\n\
            0000: 00000003 00000000 00000000 0001 03 333 /run/cha/wayland-0\n\
            0000: 00000003 00000000 00000000 0001 03 444 /run/cha/pulse/native\n\
            0000: 00000003 00000000 00000000 0001 03 555\n";
        assert_eq!(wayland_clients(table, "/run/cha/wayland-0"), 2);
        assert_eq!(wayland_clients("", "/run/cha/wayland-0"), 0);
    }

    #[test]
    fn the_app_is_confined_as_a_launch_confines_it() {
        for (profile, seccomp, apparmor) in [
            (SecurityProfile::Standard, false, false),
            (SecurityProfile::Browser, true, false),
            (SecurityProfile::Steam, true, true),
            (SecurityProfile::Vm, false, false),
        ] {
            let config = app_config("img", profile, "abcd", Run::Beside, None);
            assert_eq!(config["User"], "1000:1000");
            let host = &config["HostConfig"];
            assert_eq!(host["CapDrop"], json!(["ALL"]));
            assert_eq!(host["Init"], true);
            assert!(host.get("DeviceRequests").is_none());
            assert!(host.get("Devices").is_none());
            let options = strings(&host["SecurityOpt"]);
            assert!(options.contains(&"no-new-privileges".to_string()));
            assert_eq!(options.iter().any(|o| o.starts_with("seccomp=")), seccomp);
            assert_eq!(
                options.contains(&"apparmor=cha-sandbox".to_string()),
                apparmor
            );
            assert_eq!(config["Labels"][LABEL_CHECK], "abcd");
            assert!(config["Labels"].get("sh.cha.env").is_none());
        }
    }

    #[test]
    fn profiles_parse_by_name() {
        assert_eq!(parse_profile("steam").unwrap(), SecurityProfile::Steam);
        assert_eq!(parse_profile("vm").unwrap(), SecurityProfile::Vm);
        assert!(parse_profile("root").is_err());
    }

    #[test]
    fn a_vm_that_stopped_for_want_of_kvm_is_a_warning_not_a_failure() {
        let said = "\nqemu-system-x86_64: Could not access KVM kernel module: No such file";
        assert!(wants_kvm(SecurityProfile::Vm, said));
        assert!(!wants_kvm(SecurityProfile::Browser, said));
        assert!(!wants_kvm(SecurityProfile::Vm, "\nsegfault"));
    }

    #[test]
    fn lines_read_plainly() {
        let l = line(Level::Fail, "platform", "linux/arm64");
        assert_eq!(l.to_string(), "FAIL  platform       linux/arm64");
    }
}

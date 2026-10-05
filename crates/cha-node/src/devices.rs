//! What the devices can encode, as the streamer image says (`docs/devices.md`):
//! an Intel or AMD GPU's VA-API codecs, and the CPU's software ones (H.264,
//! and AV1 when the image has SVT-AV1). The agent doesn't link libva or the
//! encoders; it asks the image the streamer runs from, in a throwaway
//! container that gets only that render node (the CPU's gets nothing), so
//! what it reports is what a streamer there would manage.

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cha_wire::{Device, DeviceKind};
use serde::Deserialize;
use serde_json::json;
use tracing::{debug, info};

use crate::docker::Docker;
use crate::environments::valid_render_node;
use crate::inventory;

/// A probe is quick (a driver opens, asks its entrypoints); a hung one is the
/// driver's.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a probe that found a device stands. Short for one that found
/// none: a streamer image pulled later, or a driver loaded, should show soon.
const FOUND_TTL: Duration = Duration::from_secs(3600);
const MISSING_TTL: Duration = Duration::from_secs(240);

/// What `cha-streamer --probe-device` prints.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Probed {
    kind: String,
    name: String,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    render_node: Option<String>,
    #[serde(default)]
    codecs: Vec<String>,
}

/// The probed devices, kept so the inventory refresh doesn't start a
/// container per render node every time.
#[derive(Default)]
pub struct DeviceProbes {
    seen: Mutex<HashMap<String, (Instant, Option<Device>)>>,
    /// The CPU's codecs: when asked, and what the image said (`None`: it
    /// didn't).
    cpu: Mutex<Option<(Instant, Option<Vec<String>>)>>,
}

impl DeviceProbes {
    /// The codecs the CPU device makes, as the streamer image says. `None`
    /// when it can't say (an older image, or none here): the node falls back
    /// to H.264, which every image has.
    pub async fn cpu_codecs(&self, docker: &Docker, image: &str) -> Option<Vec<String>> {
        let cached = {
            let seen = self.cpu.lock().expect("probe cache lock");
            seen.as_ref().and_then(|(at, codecs)| {
                let ttl = if codecs.is_some() {
                    FOUND_TTL
                } else {
                    MISSING_TTL
                };
                (at.elapsed() < ttl).then(|| codecs.clone())
            })
        };
        if let Some(codecs) = cached {
            return codecs;
        }
        let codecs = match probe_cpu(docker, image).await {
            Ok(codecs) => Some(codecs),
            Err(err) => {
                debug!("no CPU probe, offering H.264 only: {err:#}");
                None
            }
        };
        if let Some(codecs) = &codecs {
            info!(?codecs, "CPU codecs");
        }
        *self.cpu.lock().expect("probe cache lock") = Some((Instant::now(), codecs.clone()));
        codecs
    }

    /// The VA-API devices on this host: every candidate render node the
    /// streamer image can encode with. An image that doesn't know
    /// `--probe-device` (an older one) or isn't here yields none.
    pub async fn devices(&self, docker: &Docker, image: &str) -> Vec<Device> {
        let nodes = tokio::task::spawn_blocking(inventory::vaapi_candidates)
            .await
            .unwrap_or_default();
        let mut found = Vec::new();
        for node in nodes {
            let cached = {
                let seen = self.seen.lock().expect("probe cache lock");
                seen.get(&node).and_then(|(at, device)| {
                    let ttl = if device.is_some() {
                        FOUND_TTL
                    } else {
                        MISSING_TTL
                    };
                    (at.elapsed() < ttl).then(|| device.clone())
                })
            };
            let device = match cached {
                Some(device) => device,
                None => {
                    let device = match probe(docker, image, &node).await {
                        Ok(device) => device,
                        Err(err) => {
                            debug!(%node, "no VA-API device: {err:#}");
                            None
                        }
                    };
                    if let Some(d) = &device {
                        info!(%node, name = %d.name, codecs = ?d.codecs, "VA-API device");
                    }
                    self.seen
                        .lock()
                        .expect("probe cache lock")
                        .insert(node.clone(), (Instant::now(), device.clone()));
                    device
                }
            };
            found.extend(device);
        }
        found
    }
}

/// Asks the streamer image what `render_node` can encode: a throwaway
/// container, no network, a read-only root, that one device and its group.
/// `None` when it can't encode (or the image can't tell).
pub async fn probe(docker: &Docker, image: &str, render_node: &str) -> Result<Option<Device>> {
    if !valid_render_node(render_node) {
        anyhow::bail!("{render_node} isn't a render node");
    }
    if !docker.image_exists(image).await? {
        anyhow::bail!("the streamer image {image} isn't here");
    }
    let gid = std::fs::metadata(render_node).map(|m| m.gid()).ok();
    let name = render_node.rsplit('/').next().unwrap_or("renderD");
    let mut config = probe_config(image, &format!("vaapi:{render_node}"));
    config["HostConfig"]["Devices"] = json!([{
        "PathOnHost": render_node,
        "PathInContainer": render_node,
        "CgroupPermissions": "rw",
    }]);
    config["HostConfig"]["GroupAdd"] = json!(gid.iter().map(|g| g.to_string()).collect::<Vec<_>>());
    let output = run_probe(docker, &format!("cha-probe-{name}"), &config).await?;
    parse_probe(&output, render_node)
        .context("the probe printed no device")
        .map(Some)
}

/// Asks the streamer image what the CPU device makes: the same throwaway
/// container, with no device at all.
pub async fn probe_cpu(docker: &Docker, image: &str) -> Result<Vec<String>> {
    if !docker.image_exists(image).await? {
        anyhow::bail!("the streamer image {image} isn't here");
    }
    let output = run_probe(docker, "cha-probe-cpu", &probe_config(image, "cpu")).await?;
    parse_cpu_probe(&output).context("the probe printed no CPU device")
}

/// A container that runs `cha-streamer --probe-device <spec>`: no network, a
/// read-only root, no capabilities.
fn probe_config(image: &str, spec: &str) -> serde_json::Value {
    json!({
        "Image": image,
        "Entrypoint": ["cha-streamer", "--probe-device", spec],
        "HostConfig": {
            "NetworkMode": "none",
            "ReadonlyRootfs": true,
            "CapDrop": ["ALL"],
            "SecurityOpt": ["no-new-privileges"],
        },
    })
}

/// Runs a probe container to its end; its output, or why it failed.
async fn run_probe(docker: &Docker, name: &str, config: &serde_json::Value) -> Result<String> {
    let (code, output) = docker.run(name, config, PROBE_TIMEOUT).await?;
    if code != 0 {
        let said = output.trim().lines().last().unwrap_or("no output");
        anyhow::bail!("the probe exited {code}: {said}");
    }
    Ok(output)
}

/// The probe's JSON: the last line that is an object (a probe may log before
/// it prints).
fn probed(output: &str) -> Option<Probed> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .filter(|l| l.starts_with('{'))
        .find_map(|l| serde_json::from_str(l).ok())
}

/// The codecs a CPU probe reports. `None` unless it is the CPU device, with
/// H.264 (what every image has; a report without it is not one to trust).
pub(crate) fn parse_cpu_probe(output: &str) -> Option<Vec<String>> {
    let probed = probed(output)?;
    (DeviceKind::parse(&probed.kind) == Some(DeviceKind::Cpu)
        && probed.codecs.iter().any(|c| c == "h264"))
    .then_some(probed.codecs)
}

/// The device a probe's output describes: the last line that is a JSON object
/// (a probe may log before it prints). `None` unless it is a VA-API device
/// that encodes something.
pub(crate) fn parse_probe(output: &str, render_node: &str) -> Option<Device> {
    let probed = probed(output)?;
    if DeviceKind::parse(&probed.kind) != Some(DeviceKind::Vaapi) || probed.codecs.is_empty() {
        return None;
    }
    let node = probed
        .render_node
        .filter(|n| n == render_node)
        .unwrap_or_else(|| render_node.to_string());
    let short = node.rsplit('/').next().unwrap_or(&node).to_string();
    Some(Device {
        id: format!("vaapi:{short}"),
        kind: DeviceKind::Vaapi,
        name: probed.name,
        render_node: Some(node),
        vendor: probed.vendor,
        codecs: probed.codecs,
        cores: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_a_probe_prints() {
        let out = "2026-10-05T10:00:00Z INFO opening\n\
                   {\"kind\":\"vaapi\",\"name\":\"Intel Arc A380\",\"vendor\":\"intel\",\
                   \"renderNode\":\"/dev/dri/renderD129\",\"codecs\":[\"h264\",\"hevc\",\"av1\"]}\n";
        let device = parse_probe(out, "/dev/dri/renderD129").expect("a device");
        assert_eq!(device.id, "vaapi:renderD129");
        assert_eq!(device.kind, DeviceKind::Vaapi);
        assert_eq!(device.vendor.as_deref(), Some("intel"));
        assert_eq!(device.codecs, ["h264", "hevc", "av1"]);
        assert_eq!(device.render_node.as_deref(), Some("/dev/dri/renderD129"));
    }

    #[test]
    fn a_probe_that_found_nothing_to_encode_with_is_no_device() {
        let none = "{\"kind\":\"vaapi\",\"name\":\"x\",\"codecs\":[]}";
        assert!(parse_probe(none, "/dev/dri/renderD128").is_none());
        let cpu = "{\"kind\":\"cpu\",\"name\":\"x\",\"codecs\":[\"h264\"]}";
        assert!(parse_probe(cpu, "/dev/dri/renderD128").is_none());
        // An older image's usage error.
        assert!(
            parse_probe(
                "error: unexpected argument '--probe-device'",
                "/dev/dri/renderD128"
            )
            .is_none()
        );
    }

    #[test]
    fn reads_the_cpu_codecs_a_probe_prints() {
        let out = "SVT-AV1 banner\n\
                   {\"kind\":\"cpu\",\"name\":\"Intel Core\",\"codecs\":[\"h264\",\"av1\"],\"cores\":16}\n";
        assert_eq!(parse_cpu_probe(out).unwrap(), ["h264", "av1"]);
        // An image without SVT-AV1.
        let out = "{\"kind\":\"cpu\",\"name\":\"x\",\"codecs\":[\"h264\"],\"cores\":4}";
        assert_eq!(parse_cpu_probe(out).unwrap(), ["h264"]);
    }

    #[test]
    fn a_cpu_probe_that_isnt_one_is_no_answer() {
        // An older image's usage error, a different device, no H.264.
        assert!(parse_cpu_probe("error: unexpected argument '--probe-device'").is_none());
        let vaapi = "{\"kind\":\"vaapi\",\"name\":\"x\",\"codecs\":[\"h264\"]}";
        assert!(parse_cpu_probe(vaapi).is_none());
        let none = "{\"kind\":\"cpu\",\"name\":\"x\",\"codecs\":[\"av1\"]}";
        assert!(parse_cpu_probe(none).is_none());
        assert!(parse_cpu_probe("").is_none());
    }

    #[test]
    fn the_probe_container_is_locked_down() {
        let config = probe_config("cha/streamer:dev", "cpu");
        assert_eq!(
            config["Entrypoint"],
            json!(["cha-streamer", "--probe-device", "cpu"])
        );
        let host = &config["HostConfig"];
        assert_eq!(host["NetworkMode"], "none");
        assert_eq!(host["ReadonlyRootfs"], true);
        assert_eq!(host["CapDrop"], json!(["ALL"]));
        // The CPU probe is given no device.
        assert!(host.get("Devices").is_none());
    }
}

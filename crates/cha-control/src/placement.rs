//! Where a launch runs (`docs/devices.md`): every device on every online node
//! is an option for a template, with a score and, when it can't or shouldn't
//! be chosen, a reason. `auto` is what the Launch button does.
//!
//! The score starts from the kind of device (NVIDIA over VA-API over the CPU)
//! and drops with how busy the node is now (its live usage, from
//! [`crate::nodes::NodeHub::usage`]) and with each environment already
//! running on the device. Not allowed: the CPU for an app that needs a GPU, a
//! device that encodes nothing a browser plays, and a node whose agent can't
//! keep the app's data. A GPU with little VRAM free, or a node whose image
//! disk is nearly full, stays allowed (the user may know better) but is never
//! the automatic choice.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use cha_wire::{Device, DeviceKind, GpuUsage, NodeUsage};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::auth::CurrentUser;
use crate::db::{self, Role};
use crate::environments::{self, Template};
use crate::error::{ApiError, ApiResult};
use crate::storage;

/// The codecs a browser plays that a device can offer.
const BROWSER_CODECS: [&str; 3] = ["h264", "hevc", "av1"];
/// Below this much free VRAM a GPU isn't chosen automatically.
const LOW_VRAM_BYTES: u64 = 2 << 30;
/// Below this much free space on the disk holding a node's images, an
/// option is never the automatic choice: pulling an image may fill it.
const LOW_DISK_BYTES: u64 = 5 << 30;
/// What each environment already on a device costs it.
const PER_ENVIRONMENT: f64 = 10.0;
/// What already holding the image is worth: more than any one load step, so
/// it breaks a tie and beats a slightly busier node, but a much faster device
/// still wins.
const HAS_IMAGE: i64 = 15;

pub fn routes() -> Router<AppState> {
    Router::new().route("/placements", get(list))
}

/// A device on a node.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    /// The node's id.
    pub node: String,
    /// The device's id on that node.
    pub device: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacementOption {
    pub node: String,
    pub node_name: String,
    pub device: String,
    pub kind: DeviceKind,
    /// What to call it: the GPU's name, or `CPU only`.
    pub label: String,
    pub allowed: bool,
    /// Why it isn't allowed, or what to know before choosing it anyway.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Something to know that doesn't keep `auto` away ("downloads the image
    /// first").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub score: i64,
    /// Its node is `manual`: never what `auto` picks.
    #[serde(skip)]
    manual: bool,
}

impl PlacementOption {
    /// What `auto` may pick: allowed, with nothing to warn about.
    fn automatic(&self) -> bool {
        self.allowed && self.reason.is_none() && !self.manual
    }

    fn choice(&self) -> Choice {
        Choice {
            node: self.node.clone(),
            device: self.device.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Placements {
    /// What launching without choosing does; `None` when nothing is allowed.
    pub auto: Option<Choice>,
    /// Best first, the allowed ones ahead of the others.
    pub options: Vec<PlacementOption>,
}

/// An online node, as placement sees it.
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub devices: Vec<Device>,
    pub usage: Option<NodeUsage>,
    /// Environments running on each device, by its id.
    pub running: HashMap<String, u32>,
    /// Whether its agent keeps app data (it reports a data root).
    pub keeps_app_data: bool,
    /// The `repo:tag` images its engine holds (ADR 0017).
    pub images: Vec<String>,
    /// Its agent's release, which fills `{version}` in image candidates.
    pub agent_version: Option<String>,
    /// `manual`: never the automatic choice.
    pub manual: bool,
    /// Free bytes on the disk that holds its images (the only disk it
    /// reports, or the one used for `images`); `None` when it reports none.
    pub images_disk_free: Option<u64>,
}

impl NodeView {
    /// Whether the node already holds any of `candidates`: exactly, once
    /// `{version}` is filled with its agent's release, or, when it reports
    /// none, by the name before the `:`.
    pub fn has_image(&self, candidates: &[String]) -> bool {
        candidates.iter().any(|c| match &self.agent_version {
            Some(v) => {
                let c = c.replace("{version}", v);
                self.images.contains(&c)
            }
            None => {
                let name = c.split(':').next().unwrap_or(c);
                self.images
                    .iter()
                    .any(|i| i.split(':').next().unwrap_or(i) == name)
            }
        })
    }
}

/// What a template asks of the place it runs.
pub struct Needs {
    pub gpu: bool,
    pub app_data: bool,
    /// The images the template can run from, in order; empty when it doesn't
    /// matter.
    pub images: Vec<String>,
}

impl Needs {
    pub fn of(template: &Template, app_data: bool) -> Self {
        Self {
            gpu: template.needs_gpu,
            app_data,
            images: template.image_candidates(),
        }
    }
}

/// The GPU's name without its maker's marketing: `RTX 4090`.
fn short_name(name: &str) -> &str {
    let name = name.trim();
    ["NVIDIA GeForce ", "NVIDIA "]
        .iter()
        .find_map(|p| name.strip_prefix(p))
        .unwrap_or(name)
}

fn label(device: &Device) -> String {
    match device.kind {
        DeviceKind::Cpu => "CPU only".to_string(),
        _ => short_name(&device.name).to_string(),
    }
}

/// The NVML entry of an `nvidia:<index>` device.
fn gpu_usage<'a>(device: &Device, usage: &'a NodeUsage) -> Option<&'a GpuUsage> {
    let index: u32 = device.id.strip_prefix("nvidia:")?.parse().ok()?;
    usage.gpus.iter().find(|g| g.index == index)
}

/// What a device is worth right now: its kind's base, less the load on it
/// (percent 0..100 from the node's usage) and less for each environment
/// already there.
pub fn score(kind: DeviceKind, cpu: Option<f64>, gpu: Option<&GpuUsage>, running: u32) -> i64 {
    let cpu = cpu.unwrap_or(0.0).clamp(0.0, 100.0);
    let (base, load) = match kind {
        DeviceKind::Nvidia => {
            let util = f64::from(gpu.and_then(|g| g.util).unwrap_or(0).min(100));
            let vram = gpu
                .and_then(|g| Some(g.vram_used? as f64 / g.vram_total?.max(1) as f64))
                .unwrap_or(0.0)
                .clamp(0.0, 1.0);
            (100.0, cpu * 0.05 + util * 0.3 + vram * 10.0)
        }
        // The encoder sits next to the CPU's load, not on it.
        DeviceKind::Vaapi => (60.0, cpu * 0.1),
        // x264 is the CPU's load.
        DeviceKind::Cpu => (20.0, cpu * 0.3),
    };
    (base - load - f64::from(running) * PER_ENVIRONMENT).round() as i64
}

/// Every device of `nodes` as an option for an app with `needs`.
pub fn options(needs: &Needs, nodes: &[NodeView]) -> Vec<PlacementOption> {
    let mut options = Vec::new();
    for node in nodes {
        for device in &node.devices {
            let usage = node.usage.as_ref();
            let gpu = usage.and_then(|u| gpu_usage(device, u));
            let running = node.running.get(&device.id).copied().unwrap_or(0);
            let mut allowed = true;
            let mut reason = None;
            let mut notes: Vec<&str> = Vec::new();
            if device.kind == DeviceKind::Cpu && needs.gpu {
                allowed = false;
                reason = Some("it needs a GPU".to_string());
            } else if !device
                .codecs
                .iter()
                .any(|c| BROWSER_CODECS.contains(&c.as_str()))
            {
                allowed = false;
                reason = Some("it can't encode a video format browsers play".to_string());
            } else if needs.app_data && !node.keeps_app_data {
                allowed = false;
                reason = Some(
                    "its agent needs an update to keep app data (it reports its data root once it can)"
                        .to_string(),
                );
            } else if let Some(free) = gpu
                .and_then(|g| Some(g.vram_total?.saturating_sub(g.vram_used?)))
                .filter(|free| *free < LOW_VRAM_BYTES)
            {
                reason = Some(format!(
                    "only {:.1} GB of VRAM free",
                    free as f64 / f64::from(1u32 << 30)
                ));
            } else if let Some(free) = node.images_disk_free.filter(|f| *f < LOW_DISK_BYTES) {
                reason = Some(format!(
                    "only {:.1} GB of disk free",
                    free as f64 / f64::from(1u32 << 30)
                ));
            }
            if node.manual && allowed {
                notes.push("picked by hand only");
            }
            let has_image = !needs.images.is_empty() && node.has_image(&needs.images);
            // Only worth a word when it is the one thing to know, and only
            // from a node that says what it holds (an older agent lists
            // nothing, which isn't "has nothing").
            if reason.is_none()
                && allowed
                && !needs.images.is_empty()
                && !node.images.is_empty()
                && !has_image
            {
                notes.push("downloads the image first");
            }
            options.push(PlacementOption {
                node: node.id.clone(),
                node_name: node.name.clone(),
                device: device.id.clone(),
                kind: device.kind,
                label: label(device),
                allowed,
                reason,
                note: (!notes.is_empty()).then(|| notes.join("; ")),
                score: score(device.kind, usage.map(|u| u.cpu), gpu, running)
                    + if has_image { HAS_IMAGE } else { 0 },
                manual: node.manual,
            });
        }
    }
    // Allowed first, then the ones with nothing to warn about, then by score.
    options.sort_by(|a, b| {
        (
            !a.allowed,
            !a.automatic(),
            -a.score,
            &a.node_name,
            &a.device,
        )
            .cmp(&(
                !b.allowed,
                !b.automatic(),
                -b.score,
                &b.node_name,
                &b.device,
            ))
    });
    options
}

/// The best option nothing warns about, else the best allowed one; never a
/// `manual` node's.
pub fn best(options: &[PlacementOption]) -> Option<&PlacementOption> {
    options
        .iter()
        .find(|o| o.automatic())
        .or_else(|| options.iter().find(|o| o.allowed && !o.manual))
}

pub fn placements(needs: &Needs, nodes: &[NodeView]) -> Placements {
    let options = options(needs, nodes);
    Placements {
        auto: best(&options).map(PlacementOption::choice),
        options,
    }
}

/// Why nothing can run an app, for the launch that found no place: what stood
/// in each device's way.
pub fn nothing_allowed(options: &[PlacementOption]) -> String {
    if options.is_empty() {
        return "no node is online".to_string();
    }
    let mut why: Vec<String> = options
        .iter()
        .take(3)
        .map(|o| {
            format!(
                "{} ({}): {}",
                o.node_name,
                o.label,
                o.reason.as_deref().unwrap_or("not allowed")
            )
        })
        .collect();
    if options.len() > 3 {
        why.push(format!("and {} more", options.len() - 3));
    }
    why.join("; ")
}

/// Free bytes on the disk holding a node's images: the one used for them, or
/// the only one reported.
fn images_disk_free(disks: &[cha_wire::Disk]) -> Option<u64> {
    disks
        .iter()
        .find(|d| d.uses.contains(&cha_wire::DiskUse::Images))
        .or(match disks {
            [only] => Some(only),
            _ => None,
        })
        .map(|d| d.free_bytes)
}

/// The online nodes with their devices, usage and what runs on them.
pub async fn online_nodes(state: &AppState) -> ApiResult<Vec<NodeView>> {
    let mut counts: HashMap<String, HashMap<Option<String>, u32>> = HashMap::new();
    for (node, device, n) in db::running_by_device(&state.db).await? {
        *counts.entry(node).or_default().entry(device).or_default() += n as u32;
    }
    let mut nodes = Vec::new();
    for row in db::list_nodes(&state.db).await? {
        if state.nodes.connected_since(&row.id).await.is_none() {
            continue;
        }
        let inventory = row
            .inventory
            .as_deref()
            .and_then(|j| serde_json::from_str::<cha_wire::Inventory>(j).ok());
        let devices = inventory
            .as_ref()
            .map(|inv| inv.devices_or_derived())
            .unwrap_or_default();
        // Launched before devices: on the NVIDIA GPU.
        let nvidia = devices
            .iter()
            .find(|d| d.kind == DeviceKind::Nvidia)
            .map(|d| d.id.clone());
        let mut running: HashMap<String, u32> = HashMap::new();
        for (device, n) in counts.remove(&row.id).unwrap_or_default() {
            if let Some(id) = device.or_else(|| nvidia.clone()) {
                *running.entry(id).or_default() += n;
            }
        }
        nodes.push(NodeView {
            usage: state.nodes.usage(&row.id).map(|(u, _)| u),
            keeps_app_data: inventory
                .as_ref()
                .is_some_and(|inv| inv.data_root.is_some()),
            images: inventory
                .as_ref()
                .map(|inv| inv.images.clone())
                .unwrap_or_default(),
            agent_version: inventory.as_ref().and_then(|inv| inv.agent_version.clone()),
            manual: inventory
                .as_ref()
                .is_some_and(|inv| inv.placement == Some(cha_wire::PlacementMode::Manual)),
            images_disk_free: inventory
                .as_ref()
                .and_then(|inv| images_disk_free(&inv.disks)),
            id: row.id,
            name: row.name,
            devices,
            running,
        });
    }
    Ok(nodes)
}

/// The options for `user`'s launch of `template`.
pub async fn for_template(
    state: &AppState,
    user_id: &str,
    template: &Template,
    nodes: &[NodeView],
) -> ApiResult<Placements> {
    let settings = storage::effective_for(state, user_id, template).await?;
    let app_data = storage::spec_storage(user_id, template, settings).is_some();
    Ok(placements(&Needs::of(template, app_data), nodes))
}

#[derive(Deserialize)]
struct PlacementQuery {
    template: Option<String>,
}

/// `GET /api/placements?template=<id>`: where the template could run, and
/// what Launch would pick. Without `template`: that for every template, as
/// `{ "templates": { <id>: … } }`.
async fn list(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Query(query): Query<PlacementQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    if user.role == Role::Guest {
        return Err(ApiError::forbidden(
            "guests_cannot_launch",
            "guests can't launch environments",
        ));
    }
    let nodes = online_nodes(&state).await?;
    if let Some(id) = query.template {
        let template = environments::find(&state, &id)
            .ok_or_else(|| ApiError::NotFound("no such template".into()))?;
        let placements = for_template(&state, &user.id, &template, &nodes).await?;
        return Ok(Json(
            serde_json::to_value(placements).map_err(anyhow::Error::from)?,
        ));
    }
    let mut all = serde_json::Map::new();
    for template in &environments::all(&state) {
        let placements = for_template(&state, &user.id, template, &nodes).await?;
        all.insert(
            template.id.clone(),
            serde_json::to_value(placements).map_err(anyhow::Error::from)?,
        );
    }
    Ok(Json(serde_json::json!({ "templates": all })))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1 << 30;

    fn device(id: &str, kind: DeviceKind, name: &str) -> Device {
        Device {
            id: id.into(),
            kind,
            name: name.into(),
            render_node: (kind == DeviceKind::Vaapi).then(|| "/dev/dri/renderD129".into()),
            vendor: None,
            codecs: vec!["h264".into()],
            cores: (kind == DeviceKind::Cpu).then_some(8),
        }
    }

    fn node(name: &str, devices: Vec<Device>) -> NodeView {
        NodeView {
            id: format!("id-{name}"),
            name: name.into(),
            devices,
            usage: None,
            running: HashMap::new(),
            keeps_app_data: true,
            images: Vec::new(),
            agent_version: None,
            manual: false,
            images_disk_free: None,
        }
    }

    fn rtx() -> Device {
        device("nvidia:0", DeviceKind::Nvidia, "NVIDIA GeForce RTX 4090")
    }

    fn gpu_usage_of(util: u32, used_gb: u64, total_gb: u64) -> NodeUsage {
        NodeUsage {
            gpus: vec![GpuUsage {
                index: 0,
                util: Some(util),
                vram_used: Some(used_gb * GB),
                vram_total: Some(total_gb * GB),
                ..GpuUsage::default()
            }],
            ..NodeUsage::default()
        }
    }

    const PLAIN: Needs = Needs {
        gpu: false,
        app_data: false,
        images: Vec::new(),
    };
    const GAME: Needs = Needs {
        gpu: true,
        app_data: false,
        images: Vec::new(),
    };

    #[test]
    fn kinds_rank_nvidia_then_vaapi_then_cpu_when_idle() {
        let nodes = [node(
            "a",
            vec![
                device("cpu", DeviceKind::Cpu, "CPU"),
                device("vaapi:renderD129", DeviceKind::Vaapi, "Intel Arc A380"),
                rtx(),
            ],
        )];
        let o = options(&PLAIN, &nodes);
        let kinds: Vec<_> = o.iter().map(|o| o.kind).collect();
        assert_eq!(
            kinds,
            [DeviceKind::Nvidia, DeviceKind::Vaapi, DeviceKind::Cpu]
        );
        assert_eq!(o.iter().map(|o| o.score).collect::<Vec<_>>(), [100, 60, 20]);
        assert_eq!(o[0].label, "RTX 4090");
        assert_eq!(o[2].label, "CPU only");
        assert_eq!(best(&o).unwrap().kind, DeviceKind::Nvidia);
    }

    #[test]
    fn load_and_running_environments_lower_the_score() {
        let mut busy = node("busy", vec![rtx()]);
        busy.usage = Some(NodeUsage {
            cpu: 80.0,
            ..gpu_usage_of(90, 12, 24)
        });
        busy.running.insert("nvidia:0".into(), 2);
        let o = options(&PLAIN, &[busy]);
        // 100 - (4 + 27 + 5) - 20
        assert_eq!(o[0].score, 44);
        assert!(o[0].reason.is_none(), "12 GB is plenty free");

        let idle = score(DeviceKind::Cpu, Some(0.0), None, 0);
        let loaded = score(DeviceKind::Cpu, Some(100.0), None, 0);
        assert_eq!((idle, loaded), (20, -10));
        assert!(score(DeviceKind::Vaapi, Some(50.0), None, 0) < 60);
    }

    #[test]
    fn a_busy_nvidia_node_can_lose_to_an_idle_vaapi_one() {
        let mut a = node("a", vec![rtx()]);
        a.running.insert("nvidia:0".into(), 5);
        let b = node(
            "b",
            vec![device("vaapi:renderD129", DeviceKind::Vaapi, "Arc")],
        );
        let o = options(&PLAIN, &[a, b]);
        assert_eq!(best(&o).unwrap().node_name, "b");
    }

    #[test]
    fn a_nearly_full_image_disk_stays_choosable_but_not_automatic() {
        let mut full = node("full", vec![rtx()]);
        full.images_disk_free = Some(3 << 30);
        let mut roomy = node("roomy", vec![rtx()]);
        roomy.images_disk_free = Some(200 << 30);
        let o = options(&PLAIN, &[full, roomy]);
        let full = o.iter().find(|o| o.node_name == "full").unwrap();
        assert!(full.allowed);
        assert_eq!(full.reason.as_deref(), Some("only 3.0 GB of disk free"));
        assert_eq!(best(&o).unwrap().node_name, "roomy");
        // With nowhere else it is still what auto picks.
        let mut alone = node("alone", vec![rtx()]);
        alone.images_disk_free = Some(1 << 30);
        assert_eq!(best(&options(&PLAIN, &[alone])).unwrap().node_name, "alone");
        // A node that reports no disks is unaffected.
        assert_eq!(options(&PLAIN, &[node("old", vec![rtx()])])[0].reason, None);
    }

    #[test]
    fn low_vram_is_said_before_low_disk() {
        let mut both = node("both", vec![rtx()]);
        both.usage = Some(gpu_usage_of(0, 23, 24));
        both.images_disk_free = Some(1 << 30);
        let o = options(&PLAIN, &[both]);
        assert_eq!(o[0].reason.as_deref(), Some("only 1.0 GB of VRAM free"));
    }

    #[test]
    fn the_images_disk_is_the_one_used_for_images_or_the_only_one() {
        use cha_wire::{Disk, DiskUse};
        let disk = |uses: Vec<DiskUse>, free: u64| Disk {
            uses,
            path: "/".into(),
            total_bytes: 100 << 30,
            free_bytes: free,
        };
        assert_eq!(images_disk_free(&[]), None);
        assert_eq!(
            images_disk_free(&[disk(vec![DiskUse::AppData], 7)]),
            Some(7)
        );
        assert_eq!(
            images_disk_free(&[
                disk(vec![DiskUse::AppData], 7),
                disk(vec![DiskUse::Images], 9)
            ]),
            Some(9)
        );
        assert_eq!(
            images_disk_free(&[disk(vec![DiskUse::AppData], 7), disk(vec![], 9)]),
            None
        );
    }

    #[test]
    fn the_cpu_is_never_allowed_for_apps_that_need_a_gpu() {
        let nodes = [node("a", vec![device("cpu", DeviceKind::Cpu, "CPU")])];
        let o = options(&GAME, &nodes);
        assert!(!o[0].allowed);
        assert_eq!(o[0].reason.as_deref(), Some("it needs a GPU"));
        assert!(best(&o).is_none());
        assert_eq!(placements(&GAME, &nodes).auto, None);
        assert!(nothing_allowed(&o).contains("a (CPU only): it needs a GPU"));
        // Fine for a browser.
        assert!(options(&PLAIN, &nodes)[0].allowed);
    }

    #[test]
    fn a_gpu_short_of_vram_stays_choosable_but_not_automatic() {
        let mut tight = node("tight", vec![rtx()]);
        tight.usage = Some(gpu_usage_of(0, 23, 24));
        let slow = node("slow", vec![device("cpu", DeviceKind::Cpu, "CPU")]);
        let o = options(&PLAIN, &[tight, slow]);
        let nvidia = o.iter().find(|o| o.kind == DeviceKind::Nvidia).unwrap();
        assert!(nvidia.allowed);
        assert_eq!(nvidia.reason.as_deref(), Some("only 1.0 GB of VRAM free"));
        // Automatic goes to the CPU over a GPU with a warning...
        assert_eq!(best(&o).unwrap().kind, DeviceKind::Cpu);
        // ...but a game, which the CPU can't run, takes the GPU after all.
        let g = options(
            &GAME,
            &[{
                let mut n = node("tight", vec![rtx()]);
                n.usage = Some(gpu_usage_of(0, 23, 24));
                n
            }],
        );
        assert_eq!(best(&g).unwrap().kind, DeviceKind::Nvidia);
        assert!(g[0].reason.is_some());
    }

    #[test]
    fn a_device_without_a_browser_codec_isnt_allowed() {
        let mut odd = device("vaapi:renderD129", DeviceKind::Vaapi, "Odd");
        odd.codecs = vec!["vp9".into()];
        let o = options(&PLAIN, &[node("a", vec![odd])]);
        assert!(!o[0].allowed);
    }

    #[test]
    fn a_node_that_cant_keep_app_data_isnt_allowed_for_apps_that_do() {
        let mut old = node("old", vec![rtx()]);
        old.keeps_app_data = false;
        let needs = Needs {
            gpu: false,
            app_data: true,
            images: Vec::new(),
        };
        let o = options(&needs, &[old]);
        assert!(!o[0].allowed);
        assert!(o[0].reason.as_deref().unwrap().contains("needs an update"));
    }

    #[test]
    fn a_node_that_reports_no_devices_is_read_as_one_nvidia_gpu() {
        use cha_wire::{Gpu, Inventory};
        let inv = Inventory {
            gpus: vec![Gpu {
                vendor: "nvidia".into(),
                name: "NVIDIA GeForce RTX 3080".into(),
                memory_mb: Some(10240),
                driver: None,
                render_node: Some("/dev/dri/renderD128".into()),
                encoders: vec!["h264".into(), "hevc".into()],
            }],
            ..Inventory::default()
        };
        let o = options(&PLAIN, &[node("old", inv.devices_or_derived())]);
        assert_eq!(o.len(), 1);
        assert_eq!(
            (o[0].device.as_str(), o[0].kind),
            ("nvidia:0", DeviceKind::Nvidia)
        );
    }

    fn wants(images: &[&str]) -> Needs {
        Needs {
            gpu: false,
            app_data: false,
            images: images.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn a_node_that_lists_no_images_isnt_said_to_download() {
        // An older agent reports no images: no bonus, and no note either.
        let needs = wants(&["ghcr.io/ban-red/cha-env-chrome:{version}"]);
        let o = options(&needs, &[node("old", vec![rtx()])]);
        assert_eq!(o[0].note, None);
    }

    #[test]
    fn holding_the_image_beats_an_otherwise_equal_node() {
        let needs = wants(&[
            "cha/env-chrome:dev",
            "ghcr.io/ban-red/cha-env-chrome:{version}",
        ]);
        let mut a = node("a", vec![rtx()]);
        a.images = vec!["cha/streamer:dev".into()];
        let mut b = node("b", vec![rtx()]);
        b.images = vec!["cha/env-chrome:dev".into()];
        // "a" sorts first by name, so only the bonus can put "b" ahead.
        let o = options(&needs, &[a, b]);
        assert_eq!(best(&o).unwrap().node_name, "b");
        assert_eq!(o[0].score - o[1].score, HAS_IMAGE);
        assert_eq!(o[1].note.as_deref(), Some("downloads the image first"));
        assert!(o[0].note.is_none());
        // No bonus, no note, when the template names no image.
        assert!(options(&PLAIN, &[node("a", vec![rtx()])])[0].note.is_none());
    }

    #[test]
    fn version_is_filled_from_the_nodes_agent_else_the_name_matches() {
        let c = ["ghcr.io/ban-red/cha-env-chrome:{version}".to_string()];
        let mut n = node("a", vec![rtx()]);
        n.images = vec!["ghcr.io/ban-red/cha-env-chrome:0.1.0".into()];
        n.agent_version = Some("0.1.0".into());
        assert!(n.has_image(&c));
        n.agent_version = Some("0.2.0".into());
        assert!(!n.has_image(&c), "another release is another image");
        n.agent_version = None;
        assert!(n.has_image(&c), "no version reported: the name decides");
        n.images = vec!["ghcr.io/ban-red/cha-env-firefox:0.1.0".into()];
        assert!(!n.has_image(&c));
    }

    #[test]
    fn manual_nodes_stay_allowed_but_are_never_auto() {
        let mut m = node("m", vec![rtx()]);
        m.manual = true;
        let o = options(&PLAIN, &[m]);
        assert!(o[0].allowed);
        assert_eq!(o[0].reason, None);
        assert_eq!(o[0].note.as_deref(), Some("picked by hand only"));
        assert_eq!(
            placements(
                &PLAIN,
                &[{
                    let mut m = node("m", vec![rtx()]);
                    m.manual = true;
                    m
                }]
            )
            .auto,
            None
        );
        // With a worse automatic node around, that one wins, even with the
        // image on the manual one.
        let mut m = node("m", vec![rtx()]);
        m.manual = true;
        m.images = vec!["cha/env-chrome:dev".into()];
        let cpu = node("c", vec![device("cpu", DeviceKind::Cpu, "CPU")]);
        let p = placements(&wants(&["cha/env-chrome:dev"]), &[m, cpu]);
        assert_eq!(p.auto.unwrap().node, "id-c");
        assert!(p.options.iter().any(|o| o.node == "id-m" && o.allowed));
        // Both notes apply to a manual node that lacks the image.
        let mut m = node("m", vec![rtx()]);
        m.manual = true;
        m.images = vec!["other:1".into()];
        let o = options(&wants(&["cha/env-chrome:dev"]), &[m]);
        assert_eq!(
            o[0].note.as_deref(),
            Some("picked by hand only; downloads the image first")
        );
    }

    #[test]
    fn nothing_online_says_so() {
        assert_eq!(placements(&PLAIN, &[]).auto, None);
        assert_eq!(nothing_allowed(&[]), "no node is online");
    }
}

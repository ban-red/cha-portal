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
//!
//! An app that shares data between users (Steam's library) is also placed by
//! where that shared directory is. A node that keeps it outside its data root
//! (a NAS share) and finds it unusable is never the automatic choice and
//! scores 40 less. A node that keeps it locally, while another online node
//! keeps it on a share, scores 25 less, with a note: that is the node whose
//! library is empty, and `auto` prefers the nodes on the share.

use std::collections::{BTreeMap, HashMap};

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use cha_wire::{
    Device, DeviceKind, GpuUsage, HostOptions, HostPolicy, Inventory, NodeUsage,
    SPEC_FEATURE_DATA_TEMPLATE, SPEC_FEATURE_ENV, SPEC_FEATURE_HOST_OPTIONS, SecurityProfile,
    SharedDirState, SharedDirStatus, Storage,
};
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
/// What a shared folder the node finds unusable costs.
const SHARED_BROKEN: i64 = 40;
/// What keeping the shared folder locally costs when other nodes share one.
const SHARED_LOCAL: i64 = 25;

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
#[derive(Clone)]
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
    /// The optional spec fields its agent honours (`Inventory::spec_features`).
    pub spec_features: Vec<String>,
    /// What it lets custom environments ask for; `None` is nothing.
    pub host_policy: Option<HostPolicy>,
    /// Whether it reports `/dev/kvm`. An older agent doesn't, and drops its
    /// connection on a profile it doesn't know, so it is never sent a `vm`.
    pub kvm: bool,
    /// Template (data) id → where it keeps that app's shared directory, when
    /// its owner keeps it outside the data root.
    pub shared_dirs: BTreeMap<String, String>,
    /// What its agent found when it checked each of those.
    pub shared_status: BTreeMap<String, SharedDirStatus>,
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
    /// A virtual machine runs inside the app: only a node with KVM will do.
    pub vm: bool,
    /// The images the template can run from, in order; empty when it doesn't
    /// matter.
    pub images: Vec<String>,
    /// It sends extra variables (`EnvironmentSpec::env`).
    pub env: bool,
    /// Its storage names another template's directories
    /// (`Storage::data_template`).
    pub data_template: bool,
    /// The host options (ADR 0021) it asks the node for.
    pub host: Option<HostOptions>,
    /// The app shares a directory between users: its data id and name.
    pub shared: Option<SharedUse>,
}

/// An app's shared directory, as placement asks about it.
#[derive(Clone, Debug)]
pub struct SharedUse {
    /// `Template::data_id`, the key of `Inventory::shared_dirs`.
    pub data: String,
    pub name: String,
}

impl Needs {
    pub fn of(template: &Template, storage: Option<&Storage>) -> Self {
        Self {
            gpu: template.needs_gpu,
            app_data: storage.is_some(),
            vm: template.security == SecurityProfile::Vm,
            images: template.image_candidates(),
            env: !template.env.is_empty(),
            data_template: storage.is_some_and(|s| s.data_template.is_some()),
            host: template.host_options().cloned(),
            shared: storage
                .is_some_and(|s| s.shared.is_some())
                .then(|| SharedUse {
                    data: template.data_id().to_string(),
                    name: template.name.clone(),
                }),
        }
    }
}

/// Why a node whose agent lists `features` and allows `policy` can't take a
/// spec that needs `needs`, if it can't: an older agent would drop what it
/// doesn't know, and a node's owner decides which host options are allowed.
fn spec_refusal_of(
    needs: &Needs,
    features: &[String],
    policy: Option<&HostPolicy>,
) -> Option<String> {
    let has = |f: &str| features.iter().any(|x| x == f);
    if needs.env && !has(SPEC_FEATURE_ENV) {
        return Some(format!(
            "its agent needs an update to pass environment variables (it lists \"{SPEC_FEATURE_ENV}\" once it can)"
        ));
    }
    if needs.data_template && !has(SPEC_FEATURE_DATA_TEMPLATE) {
        return Some(format!(
            "its agent needs an update to share another environment's app data (it lists \"{SPEC_FEATURE_DATA_TEMPLATE}\" once it can)"
        ));
    }
    if let Some(host) = needs.host.as_ref().filter(|h| !h.is_empty()) {
        if !has(SPEC_FEATURE_HOST_OPTIONS) {
            return Some(format!(
                "its agent needs an update to take host options (it lists \"{SPEC_FEATURE_HOST_OPTIONS}\" once it can)"
            ));
        }
        let refusals = match policy {
            Some(policy) => policy.refusals(host),
            None => vec!["allows no host options".to_string()],
        };
        if !refusals.is_empty() {
            return Some(format!("it {}", refusals.join("; it ")));
        }
    }
    None
}

/// [`spec_refusal_of`] for a node's inventory (a node that sent none lists
/// nothing).
pub fn spec_refusal(needs: &Needs, inventory: Option<&Inventory>) -> Option<String> {
    spec_refusal_of(
        needs,
        inventory.map_or(&[], |i| &i.spec_features),
        inventory.and_then(|i| i.host_options.as_ref()),
    )
}

/// A shared folder's state in words, for a reason.
fn shared_state_words(state: SharedDirState) -> &'static str {
    match state {
        SharedDirState::Ok => "it is fine",
        SharedDirState::Missing => "it isn't mounted",
        SharedDirState::Incomplete => "it lacks folders the app needs",
        SharedDirState::Unreachable => "it didn't answer in time",
        SharedDirState::ReadOnly => "it can't be written to",
    }
}

/// What a node's shared folder for the app means for a launch there: a reason
/// and a score change, or a note and a score change, or nothing. `nodes` are
/// the online ones.
fn shared_verdict(
    shared: &SharedUse,
    node: &NodeView,
    nodes: &[NodeView],
) -> (Option<String>, Option<String>, i64) {
    if node.shared_dirs.contains_key(&shared.data) {
        // An older agent sends no status: nothing known against it.
        return match node.shared_status.get(&shared.data) {
            Some(status) if status.state != SharedDirState::Ok => {
                let why = status
                    .detail
                    .clone()
                    .unwrap_or_else(|| shared_state_words(status.state).to_string());
                (
                    Some(format!(
                        "{}'s shared folder isn't usable on this node: {why}",
                        shared.name
                    )),
                    None,
                    -SHARED_BROKEN,
                )
            }
            _ => (None, None, 0),
        };
    }
    let mut others: Vec<&str> = nodes
        .iter()
        .filter(|n| n.id != node.id && n.shared_dirs.contains_key(&shared.data))
        .map(|n| n.name.as_str())
        .collect();
    if others.is_empty() {
        return (None, None, 0);
    }
    others.sort_unstable();
    (
        None,
        Some(format!(
            "keeps {}'s shared folder on this node, not on the share {} {}",
            shared.name,
            others.join(", "),
            if others.len() == 1 { "uses" } else { "use" }
        )),
        -SHARED_LOCAL,
    )
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
            } else if needs.vm && !node.kvm {
                allowed = false;
                reason = Some(
                    "no KVM (it reports /dev/kvm once its agent is up to date and the host has it)"
                        .to_string(),
                );
            } else if needs.app_data && !node.keeps_app_data {
                allowed = false;
                reason = Some(
                    "its agent needs an update to keep app data (it reports its data root once it can)"
                        .to_string(),
                );
            } else if let Some(why) =
                spec_refusal_of(needs, &node.spec_features, node.host_policy.as_ref())
            {
                allowed = false;
                reason = Some(why);
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
            let mut shared_note: Option<String> = None;
            let mut shared_score = 0;
            if let Some(shared) = needs.shared.as_ref().filter(|_| node.keeps_app_data) {
                let (why, note, delta) = shared_verdict(shared, node, nodes);
                if let Some(why) = why {
                    reason = Some(match reason {
                        Some(r) => format!("{r}; {why}"),
                        None => why,
                    });
                }
                shared_note = note;
                shared_score = delta;
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
            if let Some(note) = shared_note.as_deref() {
                notes.push(note);
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
                    + if has_image { HAS_IMAGE } else { 0 }
                    + shared_score,
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
            spec_features: inventory
                .as_ref()
                .map(|inv| inv.spec_features.clone())
                .unwrap_or_default(),
            host_policy: inventory.as_ref().and_then(|inv| inv.host_options.clone()),
            kvm: inventory.as_ref().is_some_and(|inv| inv.kvm == Some(true)),
            shared_dirs: inventory
                .as_ref()
                .map(|inv| inv.shared_dirs.clone())
                .unwrap_or_default(),
            shared_status: inventory
                .as_ref()
                .map(|inv| inv.shared_status.clone())
                .unwrap_or_default(),
            id: row.id,
            name: row.name,
            devices,
            running,
        });
    }
    Ok(nodes)
}

/// The options for `user`'s launch of `template`. A restricted user sees only
/// the nodes on their list and those a grant names for this template.
pub async fn for_template(
    state: &AppState,
    user_id: &str,
    template: &Template,
    nodes: &[NodeView],
) -> ApiResult<Placements> {
    let settings = storage::effective_for(state, user_id, template).await?;
    let app_data = storage::spec_storage(user_id, template, settings);
    let needs = Needs::of(template, app_data.as_ref());
    let Some(allowed) = db::allowed_nodes(&state.db, user_id, &template.id).await? else {
        return Ok(placements(&needs, nodes));
    };
    let nodes: Vec<NodeView> = nodes
        .iter()
        .filter(|n| allowed.contains(&n.id))
        .cloned()
        .collect();
    Ok(placements(&needs, &nodes))
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
            spec_features: Vec::new(),
            host_policy: None,
            kvm: false,
            shared_dirs: BTreeMap::new(),
            shared_status: BTreeMap::new(),
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
        vm: false,
        images: Vec::new(),
        env: false,
        data_template: false,
        host: None,
        shared: None,
    };
    const GAME: Needs = Needs {
        gpu: true,
        app_data: false,
        vm: false,
        images: Vec::new(),
        env: false,
        data_template: false,
        host: None,
        shared: None,
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
            app_data: true,
            ..PLAIN_NEEDS()
        };
        let o = options(&needs, &[old]);
        assert!(!o[0].allowed);
        assert!(o[0].reason.as_deref().unwrap().contains("needs an update"));
    }

    #[test]
    fn a_vm_runs_only_on_a_node_that_reports_kvm() {
        let needs = Needs {
            vm: true,
            ..PLAIN_NEEDS()
        };
        let mut kvm = node("kvm", vec![rtx()]);
        kvm.kvm = true;
        let o = options(&needs, &[node("plain", vec![rtx()]), kvm]);
        assert_eq!(o.len(), 2);
        // The node with KVM comes first and is allowed; the other says why not.
        assert!(o[0].allowed);
        assert_eq!(o[0].node_name, "kvm");
        assert!(!o[1].allowed);
        assert!(o[1].reason.as_deref().unwrap().starts_with("no KVM"));
        // Everything else ignores KVM.
        assert!(options(&PLAIN, &[node("plain", vec![rtx()])])[0].allowed);
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
            images: images.iter().map(|s| s.to_string()).collect(),
            ..PLAIN_NEEDS()
        }
    }

    #[allow(non_snake_case)]
    fn PLAIN_NEEDS() -> Needs {
        Needs {
            gpu: false,
            app_data: false,
            images: Vec::new(),
            env: false,
            data_template: false,
            host: None,
            vm: false,
            shared: None,
        }
    }

    fn media_mount() -> HostOptions {
        HostOptions {
            mounts: vec![cha_wire::HostMount {
                source: cha_wire::MountSource::Named {
                    name: "media".into(),
                },
                target: "/mnt/media".into(),
                read_only: false,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_spec_goes_only_to_nodes_that_understand_and_allow_it() {
        let mut env = PLAIN_NEEDS();
        env.env = true;
        let mut old = node("old", vec![rtx()]);
        let mut new = node("new", vec![rtx()]);
        new.spec_features = vec![SPEC_FEATURE_ENV.into()];
        let o = options(&env, &[old.clone(), new.clone()]);
        let by = |n: &str| o.iter().find(|o| o.node_name == n).unwrap();
        assert!(!by("old").allowed);
        assert!(
            by("old")
                .reason
                .as_deref()
                .unwrap()
                .contains("environment variables")
        );
        assert!(by("new").allowed);

        // Host options also need the node's owner to allow them.
        let mut host = PLAIN_NEEDS();
        host.host = Some(media_mount());
        new.spec_features.push(SPEC_FEATURE_HOST_OPTIONS.into());
        old.spec_features.push(SPEC_FEATURE_HOST_OPTIONS.into());
        old.host_policy = Some(HostPolicy {
            mode: cha_wire::HostOptionsMode::Allowlist,
            mounts: vec![cha_wire::AllowedMount {
                name: "media".into(),
                read_only: true,
            }],
            ..Default::default()
        });
        new.host_policy = Some(HostPolicy {
            mode: cha_wire::HostOptionsMode::Allowlist,
            ..Default::default()
        });
        let o = options(&host, &[old, new]);
        let by = |n: &str| o.iter().find(|o| o.node_name == n).unwrap();
        assert!(by("old").allowed, "{:?}", by("old").reason);
        assert!(!by("new").allowed);
        assert_eq!(
            by("new").reason.as_deref(),
            Some("it has no mount named media")
        );
        assert_eq!(best(&o).unwrap().node_name, "old");
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

    fn status(state: SharedDirState, detail: Option<&str>) -> SharedDirStatus {
        SharedDirStatus {
            state,
            fs_type: Some("nfs4".into()),
            source: None,
            detail: detail.map(String::from),
            checked_at: 1_791_000_000,
        }
    }

    /// A node with a GPU that keeps Steam's shared folder on a share
    /// (`external`), with `check` as its status, or locally.
    fn steam_node(name: &str, external: bool, check: Option<SharedDirStatus>) -> NodeView {
        let mut n = node(name, vec![rtx()]);
        if external {
            n.shared_dirs
                .insert("steam".into(), "/mnt/games/steam".into());
        }
        if let Some(check) = check {
            n.shared_status.insert("steam".into(), check);
        }
        n
    }

    fn shares_steam() -> Needs {
        Needs {
            app_data: true,
            shared: Some(SharedUse {
                data: "steam".into(),
                name: "Steam".into(),
            }),
            ..PLAIN_NEEDS()
        }
    }

    fn of<'a>(options: &'a [PlacementOption], node: &str) -> &'a PlacementOption {
        options.iter().find(|o| o.node_name == node).unwrap()
    }

    #[test]
    fn an_unusable_shared_folder_is_a_reason_and_keeps_auto_away() {
        let nodes = [
            steam_node("good", true, Some(status(SharedDirState::Ok, None))),
            steam_node(
                "broken",
                true,
                Some(status(
                    SharedDirState::Missing,
                    Some("/mnt/games isn't mounted"),
                )),
            ),
            steam_node(
                "unreach",
                true,
                Some(status(SharedDirState::Unreachable, None)),
            ),
        ];
        let o = options(&shares_steam(), &nodes);
        let (good, broken, unreach) = (of(&o, "good"), of(&o, "broken"), of(&o, "unreach"));
        assert_eq!(good.reason, None);
        assert_eq!(good.note, None);
        assert_eq!(
            broken.reason.as_deref(),
            Some("Steam's shared folder isn't usable on this node: /mnt/games isn't mounted")
        );
        assert_eq!(
            unreach.reason.as_deref(),
            Some("Steam's shared folder isn't usable on this node: it didn't answer in time")
        );
        assert!(broken.allowed && unreach.allowed);
        assert_eq!(good.score - broken.score, 40);
        assert_eq!(
            placements(&shares_steam(), &nodes).auto.unwrap().node,
            "id-good"
        );
    }

    #[test]
    fn a_node_off_the_share_scores_less_with_a_note() {
        let nodes = [
            steam_node("local", false, None),
            steam_node("nas-a", true, Some(status(SharedDirState::Ok, None))),
            steam_node("nas-b", true, Some(status(SharedDirState::Ok, None))),
        ];
        let o = options(&shares_steam(), &nodes);
        let local = of(&o, "local");
        assert_eq!(
            local.note.as_deref(),
            Some("keeps Steam's shared folder on this node, not on the share nas-a, nas-b use")
        );
        assert_eq!(local.reason, None);
        assert!(local.allowed);
        assert_eq!(of(&o, "nas-a").score - local.score, 25);
        assert_eq!(of(&o, "nas-a").note, None);
        assert_ne!(
            placements(&shares_steam(), &nodes).auto.unwrap().node,
            "id-local"
        );

        // With one other node the note says "uses"; a note joins another.
        let mut manual = steam_node("local", false, None);
        manual.manual = true;
        let o = options(
            &shares_steam(),
            &[
                manual,
                steam_node("nas", true, Some(status(SharedDirState::Ok, None))),
            ],
        );
        assert_eq!(
            of(&o, "local").note.as_deref(),
            Some(
                "picked by hand only; keeps Steam's shared folder on this node, not on the share nas uses"
            )
        );
    }

    #[test]
    fn shared_folders_matter_only_where_the_app_shares_and_the_agent_says() {
        // Old agents report no status: external counts as fine. A lone local
        // node has no share to differ from. An app that shares nothing is
        // left alone.
        let old = [
            steam_node("old", true, None),
            steam_node("local", false, None),
        ];
        let o = options(&shares_steam(), &old);
        assert_eq!(of(&o, "old").reason, None);
        assert_eq!(of(&o, "old").score, of(&o, "local").score + 25);
        let alone = [steam_node("local", false, None)];
        assert_eq!(options(&shares_steam(), &alone)[0].note, None);
        let broken = [steam_node(
            "broken",
            true,
            Some(status(SharedDirState::Missing, None)),
        )];
        let o = options(&PLAIN, &broken);
        assert_eq!((o[0].reason.clone(), o[0].note.clone()), (None, None));
        // An offline node isn't in the list, so it doesn't make the others "off".
        let o = options(&shares_steam(), &[steam_node("local", false, None)]);
        assert_eq!(o[0].note, None);
    }
}

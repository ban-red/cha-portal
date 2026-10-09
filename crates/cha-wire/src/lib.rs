//! Messages between a Cha Node agent and the portal (`cha-control`).
//!
//! ADR 0001: one WebSocket per node carries JSON messages. Requests carry an
//! `id` that the reply echoes; pushes have none.
//!
//! **Enrollment** (HTTP, once): the node redeems an admin's join token with its
//! Ed25519 public key and gets a node id.
//!
//! **Connection:**
//! 1. Portal → [`ToNode::Challenge`].
//! 2. Node → [`ToPortal::Hello`], signing the challenge with its key ([`hello_message`]).
//! 3. Portal → [`ToNode::Welcome`].
//! 4. Node → [`ToPortal::Inventory`] and [`ToPortal::Environments`] (what it
//!    is running, so both sides reconcile), then [`ToPortal::Heartbeat`] every
//!    `heartbeat_secs`. Requests flow both ways; the node pushes
//!    [`ToPortal::EnvironmentExited`] when an environment stops on its own, and
//!    [`ToPortal::EnvironmentProgress`] while a start has something slow to say,
//!    and [`ToPortal::EnvironmentWarning`] when a running one needs the user's
//!    attention (a portal that understands it says so in its welcome).
//!
//! App data (the layout is in `storage.rs`): the portal names each environment's directories
//! under the node's data root in [`EnvironmentSpec::storage`], and asks the
//! node to delete a user's with [`NodeRequest::DeleteUserData`]. Messages only
//! gained optional fields and variants, so [`PROTOCOL_VERSION`] stays: a
//! node that predates storage ignores `storage` (the portal checks the node
//! reports a data root before sending it), and a portal that predates it still
//! sends `home`, which the node honours.
//!
//! Warnings ([`ToPortal::EnvironmentWarning`]) are a variant an older portal
//! can't read, and it answers a message it can't read by hanging up. So the
//! portal says in its welcome that it understands them
//! ([`ToNode::Welcome::environment_warnings`]), and a node sends none until it
//! does; an older node never sends one.
//!
//! The log of an environment that died ([`ToPortal::EnvironmentExited::log`]) is
//! one more optional field, and serde ignores fields it doesn't know: an older
//! portal reads the message and drops the log, an older node sends none.
//!
//! Usage ([`ToPortal::Usage`]) is the same kind of variant, so it has the same
//! guard: [`ToNode::Welcome::node_usage`]. A node that is told so sends one
//! every few seconds ([`USAGE_INTERVAL_SECS`]).
//!
//! The gamepad kind ([`EnvironmentSpec::gamepad`]) is one more optional field:
//! a node that predates it ignores it and makes Xbox 360 pads, which is what
//! a spec without it means, so an older portal's launches are unchanged.
//!
//! Moonlight hosts (ADR 0008) are the same kind of addition: the node sends
//! [`ToPortal::MoonlightHosts`] only once the welcome says the portal reads
//! them ([`ToNode::Welcome::moonlight`]), and [`EnvironmentSpec::gateway`] is an
//! optional field the portal sets only for a node that has sent that list.
//!
//! Devices (`docs/devices.md`) are two more optional fields:
//! [`Inventory::devices`] and [`EnvironmentSpec::device`]. A node that predates
//! them reports none, and [`Inventory::devices_or_derived`] reads its NVIDIA
//! GPU as the one `nvidia` device it always ran on; a spec without a device
//! means `nvidia`, which an older node does whatever the spec says.

pub mod claim;
mod host;
mod storage;

pub use host::*;
pub use storage::*;

use std::collections::BTreeMap;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Bumped on breaking changes to these messages.
pub const PROTOCOL_VERSION: u32 = 1;

/// The node connection's WebSocket path on the portal.
pub const CONNECT_PATH: &str = "/api/node/connect";
/// The enrollment endpoint's path on the portal.
pub const ENROLL_PATH: &str = "/api/node/enroll";
/// Where a node dials back for a media relay (ADR 0022), followed by the
/// relay id: `/api/node/relay/<relay id>`.
pub const RELAY_PATH: &str = "/api/node/relay";
/// The headers a node's relay connection proves itself with: its id, and its
/// signature of [`relay_message`].
pub const RELAY_NODE_HEADER: &str = "x-cha-node";
pub const RELAY_SIGNATURE_HEADER: &str = "x-cha-signature";

/// WebSocket close codes the portal uses to tell a node why it hung up.
pub mod close {
    /// The node isn't enrolled, or an admin removed it: reconnecting won't help.
    pub const UNKNOWN_NODE: u16 = 4001;
    /// A newer connection from the same node took over.
    pub const REPLACED: u16 = 4002;
    /// The hello didn't verify against the node's enrolled key.
    pub const BAD_SIGNATURE: u16 = 4003;
}

/// `POST /api/node/enroll`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollRequest {
    pub token: String,
    /// Base64 Ed25519 public key.
    pub public_key: String,
    pub name: String,
    pub agent_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollResponse {
    pub node_id: String,
}

/// Portal → node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToNode {
    Challenge {
        nonce: String,
        protocol: u32,
    },
    Welcome {
        node_id: String,
        heartbeat_secs: u64,
        /// The portal understands [`ToPortal::EnvironmentWarning`]. A portal
        /// that predates it drops the connection over a message it doesn't
        /// know, so a node sends warnings only when this is set (absent from
        /// such a portal's welcome).
        #[serde(default)]
        environment_warnings: bool,
        /// The portal understands [`ToPortal::Usage`]; the same guard, for the
        /// same reason.
        #[serde(default)]
        node_usage: bool,
        /// The portal reads [`ToPortal::MoonlightHosts`] and
        /// [`ToPortal::MoonlightPaired`]; the same guard.
        #[serde(default)]
        moonlight: bool,
        /// The portal reads the `GameStream*` messages of [`ToPortal`] and
        /// sends the node's host [`NodeRequest::GameStreamPin`] and
        /// [`NodeRequest::GameStreamDevices`], and answers the node's
        /// [`PortalRequest::GameStreamLaunch`] and
        /// [`PortalRequest::GameStreamStop`]; the same guard.
        #[serde(default)]
        gamestream: bool,
        /// The portal answers [`PortalRequest::GameStreamLaunch`] and
        /// [`PortalRequest::GameStreamStop`] (G3.1). Apart from `gamestream`
        /// because a portal from before G3.1 reads the other `GameStream*`
        /// messages but drops the connection on these; without it the node's
        /// host lists only running environments and never sends them.
        #[serde(default)]
        gamestream_launch: bool,
        /// The portal reads [`ToPortal::AgentUpdate`] (ADR 0018); the same
        /// guard.
        #[serde(default)]
        agent_updates: bool,
    },
    /// Move this node's agent to `version` (`0.2.1`), ADR 0018. Sent only to
    /// an agent whose inventory says it can ([`Inventory::update`]); the
    /// agent picks the image itself, from the one it runs.
    UpdateAgent {
        version: String,
    },
    Request {
        id: u64,
        request: NodeRequest,
    },
    Response {
        id: u64,
        result: Result<PortalResponse, String>,
    },
}

/// Node → portal.
// The inventory is sent a few times an hour; boxing it would touch every
// sender and test for nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPortal {
    Hello {
        node_id: String,
        /// Base64 Ed25519 signature of [`hello_message`].
        signature: String,
        agent_version: String,
        protocol: u32,
    },
    Heartbeat,
    Inventory {
        inventory: Inventory,
    },
    /// The environments this node is running (ids), sent after every welcome.
    Environments {
        running: Vec<String>,
    },
    /// An environment stopped without being asked (its app exited, or a
    /// container died); the node has cleaned it up.
    EnvironmentExited {
        id: String,
        detail: String,
        /// It ended with an error (a non-zero exit, a crash) rather than the
        /// app quitting normally.
        failed: bool,
        /// The last lines of what its containers logged (the streamer's, then
        /// the app's), kept because the node removes them: for the owner to
        /// read. Empty when there was none or the node is older.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        log: Vec<String>,
    },
    /// What a starting environment is doing, when it takes long enough to be
    /// worth saying (copying a user's old home into its new directory). The
    /// portal shows it as the environment's detail until it is running.
    EnvironmentProgress {
        id: String,
        detail: String,
        /// How far along, when it can be counted (an image download):
        /// `done` of `total`, in `unit` (`bytes`). Absent from nodes that
        /// predate them, and for steps that can't be counted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        done: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    /// Something the user should know about a running environment: set when
    /// it appears, `None` when it is gone (or has been stopped). The portal
    /// shows it with the environment.
    EnvironmentWarning {
        id: String,
        warning: Option<String>,
    },
    /// How an update of the agent ([`ToNode::UpdateAgent`]) is going (only
    /// once the welcome says the portal reads it). Success needs none: the
    /// node reconnects with its new `agent_version`.
    AgentUpdate {
        state: AgentUpdateState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Bytes of the images downloaded so far, while `pulling`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        done: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
    },
    /// The machine's use now, every [`USAGE_INTERVAL_SECS`] while connected
    /// (only once the welcome says the portal reads it).
    Usage {
        usage: NodeUsage,
    },
    /// The Moonlight hosts (Sunshine, Apollo) this node sees on its network:
    /// the whole list, sent when it changes (only once the welcome says the
    /// portal reads it).
    MoonlightHosts {
        hosts: Vec<MoonlightHost>,
    },
    /// A pairing started with [`NodeRequest::MoonlightPair`] ended: the PIN
    /// was entered on the host (`ok`), or it failed or timed out.
    MoonlightPaired {
        unique_id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// A Moonlight client asked this node's GameStream host to pair and shows
    /// a PIN; a signed-in user types it in the portal (ADR 0009). Only once
    /// the welcome says the portal reads it.
    GameStreamPairRequest {
        /// Names the attempt in [`NodeRequest::GameStreamPin`] and the
        /// answers below; unique on this node.
        attempt_id: String,
        /// What the client calls itself (its device name).
        device_name: String,
        /// The client's address.
        address: String,
        /// Seconds the attempt stays open.
        expires_in_secs: u64,
    },
    /// The pairing of `attempt_id` finished: the client is paired, by its
    /// certificate's fingerprint (SHA-256, lower-case hex). The portal keeps
    /// the device and sends the node's whole list back.
    GameStreamPaired {
        attempt_id: String,
        fingerprint: String,
        /// The `uniqueid` the client stated: a label.
        unique_id: String,
        name: String,
    },
    /// The pairing of `attempt_id` ended without a device: the PIN was wrong,
    /// or the client gave up.
    GameStreamPairFailed {
        attempt_id: String,
        reason: String,
    },
    /// A client unpaired itself, over its own HTTPS connection.
    GameStreamUnpaired {
        fingerprint: String,
    },
    Request {
        id: u64,
        request: PortalRequest,
    },
    Response {
        id: u64,
        result: Result<NodeResponse, String>,
    },
}

/// What the portal asks a node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum NodeRequest {
    Ping,
    /// Start an environment: its streamer, then its app. Idempotent by id.
    StartEnvironment {
        environment: EnvironmentSpec,
    },
    /// Stop and remove an environment. Idempotent: an unknown id is stopped.
    StopEnvironment {
        id: String,
    },
    /// A browser wants to watch (and drive) an environment: hand its WebRTC
    /// offer to the environment's streamer, with the portal's media token.
    Connect {
        environment_id: String,
        /// `h264`, `hevc` or `av1`.
        codec: String,
        /// The browser's SDP offer (`{"type": "offer", "sdp": …}`).
        offer: serde_json::Value,
        media_token: String,
    },
    /// The streamer's own description (`GET /info`): its WebTransport port,
    /// certificate hash and addresses, for a browser connecting that way.
    StreamerInfo {
        environment_id: String,
    },
    /// A browser is waiting on the portal for a WebSocket stream (ADR 0022):
    /// open the streamer's `GET /ws/media` with the media token, dial the
    /// portal back on [`RELAY_PATH`]`/<relay_id>` and pass messages both ways
    /// until either end closes. Answered with [`NodeResponse::RelayOpened`]
    /// once both are open. Sent only to a node whose inventory says
    /// [`Inventory::relay`].
    OpenRelay {
        relay_id: String,
        environment_id: String,
        /// `h264`, `hevc` or `av1`.
        codec: String,
        media_token: String,
    },
    /// Delete what a user keeps for an app on this node: their directory
    /// `users/<user>/<template>` under the data root, the app's home and
    /// everything else of theirs in it. Nothing if there is none. Refused
    /// while an environment of theirs for that app runs here.
    DeleteUserData {
        user: String,
        template: String,
    },
    /// Start pairing with a found Moonlight host. Answers with the PIN as soon
    /// as there is one; the node goes on in the background and sends
    /// [`ToPortal::MoonlightPaired`] when the PIN has been entered (or not).
    MoonlightPair {
        unique_id: String,
    },
    /// The apps a paired Moonlight host offers.
    MoonlightApps {
        unique_id: String,
    },
    /// The PIN a signed-in user typed for a [`ToPortal::GameStreamPairRequest`];
    /// `user_id` becomes the paired device's owner. Refused when the attempt
    /// is unknown or over.
    GameStreamPin {
        attempt_id: String,
        pin: String,
        user_id: String,
    },
    /// The devices paired with this node's GameStream host: the whole list,
    /// which replaces what the node had. Sent after every welcome and every
    /// change. Until the first one arrives the node counts nobody as paired.
    GameStreamDevices {
        devices: Vec<GameStreamDevice>,
    },
}

/// A Moonlight client paired with a node's GameStream host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameStreamDevice {
    /// SHA-256 of the client's certificate, lower-case hex: its identity.
    pub fingerprint: String,
    /// What it stated as `uniqueid` when pairing, if the portal knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique_id: Option<String>,
    pub name: String,
    /// The portal user whose environments it sees.
    pub user_id: String,
}

/// What a node's inventory says of its GameStream host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStreamInfo {
    /// The host's HTTP port, which a Moonlight client adds a host by.
    pub http_port: u16,
    /// What clients call the host.
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum NodeResponse {
    Pong {
        unix_ms: u64,
    },
    EnvironmentStarted {
        id: String,
        streamer: StreamerEndpoint,
        /// The ports published for [`HostOptions::ports`], each with the
        /// node's port filled in. Empty without any.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ports: Vec<HostPort>,
    },
    EnvironmentStopped {
        id: String,
    },
    /// The streamer's SDP answer.
    Answer {
        answer: serde_json::Value,
    },
    StreamerInfo {
        info: serde_json::Value,
    },
    /// Both ends of a [`NodeRequest::OpenRelay`] are open.
    RelayOpened,
    UserDataDeleted {
        user: String,
        template: String,
    },
    MoonlightPairing {
        /// Four digits, for the user to enter on the host.
        pin: String,
    },
    MoonlightApps {
        apps: Vec<MoonlightApp>,
    },
    /// A [`NodeRequest::GameStreamPin`] or [`NodeRequest::GameStreamDevices`]
    /// was taken.
    Accepted,
}

/// A Moonlight host a node found on its network (`_nvstream._tcp`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightHost {
    /// The host's `uniqueid` from `/serverinfo`.
    pub unique_id: String,
    pub name: String,
    pub address: String,
    pub http_port: u16,
    pub https_port: u16,
    /// This node is paired with it.
    pub paired: bool,
    /// What it encodes: `h264`, `hevc`.
    #[serde(default)]
    pub codecs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
}

/// An app a Moonlight host offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoonlightApp {
    pub id: u32,
    pub name: String,
    #[serde(default)]
    pub hdr: bool,
}

/// The Moonlight host and app a gateway environment streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySpec {
    pub unique_id: String,
    pub address: String,
    pub http_port: u16,
    pub https_port: u16,
    pub app_id: u32,
}

/// What to run, decided by the portal from a catalog template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSpec {
    pub id: String,
    /// The app's image, e.g. `cha/env-chrome:dev`: the first of
    /// [`Self::image_candidates`] when there are any, for nodes that predate
    /// them.
    pub image: String,
    /// The images to try, in order (ADR 0017): a local dev build, then the
    /// published reference, which may hold `{version}` for the node to fill
    /// with its agent's release. The node runs the first it has, else pulls
    /// the first that names a registry. Empty from a portal that predates
    /// them: the node uses [`Self::image`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_candidates: Vec<String>,
    pub security: SecurityProfile,
    /// `/dev/shm` for the app (browsers need more than Docker's 64 MB).
    pub shm_mb: u32,
    pub width: u32,
    pub height: u32,
    /// Frames per second: 60, 90 or 120 from the portal. The streamer paces
    /// to it and the app gets it as its refresh rate (`CHA_REFRESH`).
    pub fps: u32,
    /// The portal's public key (base64 Ed25519): the streamer accepts only
    /// connections that carry a media token it signed.
    pub portal_key: String,
    /// A Docker volume mounted as the app's home: how the first slice of
    /// persistence kept it, one per user and template, named by
    /// [`home_volume_name`] (the node refuses any other name). A portal that
    /// has [`Self::storage`] doesn't send this; a node honours it for one that
    /// doesn't yet. Not with `storage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// The user who launched it: with `template`, whose directories under the
    /// node's data root `storage` names, and the labels on the app's container.
    /// Empty from a portal that doesn't say.
    #[serde(default)]
    pub owner: String,
    /// The catalog template's id.
    #[serde(default)]
    pub template: String,
    /// The app's persistent and shared data under the node's data root, if it
    /// has any ([`Storage::check`] says what a node accepts).
    /// Boxed: a spec is inside every start request, and the messages' sizes
    /// are that of their largest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<Box<Storage>>,
    /// The kind of virtual controller the app gets; `None` is `xbox360`. A
    /// node that predates it makes Xbox 360 pads whatever this says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad: Option<GamepadKind>,
    /// What it runs on; `None` is `nvidia`. A node that predates it always
    /// uses the NVIDIA GPU. Boxed, like `storage`: the spec is inside every
    /// start request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<Box<DeviceChoice>>,
    /// A Moonlight app to stream through `cha-gateway` instead of running
    /// `image` beside the streamer (ADR 0008). A node that predates it would
    /// run `image` as an app, so the portal sends it only to a node that has
    /// sent [`ToPortal::MoonlightHosts`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<Box<GatewaySpec>>,
    /// Extra variables for the app, from a custom environment (ADR 0021),
    /// checked by [`check_env`]. The portal sends them only to a node that
    /// lists [`SPEC_FEATURE_ENV`]: an older one would start the app without.
    /// Boxed, like `storage`: the spec is inside every start request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Box<BTreeMap<String, String>>>,
    /// Mounts, ports, capabilities and devices the node's owner allows
    /// (ADR 0021). Sent only to a node that lists
    /// [`SPEC_FEATURE_HOST_OPTIONS`] and whose [`Inventory::host_options`]
    /// allows them; the node checks again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<Box<HostOptions>>,
}

impl EnvironmentSpec {
    /// The template whose data directories this environment uses
    /// ([`Storage::data_template`]).
    pub fn data_template(&self) -> &str {
        self.storage
            .as_ref()
            .and_then(|s| s.data_template.as_deref())
            .unwrap_or(&self.template)
    }
}

/// What kind of device an environment runs on (`docs/devices.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    /// An NVIDIA GPU: NVENC, and the app gets it through CDI.
    #[default]
    Nvidia,
    /// An Intel or AMD GPU: VA-API, and the app gets its render node.
    Vaapi,
    /// No GPU: software rendering and x264.
    Cpu,
}

impl DeviceKind {
    /// The name on the wire and as the streamer's `--device`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nvidia => "nvidia",
            Self::Vaapi => "vaapi",
            Self::Cpu => "cpu",
        }
    }

    /// The kind named `s`, if it is one.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "nvidia" => Some(Self::Nvidia),
            "vaapi" => Some(Self::Vaapi),
            "cpu" => Some(Self::Cpu),
            _ => None,
        }
    }
}

/// The device a launch picked, as the spec carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceChoice {
    /// The inventory device's id (`nvidia:0`, `vaapi:renderD129`, `cpu`).
    pub id: String,
    pub kind: DeviceKind,
    /// The render node a `vaapi` environment gets, e.g. `/dev/dri/renderD129`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_node: Option<String>,
}

/// A virtual controller an environment's streamer makes (`docs/controllers.md`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GamepadKind {
    /// An Xbox 360 pad over uinput: what every game and SDL know.
    #[default]
    Xbox360,
    /// A wired DualSense over uhid.
    Dualsense,
    /// A wired Steam Controller over uhid.
    Steam,
}

impl GamepadKind {
    /// The name on the wire, in the catalog and as the streamer's `--pad-kind`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Xbox360 => "xbox360",
            Self::Dualsense => "dualsense",
            Self::Steam => "steam",
        }
    }

    /// The kind named `s`, if it is one.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "xbox360" => Some(Self::Xbox360),
            "dualsense" => Some(Self::Dualsense),
            "steam" => Some(Self::Steam),
            _ => None,
        }
    }

    /// Whether the streamer makes it through uhid (the others use uinput).
    pub fn needs_uhid(self) -> bool {
        !matches!(self, Self::Xbox360)
    }
}

/// What every legacy home volume's name starts with.
pub const HOME_VOLUME_PREFIX: &str = "cha-home-";

/// The home volume `owner` had in `template` before data moved under the node's
/// data root, and that `home` still names for older portals (ids, as the
/// portal has them):
/// `cha-home-<owner>-<template>`, with anything Docker's volume names don't
/// allow made `_`. User ids are UUIDs and template ids are slugs, so no two
/// pairs end up with one name.
pub fn home_volume_name(owner: &str, template: &str) -> String {
    let safe = |id: &str| {
        id.chars()
            .map(|c| if volume_char(c) { c } else { '_' })
            .collect::<String>()
    };
    format!("{HOME_VOLUME_PREFIX}{}-{}", safe(owner), safe(template))
}

/// Whether `name` is a home volume's name: ours, and one Docker takes. A node
/// mounts nothing else as an app's home (its own `state` volume, say).
pub fn is_home_volume_name(name: &str) -> bool {
    name.len() > HOME_VOLUME_PREFIX.len()
        && name.starts_with(HOME_VOLUME_PREFIX)
        && name.chars().all(volume_char)
}

/// The user and template ids in a home volume's name, if it is one of the
/// portal's (`cha-home-<user id>-<template id>`, [`home_volume_name`]).
pub fn parse_home_volume_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(HOME_VOLUME_PREFIX)?;
    let (user, template) = rest.split_at_checked(36)?;
    let template = template.strip_prefix('-')?;
    (valid_user_id(user) && valid_template_id(template)).then_some((user, template))
}

/// Docker's volume names: `[a-zA-Z0-9][a-zA-Z0-9_.-]*`.
fn volume_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')
}

/// The app container's confinement (plan §4.2). Every profile runs the app as
/// an unprivileged user with no capabilities and no privilege gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityProfile {
    /// Docker's default seccomp profile.
    Standard,
    /// Also lets the app create namespaces, so browser sandboxes stay on.
    Browser,
    /// `browser`, plus mounts inside the app's own user namespaces (the
    /// `cha-sandbox` AppArmor profile, which the owner loads on the node):
    /// Steam's pressure-vessel builds a container for every game.
    Steam,
    /// Docker's default seccomp; the app also gets `/dev/kvm`, `/dev/udmabuf`
    /// when the host has it, and their groups, and a memory limit: a
    /// QEMU/KVM virtual machine runs inside it.
    Vm,
}

/// Where an environment's streamer listens on its node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamerEndpoint {
    pub http_port: u16,
    pub webrtc_port: u16,
    /// 0 when it has none (an older streamer).
    #[serde(default)]
    pub webtransport_port: u16,
}

/// What a node asks the portal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PortalRequest {
    Ping,
    /// A paired Moonlight client launched `template_id` on this node's
    /// GameStream host and nothing of `user_id`'s runs it here: start it on
    /// this node, as a launch in the portal would, and answer once it runs
    /// (or failed, with the reason as the error). Only sent once the welcome
    /// says the portal reads the `GameStream*` messages.
    GameStreamLaunch {
        user_id: String,
        template_id: String,
    },
    /// The client quit an app its launch started: stop `environment_id`, if it
    /// is `user_id`'s and runs on this node.
    GameStreamStop {
        user_id: String,
        environment_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PortalResponse {
    Pong {
        unix_ms: u64,
    },
    /// The environment of a [`PortalRequest::GameStreamLaunch`] is running.
    GameStreamLaunched {
        environment_id: String,
        /// This launch started it. `false`: it was already starting (an
        /// earlier launch, or the browser's) and the launch joined it.
        created: bool,
    },
    /// A [`PortalRequest::GameStreamStop`] was taken: the environment is
    /// stopping, or already gone.
    GameStreamStopped,
}

/// What a node has, refreshed when it changes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub cpus: u32,
    pub memory_mb: u64,
    pub gpus: Vec<Gpu>,
    /// Addresses browsers might reach the node at (LAN, overlay).
    pub addresses: Vec<String>,
    /// Where this node keeps app data (`CHA_DATA_ROOT`), when its agent can
    /// run environments and so knows. Absent from agents that predate
    /// storage: the portal won't send them a `storage` it can't honour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_root: Option<String>,
    /// Template id → where the node's owner keeps that template's shared
    /// directory, for the ones kept outside the data root (a NAS share): the
    /// path on the node, and in the app's container too.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shared_dirs: BTreeMap<String, String>,
    /// Template id → how that shared directory looks from the agent, for
    /// each one in [`Self::shared_dirs`]: checked at start, with every
    /// inventory refresh and when a launch finds it unusable. Absent from
    /// agents that predate it: the portal then knows only the paths.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shared_status: BTreeMap<String, SharedDirStatus>,
    /// What environments can run on here. Absent from nodes that predate
    /// devices: [`Self::devices_or_derived`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devices: Option<Vec<Device>>,
    /// This node runs a GameStream host for Moonlight clients (ADR 0009).
    /// Absent from agents that predate it, or have it off: the portal then
    /// sends none of the `GameStream*` requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamestream: Option<GameStreamInfo>,
    /// The images the node holds (`repo:tag`, at most 500), so placement can
    /// prefer a node that needn't download (ADR 0017). Empty from nodes that
    /// predate it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// The agent's release (`0.1.0`), which fills `{version}` in image
    /// candidates. Absent from nodes that predate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_version: Option<String>,
    /// `manual`: never the automatic choice, only by hand (`CHA_PLACEMENT`),
    /// for test beds. Absent means automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<PlacementMode>,
    /// Free and total space on the node's disks that matter: Docker's
    /// (images) and the data root's (app data); one entry when they are the
    /// same filesystem. Empty from agents that predate it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<Disk>,
    /// What the node runs on, as best the agent can tell. Absent from agents
    /// that predate it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// Whether the portal can update this agent (ADR 0018). Absent from
    /// agents that predate it, which can't be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<AgentUpdatability>,
    /// Optional [`EnvironmentSpec`] fields the agent honours, beyond those
    /// that predate this list ([`SPEC_FEATURE_ENV`] and the others). Empty
    /// from agents that predate it: the portal sends them none of those.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spec_features: Vec<String>,
    /// What custom environments may ask of this node (ADR 0021). Absent from
    /// agents that predate it, which allow nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_options: Option<HostPolicy>,
    /// The agent answers [`NodeRequest::OpenRelay`] (ADR 0022). An agent that
    /// predates it can't read that request and drops the connection, so the
    /// portal sends it only when this is set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relay: bool,
    /// The node has `/dev/kvm`, so it can run `vm` environments. Absent from
    /// agents that predate it, which can't, and which drop the connection on
    /// a profile they don't know: the portal sends none a `vm` spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kvm: Option<bool>,
}

impl Inventory {
    /// The agent honours this optional spec field.
    pub fn has_spec_feature(&self, feature: &str) -> bool {
        self.spec_features.iter().any(|f| f == feature)
    }
}

/// The image the agent runs, and whether it can be updated from the portal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentUpdatability {
    /// `ghcr.io/ban-red/cha-node:0.2.0`, or a local build (`cha-node:dev`).
    pub image: String,
    pub updatable: bool,
    /// Why not, when not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Where an agent update is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentUpdateState {
    /// Downloading the new agent and streamer images.
    Pulling,
    /// The helper is replacing the agent's container.
    Swapping,
    /// It didn't start; the old agent keeps running.
    Failed,
    /// The new agent didn't connect; the previous one runs again.
    RolledBack,
}

/// What a disk holds for the node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiskUse {
    /// Docker's images and containers.
    Images,
    /// Environments' app data (the data root).
    AppData,
}

/// A filesystem on the node and how full it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    pub uses: Vec<DiskUse>,
    /// A path to show the admin: Docker's root directory (or "Docker's disk"
    /// when the engine didn't say) or the data root.
    pub path: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// What kind of machine a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlatformKind {
    BareMetal,
    Vm,
    Lxc,
    Wsl,
    DockerDesktop,
    Unknown,
}

/// What a node runs on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Platform {
    pub kind: PlatformKind,
    /// A short phrase: "KVM (QEMU)", "LXC container", "WSL 2".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The kernel release (`6.14.11-4-pve`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
}

/// Whether placement may pick a node by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlacementMode {
    Auto,
    Manual,
}

impl Inventory {
    /// The devices this node offers: what it reported, or, for a node that
    /// predates them, one `nvidia` device for its first NVIDIA GPU with
    /// encoders (what it always ran on: CDI gives environments all of them
    /// and the streamer composites on one) and none otherwise.
    pub fn devices_or_derived(&self) -> Vec<Device> {
        if let Some(devices) = &self.devices {
            return devices.clone();
        }
        self.gpus
            .iter()
            .filter(|g| g.vendor == "nvidia")
            .enumerate()
            .filter(|(_, g)| !g.encoders.is_empty())
            .take(1)
            .map(|(i, g)| Device {
                id: format!("nvidia:{i}"),
                kind: DeviceKind::Nvidia,
                name: g.name.clone(),
                render_node: g.render_node.clone(),
                vendor: Some("nvidia".into()),
                codecs: g.encoders.clone(),
                cores: None,
            })
            .collect()
    }
}

/// Something an environment can run on (`docs/devices.md`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    /// Stable on the node: `nvidia:<index>`, `vaapi:renderD<N>`, `cpu`.
    pub id: String,
    pub kind: DeviceKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_node: Option<String>,
    /// `intel`, `amd` or `nvidia`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    /// What it encodes: `h264`, `hevc`, `av1`, …
    #[serde(default)]
    pub codecs: Vec<String>,
    /// The core count, for `cpu`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cores: Option<u32>,
}

/// Whether a shared directory kept outside the data root (`CHA_SHARED_DIRS`,
/// a NAS share) can be used by launches on this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedDirState {
    /// Mounted, readable, and the app's per-user places are there.
    Ok,
    /// Can't be opened: not mounted, or not bound into the agent.
    Missing,
    /// Opens, but a per-user place is absent or not a plain directory.
    Incomplete,
    /// The check didn't answer in time (a `hard` NFS mount whose server is away).
    Unreachable,
    /// The agent's uid 1000 (what apps write as) can't write there.
    ReadOnly,
}

/// One shared directory's check, from the agent ([`Inventory::shared_status`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedDirStatus {
    pub state: SharedDirState,
    /// The filesystem, from the mount table (`nfs4`, `cifs`, `ext4`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs_type: Option<String>,
    /// What is mounted there (`192.168.11.120:/mnt/user/games/steam`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Why it isn't `ok`, in a sentence for the portal to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// When it was checked, Unix seconds.
    pub checked_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Gpu {
    /// `nvidia`, `amd`, `intel`, …
    pub vendor: String,
    pub name: String,
    pub memory_mb: Option<u64>,
    pub driver: Option<String>,
    /// e.g. `/dev/dri/renderD128`.
    pub render_node: Option<String>,
    /// Hardware encoders: `h264`, `hevc`, `av1`.
    pub encoders: Vec<String>,
}

/// How often a node reports its [`NodeUsage`].
pub const USAGE_INTERVAL_SECS: u64 = 3;

/// A node's CPU, RAM and GPU use at one moment (percent 0..100, bytes, watts,
/// °C).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeUsage {
    pub cpu: f64,
    pub cores: u32,
    /// The 1, 5 and 15 minute load averages.
    pub load: [f64; 3],
    pub mem_used: u64,
    pub mem_total: u64,
    /// Every NVIDIA GPU, in NVML's order; none if the agent can't see NVML.
    pub gpus: Vec<GpuUsage>,
    /// Environments this node is running.
    pub environments: u32,
    /// What each running environment uses of it (absent from an older node).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub by_environment: Vec<EnvironmentUsage>,
}

/// One environment's use of its node: its app and streamer containers together.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentUsage {
    pub id: String,
    /// Percent of the whole machine's CPU, like [`NodeUsage::cpu`].
    pub cpu: f64,
    /// Bytes of RAM (without reclaimable file cache).
    pub mem: u64,
    /// Bytes of GPU memory its processes hold; absent when the node can't tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram: Option<u64>,
}

/// One GPU's use; what the driver didn't give is absent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuUsage {
    pub index: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub util: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vram_total: Option<u64>,
    /// NVENC and NVDEC utilisation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enc: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dec: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_limit: Option<f64>,
}

/// The bytes a node signs to prove it holds its key: bound to this connection's
/// challenge and to the node id.
pub fn hello_message(nonce: &str, node_id: &str) -> Vec<u8> {
    format!("cha-node-hello/v{PROTOCOL_VERSION}:{nonce}:{node_id}").into_bytes()
}

/// The bytes a node signs to open the media relay `relay_id`
/// ([`NodeRequest::OpenRelay`]).
pub fn relay_message(relay_id: &str, node_id: &str) -> Vec<u8> {
    format!("cha-node-relay/v{PROTOCOL_VERSION}:{relay_id}:{node_id}").into_bytes()
}

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("not valid base64")]
    Base64,
    #[error("wrong length")]
    Length,
    #[error("not a valid Ed25519 key")]
    Key,
    #[error("signature doesn't verify")]
    Signature,
}

/// An Ed25519 key pair: a node's identity, or the portal's signing key.
pub struct NodeKey(SigningKey);

impl NodeKey {
    pub fn from_secret(secret: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&secret))
    }

    pub fn secret(&self) -> [u8; 32] {
        self.0.to_bytes()
    }

    pub fn public_b64(&self) -> String {
        STANDARD.encode(self.0.verifying_key().to_bytes())
    }

    pub fn sign_b64(&self, message: &[u8]) -> String {
        STANDARD.encode(self.0.sign(message).to_bytes())
    }
}

/// Parses a base64 Ed25519 public key.
pub fn parse_public_key(b64: &str) -> Result<VerifyingKey, KeyError> {
    let bytes: [u8; 32] = STANDARD
        .decode(b64)
        .map_err(|_| KeyError::Base64)?
        .try_into()
        .map_err(|_| KeyError::Length)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| KeyError::Key)
}

/// Checks a base64 signature of `message` by `public_key_b64`.
pub fn verify_b64(
    public_key_b64: &str,
    message: &[u8],
    signature_b64: &str,
) -> Result<(), KeyError> {
    let key = parse_public_key(public_key_b64)?;
    let sig: [u8; 64] = STANDARD
        .decode(signature_b64)
        .map_err(|_| KeyError::Base64)?
        .try_into()
        .map_err(|_| KeyError::Length)?;
    key.verify(message, &Signature::from_bytes(&sig))
        .map_err(|_| KeyError::Signature)
}

/// What a media token lets its bearer do: connect to one environment, briefly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaClaims {
    /// The environment.
    pub env: String,
    /// The user it was issued to.
    pub sub: String,
    /// `owner`, `admin`, or from a share link `player`, `viewer` or
    /// `controller` (ADRs 0014 and 0015). A streamer treats a role it doesn't
    /// know as a viewer with no input.
    pub role: String,
    /// A player's gamepad slot, 1 to 3 (player 2 to 4); absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u8>,
    /// Expiry, Unix seconds.
    pub exp: i64,
}

const MEDIA_TOKEN_CONTEXT: &str = "cha-media/v1.";

/// Signs `claims` as `<base64url claims>.<base64url signature>`.
pub fn sign_media_token(key: &NodeKey, claims: &MediaClaims) -> String {
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let signature = key
        .0
        .sign(format!("{MEDIA_TOKEN_CONTEXT}{body}").as_bytes());
    format!("{body}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MediaTokenError {
    #[error("malformed token")]
    Malformed,
    #[error("bad signature")]
    Signature,
    #[error("expired")]
    Expired,
    #[error("for another environment")]
    WrongEnvironment,
}

/// Checks a media token against the portal's public key, its expiry (at
/// `now`, Unix seconds) and the environment it must be for.
pub fn verify_media_token(
    portal_key_b64: &str,
    token: &str,
    environment: &str,
    now: i64,
) -> Result<MediaClaims, MediaTokenError> {
    let (body, signature) = token.split_once('.').ok_or(MediaTokenError::Malformed)?;
    let key = parse_public_key(portal_key_b64).map_err(|_| MediaTokenError::Signature)?;
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(signature)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(MediaTokenError::Malformed)?;
    key.verify(
        format!("{MEDIA_TOKEN_CONTEXT}{body}").as_bytes(),
        &Signature::from_bytes(&signature),
    )
    .map_err(|_| MediaTokenError::Signature)?;
    let claims: MediaClaims = URL_SAFE_NO_PAD
        .decode(body)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or(MediaTokenError::Malformed)?;
    if claims.exp < now {
        return Err(MediaTokenError::Expired);
    }
    if claims.env != environment {
        return Err(MediaTokenError::WrongEnvironment);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_updates_round_trip() {
        let msg = ToNode::UpdateAgent {
            version: "0.2.1".into(),
        };
        assert_eq!(
            serde_json::to_value(&msg).unwrap(),
            serde_json::json!({"type": "update_agent", "version": "0.2.1"})
        );
        let progress = ToPortal::AgentUpdate {
            state: AgentUpdateState::RolledBack,
            detail: Some("the new agent didn't connect".into()),
            done: None,
            total: None,
        };
        let json = serde_json::to_value(&progress).unwrap();
        assert_eq!(json["state"], "rolled-back");
        assert!(json.get("done").is_none());
        assert!(matches!(
            serde_json::from_value::<ToPortal>(json).unwrap(),
            ToPortal::AgentUpdate {
                state: AgentUpdateState::RolledBack,
                ..
            }
        ));
        let can = AgentUpdatability {
            image: "cha-node:dev".into(),
            updatable: false,
            reason: Some("a local build".into()),
        };
        let json = serde_json::to_value(&can).unwrap();
        assert_eq!(json["updatable"], false);
        assert_eq!(
            serde_json::from_value::<AgentUpdatability>(json).unwrap(),
            can
        );
    }

    #[test]
    fn signed_hello_verifies_only_for_its_challenge() {
        let key = NodeKey::from_secret([7u8; 32]);
        let msg = hello_message("nonce-1", "node-a");
        let sig = key.sign_b64(&msg);
        assert!(verify_b64(&key.public_b64(), &msg, &sig).is_ok());
        assert!(verify_b64(&key.public_b64(), &hello_message("nonce-2", "node-a"), &sig).is_err());
        assert!(verify_b64(&key.public_b64(), &hello_message("nonce-1", "node-b"), &sig).is_err());
        let other = NodeKey::from_secret([8u8; 32]);
        assert!(verify_b64(&other.public_b64(), &msg, &sig).is_err());
    }

    #[test]
    fn warnings_are_sent_only_to_a_portal_that_says_it_reads_them() {
        let said = serde_json::to_value(ToPortal::EnvironmentWarning {
            id: "e1".into(),
            warning: None,
        })
        .unwrap();
        assert_eq!(
            said,
            serde_json::json!({ "type": "environment_warning", "id": "e1", "warning": null })
        );
        // A welcome from a portal that predates them doesn't say so.
        let old: ToNode = serde_json::from_value(
            serde_json::json!({ "type": "welcome", "node_id": "n", "heartbeat_secs": 15 }),
        )
        .unwrap();
        assert!(matches!(
            old,
            ToNode::Welcome {
                environment_warnings: false,
                node_usage: false,
                moonlight: false,
                gamestream: false,
                gamestream_launch: false,
                ..
            }
        ));
    }

    #[test]
    fn the_gamestream_messages_round_trip() {
        let to_portal = [
            ToPortal::GameStreamPairRequest {
                attempt_id: "a1".into(),
                device_name: "Steam Deck".into(),
                address: "192.168.1.9".into(),
                expires_in_secs: 120,
            },
            ToPortal::GameStreamPaired {
                attempt_id: "a1".into(),
                fingerprint: "ab".repeat(32),
                unique_id: "0123456789ABCDEF".into(),
                name: "Steam Deck".into(),
            },
            ToPortal::GameStreamPairFailed {
                attempt_id: "a1".into(),
                reason: "wrong PIN".into(),
            },
            ToPortal::GameStreamUnpaired {
                fingerprint: "ab".repeat(32),
            },
        ];
        for msg in to_portal {
            let text = serde_json::to_string(&msg).unwrap();
            let back: ToPortal = serde_json::from_str(&text).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), text);
        }
        let device = GameStreamDevice {
            fingerprint: "cd".repeat(32),
            unique_id: None,
            name: "Phone".into(),
            user_id: "u1".into(),
        };
        let requests = [
            NodeRequest::GameStreamPin {
                attempt_id: "a1".into(),
                pin: "1234".into(),
                user_id: "u1".into(),
            },
            NodeRequest::GameStreamDevices {
                devices: vec![device.clone()],
            },
        ];
        for request in requests {
            let text = serde_json::to_string(&ToNode::Request { id: 3, request }).unwrap();
            let back: ToNode = serde_json::from_str(&text).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), text);
        }
        let said = serde_json::to_value(NodeRequest::GameStreamDevices {
            devices: vec![device],
        })
        .unwrap();
        assert_eq!(said["op"], "game_stream_devices");
        assert!(said["devices"][0].get("unique_id").is_none());
        let accepted = serde_json::to_string(&NodeResponse::Accepted).unwrap();
        assert!(matches!(
            serde_json::from_str::<NodeResponse>(&accepted).unwrap(),
            NodeResponse::Accepted
        ));
    }

    #[test]
    fn launching_has_its_own_welcome_flag_which_defaults_off() {
        let old: ToNode = serde_json::from_value(serde_json::json!({
            "type": "welcome", "node_id": "n", "heartbeat_secs": 15, "gamestream": true
        }))
        .unwrap();
        assert!(matches!(
            old,
            ToNode::Welcome {
                gamestream: true,
                gamestream_launch: false,
                ..
            }
        ));
        let new = ToNode::Welcome {
            node_id: "n".into(),
            heartbeat_secs: 15,
            environment_warnings: true,
            node_usage: true,
            moonlight: true,
            gamestream: true,
            gamestream_launch: true,
            agent_updates: true,
        };
        let text = serde_json::to_string(&new).unwrap();
        assert!(matches!(
            serde_json::from_str::<ToNode>(&text).unwrap(),
            ToNode::Welcome {
                gamestream_launch: true,
                ..
            }
        ));
    }

    #[test]
    fn the_nodes_gamestream_requests_to_the_portal_round_trip() {
        let requests = [
            PortalRequest::GameStreamLaunch {
                user_id: "u1".into(),
                template_id: "chrome".into(),
            },
            PortalRequest::GameStreamStop {
                user_id: "u1".into(),
                environment_id: "e1".into(),
            },
        ];
        for request in requests {
            let msg = ToPortal::Request { id: 7, request };
            let text = serde_json::to_string(&msg).unwrap();
            let back: ToPortal = serde_json::from_str(&text).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), text);
        }
        let said = serde_json::to_value(PortalRequest::GameStreamLaunch {
            user_id: "u1".into(),
            template_id: "chrome".into(),
        })
        .unwrap();
        assert_eq!(
            said,
            serde_json::json!({ "op": "game_stream_launch", "user_id": "u1", "template_id": "chrome" })
        );
        let results: [Result<PortalResponse, String>; 3] = [
            Ok(PortalResponse::GameStreamLaunched {
                environment_id: "e1".into(),
                created: true,
            }),
            Ok(PortalResponse::GameStreamStopped),
            Err("your Steam is already running on box".into()),
        ];
        for result in results {
            let msg = ToNode::Response { id: 7, result };
            let text = serde_json::to_string(&msg).unwrap();
            let back: ToNode = serde_json::from_str(&text).unwrap();
            assert_eq!(serde_json::to_string(&back).unwrap(), text);
        }
    }

    #[test]
    fn an_inventory_without_gamestream_says_so_by_leaving_it_out() {
        let mut inventory = Inventory::default();
        let plain = serde_json::to_value(&inventory).unwrap();
        assert!(plain.get("gamestream").is_none());
        let old: Inventory = serde_json::from_value(plain).unwrap();
        assert_eq!(old.gamestream, None);
        inventory.gamestream = Some(GameStreamInfo {
            http_port: 47989,
            name: "box".into(),
        });
        let said = serde_json::to_value(&inventory).unwrap();
        assert_eq!(
            said["gamestream"],
            serde_json::json!({ "httpPort": 47989, "name": "box" })
        );
        let back: Inventory = serde_json::from_value(said).unwrap();
        assert_eq!(back, inventory);
    }

    #[test]
    fn moonlight_messages_round_trip_and_a_gateway_spec_is_optional() {
        let host = MoonlightHost {
            unique_id: "ABC".into(),
            name: "gaming-pc".into(),
            address: "192.168.1.9".into(),
            http_port: 47989,
            https_port: 47984,
            paired: true,
            codecs: vec!["h264".into(), "hevc".into()],
            app_version: None,
        };
        let json = serde_json::to_value(ToPortal::MoonlightHosts {
            hosts: vec![host.clone()],
        })
        .unwrap();
        assert_eq!(json["type"], "moonlight_hosts");
        assert_eq!(json["hosts"][0]["uniqueId"], "ABC");
        assert!(json["hosts"][0].get("appVersion").is_none());
        let back: ToPortal = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ToPortal::MoonlightHosts { hosts } if hosts == vec![host]));

        let paired = serde_json::to_value(ToPortal::MoonlightPaired {
            unique_id: "ABC".into(),
            ok: false,
            message: Some("timed out".into()),
        })
        .unwrap();
        assert_eq!(paired["message"], "timed out");
        assert!(matches!(
            serde_json::from_value::<ToPortal>(paired).unwrap(),
            ToPortal::MoonlightPaired { ok: false, .. }
        ));

        let req = serde_json::to_value(NodeRequest::MoonlightPair {
            unique_id: "ABC".into(),
        })
        .unwrap();
        assert_eq!(
            req,
            serde_json::json!({ "op": "moonlight_pair", "unique_id": "ABC" })
        );
        let resp: NodeResponse =
            serde_json::from_value(serde_json::json!({ "op": "moonlight_pairing", "pin": "1234" }))
                .unwrap();
        assert!(matches!(resp, NodeResponse::MoonlightPairing { pin } if pin == "1234"));
        let apps: NodeResponse = serde_json::from_value(
            serde_json::json!({ "op": "moonlight_apps", "apps": [{ "id": 1, "name": "Desktop" }] }),
        )
        .unwrap();
        assert!(matches!(apps, NodeResponse::MoonlightApps { apps } if !apps[0].hdr));

        // A welcome from a portal that predates Moonlight doesn't carry the flag.
        let old: ToNode = serde_json::from_value(
            serde_json::json!({ "type": "welcome", "node_id": "n", "heartbeat_secs": 15 }),
        )
        .unwrap();
        assert!(matches!(
            old,
            ToNode::Welcome {
                moonlight: false,
                ..
            }
        ));

        // A spec without a gateway leaves the key out and reads back without one.
        let gw = GatewaySpec {
            unique_id: "ABC".into(),
            address: "192.168.1.9".into(),
            http_port: 47989,
            https_port: 47984,
            app_id: 881448767,
        };
        let spec: EnvironmentSpec = serde_json::from_value(serde_json::json!({
            "id": "e", "image": "cha/gateway:dev", "security": "standard", "shmMb": 64,
            "width": 1920, "height": 1080, "fps": 60, "portalKey": "k",
            "gateway": serde_json::to_value(&gw).unwrap()
        }))
        .unwrap();
        assert_eq!(spec.gateway.as_deref(), Some(&gw));
        let plain = serde_json::to_value(EnvironmentSpec {
            gateway: None,
            env: None,
            host: None,
            ..spec
        })
        .unwrap();
        assert!(plain.get("gateway").is_none());
    }

    #[test]
    fn usage_is_camel_case_and_leaves_out_what_the_gpu_didnt_say() {
        let usage = NodeUsage {
            cpu: 12.5,
            cores: 16,
            load: [1.0, 0.5, 0.25],
            mem_used: 4,
            mem_total: 8,
            gpus: vec![GpuUsage {
                index: 0,
                name: "RTX".into(),
                util: Some(40),
                ..GpuUsage::default()
            }],
            environments: 2,
            by_environment: vec![EnvironmentUsage {
                id: "e1".into(),
                cpu: 3.5,
                mem: 1024,
                vram: None,
            }],
        };
        let json = serde_json::to_value(ToPortal::Usage {
            usage: usage.clone(),
        })
        .unwrap();
        assert_eq!(json["type"], "usage");
        assert_eq!(json["usage"]["memUsed"], 4);
        // What the node couldn't say is left out; an older node sends none at all.
        assert_eq!(
            json["usage"]["byEnvironment"][0],
            serde_json::json!({ "id": "e1", "cpu": 3.5, "mem": 1024 })
        );
        assert_eq!(
            json["usage"]["gpus"][0],
            serde_json::json!({ "index": 0, "name": "RTX", "util": 40 })
        );
        let back: ToPortal = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ToPortal::Usage { usage: u } if u == usage));
    }

    #[test]
    fn an_exit_carries_its_log_and_both_ages_read_each_other() {
        // An older node sends none.
        let old: ToPortal = serde_json::from_value(serde_json::json!({
            "type": "environment_exited", "id": "e1", "detail": "gone", "failed": true
        }))
        .unwrap();
        assert!(matches!(old, ToPortal::EnvironmentExited { ref log, .. } if log.is_empty()));

        let new = serde_json::to_value(ToPortal::EnvironmentExited {
            id: "e1".into(),
            detail: "gone".into(),
            failed: true,
            log: vec!["Error: boom".into()],
        })
        .unwrap();
        assert_eq!(new["log"], serde_json::json!(["Error: boom"]));
        // A message without a log doesn't carry the key.
        let bare = serde_json::to_value(ToPortal::EnvironmentExited {
            id: "e1".into(),
            detail: "gone".into(),
            failed: false,
            log: Vec::new(),
        })
        .unwrap();
        assert!(bare.get("log").is_none());

        // An older portal's enum has no `log`; it must still read the message.
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        #[allow(dead_code)]
        enum Older {
            EnvironmentExited {
                id: String,
                detail: String,
                failed: bool,
            },
        }
        let older: Older = serde_json::from_value(new).unwrap();
        assert!(matches!(
            older,
            Older::EnvironmentExited { failed: true, .. }
        ));
    }

    #[test]
    fn messages_have_stable_json() {
        let hello = serde_json::to_value(ToNode::Challenge {
            nonce: "n".into(),
            protocol: 1,
        })
        .unwrap();
        assert_eq!(
            hello,
            serde_json::json!({ "type": "challenge", "nonce": "n", "protocol": 1 })
        );
        let req = serde_json::to_value(ToNode::Request {
            id: 3,
            request: NodeRequest::Ping,
        })
        .unwrap();
        assert_eq!(
            req,
            serde_json::json!({ "type": "request", "id": 3, "request": { "op": "ping" } })
        );
        let back: ToPortal =
            serde_json::from_value(serde_json::json!({ "type": "heartbeat" })).unwrap();
        assert!(matches!(back, ToPortal::Heartbeat));
        let start = serde_json::to_value(NodeRequest::StartEnvironment {
            environment: EnvironmentSpec {
                id: "e1".into(),
                image: "cha/env-chrome:dev".into(),
                image_candidates: Vec::new(),
                security: SecurityProfile::Browser,
                shm_mb: 1024,
                width: 2560,
                height: 1440,
                fps: 60,
                portal_key: "k".into(),
                home: None,
                owner: "u1".into(),
                template: "chrome".into(),
                storage: None,
                gamepad: None,
                device: None,
                gateway: None,
                env: None,
                host: None,
            },
        })
        .unwrap();
        assert_eq!(start["op"], "start_environment");
        assert_eq!(start["environment"]["security"], "browser");
        assert_eq!(start["environment"]["shmMb"], 1024);
        assert!(start["environment"].get("home").is_none());
        assert_eq!(start["environment"]["owner"], "u1");
        assert_eq!(start["environment"]["template"], "chrome");
    }

    #[test]
    fn specs_from_before_homes_still_parse() {
        // What a portal sent before `home`, `owner` and `template` existed.
        let old = serde_json::json!({
            "id": "e1",
            "image": "cha/env-chrome:dev",
            "security": "browser",
            "shmMb": 1024,
            "width": 2560,
            "height": 1440,
            "fps": 60,
            "portalKey": "k",
        });
        let spec: EnvironmentSpec = serde_json::from_value(old).unwrap();
        assert_eq!(spec.home, None);
        assert!(spec.owner.is_empty() && spec.template.is_empty());

        // One with a home, as the first slice sent it (a name, no labels).
        let with_home = serde_json::json!({
            "id": "e2",
            "image": "cha/env-steam:dev",
            "security": "steam",
            "shmMb": 2048,
            "width": 2560,
            "height": 1440,
            "fps": 60,
            "portalKey": "k",
            "home": "cha-home-u1-steam",
            // A field from a newer portal: ignored, as `owner` and `template`
            // are by a node that predates them.
            "owner": "u1",
            "template": "steam",
            "somethingNew": 1,
        });
        let spec: EnvironmentSpec = serde_json::from_value(with_home).unwrap();
        assert_eq!(spec.home.as_deref(), Some("cha-home-u1-steam"));
        assert_eq!(
            (spec.owner.as_str(), spec.template.as_str()),
            ("u1", "steam")
        );
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            serde_json::from_value::<EnvironmentSpec>(json).unwrap(),
            spec
        );
    }

    #[test]
    fn gamepad_kinds_are_optional_and_lowercase() {
        let spec = |gamepad| EnvironmentSpec {
            id: "e1".into(),
            image: "i".into(),
            image_candidates: Vec::new(),
            security: SecurityProfile::Standard,
            shm_mb: 64,
            width: 1,
            height: 1,
            fps: 1,
            portal_key: "k".into(),
            home: None,
            owner: String::new(),
            template: String::new(),
            storage: None,
            gamepad,
            device: None,
            gateway: None,
            env: None,
            host: None,
        };
        // Absent when unset, so an older node reads what it always did.
        let json = serde_json::to_value(spec(None)).unwrap();
        assert!(json.get("gamepad").is_none());
        assert_eq!(
            serde_json::from_value::<EnvironmentSpec>(json)
                .unwrap()
                .gamepad,
            None
        );
        for (kind, name) in [
            (GamepadKind::Xbox360, "xbox360"),
            (GamepadKind::Dualsense, "dualsense"),
            (GamepadKind::Steam, "steam"),
        ] {
            let json = serde_json::to_value(spec(Some(kind))).unwrap();
            assert_eq!(json["gamepad"], name);
            assert_eq!(kind.as_str(), name);
            assert_eq!(GamepadKind::parse(name), Some(kind));
            assert_eq!(
                serde_json::from_value::<EnvironmentSpec>(json)
                    .unwrap()
                    .gamepad,
                Some(kind)
            );
        }
        assert_eq!(GamepadKind::default(), GamepadKind::Xbox360);
        assert_eq!(GamepadKind::parse("ps5"), None);
        assert!(GamepadKind::Steam.needs_uhid() && !GamepadKind::Xbox360.needs_uhid());
        // A newer portal's spec with a kind this build doesn't know is refused,
        // rather than quietly given the wrong pad.
        let bad = serde_json::json!({
            "id": "e1", "image": "i", "security": "standard", "shmMb": 64,
            "width": 1, "height": 1, "fps": 1, "portalKey": "k", "gamepad": "ps5",
        });
        assert!(serde_json::from_value::<EnvironmentSpec>(bad).is_err());
    }

    #[test]
    fn storage_specs_round_trip_and_old_ones_have_none() {
        let user = "01a10527-f79f-761b-962f-4b26924a2e68";
        let spec = EnvironmentSpec {
            id: "e1".into(),
            image: "cha/env-steam:dev".into(),
            image_candidates: Vec::new(),
            security: SecurityProfile::Steam,
            shm_mb: 2048,
            width: 2560,
            height: 1440,
            fps: 60,
            portal_key: "k".into(),
            home: None,
            owner: user.into(),
            template: "steam".into(),
            storage: Some(Box::new(Storage {
                home: Some(user_dir(user, "steam")),
                shared: Some(Shared {
                    path: shared_dir("steam"),
                    writable: false,
                    per_user: vec!["steamapps/compatdata".into()],
                }),
                legacy_volume: Some(home_volume_name(user, "steam")),
                data_template: None,
            })),
            gamepad: None,
            device: None,
            gateway: None,
            env: None,
            host: None,
        };
        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(
            json["storage"],
            serde_json::json!({
                "home": "users/01a10527-f79f-761b-962f-4b26924a2e68/steam",
                "shared": {
                    "path": "shared/steam",
                    "writable": false,
                    "perUser": ["steamapps/compatdata"],
                },
                "legacyVolume": "cha-home-01a10527-f79f-761b-962f-4b26924a2e68-steam",
            })
        );
        assert!(json.get("home").is_none());
        assert_eq!(
            serde_json::from_value::<EnvironmentSpec>(json).unwrap(),
            spec
        );

        // Parts a portal leaves out are absent, and absent parts parse.
        let bare = serde_json::to_value(EnvironmentSpec {
            storage: Some(Box::default()),
            ..spec.clone()
        })
        .unwrap();
        assert_eq!(bare["storage"], serde_json::json!({}));
        let shared_only =
            serde_json::json!({ "shared": { "path": "shared/steam", "writable": true } });
        let parsed: Storage = serde_json::from_value(shared_only).unwrap();
        assert_eq!(parsed.home, None);
        assert!(parsed.shared.unwrap().per_user.is_empty());

        // A spec from before storage (or a portal that sends none): no storage.
        let old = serde_json::json!({
            "id": "e1", "image": "i", "security": "standard", "shmMb": 64,
            "width": 1, "height": 1, "fps": 1, "portalKey": "k",
        });
        assert_eq!(
            serde_json::from_value::<EnvironmentSpec>(old)
                .unwrap()
                .storage,
            None
        );
    }

    #[test]
    fn a_node_that_predates_storage_ignores_it() {
        // Fields a node that predates storage doesn't know are ignored, as
        // serde does by default: what it reads is the spec's older half.
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Before {
            id: String,
            home: Option<String>,
        }
        let spec = serde_json::json!({
            "id": "e1", "image": "i", "security": "standard", "shmMb": 64,
            "width": 1, "height": 1, "fps": 1, "portalKey": "k",
            "storage": { "shared": { "path": "shared/steam", "writable": true } },
        });
        let before: Before = serde_json::from_value(spec).unwrap();
        assert_eq!((before.id.as_str(), before.home), ("e1", None));
    }

    #[test]
    fn storage_requests_have_stable_json() {
        let delete = serde_json::to_value(NodeRequest::DeleteUserData {
            user: "u".into(),
            template: "steam".into(),
        })
        .unwrap();
        assert_eq!(
            delete,
            serde_json::json!({ "op": "delete_user_data", "user": "u", "template": "steam" })
        );
        let done = serde_json::to_value(NodeResponse::UserDataDeleted {
            user: "u".into(),
            template: "steam".into(),
        })
        .unwrap();
        assert_eq!(done["op"], "user_data_deleted");
        let progress = serde_json::to_value(ToPortal::EnvironmentProgress {
            id: "e1".into(),
            detail: "moving your files".into(),
            done: None,
            total: None,
            unit: None,
        })
        .unwrap();
        assert_eq!(
            progress,
            serde_json::json!({ "type": "environment_progress", "id": "e1", "detail": "moving your files" })
        );
    }

    #[test]
    fn inventories_from_before_storage_have_no_data_root() {
        let old = serde_json::json!({
            "hostname": "h", "os": "o", "arch": "a", "cpus": 1, "memoryMb": 2,
            "gpus": [], "addresses": [],
        });
        let inv: Inventory = serde_json::from_value(old).unwrap();
        assert_eq!(inv.data_root, None);
        let json = serde_json::to_value(&inv).unwrap();
        assert!(json.get("dataRoot").is_none(), "absent, not null");
        assert!(json.get("sharedDirs").is_none());
        let with = Inventory {
            data_root: Some(DEFAULT_DATA_ROOT.into()),
            shared_dirs: [("steam".to_string(), "/mnt/games/steam".to_string())].into(),
            ..inv
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["dataRoot"], "/srv/cha-portal");
        assert_eq!(json["sharedDirs"]["steam"], "/mnt/games/steam");
        assert_eq!(serde_json::from_value::<Inventory>(json).unwrap(), with);
    }

    #[test]
    fn shared_status_is_optional_and_round_trips() {
        let old = serde_json::json!({
            "hostname": "h", "os": "o", "arch": "a", "cpus": 1, "memoryMb": 2,
            "gpus": [], "addresses": [], "sharedDirs": {"steam": "/mnt/games/steam"},
        });
        let inv: Inventory = serde_json::from_value(old).unwrap();
        assert!(inv.shared_status.is_empty());
        assert!(
            serde_json::to_value(&inv)
                .unwrap()
                .get("sharedStatus")
                .is_none()
        );
        let with = Inventory {
            shared_status: [(
                "steam".to_string(),
                SharedDirStatus {
                    state: SharedDirState::Missing,
                    fs_type: None,
                    source: None,
                    detail: Some("not mounted".into()),
                    checked_at: 1_791_000_000,
                },
            )]
            .into(),
            ..inv
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["sharedStatus"]["steam"]["state"], "missing");
        assert_eq!(json["sharedStatus"]["steam"]["checkedAt"], 1_791_000_000);
        assert!(json["sharedStatus"]["steam"].get("fsType").is_none());
        assert_eq!(serde_json::from_value::<Inventory>(json).unwrap(), with);
    }

    #[test]
    fn devices_are_optional_and_an_older_nodes_gpu_is_one() {
        let old = serde_json::json!({
            "hostname": "h", "os": "o", "arch": "a", "cpus": 8, "memoryMb": 2,
            "gpus": [
                { "vendor": "intel", "name": "i", "memoryMb": null, "driver": null,
                  "renderNode": "/dev/dri/renderD128", "encoders": [] },
                { "vendor": "nvidia", "name": "RTX 4090", "memoryMb": 24564, "driver": "570",
                  "renderNode": "/dev/dri/renderD129", "encoders": ["h264", "hevc", "av1"] },
            ],
            "addresses": [],
        });
        let inv: Inventory = serde_json::from_value(old).unwrap();
        assert_eq!(inv.devices, None);
        assert!(serde_json::to_value(&inv).unwrap().get("devices").is_none());
        let derived = inv.devices_or_derived();
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0].id, "nvidia:0");
        assert_eq!(derived[0].kind, DeviceKind::Nvidia);
        assert_eq!(derived[0].codecs, ["h264", "hevc", "av1"]);
        // A GPU without encoders is no device; reported devices win.
        let none = Inventory {
            gpus: vec![Gpu {
                encoders: Vec::new(),
                ..inv.gpus[1].clone()
            }],
            ..inv.clone()
        };
        assert!(none.devices_or_derived().is_empty());
        let cpu = Device {
            id: "cpu".into(),
            kind: DeviceKind::Cpu,
            name: "CPU".into(),
            render_node: None,
            vendor: None,
            codecs: vec!["h264".into()],
            cores: Some(8),
        };
        let reported = Inventory {
            devices: Some(vec![cpu.clone()]),
            ..inv
        };
        let json = serde_json::to_value(&reported).unwrap();
        assert_eq!(json["devices"][0]["kind"], "cpu");
        assert_eq!(json["devices"][0]["cores"], 8);
        assert!(json["devices"][0].get("renderNode").is_none());
        assert_eq!(reported.devices_or_derived(), [cpu]);
    }

    #[test]
    fn kvm_is_optional_and_a_vm_profile_round_trips() {
        let old = serde_json::json!({
            "hostname": "h", "os": "o", "arch": "a", "cpus": 1, "memoryMb": 2,
            "gpus": [], "addresses": [],
        });
        let inv: Inventory = serde_json::from_value(old).unwrap();
        assert_eq!(inv.kvm, None);
        assert!(serde_json::to_value(&inv).unwrap().get("kvm").is_none());
        let with = Inventory {
            kvm: Some(true),
            ..inv
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["kvm"], true);
        assert_eq!(serde_json::from_value::<Inventory>(json).unwrap(), with);
        assert_eq!(
            serde_json::to_value(SecurityProfile::Vm).unwrap(),
            serde_json::json!("vm")
        );
        assert_eq!(
            serde_json::from_value::<SecurityProfile>(serde_json::json!("vm")).unwrap(),
            SecurityProfile::Vm
        );
    }

    #[test]
    fn disks_and_platform_round_trip_and_are_optional() {
        let old = serde_json::json!({
            "hostname": "h", "os": "o", "arch": "a", "cpus": 1, "memoryMb": 2,
            "gpus": [], "addresses": [],
        });
        let inv: Inventory = serde_json::from_value(old).unwrap();
        assert!(inv.disks.is_empty());
        assert_eq!(inv.platform, None);
        let json = serde_json::to_value(&inv).unwrap();
        assert!(json.get("disks").is_none());
        assert!(json.get("platform").is_none());

        let new = serde_json::json!({
            "hostname": "h", "os": "o", "arch": "a", "cpus": 1, "memoryMb": 2,
            "gpus": [], "addresses": [],
            "disks": [{ "uses": ["images", "appData"], "path": "/srv/cha-portal",
                        "totalBytes": 34359738368u64, "freeBytes": 18253611008u64 }],
            "platform": { "kind": "lxc", "detail": "LXC container", "kernel": "6.14.11-4-pve" },
        });
        let inv: Inventory = serde_json::from_value(new.clone()).unwrap();
        assert_eq!(inv.disks[0].uses, [DiskUse::Images, DiskUse::AppData]);
        assert_eq!(inv.disks[0].free_bytes, 18253611008);
        assert_eq!(inv.platform.as_ref().unwrap().kind, PlatformKind::Lxc);
        assert_eq!(serde_json::to_value(&inv).unwrap(), new);

        for (kind, name) in [
            (PlatformKind::BareMetal, "bare-metal"),
            (PlatformKind::Vm, "vm"),
            (PlatformKind::Wsl, "wsl"),
            (PlatformKind::DockerDesktop, "docker-desktop"),
            (PlatformKind::Unknown, "unknown"),
        ] {
            let platform = Platform {
                kind,
                detail: None,
                kernel: None,
            };
            let json = serde_json::to_value(&platform).unwrap();
            assert_eq!(json, serde_json::json!({ "kind": name }));
            assert_eq!(serde_json::from_value::<Platform>(json).unwrap(), platform);
        }
    }

    #[test]
    fn a_specs_device_is_optional() {
        let spec: EnvironmentSpec = serde_json::from_value(serde_json::json!({
            "id": "e", "image": "i", "security": "standard", "shmMb": 1,
            "width": 1, "height": 1, "fps": 1, "portalKey": "k",
        }))
        .unwrap();
        assert_eq!(spec.device, None);
        let with = EnvironmentSpec {
            device: Some(Box::new(DeviceChoice {
                id: "vaapi:renderD129".into(),
                kind: DeviceKind::Vaapi,
                render_node: Some("/dev/dri/renderD129".into()),
            })),
            ..spec
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(
            json["device"],
            serde_json::json!({
                "id": "vaapi:renderD129", "kind": "vaapi", "renderNode": "/dev/dri/renderD129"
            })
        );
        assert_eq!(
            serde_json::from_value::<EnvironmentSpec>(json).unwrap(),
            with
        );
    }

    #[test]
    fn home_volumes_are_named_for_docker() {
        let uuid = "01a10527-f79f-761b-962f-4b26924a2e68";
        assert_eq!(
            home_volume_name(uuid, "steam"),
            "cha-home-01a10527-f79f-761b-962f-4b26924a2e68-steam"
        );
        assert_eq!(
            home_volume_name(uuid, "test-pattern"),
            "cha-home-01a10527-f79f-761b-962f-4b26924a2e68-test-pattern"
        );
        // Anything outside Docker's charset becomes `_`.
        let odd = home_volume_name("a b/c:d", "é.x");
        assert_eq!(odd, "cha-home-a_b_c_d-_.x");
        assert!(is_home_volume_name(&odd));
        assert!(is_home_volume_name(&home_volume_name(uuid, "steam")));
    }

    #[test]
    fn a_home_volumes_name_gives_its_ids_back() {
        let uuid = "01a10527-f79f-761b-962f-4b26924a2e68";
        assert_eq!(
            parse_home_volume_name(&home_volume_name(uuid, "test-pattern")),
            Some((uuid, "test-pattern"))
        );
        for bad in [
            "cha-home-",
            "cha-home-u1-steam",
            "cha-home-01a10527-f79f-761b-962f-4b26924a2e68",
            "cha-home-01a10527-f79f-761b-962f-4b26924a2e68-",
            "cha-home-01a10527-f79f-761b-962f-4b26924a2e68-Steam",
            "cha-node_state",
            "cha-home-é",
        ] {
            assert_eq!(parse_home_volume_name(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn only_home_volumes_count_as_homes() {
        assert!(is_home_volume_name("cha-home-u1-steam"));
        assert!(!is_home_volume_name("cha-home-"));
        // The node's own volumes, other containers' and malformed names.
        assert!(!is_home_volume_name("cha-node_state"));
        assert!(!is_home_volume_name("cha-env-e1"));
        assert!(!is_home_volume_name("cha-home-u1/../x"));
        assert!(!is_home_volume_name("cha-home-u1 steam"));
        assert!(!is_home_volume_name("/var/lib/steam"));
        assert!(!is_home_volume_name(""));
    }

    #[test]
    fn media_tokens_verify_only_as_issued() {
        let portal = NodeKey::from_secret([3u8; 32]);
        let claims = MediaClaims {
            env: "e1".into(),
            sub: "u1".into(),
            role: "owner".into(),
            slot: None,
            exp: 1_000,
        };
        let token = sign_media_token(&portal, &claims);
        let key = portal.public_b64();
        assert_eq!(verify_media_token(&key, &token, "e1", 999), Ok(claims));
        // A share's player token carries its slot.
        let player = MediaClaims {
            role: "player".into(),
            slot: Some(2),
            sub: "share:s1".into(),
            ..verify_media_token(&key, &token, "e1", 999).unwrap()
        };
        let token = sign_media_token(&portal, &player);
        assert_eq!(verify_media_token(&key, &token, "e1", 999), Ok(player));
        assert_eq!(
            verify_media_token(&key, &token, "e1", 1_001),
            Err(MediaTokenError::Expired)
        );
        assert_eq!(
            verify_media_token(&key, &token, "e2", 999),
            Err(MediaTokenError::WrongEnvironment)
        );
        let other = NodeKey::from_secret([4u8; 32]).public_b64();
        assert_eq!(
            verify_media_token(&other, &token, "e1", 999),
            Err(MediaTokenError::Signature)
        );
        // A tampered body breaks the signature.
        let (body, sig) = token.split_once('.').unwrap();
        let forged = format!("{}x.{sig}", &body[..body.len() - 1]);
        assert!(verify_media_token(&key, &forged, "e1", 999).is_err());
        assert!(!token.contains(['+', '/', '=']), "URL-safe");
    }
}

// Typed calls to cha-control's `/api`. Same-origin cookies carry the session;
// every mutating call sends JSON (the server's CSRF defence relies on it).

import type { ShareSpec } from "./shares";
import type { UserPrefs } from "./themes";

export type Role = "admin" | "user" | "guest";

export interface User {
  id: string;
  username: string;
  displayName: string;
  role: Role;
  disabled: boolean;
  createdAt: number;
  email: string | null;
  /** Most environments at once; null: the portal's default. */
  maxInstances: number | null;
  /** May launch only on the nodes picked for them (and what is granted). */
  nodeRestricted: boolean;
}

/** Who is behind a "view as" session: the admin who switched. */
export interface Impersonator {
  id: string;
  username: string;
  displayName: string;
}

/** `GET /api/me`: the user, and the admin viewing as them, if any. */
export type Me = User & { impersonator: Impersonator | null };

/** One app on one node an admin lets a user launch, even if the node is off limits. */
export interface Grant {
  id: string;
  nodeId: string;
  templateId: string;
  createdAt: number;
}

/** `GET /api/users/{id}/access` (admin). */
export interface UserAccess {
  nodeRestricted: boolean;
  nodeIds: string[];
  maxInstances: number | null;
  /** The limit in force: maxInstances, else the portal's default. */
  effectiveMax: number;
  /** Environments the user has now. */
  live: number;
  grants: Grant[];
}

export interface UserAccessBody {
  nodeRestricted: boolean;
  nodeIds: string[];
  maxInstances: number | null;
}

/** `GET /api/me/grants`: what was shared with the signed-in user. */
export interface MyGrant {
  id: string;
  nodeId: string;
  nodeName: string;
  online: boolean;
  templateId: string;
}

export interface AuditEntry {
  id: number;
  at: number;
  actorId: string | null;
  action: string;
  target: string | null;
  detail: string | null;
  ip: string | null;
}

export interface Gpu {
  vendor: string;
  name: string;
  memoryMb: number | null;
  driver: string | null;
  renderNode: string | null;
  /** Hardware encoders: "h264", "hevc", "av1". */
  encoders: string[];
}

/** What an environment can run on (`docs/devices.md`). */
export type DeviceKind = "nvidia" | "vaapi" | "cpu";

export interface Device {
  /** Stable on the node: "nvidia:0", "vaapi:renderD129", "cpu". */
  id: string;
  kind: DeviceKind;
  name: string;
  renderNode?: string;
  vendor?: string;
  codecs: string[];
  cores?: number;
}

export interface Inventory {
  hostname: string;
  os: string;
  arch: string;
  cpus: number;
  memoryMb: number;
  gpus: Gpu[];
  addresses: string[];
  /** Absent from nodes that predate devices. */
  devices?: Device[];
  /** Absent from agents that predate disk reporting. */
  disks?: { uses: ("images" | "appData")[]; path: string; totalBytes: number; freeBytes: number }[];
  /** What the node runs on; absent from older agents. */
  platform?: {
    kind: "bare-metal" | "vm" | "lxc" | "wsl" | "docker-desktop" | "unknown";
    detail?: string | null;
    kernel?: string | null;
  } | null;
  /** The agent's image and whether the portal can update it; absent from agents that predate updates. */
  update?: { image: string; updatable: boolean; reason?: string | null } | null;
}

/** How an update of a node's agent is going (the node's own words in `detail`; bytes while pulling). */
export interface AgentUpdateProgress {
  state: "pulling" | "swapping" | "failed" | "rolled-back";
  detail?: string | null;
  done?: number | null;
  total?: number | null;
}

/** A machine that runs environments (named to stay clear of the DOM's `Node`). */
export interface NodeInfo {
  id: string;
  name: string;
  agentVersion: string | null;
  enrolledAt: number;
  lastSeenAt: number | null;
  online: boolean;
  connectedAt: number | null;
  inventory: Inventory | null;
  /** What it uses now; null while offline or when it hasn't reported lately. */
  usage: NodeUsage | null;
  /** The portal's release, when this node's agent is older and can be updated to it. */
  updateTo?: string | null;
  /** Why an older agent can't be updated from here. */
  updateBlocked?: string | null;
  updateProgress?: AgentUpdateProgress | null;
  /** The node's shared directories and whether each is usable; absent from older portals. */
  shared?: NodeSharedDir[];
}

/** What a node's check of a shared directory found. */
export type SharedDirState = "ok" | "missing" | "incomplete" | "unreachable" | "read_only";

/** The check's result, as the node reports it; all absent from older agents. */
export interface SharedDirCheck {
  state?: SharedDirState;
  fsType?: string;
  source?: string;
  detail?: string;
  /** Unix seconds. */
  checkedAt?: number;
}

export interface NodeSharedDir extends SharedDirCheck {
  template: string;
  name: string;
  path: string;
}

/** One node's view of an app's shared directory (admin storage). */
export interface AdminStorageNode extends SharedDirCheck {
  nodeId: string;
  nodeName: string;
  online: boolean;
  /** "external" when set with CHA_SHARED_DIRS (a share), "local" under the data root. */
  location: "external" | "local";
  path: string;
}

/** A node's CPU, RAM and GPU use (percent 0..100, bytes, watts, °C), refreshed every few seconds. */
export interface NodeUsage {
  cpu: number;
  cores: number;
  /** The 1, 5 and 15 minute load averages. */
  load: [number, number, number];
  memUsed: number;
  memTotal: number;
  gpus: GpuUsage[];
  /** Environments it is running. */
  environments: number;
  /** When it was reported (unix seconds). */
  at: number;
}

/** One NVIDIA GPU; a figure the driver didn't give is absent. */
export interface GpuUsage {
  index: number;
  name: string;
  util?: number;
  vramUsed?: number;
  vramTotal?: number;
  /** NVENC and NVDEC utilisation. */
  enc?: number;
  dec?: number;
  temp?: number;
  power?: number;
  powerLimit?: number;
}

export type SecurityProfile = "standard" | "browser" | "steam" | "vm";

/** Something users can launch (`images/catalog.json`). */
/** Portal-wide settings (`/admin/settings`). */
export interface AdminSettings {
  /** Minutes an environment nobody is watching keeps running; 0 turns the shutoff off. */
  idleShutdownMinutes: number;
}

export interface Template {
  id: string;
  name: string;
  description: string;
  image: string;
  /** "browser", "desktop", "test", … */
  class: string;
  /** It has a logo, served at `catalogIconUrl(id)`. */
  icon?: string;
  security: SecurityProfile;
  shmMb: number;
  /** Its display has a fixed size: the page never asks for a resize. */
  fixedSize?: boolean;
  /** It needs real 3D (Steam, games): it never runs on the CPU. */
  needsGpu?: boolean;
  /** What the app shares across users by default, and the parts each user keeps apart. */
  shared?: { access: SharedAccess; perUser: string[] } | null;
  /** On a custom environment only (ADR 0021). */
  custom?: { base: string; shareData: boolean; host: HostOptions | null };
}

/** A device on a node, as a launch names it. */
export interface PlacementChoice {
  node: string;
  device: string;
}

/** One place a template could run, from `GET /api/placements`. */
export interface PlacementOption extends PlacementChoice {
  nodeName: string;
  kind: DeviceKind;
  /** The GPU's name, or "CPU only". */
  label: string;
  allowed: boolean;
  /** Why it isn't allowed, or what to know before choosing it anyway. */
  reason?: string;
  /** Worth knowing, not a problem (e.g. "downloads the image first"). */
  note?: string | null;
  score: number;
}

export interface Placements {
  /** What Launch does; null when nothing is allowed. */
  auto: PlacementChoice | null;
  /** Best first, the allowed ones ahead of the others. */
  options: PlacementOption[];
}

export type EnvironmentState = "starting" | "running" | "stopping" | "destroyed" | "failed";

/** One running environment's use of its node. */
export interface EnvironmentUsage {
  /** Percent of the node's whole CPU. */
  cpu: number;
  /** Bytes of RAM. */
  mem: number;
  /** Bytes of GPU memory; absent when the node can't tell. */
  vram?: number;
  /** The node's RAM, and the memory of the GPU it runs on. */
  memTotal: number;
  vramTotal?: number;
}

/** A starting environment's counted step (an image download). */
export interface EnvironmentProgress {
  done: number;
  total: number;
  /** "bytes". */
  unit: string;
}

export interface Environment {
  id: string;
  templateId: string;
  templateName: string;
  ownerId: string;
  nodeId: string | null;
  nodeName: string | null;
  state: EnvironmentState;
  /** Why it failed or ended. */
  detail: string | null;
  /** How far a download or similar step is, while it starts; null (or absent from older portals) when not counted. */
  progress?: EnvironmentProgress | null;
  /** Something its node noticed while it runs, for the user (null: nothing). */
  warning: string | null;
  /** The last lines its containers logged when it died (its owner and admins; null: none kept). */
  log: string[] | null;
  /** The codecs its device encodes (a CPU one: H.264 only); absent from older portals. */
  codecs?: string[] | null;
  /** The device it runs on; absent from older portals, null when the node doesn't say. */
  device?: { kind: DeviceKind; name: string } | null;
  /** What it uses of its node now; absent while it isn't running or its node doesn't report it. */
  usage?: EnvironmentUsage | null;
  createdAt: number;
  updatedAt: number;
  /** Where its streamer listens while it runs. */
  streamer: { host: string | null; httpPort: number; webrtcPort: number } | null;
  /** The node's ports it asked for (custom environments, ADR 0021), the host's port filled in. */
  ports?: HostPort[] | null;
}

/** How the other users' copies of an app reach the app's shared data. */
export type SharedAccess = "none" | "read" | "write";

/** One app's data setting for the signed-in user (`GET /storage`). */
export interface StorageApp {
  /** The template id, e.g. "steam". */
  template: string;
  name: string;
  /** The user's effective setting. */
  persistent: boolean;
  /** The admin's default. */
  default: boolean;
  sharedAccess: SharedAccess;
  /** The user has an environment of this app running. */
  live: boolean;
  /** Where the node keeps the shared data, when that's outside the data root (a NAS share). */
  sharedPath?: string;
}

export interface StorageInfo {
  /** Where app data lives on the node, e.g. "/srv/cha-portal". */
  root: string;
  apps: StorageApp[];
}

export interface AdminStorageApp {
  template: string;
  name: string;
  defaultPersistent: boolean;
  sharedAccess: SharedAccess;
  /** As in `StorageApp`. */
  sharedPath?: string;
  /** Per node; absent from older portals. */
  nodes?: AdminStorageNode[];
}

export interface AdminStorageInfo {
  root: string;
  apps: AdminStorageApp[];
}

// ---- Loaded catalogs (admin; ADR 0019) ----

export type CatalogSecurity = "standard" | "browser" | "steam" | "vm";

export interface CatalogTemplateView {
  /** Namespaced: `<catalog>.<app>`. */
  id: string;
  app: string;
  name: string;
  description: string;
  image: string;
  imageHost: string;
  class: string;
  security: CatalogSecurity;
  /** A user can launch it. */
  available: boolean;
  unavailableReason: string | null;
  /** An admin approved its `browser`, `steam` or `vm` profile. */
  approved: boolean;
  /** Served at `catalogIconUrl(id)`. */
  hasIcon: boolean;
  iconError: string | null;
}

export interface CatalogView {
  slug: string;
  name: string;
  /** `null` for a pasted catalog. */
  url: string | null;
  /** Unix seconds. */
  addedAt: number;
  fetchedAt: number;
  lastError: string | null;
  lastErrorAt: number | null;
  templates: CatalogTemplateView[];
}

/** A catalog to add: its URL or its document (a JSON object or JSON text). The slug may be left out. */
export type CatalogSource = { slug?: string } & ({ url: string } | { document: unknown });

// ---- Custom environments and host options (admin; ADR 0021) ----

export type NetworkFs = "nfs" | "nfs4" | "cifs";
export type PortProtocol = "tcp" | "udp";
export type HostOptionsMode = "off" | "allowlist" | "full";

export type MountSource =
  | { kind: "named"; name: string }
  | { kind: "path"; path: string }
  | { kind: "network"; fsType: NetworkFs; device: string; options?: string };

export interface HostMount {
  source: MountSource;
  /** Absolute path in the app's container. */
  target: string;
  readOnly?: boolean;
}

export interface HostPort {
  container: number;
  protocol: PortProtocol;
  /** The node's port; absent: the node picks one. */
  host?: number | null;
}

/** What a custom environment asks of its node beyond its template; empty lists may be left out. */
export interface HostOptions {
  mounts?: HostMount[];
  ports?: HostPort[];
  capAdd?: string[];
  devices?: string[];
  privileged?: boolean;
  networkHost?: boolean;
  securityOpt?: string[];
}

export interface AllowedMount {
  name: string;
  readOnly?: boolean;
}

export interface PortRange {
  start: number;
  end: number;
  protocol: PortProtocol;
}

/** What a node allows (`CHA_HOST_OPTIONS`). */
export interface HostPolicy {
  mode: HostOptionsMode;
  mounts?: AllowedMount[];
  ports?: PortRange[];
  caps?: string[];
  devices?: string[];
}

/** One node's host options, from `GET /api/admin/host-options`. */
export interface NodeHostOptions {
  nodeId: string;
  nodeName: string;
  online: boolean;
  /** What the agent understands in a spec ("env", "data-template", "host-options"). */
  specFeatures: string[];
  /** Null: the agent doesn't report one (it predates host options). */
  policy: HostPolicy | null;
}

/** The fields a custom environment may change; absent ones follow the base. */
export interface CustomOverrides {
  name?: string;
  description?: string;
  image?: string;
  class?: string;
  shmMb?: number;
  fps?: Fps;
  gamepad?: PadKind;
  fixedSize?: boolean;
  needsGpu?: boolean;
  persistent?: boolean;
  sharedAccess?: SharedAccess;
  security?: CatalogSecurity;
  env?: Record<string, string>;
}

export interface CustomTemplate {
  /** `custom.<slug>`. */
  id: string;
  slug: string;
  base: string;
  baseName: string;
  shareData: boolean;
  overrides: CustomOverrides;
  host: HostOptions | null;
  hasIcon: boolean;
  /** Why it can't be launched now (its base is gone, …); null: it can. */
  unavailable: string | null;
  createdAt: number;
  updatedAt: number;
}

/** The virtual controller an app sees (docs/controllers.md). */
export type PadKind = "xbox360" | "dualsense" | "steam";

/** One app's virtual controller for the signed-in user (`GET /controllers/apps`). */
export interface ControllerApp {
  /** The template id, e.g. "steam". */
  template: string;
  name: string;
  /** The user's choice; null follows the default. */
  kind: PadKind | null;
  /** What the app gets when the user hasn't chosen. */
  default: PadKind;
}

/** The frame rates an app can run at. */
export type Fps = 60 | 90 | 120;

/** One app's frame rate for the signed-in user (`GET /apps/settings`). */
export interface AppSettings {
  /** The template id, e.g. "steam". */
  template: string;
  name: string;
  /** The user's choice; null follows the default. */
  fps: Fps | null;
  /** What the app gets when the user hasn't chosen. */
  defaultFps: Fps;
}

/** An unclaimed node agent advertising itself on the portal's LAN. */
export interface DiscoveredNode {
  id: string;
  name: string;
  gpu: string;
  fingerprint: string;
  addresses: string[];
  port: number;
  /** Unix ms. */
  lastSeen: number;
}

export interface DiscoveredNodes {
  /** False when the portal's discovery is turned off (`CHA_DISCOVER_NODES=false`). */
  enabled: boolean;
  nodes: DiscoveredNode[];
}

export interface ClaimNodeResult {
  nodeId: string;
  name: string;
}

/** A Moonlight host (Sunshine/Apollo) a node sees on its LAN, not adopted yet. */
export interface FoundMoonlightHost {
  key: string;
  nodeId: string;
  nodeName: string;
  name: string;
  address: string;
  uniqueId: string;
  /** The node is already paired with it. */
  paired: boolean;
}

export interface MoonlightApp {
  /** The template id to launch: `moonlight:<host>:<app>`. */
  templateId: string;
  appId: number;
  name: string;
  hdr: boolean;
}

export interface AdoptedHost {
  id: string;
  name: string;
  nodeId: string;
  nodeName: string;
  online: boolean;
  codecs: string[];
  apps: MoonlightApp[];
  /** When the app list was fetched (unix seconds). */
  appsAt: number | null;
  /** Someone is streaming from it (a host serves one session at a time). */
  busy: { environmentId: string; owner: string } | null;
}

export type AdoptResult =
  | { status: "adopted"; host: AdoptedHost }
  | { status: "pairing"; pin: string; pinUrl: string; hostName: string };

export interface PairingState {
  status: "pairing" | "adopted" | "failed";
  message?: string;
  host?: AdoptedHost;
}

/** An error the API returned: HTTP status, stable code, readable message. */
/** A Moonlight device asking to pair: its PIN is typed in the portal. */
export interface GamestreamPairingRequest {
  id: string;
  nodeId: string;
  nodeName: string;
  deviceName: string;
  address: string;
  /** Unix seconds. */
  expiresAt: number;
}

export interface GamestreamDevice {
  id: string;
  name: string;
  nodeId: string;
  nodeName: string;
  pairedAt: number;
  owner: { id: string; name: string };
}

/** A node with GameStream on: what to add in Moonlight to reach it. */
export interface GamestreamHost {
  nodeId: string;
  nodeName: string;
  address: string;
  httpPort: number;
}

/** A Cha Player install signed in as the user (Settings → Devices). */
export interface PlayerDevice {
  id: string;
  name: string;
  createdAt: number;
  lastUsedAt: number;
  lastIp: string | null;
}

/** A player waiting for approval at `/link`. */
export interface DeviceCodeInfo {
  name: string;
  createdAt: number;
  expiresAt: number;
}

export interface ConnectBody {
  codec: string;
  offer?: RTCSessionDescriptionInit;
  transport?: "webrtc" | "webtransport" | "websocket";
}

export interface ConnectResult {
  codec: string;
  transport: "webrtc" | "webtransport" | "websocket";
  answer?: RTCSessionDescriptionInit;
  /** WebTransport: the streamer's URLs, best first, and its certificate's hash. WebSocket: paths on the portal (`/api/media/<ticket>`), no hash. */
  urls?: string[];
  certHash?: string;
}

/** A live share link of an environment (never its token). */
export interface Share {
  id: string;
  role: "player" | "viewer" | "controller";
  /** A player's pad, 1 to 3: player 2 to 4; null for the other roles. */
  slot: number | null;
  createdAt: number;
  expiresAt: number;
  /** An internet link (ADR 0022): on the tunnel's address. */
  wan: boolean;
}

interface RawShare {
  id: string;
  role: "player" | "viewer" | "controller";
  slot: number | null;
  created_at?: number;
  expires_at: number;
  wan?: boolean;
}

const toShare = (r: RawShare): Share => ({
  id: r.id,
  role: r.role,
  slot: r.slot ?? null,
  createdAt: r.created_at ?? 0,
  expiresAt: r.expires_at,
  wan: r.wan ?? false,
});

/** What a share link is for, for its guest. */
export interface ShareInfo {
  app: string;
  owner: string;
  role: "player" | "viewer" | "controller";
  /** A player's pad, 1 to 3; null for the other roles. */
  slot: number | null;
  state: string;
  /** What the environment's device encodes (null: unknown). */
  codecs: string[] | null;
  /** The link is an internet link (ADR 0022), opened through the tunnel. */
  wan: boolean;
  /** The portal has TURN, so WebRTC may work across the internet. */
  turn: boolean;
}

/** The Cloudflare Tunnel behind internet links (ADR 0022). */
export interface TunnelStatus {
  mode: "off" | "quick" | "named";
  state: "stopped" | "starting" | "up" | "failed";
  url: string | null;
  error: string | null;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

const request = <T>(method: string, path: string, body?: unknown) =>
  requestRaw<T>(method, path, body === undefined ? undefined : JSON.stringify(body), "application/json");

async function requestRaw<T>(method: string, path: string, body: string | undefined, contentType: string): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    credentials: "same-origin",
    headers: body === undefined ? undefined : { "content-type": contentType },
    body,
  });
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    // Not JSON (e.g. a proxy error page); fall through to the status.
  }
  if (!res.ok) {
    const err = (data ?? {}) as { error?: string; message?: string };
    throw new ApiError(res.status, err.error ?? `http_${res.status}`, err.message ?? res.statusText);
  }
  return data as T;
}

export const api = {
  /** A one-use link ticket for Cha Player (60 s); `launch` is a template id. */
  deviceTicket: (launch?: string) =>
    request<{ ticket: string; expires_in: number }>("POST", "/devices/tickets", launch ? { launch } : {}),
  deviceCode: async (userCode: string): Promise<DeviceCodeInfo> => {
    const r = await request<{ name: string; created_at: number; expires_at: number }>(
      "GET",
      `/devices/codes/${encodeURIComponent(userCode)}`,
    );
    return { name: r.name, createdAt: r.created_at, expiresAt: r.expires_at };
  },
  approveDeviceCode: (userCode: string) =>
    request<{ ok: true }>("POST", `/devices/codes/${encodeURIComponent(userCode)}/approve`, {}),
  denyDeviceCode: (userCode: string) =>
    request<{ ok: true }>("POST", `/devices/codes/${encodeURIComponent(userCode)}/deny`, {}),
  playerDevices: async (): Promise<PlayerDevice[]> => {
    const rows = await request<
      { id: string; name: string; created_at: number; last_used_at: number; last_ip: string | null }[]
    >("GET", "/devices");
    return rows.map((d) => ({
      id: d.id,
      name: d.name,
      createdAt: d.created_at,
      lastUsedAt: d.last_used_at,
      lastIp: d.last_ip,
    }));
  },
  revokePlayerDevice: (id: string) => request<null>("DELETE", `/devices/${encodeURIComponent(id)}`),
  setupStatus: () => request<{ needed: boolean; devLogin: boolean }>("GET", "/setup"),
  setup: (body: { username: string; displayName?: string; password: string }) =>
    request<User>("POST", "/setup", body),
  login: (body: { username: string; password: string }) => request<User>("POST", "/auth/login", body),
  devLogin: (username?: string) => request<User>("POST", "/auth/dev-login", username ? { username } : {}),
  devAccounts: () =>
    request<{ accounts: { username: string; displayName: string }[] }>("GET", "/auth/dev-accounts"),
  logout: () => request<null>("POST", "/auth/logout", {}),
  me: () => request<Me>("GET", "/me"),
  switchUser: (userId: string) => request<User>("POST", "/auth/switch", { userId }),
  switchBack: () => request<User>("POST", "/auth/switch-back", {}),
  switchable: () => request<User[]>("GET", "/auth/switchable"),
  myGrants: () => request<MyGrant[]>("GET", "/me/grants"),
  userAccess: (id: string) => request<UserAccess>("GET", `/users/${encodeURIComponent(id)}/access`),
  setUserAccess: (id: string, body: UserAccessBody) =>
    request<unknown>("PUT", `/users/${encodeURIComponent(id)}/access`, body),
  addGrant: (id: string, body: { nodeId: string; templateId: string }) =>
    request<Grant>("POST", `/users/${encodeURIComponent(id)}/grants`, body),
  removeGrant: (id: string, grantId: string) =>
    request<null>("DELETE", `/users/${encodeURIComponent(id)}/grants/${encodeURIComponent(grantId)}`),
  users: () => request<User[]>("GET", "/users"),
  createUser: (body: { email?: string; username?: string; displayName?: string; password: string; role: Role }) =>
    request<User>("POST", "/users", body),
  deleteUser: (id: string) => request<null>("DELETE", `/users/${encodeURIComponent(id)}`),
  audit: () => request<AuditEntry[]>("GET", "/audit"),
  health: () => request<{ status: string; version: string }>("GET", "/health"),
  nodes: () => request<NodeInfo[]>("GET", "/nodes"),
  discoveredNodes: () => request<DiscoveredNodes>("GET", "/nodes/discovered"),
  claimNode: (body: { id: string; code: string }) =>
    request<ClaimNodeResult>("POST", "/nodes/discovered/claim", body),
  createJoinToken: (body: { label?: string }) =>
    request<{ token: string; expiresAt: number }>("POST", "/nodes/join-tokens", body),
  renameNode: (id: string, name: string) =>
    request<{ id: string; name: string }>("PATCH", `/nodes/${encodeURIComponent(id)}`, { name }),
  updateNode: (id: string) =>
    request<{ id: string; updateTo: string }>("POST", `/nodes/${encodeURIComponent(id)}/update`, {}),
  removeNode: (id: string) => request<{ removed: string }>("DELETE", `/nodes/${encodeURIComponent(id)}`),
  gamestreamPairing: () => request<{ requests: GamestreamPairingRequest[] }>("GET", "/gamestream/pairing"),
  gamestreamPair: (id: string, pin: string) =>
    request<{ device: GamestreamDevice }>("POST", `/gamestream/pairing/${encodeURIComponent(id)}`, { pin }),
  gamestreamDevices: () => request<{ devices: GamestreamDevice[] }>("GET", "/gamestream/devices"),
  removeGamestreamDevice: (id: string) => request<null>("DELETE", `/gamestream/devices/${encodeURIComponent(id)}`),
  gamestreamHosts: () => request<{ hosts: GamestreamHost[] }>("GET", "/gamestream/hosts"),
  pingNode: (id: string) => request<{ rttMs: number; nodeUnixMs: number }>("POST", `/nodes/${encodeURIComponent(id)}/ping`, {}),
  foundMoonlightHosts: () => request<{ hosts: FoundMoonlightHost[] }>("GET", "/moonlight/found"),
  adoptMoonlightHost: (key: string) => request<AdoptResult>("POST", "/moonlight/adopt", { key }),
  moonlightPairing: (key: string) => request<PairingState>("GET", `/moonlight/pairing/${encodeURIComponent(key)}`),
  moonlightHosts: () => request<{ hosts: AdoptedHost[] }>("GET", "/moonlight/hosts"),
  refreshMoonlightHost: (id: string) =>
    request<AdoptedHost>("POST", `/moonlight/hosts/${encodeURIComponent(id)}/refresh`, {}),
  removeMoonlightHost: (id: string) => request<null>("DELETE", `/moonlight/hosts/${encodeURIComponent(id)}`),
  catalog: () => request<Template[]>("GET", "/catalog"),
  environments: () => request<Environment[]>("GET", "/environments"),
  /** Without a choice, the server picks the best place (what `placements` calls `auto`). */
  launch: (templateId: string, choice?: { node: string; device?: string }) =>
    request<Environment>("POST", "/environments", { templateId, ...choice }),
  /** Where every template could run, with live node usage. 404 until the server supports it. */
  placements: () => request<{ templates: Record<string, Placements> }>("GET", "/placements"),
  stopEnvironment: (id: string) => request<Environment>("DELETE", `/environments/${encodeURIComponent(id)}`),
  environment: (id: string) => request<Environment>("GET", `/environments/${encodeURIComponent(id)}`),
  /** Per-app data settings for the signed-in user. 404 until the server supports them. */
  storage: () => request<StorageInfo>("GET", "/storage"),
  /** 409 `live` while the user's environment of this app runs. */
  setStoragePersistent: (template: string, persistent: boolean) =>
    request<StorageApp>("PUT", `/storage/${encodeURIComponent(template)}`, { persistent }),
  /** Deletes the user's saved data for the app: 409 while it runs, 502 if the node failed. */
  resetStorage: (template: string) =>
    request<unknown>("POST", `/storage/${encodeURIComponent(template)}/reset`, {}),
  /** Which virtual controller each app gets. 404 until the server supports it. */
  controllerApps: () => request<{ apps: ControllerApp[] }>("GET", "/controllers/apps"),
  /** `null` goes back to the app's default. Applies from the app's next launch. */
  setControllerKind: (template: string, kind: PadKind | null) =>
    request<unknown>("PUT", `/controllers/apps/${encodeURIComponent(template)}`, { kind }),
  /** Each app's frame rate. 404 until the server supports it. */
  appSettings: () => request<{ apps: AppSettings[] }>("GET", "/apps/settings"),
  /** `null` goes back to the app's default. Applies from the app's next launch. */
  setAppFps: (template: string, fps: Fps | null) =>
    request<unknown>("PUT", `/apps/settings/${encodeURIComponent(template)}`, { fps }),
  /** The signed-in user's saved interface preferences (empty until they save any). 404 on an older server. */
  prefs: () => request<{ prefs: Partial<UserPrefs> }>("GET", "/me/prefs"),
  /** Replaces the stored preferences whole. */
  setPrefs: (prefs: Partial<UserPrefs>) => request<{ prefs: Partial<UserPrefs> }>("PUT", "/me/prefs", { prefs }),
  adminSettings: () => request<AdminSettings>("GET", "/admin/settings"),
  setAdminSettings: (body: AdminSettings) => request<AdminSettings>("PUT", "/admin/settings", body),
  adminStorage: () => request<AdminStorageInfo>("GET", "/admin/storage"),
  setAdminStorage: (template: string, body: { defaultPersistent?: boolean; sharedAccess?: SharedAccess }) =>
    request<AdminStorageApp>("PUT", `/admin/storage/${encodeURIComponent(template)}`, body),
  // Loaded catalogs (admin; ADR 0019). Errors carry the server's `message` as is.
  adminCatalogs: () => request<{ catalogs: CatalogView[] }>("GET", "/admin/catalogs"),
  addCatalog: (source: CatalogSource) => request<CatalogView>("POST", "/admin/catalogs", source),
  /** A URL catalog refetches; a pasted one needs the new `document` (else 400 `no_url`). */
  refreshCatalog: (slug: string, document?: unknown) =>
    request<CatalogView>(
      "POST",
      `/admin/catalogs/${encodeURIComponent(slug)}/refresh`,
      document === undefined ? {} : { document },
    ),
  approveCatalogTemplate: (slug: string, app: string, approved: boolean) =>
    request<CatalogView>(
      "PUT",
      `/admin/catalogs/${encodeURIComponent(slug)}/templates/${encodeURIComponent(app)}/approval`,
      { approved },
    ),
  /** 409 `in_use` while an environment of one of its apps is live. */
  removeCatalog: (slug: string) => request<{ removed: string }>("DELETE", `/admin/catalogs/${encodeURIComponent(slug)}`),
  // Custom environments (admin; ADR 0021). Errors carry the server's `message`; 400 names the field, 409 is a conflict.
  customTemplates: () => request<CustomTemplate[]>("GET", "/admin/custom-templates"),
  createCustomTemplate: (body: {
    slug: string;
    base: string;
    shareData: boolean;
    overrides: CustomOverrides;
    host?: HostOptions | null;
  }) => request<CustomTemplate>("POST", "/admin/custom-templates", body),
  /** 409 while the environment (or the data it shares) is live and `shareData` changes. */
  updateCustomTemplate: (id: string, body: { overrides: CustomOverrides; host?: HostOptions | null; shareData: boolean }) =>
    request<CustomTemplate>("PUT", `/admin/custom-templates/${encodeURIComponent(id)}`, body),
  /** 409 while it is live. */
  deleteCustomTemplate: (id: string) => request<null>("DELETE", `/admin/custom-templates/${encodeURIComponent(id)}`),
  /** An SVG, sent as the body. */
  setCustomIcon: (id: string, svg: string) =>
    requestRaw<null>("PUT", `/admin/custom-templates/${encodeURIComponent(id)}/icon`, svg, "image/svg+xml"),
  deleteCustomIcon: (id: string) => request<null>("DELETE", `/admin/custom-templates/${encodeURIComponent(id)}/icon`),
  /** Each node's host-options policy. */
  hostOptions: () => request<NodeHostOptions[]>("GET", "/admin/host-options"),
  /** STUN and TURN for the next connection (TURN credentials last a day). */
  iceServers: () => request<{ iceServers: RTCIceServer[] }>("GET", "/ice"),
  /** Brokers a WebRTC connection to the environment's streamer. */
  connect: (id: string, body: ConnectBody) =>
    request<ConnectResult>("POST", `/environments/${encodeURIComponent(id)}/connect`, body),

  // Share links (ADRs 0014 and 0015).
  shares: async (environmentId: string): Promise<Share[]> => {
    const rows = await request<RawShare[]>("GET", `/environments/${encodeURIComponent(environmentId)}/shares`);
    return rows.map(toShare);
  },
  /** Makes a player link (with a slot, 1 to 3), a viewer link or a controller link; the full `url` comes back only now (absolute for an internet link, which opens the tunnel first). */
  createShare: async (environmentId: string, spec: ShareSpec): Promise<Share & { url: string }> => {
    const r = await request<RawShare & { url: string }>("POST", `/environments/${encodeURIComponent(environmentId)}/shares`, spec);
    return { ...toShare(r), url: r.url };
  },
  revokeShare: (environmentId: string, shareId: string) =>
    request<null>("DELETE", `/environments/${encodeURIComponent(environmentId)}/shares/${encodeURIComponent(shareId)}`),
  /** What a link is for (no sign-in); 404 when it is unknown, revoked, expired or its game stopped. */
  shareInfo: (token: string) => request<ShareInfo>("GET", `/shares/${encodeURIComponent(token)}`),
  /** Brokers a guest's connection (no sign-in), like `connect`. */
  shareConnect: (token: string, body: ConnectBody) =>
    request<ConnectResult>("POST", `/shares/${encodeURIComponent(token)}/connect`, body),
  /** STUN and TURN for a guest's connection (no sign-in). */
  shareIce: (token: string) => request<{ iceServers: RTCIceServer[] }>("GET", `/shares/${encodeURIComponent(token)}/ice`),
  /** The tunnel internet links use; `mode` is "off" when the feature is. */
  tunnel: () => request<TunnelStatus>("GET", "/tunnel"),
};

/** A catalog template's logo (only for templates with `icon`). */
export const catalogIconUrl = (id: string) => `/api/catalog/${encodeURIComponent(id)}/icon`;

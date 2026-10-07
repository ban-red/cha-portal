// Typed calls to cha-control's `/api`. Same-origin cookies carry the session;
// every mutating call sends JSON (the server's CSRF defence relies on it).

import type { UserPrefs } from "./themes";

export type Role = "admin" | "user" | "guest";

export interface User {
  id: string;
  username: string;
  displayName: string;
  role: Role;
  disabled: boolean;
  createdAt: number;
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

export type SecurityProfile = "standard" | "browser";

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
}

export interface AdminStorageInfo {
  root: string;
  apps: AdminStorageApp[];
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
export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`/api${path}`, {
    method,
    credentials: "same-origin",
    headers: body === undefined ? undefined : { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
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
  setupStatus: () => request<{ needed: boolean; devLogin: boolean }>("GET", "/setup"),
  setup: (body: { username: string; displayName?: string; password: string }) =>
    request<User>("POST", "/setup", body),
  login: (body: { username: string; password: string }) => request<User>("POST", "/auth/login", body),
  devLogin: (username?: string) => request<User>("POST", "/auth/dev-login", username ? { username } : {}),
  devAccounts: () =>
    request<{ accounts: { username: string; displayName: string }[] }>("GET", "/auth/dev-accounts"),
  logout: () => request<null>("POST", "/auth/logout", {}),
  me: () => request<User>("GET", "/me"),
  users: () => request<User[]>("GET", "/users"),
  createUser: (body: { username: string; displayName?: string; password: string; role: Role }) =>
    request<User>("POST", "/users", body),
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
  removeNode: (id: string) => request<{ removed: string }>("DELETE", `/nodes/${encodeURIComponent(id)}`),
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
  launch: (templateId: string, choice?: PlacementChoice) =>
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
  /** STUN and TURN for the next connection (TURN credentials last a day). */
  iceServers: () => request<{ iceServers: RTCIceServer[] }>("GET", "/ice"),
  /** Brokers a WebRTC connection to the environment's streamer. */
  connect: (
    id: string,
    body: { codec: string; offer?: RTCSessionDescriptionInit; transport?: "webrtc" | "webtransport" },
  ) =>
    request<{
      codec: string;
      transport: "webrtc" | "webtransport";
      answer?: RTCSessionDescriptionInit;
      /** WebTransport: the streamer's URLs, best first, and its certificate's hash. */
      urls?: string[];
      certHash?: string;
    }>(
      "POST",
      `/environments/${encodeURIComponent(id)}/connect`,
      body,
    ),
};

/** A catalog template's logo (only for templates with `icon`). */
export const catalogIconUrl = (id: string) => `/api/catalog/${encodeURIComponent(id)}/icon`;

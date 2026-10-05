// Typed calls to cha-control's `/api`. Same-origin cookies carry the session;
// every mutating call sends JSON (the server's CSRF defence relies on it).

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

export interface Inventory {
  hostname: string;
  os: string;
  arch: string;
  cpus: number;
  memoryMb: number;
  gpus: Gpu[];
  addresses: string[];
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
}

export type SecurityProfile = "standard" | "browser";

/** Something users can launch (`images/catalog.json`). */
export interface Template {
  id: string;
  name: string;
  description: string;
  image: string;
  /** "browser", "desktop", "test", … */
  class: string;
  security: SecurityProfile;
  shmMb: number;
  /** Its display has a fixed size: the page never asks for a resize. */
  fixedSize?: boolean;
  /** What the app shares across users by default, and the parts each user keeps apart. */
  shared?: { access: SharedAccess; perUser: string[] } | null;
}

export type EnvironmentState = "starting" | "running" | "stopping" | "destroyed" | "failed";

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
  setup: (body: { token: string; username: string; displayName?: string; password: string }) =>
    request<User>("POST", "/setup", body),
  login: (body: { username: string; password: string }) => request<User>("POST", "/auth/login", body),
  devLogin: () => request<User>("POST", "/auth/dev-login", {}),
  logout: () => request<null>("POST", "/auth/logout", {}),
  me: () => request<User>("GET", "/me"),
  users: () => request<User[]>("GET", "/users"),
  createUser: (body: { username: string; displayName?: string; password: string; role: Role }) =>
    request<User>("POST", "/users", body),
  audit: () => request<AuditEntry[]>("GET", "/audit"),
  nodes: () => request<NodeInfo[]>("GET", "/nodes"),
  createJoinToken: (body: { label?: string }) =>
    request<{ token: string; expiresAt: number }>("POST", "/nodes/join-tokens", body),
  removeNode: (id: string) => request<{ removed: string }>("DELETE", `/nodes/${encodeURIComponent(id)}`),
  pingNode: (id: string) => request<{ rttMs: number; nodeUnixMs: number }>("POST", `/nodes/${encodeURIComponent(id)}/ping`, {}),
  catalog: () => request<Template[]>("GET", "/catalog"),
  environments: () => request<Environment[]>("GET", "/environments"),
  launch: (templateId: string) => request<Environment>("POST", "/environments", { templateId }),
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

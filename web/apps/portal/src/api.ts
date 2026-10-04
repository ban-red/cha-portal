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
  createdAt: number;
  updatedAt: number;
  /** Where its streamer listens while it runs. */
  streamer: { host: string | null; httpPort: number; webrtcPort: number } | null;
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
  setupStatus: () => request<{ needed: boolean }>("GET", "/setup"),
  setup: (body: { token: string; username: string; displayName?: string; password: string }) =>
    request<User>("POST", "/setup", body),
  login: (body: { username: string; password: string }) => request<User>("POST", "/auth/login", body),
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
  /** STUN and TURN for the next connection (TURN credentials last a day). */
  iceServers: () => request<{ iceServers: RTCIceServer[] }>("GET", "/ice"),
  /** Brokers a WebRTC connection to the environment's streamer. */
  connect: (id: string, body: { codec: string; offer: RTCSessionDescriptionInit }) =>
    request<{ answer: RTCSessionDescriptionInit; codec: string }>(
      "POST",
      `/environments/${encodeURIComponent(id)}/connect`,
      body,
    ),
};

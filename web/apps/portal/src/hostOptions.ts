// Host options of a custom environment (ADR 0021): the same checks the Rust side runs
// (`crates/cha-wire/src/host.rs`), so the editor can say, per node, whether it would accept
// the options, before anything is saved. The server stays authoritative.

import type {
  HostMount,
  HostOptions,
  HostOptionsMode,
  HostPolicy,
  NodeHostOptions,
  PortProtocol,
  PortRange,
} from "./api";

export const SPEC_FEATURE_ENV = "env";
export const SPEC_FEATURE_HOST_OPTIONS = "host-options";

/** Paths in the app's container the agent mounts itself (`RESERVED_TARGETS`). */
export const RESERVED_TARGETS = ["/run/cha", "/dev/input", "/home/cha", "/srv/cha-portal/shared"] as const;
const DOCKER_SOCKETS = ["/var/run/docker.sock", "/run/docker.sock"] as const;

export const MODE_LABEL: Record<HostOptionsMode, string> = { off: "Off", allowlist: "Allowlist", full: "Full" };

export function validMountName(name: string): boolean {
  return name.length > 0 && name.length <= 32 && /^[a-z0-9_-]+$/.test(name);
}

export function validCapName(cap: string): boolean {
  return cap.length > 0 && cap.length <= 32 && !cap.startsWith("CAP_") && /^[A-Z0-9_]+$/.test(cap);
}

export function validAbsPath(path: string): boolean {
  if (!path.startsWith("/") || path.length > 1024 || /[\0,:]/.test(path)) return false;
  return path === "/" || path.slice(1).split("/").every((p) => p !== "" && p !== "." && p !== "..");
}

/** `a` is `b` or under it. */
function under(a: string, b: string): boolean {
  return b === "/" || a === b || a.startsWith(b + "/");
}

/** No options asked for at all. */
export function isEmptyHost(h: HostOptions | null | undefined): boolean {
  return (
    !h ||
    (!h.mounts?.length &&
      !h.ports?.length &&
      !h.capAdd?.length &&
      !h.devices?.length &&
      !h.privileged &&
      !h.networkHost &&
      !h.securityOpt?.length)
  );
}

/** What to send: `null` for nothing, otherwise the options without empty lists or false flags. */
export function normalizeHost(h: HostOptions | null | undefined): HostOptions | null {
  if (isEmptyHost(h) || !h) return null;
  const out: HostOptions = {};
  if (h.mounts?.length) out.mounts = h.mounts.map((m) => ({ ...m, readOnly: !!m.readOnly }));
  if (h.ports?.length) out.ports = h.ports.map((p) => (p.host ? { ...p } : { container: p.container, protocol: p.protocol }));
  if (h.capAdd?.length) out.capAdd = [...h.capAdd];
  if (h.devices?.length) out.devices = [...h.devices];
  if (h.privileged) out.privileged = true;
  if (h.networkHost) out.networkHost = true;
  if (h.securityOpt?.length) out.securityOpt = [...h.securityOpt];
  return out;
}

/** Whether the request is well-formed whatever any node allows (`HostOptions::check_shape`); the first problem, or null. */
export function checkShape(h: HostOptions | null | undefined): string | null {
  if (!h) return null;
  const targets: string[] = [];
  for (const m of h.mounts ?? []) {
    if (!validAbsPath(m.target) || m.target === "/") return `${JSON.stringify(m.target)} isn't an absolute path to mount at`;
    const reserved = RESERVED_TARGETS.find((r) => under(m.target, r) || under(r, m.target));
    if (reserved) return `${m.target} would cover ${reserved}, which the node mounts itself`;
    if (m.target.startsWith("/dev/hidraw")) return `${m.target} is where the node puts controllers`;
    const clash = targets.find((t) => under(m.target, t) || under(t, m.target));
    if (clash) return `${m.target} and ${clash} overlap`;
    targets.push(m.target);
    const s = m.source;
    if (s.kind === "named") {
      if (!validMountName(s.name)) return `${JSON.stringify(s.name)} isn't a mount name`;
    } else if (s.kind === "path") {
      if (!validAbsPath(s.path)) return `${JSON.stringify(s.path)} isn't an absolute host path`;
      if (DOCKER_SOCKETS.some((d) => under(d, s.path))) return `${s.path} holds the node's Docker socket`;
    } else {
      if (!s.device || /[\0,]/.test(s.device)) return `${JSON.stringify(s.device)} isn't a share to mount`;
      if ((s.options ?? "").includes("\0")) return "the share's options hold a NUL byte";
    }
  }
  const seen = new Set<string>();
  for (const p of h.ports ?? []) {
    if (!Number.isInteger(p.container) || p.container < 1 || p.container > 65535 || p.host === 0) return "port 0 isn't a port";
    if (p.host != null && (!Number.isInteger(p.host) || p.host < 1 || p.host > 65535)) return `${p.host} isn't a port`;
    const key = `${p.container}/${p.protocol}`;
    if (seen.has(key)) return `${key} is listed twice`;
    seen.add(key);
  }
  if (h.networkHost && h.ports?.length) return "ports mean nothing on the host's network";
  for (const cap of h.capAdd ?? []) {
    if (!validCapName(cap)) return `${JSON.stringify(cap)} isn't a capability (SYS_NICE, without CAP_)`;
  }
  for (const dev of h.devices ?? []) {
    if (!validAbsPath(dev) || !dev.startsWith("/dev/")) return `${JSON.stringify(dev)} isn't a device under /dev`;
  }
  for (const opt of h.securityOpt ?? []) {
    if (!opt || opt.includes("\0")) return `${JSON.stringify(opt)} isn't a security option`;
  }
  return null;
}

const rangeContains = (r: PortRange, port: number, protocol: PortProtocol) =>
  r.protocol === protocol && port >= r.start && port <= r.end;

/** Every reason this node refuses the request; empty when it allows it (`HostPolicy::refusals`). */
export function refusals(policy: HostPolicy, req: HostOptions | null | undefined): string[] {
  if (!req || isEmptyHost(req)) return [];
  const out: string[] = [];
  if (policy.mode === "off") {
    out.push("allows no host options");
  } else if (policy.mode === "allowlist") {
    for (const m of req.mounts ?? []) {
      const s = m.source;
      if (s.kind === "named") {
        if (!(policy.mounts ?? []).some((a) => a.name === s.name)) out.push(`has no mount named ${s.name}`);
      } else if (s.kind === "path") {
        out.push(`allows host paths (${s.path}) only in full mode`);
      } else {
        out.push(`allows network shares (${s.device}) only in full mode`);
      }
    }
    for (const p of req.ports ?? []) {
      const ranges = policy.ports ?? [];
      const ok = p.host ? ranges.some((r) => rangeContains(r, p.host!, p.protocol)) : ranges.some((r) => r.protocol === p.protocol);
      if (!ok) out.push(`doesn't allow ${p.host ? `${p.host}/${p.protocol}` : `${p.protocol} ports`}`);
    }
    for (const cap of req.capAdd ?? []) if (!(policy.caps ?? []).includes(cap)) out.push(`doesn't allow ${cap}`);
    for (const dev of req.devices ?? []) if (!(policy.devices ?? []).includes(dev)) out.push(`doesn't allow ${dev}`);
    if (req.privileged) out.push("allows privileged only in full mode");
    if (req.networkHost) out.push("allows the host's network only in full mode");
    if (req.securityOpt?.length) out.push("allows security options only in full mode");
  }
  return out;
}

export const allows = (policy: HostPolicy, req: HostOptions | null | undefined) => refusals(policy, req).length === 0;

// ---- Across nodes -------------------------------------------------------------------------

export interface NodeVerdict {
  nodeId: string;
  nodeName: string;
  online: boolean;
  mode: HostOptionsMode | null;
  /** Empty: the node would accept it. */
  reasons: string[];
}

/**
 * Per node: would it accept this custom environment? `needsEnv` is true when the environment sets
 * variables. A node whose agent predates host options (no policy) refuses any option.
 */
export function nodeVerdicts(nodes: NodeHostOptions[], req: HostOptions | null | undefined, needsEnv = false): NodeVerdict[] {
  return nodes.map((n) => {
    const reasons: string[] = [];
    if (!isEmptyHost(req)) {
      if (!n.policy) reasons.push("doesn't report host options (an older agent)");
      else {
        reasons.push(...refusals(n.policy, req));
        if (!n.specFeatures.includes(SPEC_FEATURE_HOST_OPTIONS)) reasons.push("is an agent that doesn't read host options yet");
      }
    }
    if (needsEnv && !n.specFeatures.includes(SPEC_FEATURE_ENV)) reasons.push("is an agent that doesn't pass variables yet");
    return { nodeId: n.nodeId, nodeName: n.nodeName, online: n.online, mode: n.policy?.mode ?? null, reasons };
  });
}

/** No node would accept it (and there are nodes to ask). */
export const noNodeAccepts = (verdicts: NodeVerdict[]) => verdicts.length > 0 && verdicts.every((v) => v.reasons.length > 0);

export interface MountChoice {
  name: string;
  /** Nodes that allow it read-only only. */
  readOnlyOn: string[];
  /** Nodes that have a mount of this name. */
  nodes: string[];
}

/** The union of the mount names across the nodes that allow any (allowlist or full). */
export function mountChoices(nodes: NodeHostOptions[]): MountChoice[] {
  const by = new Map<string, MountChoice>();
  for (const n of nodes) {
    if (!n.policy || n.policy.mode === "off") continue;
    for (const m of n.policy.mounts ?? []) {
      const c = by.get(m.name) ?? { name: m.name, readOnlyOn: [], nodes: [] };
      c.nodes.push(n.nodeName);
      if (m.readOnly) c.readOnlyOn.push(n.nodeName);
      by.set(m.name, c);
    }
  }
  return [...by.values()].sort((a, b) => a.name.localeCompare(b.name));
}

export const anyFull = (nodes: NodeHostOptions[]) => nodes.some((n) => n.policy?.mode === "full");

/** Capabilities and devices some allowlist or full node names, sorted. */
export function allowedChoices(nodes: NodeHostOptions[], key: "caps" | "devices"): string[] {
  const set = new Set<string>();
  for (const n of nodes) if (n.policy && n.policy.mode !== "off") for (const v of n.policy[key] ?? []) set.add(v);
  return [...set].sort();
}

/** "27015-27030/udp, 25565/tcp" across the nodes that allow ports; the empty string when none do. */
export function portHint(nodes: NodeHostOptions[]): string {
  const seen = new Set<string>();
  for (const n of nodes) {
    if (!n.policy || n.policy.mode !== "allowlist") continue;
    for (const r of n.policy.ports ?? []) seen.add(rangeText(r));
  }
  return [...seen].join(", ");
}

export const rangeText = (r: PortRange) => `${r.start === r.end ? r.start : `${r.start}-${r.end}`}/${r.protocol}`;

/** A one-line summary of what a node allows. */
export function policySummary(p: HostPolicy | null): string {
  if (!p) return "Not reported";
  if (p.mode === "off") return "Off";
  if (p.mode === "full") return "Full: anything, host paths and privileged included";
  const parts = [
    count(p.mounts?.length ?? 0, "mount"),
    count(p.ports?.length ?? 0, "port range"),
    count(p.caps?.length ?? 0, "capability", "capabilities"),
    count(p.devices?.length ?? 0, "device"),
  ].filter(Boolean);
  return parts.length ? `Allowlist: ${parts.join(", ")}` : "Allowlist: nothing named yet";
}

function count(n: number, one: string, many = `${one}s`): string {
  return n ? `${n} ${n === 1 ? one : many}` : "";
}

/** A mount's source in one phrase, for lists. */
export function mountText(m: HostMount): string {
  const s = m.source;
  const from = s.kind === "named" ? s.name : s.kind === "path" ? s.path : `${s.fsType}:${s.device}`;
  return `${from} → ${m.target}${m.readOnly ? " (read-only)" : ""}`;
}

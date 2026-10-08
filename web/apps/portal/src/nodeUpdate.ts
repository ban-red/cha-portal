// What a node card says about updating the node's agent (ADR 0018): the offer,
// the reason there is none, the progress, and how it ended.
import type { EnvironmentProgress, NodeInfo } from "./api";

export interface UpdateLine {
  /** `text-*` role of the line. */
  tone: "info" | "muted" | "warn" | "ok";
  text: string;
  /** The counted download, for the launch progress bar. */
  progress: EnvironmentProgress | null;
  /** The button, if any. */
  action: "update" | "retry" | null;
}

/** An update is going on: the page polls faster. */
export function updateRunning(node: Pick<NodeInfo, "updateProgress">): boolean {
  const state = node.updateProgress?.state;
  return state === "pulling" || state === "swapping";
}

/** The version a running update is moving the node to, to recognise its arrival. */
export function expectedVersion(node: Pick<NodeInfo, "updateProgress" | "updateTo">): string | null {
  return updateRunning(node) ? (node.updateTo ?? null) : null;
}

/** The node's line for updates; `updatedTo` is the version it just arrived at. Null: say nothing. */
export function updateLine(
  node: Pick<NodeInfo, "agentVersion" | "online" | "updateTo" | "updateBlocked" | "updateProgress">,
  updatedTo: string | null,
): UpdateLine | null {
  const p = node.updateProgress;
  if (p?.state === "pulling") {
    const counted = !!p.total && p.total > 0;
    return {
      tone: "info",
      text: counted ? "Downloading the new agent" : "Downloading the new agent…",
      progress: counted ? { done: p.done ?? 0, total: p.total ?? 0, unit: "bytes" } : null,
      action: null,
    };
  }
  if (p?.state === "swapping") {
    return { tone: "info", text: "Restarting the agent…", progress: null, action: null };
  }
  if (p) {
    const why = p.detail?.trim();
    const head =
      p.state === "rolled-back"
        ? "The new agent didn't connect, so the previous one is running again"
        : "The update failed";
    return { tone: "warn", text: why ? `${head}: ${why}` : head, progress: null, action: node.online ? "retry" : null };
  }
  if (updatedTo) return { tone: "ok", text: `Updated to v${updatedTo}`, progress: null, action: null };
  if (node.updateTo) {
    const from = node.agentVersion ? `v${node.agentVersion} → ` : "";
    return { tone: "info", text: `Update available: ${from}v${node.updateTo}`, progress: null, action: "update" };
  }
  if (node.updateBlocked) return { tone: "muted", text: node.updateBlocked, progress: null, action: null };
  return null;
}

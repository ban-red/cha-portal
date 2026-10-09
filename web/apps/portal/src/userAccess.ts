import type { Grant, NodeInfo, Template, UserAccess, UserAccessBody } from "./api";

/** The access form as typed: the limit stays text so an empty box means "default". */
export interface AccessForm {
  nodeRestricted: boolean;
  nodeIds: string[];
  maxInstances: string;
}

export const MAX_INSTANCES_LIMIT = 64;

export function formFromAccess(a: Pick<UserAccess, "nodeRestricted" | "nodeIds" | "maxInstances">): AccessForm {
  return {
    nodeRestricted: a.nodeRestricted,
    nodeIds: [...a.nodeIds].sort(),
    maxInstances: a.maxInstances === null ? "" : String(a.maxInstances),
  };
}

/** The limit typed: null for empty (the default), a whole number 1..=64, or why not. */
export function parseMaxInstances(text: string): { ok: true; value: number | null } | { ok: false; error: string } {
  const t = text.trim();
  if (!t) return { ok: true, value: null };
  if (!/^\d+$/.test(t)) return { ok: false, error: `Enter a whole number from 1 to ${MAX_INSTANCES_LIMIT}, or leave it empty.` };
  const n = Number(t);
  if (n < 1 || n > MAX_INSTANCES_LIMIT) return { ok: false, error: `The limit is from 1 to ${MAX_INSTANCES_LIMIT}.` };
  return { ok: true, value: n };
}

/** The request body for a form, or the error to show. */
export function accessBody(form: AccessForm): { ok: true; body: UserAccessBody } | { ok: false; error: string } {
  const max = parseMaxInstances(form.maxInstances);
  if (!max.ok) return max;
  return { ok: true, body: { nodeRestricted: form.nodeRestricted, nodeIds: [...form.nodeIds].sort(), maxInstances: max.value } };
}

/** The form differs from what the server has (so Save does something). */
export function accessChanged(form: AccessForm, saved: AccessForm): boolean {
  const a = [...form.nodeIds].sort();
  const b = [...saved.nodeIds].sort();
  return (
    form.nodeRestricted !== saved.nodeRestricted ||
    form.maxInstances.trim() !== saved.maxInstances.trim() ||
    a.length !== b.length ||
    a.some((id, i) => id !== b[i])
  );
}

/** Tick or untick a node. */
export function toggleNode(ids: readonly string[], id: string, on: boolean): string[] {
  const without = ids.filter((x) => x !== id);
  return on ? [...without, id] : without;
}

/** "Steam on Garage" for a grant; falls back to the raw ids when a node or app is gone. */
export function grantLabel(g: Pick<Grant, "nodeId" | "templateId">, nodes: readonly Pick<NodeInfo, "id" | "name">[], templates: readonly Pick<Template, "id" | "name">[]) {
  const node = nodes.find((n) => n.id === g.nodeId)?.name ?? "a removed node";
  const app = templates.find((t) => t.id === g.templateId)?.name ?? g.templateId;
  return { app, node, text: `${app} on ${node}` };
}

/** Whether a (node, template) grant is already in the list. */
export const hasGrant = (grants: readonly Grant[], nodeId: string, templateId: string) =>
  grants.some((g) => g.nodeId === nodeId && g.templateId === templateId);

/** "2 of 4 environments running" under the limit box. */
export function limitHint(a: Pick<UserAccess, "effectiveMax" | "live">): string {
  return `${a.live} of ${a.effectiveMax} ${a.effectiveMax === 1 ? "environment" : "environments"} in use now.`;
}

import { useQuery } from "@tanstack/vue-query";
import { computed, type Ref } from "vue";

import { api, type DeviceKind, type PlacementOption, type Placements } from "./api";
import { notAvailable } from "./storage";

// Where each app could run, and what Launch would pick (`docs/devices.md`).
// The scores follow the nodes' live usage, so this refreshes while the page is
// visible. Used by the app cards.

export const PLACEMENTS_KEY = ["placements"] as const;
const REFRESH_MS = 10_000;

export const KIND_LABEL: Record<DeviceKind, string> = { nvidia: "NVIDIA", vaapi: "VA-API", cpu: "CPU" };

export function usePlacements(enabled: Ref<boolean>) {
  const query = useQuery({
    queryKey: PLACEMENTS_KEY,
    queryFn: api.placements,
    enabled,
    staleTime: 5_000,
    // Paused while the tab is in the background.
    refetchInterval: REFRESH_MS,
    retry: (count, err) => !notAvailable(err) && count < 2,
  });
  // An older server has no such call: the cards launch as they always did.
  const missing = computed(() => notAvailable(query.error.value));
  const byTemplate = computed(() => query.data.value?.templates ?? {});
  return { query, missing, byTemplate };
}

/** The option Launch would pick, if there is one. */
export function autoOption(p: Placements | undefined): PlacementOption | null {
  if (!p?.auto) return null;
  const { node, device } = p.auto;
  return p.options.find((o) => o.node === node && o.device === device) ?? null;
}

/** "gpu-node · RTX 4090", or "gpu-node · CPU only". */
export const describe = (o: PlacementOption) => `${o.nodeName} · ${o.label}`;

/** Why nothing can run the app, when that's so: what stood in the way of the best device. */
export function nowhereToRun(p: Placements): string {
  const first = p.options[0];
  return first ? `${describe(first)}: ${first.reason ?? "not available"}` : "No node is online.";
}


<script setup lang="ts">
// The header's health bar (admins): is the portal answering, how many nodes are online, what
// the busiest GPU is doing, and which Moonlight hosts are down. Each chip is a dot (green fine,
// amber worth a look, red down) and a short figure; the whole bar links to the Nodes page.
import { useQuery } from "@tanstack/vue-query";
import { computed } from "vue";

import { api } from "../api";

type Tone = "ok" | "warn" | "bad";
interface Chip {
  id: string;
  label: string;
  value: string;
  tone: Tone;
  title: string;
}

const POLL = { refetchInterval: 5000, refetchIntervalInBackground: false } as const;

// The portal itself: how long its own health call takes, and whether it answers at all.
const portal = useQuery({
  queryKey: ["health-bar", "portal"],
  queryFn: async () => {
    const started = performance.now();
    const h = await api.health();
    return { ms: performance.now() - started, version: h.version };
  },
  ...POLL,
  retry: false,
});
const nodes = useQuery({ queryKey: ["nodes"], queryFn: api.nodes, ...POLL });
const hosts = useQuery({ queryKey: ["moonlight-hosts"], queryFn: api.moonlightHosts, ...POLL });

const pct = (used: number, total: number) => (total > 0 ? (used / total) * 100 : 0);
const toneOf = (v: number, warn = 80, bad = 95): Tone => (v >= bad ? "bad" : v >= warn ? "warn" : "ok");
const worst = (tones: Tone[]): Tone => (tones.includes("bad") ? "bad" : tones.includes("warn") ? "warn" : "ok");

const chips = computed<Chip[]>(() => {
  const out: Chip[] = [];

  if (portal.isError.value) {
    out.push({ id: "portal", label: "Portal", value: "down", tone: "bad", title: "The portal isn't answering" });
  } else if (portal.data.value) {
    const { ms, version } = portal.data.value;
    out.push({
      id: "portal",
      label: "Portal",
      value: `${Math.round(ms)} ms`,
      tone: ms > 1000 ? "bad" : ms > 300 ? "warn" : "ok",
      title: `Portal v${version} answers in ${Math.round(ms)} ms`,
    });
  }

  const list = nodes.data.value;
  if (list) {
    const online = list.filter((n) => n.online);
    const offline = list.length - online.length;
    out.push({
      id: "nodes",
      label: "Nodes",
      value: list.length ? `${online.length}/${list.length}` : "none",
      tone: !list.length || online.length === 0 ? "bad" : offline ? "warn" : "ok",
      title: offline
        ? `Offline: ${list.filter((n) => !n.online).map((n) => n.name).join(", ")}`
        : `${online.length} of ${list.length} nodes online`,
    });

    const usage = online.flatMap((n) => (n.usage ? [{ node: n.name, usage: n.usage }] : []));
    const gpus = usage.flatMap(({ node, usage: u }) => u.gpus.map((g) => ({ node, ...g })));
    const running = usage.reduce((sum, { usage: u }) => sum + u.environments, 0);
    if (online.length) {
      out.push({ id: "env", label: "Running", value: String(running), tone: "ok", title: `${running} environments running across the nodes` });
    }
    if (gpus.length) {
      const top = gpus.reduce((a, b) => ((b.util ?? 0) > (a.util ?? 0) ? b : a));
      const vram = gpus.reduce((a, b) => (pct(b.vramUsed ?? 0, b.vramTotal ?? 0) > pct(a.vramUsed ?? 0, a.vramTotal ?? 0) ? b : a));
      const vramPct = pct(vram.vramUsed ?? 0, vram.vramTotal ?? 0);
      out.push({
        id: "gpu",
        label: "GPU",
        value: top.util !== undefined ? `${Math.round(top.util)}%` : "–",
        tone: toneOf(top.util ?? 0),
        title: `Busiest GPU: ${top.name} on ${top.node}`,
      });
      if (vram.vramTotal) {
        out.push({
          id: "vram",
          label: "VRAM",
          value: `${Math.round(vramPct)}%`,
          tone: toneOf(vramPct),
          title: `Fullest VRAM: ${vram.name} on ${vram.node}`,
        });
      }
    }
    const hot = gpus.filter((g) => g.temp !== undefined && g.temp >= 85);
    if (hot.length) {
      out.push({ id: "temp", label: "Temp", value: `${Math.max(...hot.map((g) => g.temp!))} °C`, tone: "warn", title: `Running hot: ${hot.map((g) => `${g.name} on ${g.node}`).join(", ")}` });
    }
    const cpu = usage.reduce((m, { usage: u }) => Math.max(m, u.cpu), 0);
    if (cpu >= 90) out.push({ id: "cpu", label: "CPU", value: `${Math.round(cpu)}%`, tone: toneOf(cpu, 90, 98), title: "The busiest node's CPU" });
  }

  const mh = hosts.data.value?.hosts;
  if (mh?.length) {
    const down = mh.filter((h) => !h.online);
    out.push({
      id: "moonlight",
      label: "Moonlight",
      value: `${mh.length - down.length}/${mh.length}`,
      tone: down.length ? "warn" : "ok",
      title: down.length ? `Offline: ${down.map((h) => h.name).join(", ")}` : "All adopted Moonlight hosts are online",
    });
  }
  return out;
});

const overall = computed(() => worst(chips.value.map((c) => c.tone)));
const DOT: Record<Tone, string> = { ok: "bg-ok", warn: "bg-warn", bad: "bg-danger" };
const TEXT: Record<Tone, string> = { ok: "text-ink-2", warn: "text-warn", bad: "text-danger" };
</script>

<template>
  <RouterLink
    v-if="chips.length"
    to="/admin/nodes"
    class="hidden shrink-0 items-center gap-2.5 rounded-lg border border-line px-3 py-1.5 text-xs transition hover:bg-panel-2 @min-[40rem]:flex @min-[80rem]:gap-3"
    :aria-label="`System health: ${overall === 'ok' ? 'all fine' : overall === 'warn' ? 'needs a look' : 'a service is down'}. Open the nodes page.`"
  >
    <span v-for="c in chips" :key="c.id" class="flex items-center gap-1.5 whitespace-nowrap" :title="c.title">
      <span class="size-1.5 rounded-full" :class="DOT[c.tone]" aria-hidden="true" />
      <span class="sr-only text-ink-3 @min-[80rem]:not-sr-only">{{ c.label }}</span>
      <span class="font-medium tabular-nums" :class="TEXT[c.tone]">{{ c.value }}</span>
    </span>
  </RouterLink>
</template>

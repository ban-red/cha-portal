<script setup lang="ts">
import { TriangleAlert } from "lucide-vue-next";
import { computed } from "vue";

import type { NodeUsage } from "../api";
import { gigabytes } from "../format";

const props = defineProps<{ usage: NodeUsage }>();

/** A meter reads "High" (with an icon and a warm colour) from here. */
const HIGH = 90;

const clamp = (pct: number) => Math.min(100, Math.max(0, pct));
const high = (pct: number) => pct >= HIGH;
const share = (used?: number, total?: number) => (used !== undefined && total ? (used / total) * 100 : 0);

const memPct = computed(() => share(props.usage.memUsed, props.usage.memTotal));
const load = computed(() => props.usage.load.map((l) => l.toFixed(2)).join(" "));

interface Meter {
  label: string;
  pct: number;
  /** What is printed beside the label. */
  value: string;
}
const cpu = computed<Meter>(() => ({ label: "CPU", pct: props.usage.cpu, value: `${Math.round(props.usage.cpu)}%` }));
const ram = computed<Meter>(() => ({
  label: "RAM",
  pct: memPct.value,
  value: `${gigabytes(props.usage.memUsed)} / ${gigabytes(props.usage.memTotal)}`,
}));
const gpuMeters = computed(() =>
  props.usage.gpus.map((gpu) => {
    const meters: Meter[] = [];
    if (gpu.util !== undefined) meters.push({ label: "GPU", pct: gpu.util, value: `${gpu.util}%` });
    if (gpu.vramUsed !== undefined && gpu.vramTotal) {
      meters.push({
        label: "VRAM",
        pct: share(gpu.vramUsed, gpu.vramTotal),
        value: `${gigabytes(gpu.vramUsed)} / ${gigabytes(gpu.vramTotal)}`,
      });
    }
    return { gpu, meters };
  }),
);
</script>

<template>
  <section class="mt-4 space-y-2.5 rounded-lg border border-line bg-canvas p-3 text-xs" aria-label="Live usage">
    <h3 class="flex items-center justify-between text-ink-3">
      <span class="font-medium">Live</span>
      <span>{{ usage.environments }} {{ usage.environments === 1 ? "environment" : "environments" }}</span>
    </h3>

    <template v-for="(m, i) in [cpu, ram]" :key="m.label">
      <div>
        <div class="flex items-baseline justify-between gap-3">
          <span class="text-ink-2">{{ m.label }}</span>
          <span class="inline-flex items-center gap-1 tabular-nums" :class="high(m.pct) ? 'text-warn' : 'text-ink'">
            <template v-if="high(m.pct)"><TriangleAlert class="size-3.5" aria-hidden="true" /><span class="font-medium">High</span></template>
            {{ m.value }}
          </span>
        </div>
        <div
          class="mt-1 h-1.5 overflow-hidden rounded-full bg-line"
          role="meter"
          :aria-label="m.label"
          aria-valuemin="0"
          aria-valuemax="100"
          :aria-valuenow="Math.round(clamp(m.pct))"
          :aria-valuetext="`${m.value}${high(m.pct) ? ', high' : ''}`"
        >
          <div class="h-full rounded-full transition-[width]" :class="high(m.pct) ? 'bg-warn' : 'bg-info'" :style="{ width: clamp(m.pct) + '%' }" />
        </div>
        <p v-if="i === 0" class="mt-1 text-ink-3">{{ usage.cores }} cores · load {{ load }}</p>
      </div>
    </template>

    <div v-for="{ gpu, meters } in gpuMeters" :key="gpu.index" class="space-y-1">
      <p class="truncate text-ink-2">{{ gpu.name || `GPU ${gpu.index}` }}</p>
      <div v-for="m in meters" :key="m.label">
        <div class="flex items-baseline justify-between gap-3">
          <span class="text-ink-3">{{ m.label }}</span>
          <span class="inline-flex items-center gap-1 tabular-nums" :class="high(m.pct) ? 'text-warn' : 'text-ink'">
            <template v-if="high(m.pct)"><TriangleAlert class="size-3.5" aria-hidden="true" /><span class="font-medium">High</span></template>
            {{ m.value }}
          </span>
        </div>
        <div
          class="mt-1 h-1.5 overflow-hidden rounded-full bg-line"
          role="meter"
          :aria-label="`${gpu.name || `GPU ${gpu.index}`} ${m.label}`"
          aria-valuemin="0"
          aria-valuemax="100"
          :aria-valuenow="Math.round(clamp(m.pct))"
          :aria-valuetext="`${m.value}${high(m.pct) ? ', high' : ''}`"
        >
          <div class="h-full rounded-full transition-[width]" :class="high(m.pct) ? 'bg-warn' : 'bg-info'" :style="{ width: clamp(m.pct) + '%' }" />
        </div>
      </div>
      <p class="flex flex-wrap gap-x-3 text-ink-3 tabular-nums">
        <span v-if="gpu.enc !== undefined">NVENC {{ gpu.enc }}%</span>
        <span v-if="gpu.dec !== undefined">NVDEC {{ gpu.dec }}%</span>
        <span v-if="gpu.temp !== undefined">{{ gpu.temp }} °C</span>
        <span v-if="gpu.power !== undefined">
          {{ Math.round(gpu.power) }}<template v-if="gpu.powerLimit"> / {{ Math.round(gpu.powerLimit) }}</template> W
        </span>
      </p>
    </div>
  </section>
</template>

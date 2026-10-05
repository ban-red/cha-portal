<script setup lang="ts">
import { computed } from "vue";

import type { NodeUsage } from "../api";
import { gigabytes } from "../format";

const props = defineProps<{ usage: NodeUsage }>();

/** Bars turn amber from here. */
const HIGH = 90;

const clamp = (pct: number) => Math.min(100, Math.max(0, pct));
const high = (pct: number) => pct >= HIGH;
const share = (used?: number, total?: number) => (used !== undefined && total ? (used / total) * 100 : 0);

const memPct = computed(() => share(props.usage.memUsed, props.usage.memTotal));
const load = computed(() => props.usage.load.map((l) => l.toFixed(2)).join(" "));
</script>

<template>
  <section class="mt-4 space-y-2.5 rounded-lg border border-line bg-canvas p-3 text-xs">
    <h3 class="flex items-center justify-between text-ink-3">
      <span class="font-medium tracking-wide uppercase">Live</span>
      <span>{{ usage.environments }} {{ usage.environments === 1 ? "environment" : "environments" }}</span>
    </h3>

    <div>
      <div class="flex items-baseline justify-between gap-3">
        <span class="text-ink-2">CPU</span>
        <span class="tabular-nums" :class="high(usage.cpu) ? 'text-warn' : 'text-ink'">{{ Math.round(usage.cpu) }}%</span>
      </div>
      <div class="mt-1 h-1.5 overflow-hidden rounded-full bg-line">
        <div class="h-full rounded-full transition-[width]" :class="high(usage.cpu) ? 'bg-warn' : 'bg-accent'" :style="{ width: clamp(usage.cpu) + '%' }" />
      </div>
      <p class="mt-1 text-ink-3">{{ usage.cores }} cores · load {{ load }}</p>
    </div>

    <div>
      <div class="flex items-baseline justify-between gap-3">
        <span class="text-ink-2">RAM</span>
        <span class="tabular-nums" :class="high(memPct) ? 'text-warn' : 'text-ink'">
          {{ gigabytes(usage.memUsed) }} / {{ gigabytes(usage.memTotal) }}
        </span>
      </div>
      <div class="mt-1 h-1.5 overflow-hidden rounded-full bg-line">
        <div class="h-full rounded-full transition-[width]" :class="high(memPct) ? 'bg-warn' : 'bg-accent'" :style="{ width: clamp(memPct) + '%' }" />
      </div>
    </div>

    <div v-for="gpu in usage.gpus" :key="gpu.index" class="space-y-1">
      <p class="truncate text-ink-2">{{ gpu.name || `GPU ${gpu.index}` }}</p>
      <div v-if="gpu.util !== undefined">
        <div class="flex items-baseline justify-between gap-3">
          <span class="text-ink-3">GPU</span>
          <span class="tabular-nums" :class="high(gpu.util) ? 'text-warn' : 'text-ink'">{{ gpu.util }}%</span>
        </div>
        <div class="mt-1 h-1.5 overflow-hidden rounded-full bg-line">
          <div class="h-full rounded-full transition-[width]" :class="high(gpu.util) ? 'bg-warn' : 'bg-accent'" :style="{ width: clamp(gpu.util) + '%' }" />
        </div>
      </div>
      <div v-if="gpu.vramUsed !== undefined && gpu.vramTotal">
        <div class="flex items-baseline justify-between gap-3">
          <span class="text-ink-3">VRAM</span>
          <span class="tabular-nums" :class="high(share(gpu.vramUsed, gpu.vramTotal)) ? 'text-warn' : 'text-ink'">
            {{ gigabytes(gpu.vramUsed) }} / {{ gigabytes(gpu.vramTotal) }}
          </span>
        </div>
        <div class="mt-1 h-1.5 overflow-hidden rounded-full bg-line">
          <div
            class="h-full rounded-full transition-[width]"
            :class="high(share(gpu.vramUsed, gpu.vramTotal)) ? 'bg-warn' : 'bg-accent'"
            :style="{ width: clamp(share(gpu.vramUsed, gpu.vramTotal)) + '%' }"
          />
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

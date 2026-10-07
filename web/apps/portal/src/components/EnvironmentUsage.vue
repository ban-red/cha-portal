<script setup lang="ts">
// A running app's CPU, RAM and GPU memory, in one quiet line: RAM and VRAM as "used / total GB
// (share)", in gigabytes throughout. The node reports it every few seconds; VRAM is left out
// when the node can't tell which GPU memory is whose.
import type { EnvironmentUsage } from "../api";

defineProps<{ usage: EnvironmentUsage }>();

const GB = 2 ** 30;
/** A figure in gigabytes: two decimals below 10 (0.46), one above (31.2). */
const gb = (bytes: number) => (bytes / GB).toFixed(bytes / GB < 10 ? 2 : 1);
const share = (used: number, total: number) => {
  const p = total > 0 ? (used / total) * 100 : 0;
  return p < 10 ? p.toFixed(1) : Math.round(p).toString();
};
</script>

<template>
  <p class="flex flex-wrap gap-x-4 text-xs text-ink-3 tabular-nums" aria-label="Resource use">
    <span title="Share of the node's CPU this app uses">
      CPU <span class="text-ink-2">{{ share(usage.cpu, 100) }}%</span>
    </span>
    <span title="RAM this app holds, of the node's RAM">
      RAM <span class="text-ink-2">{{ gb(usage.mem) }}<template v-if="usage.memTotal"> / {{ gb(usage.memTotal) }}</template> GB</span>
      <span v-if="usage.memTotal"> ({{ share(usage.mem, usage.memTotal) }}%)</span>
    </span>
    <span v-if="usage.vram !== undefined" title="GPU memory this app holds, of the GPU's memory">
      VRAM <span class="text-ink-2">{{ gb(usage.vram) }}<template v-if="usage.vramTotal"> / {{ gb(usage.vramTotal) }}</template> GB</span>
      <span v-if="usage.vramTotal"> ({{ share(usage.vram, usage.vramTotal) }}%)</span>
    </span>
  </p>
</template>

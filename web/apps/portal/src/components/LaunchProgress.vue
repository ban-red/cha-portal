<script setup lang="ts">
// A starting environment's progress: a bar and "412 of 890 MB" when its node
// counts the step (an image download), else the node's detail text, if any.
import { computed } from "vue";

import type { Environment } from "../api";
import { progressPercent, progressText } from "../launchProgress";

const props = defineProps<{
  env: Pick<Environment, "detail" | "progress">;
  /** Detail text, as a small line, and a thinner bar. */
  compact?: boolean;
}>();

const percent = computed(() => (props.env.progress ? progressPercent(props.env.progress) : null));
const text = computed(() => (props.env.progress ? progressText(props.env.progress) : null));
</script>

<template>
  <div v-if="env.progress || env.detail" class="min-w-0" :title="env.detail ?? undefined">
    <template v-if="env.progress && percent !== null">
      <div
        class="overflow-hidden rounded-full bg-canvas"
        :class="compact ? 'h-1' : 'h-1.5'"
        role="progressbar"
        :aria-label="env.detail ?? 'Downloading'"
        :aria-valuetext="text ?? undefined"
        aria-valuemin="0"
        aria-valuemax="100"
        :aria-valuenow="Math.round(percent)"
      >
        <div class="h-full rounded-full bg-accent transition-[width] duration-300 ease-out" :style="{ width: `${percent}%` }" />
      </div>
      <p class="mt-1 truncate text-ink-2 tabular-nums" :class="compact ? 'text-xs' : 'text-sm'">{{ text }}</p>
    </template>
    <p v-else class="truncate text-ink-2" :class="compact ? 'text-xs' : 'text-sm'">{{ env.detail }}</p>
  </div>
</template>

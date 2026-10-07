<script setup lang="ts">
// Whether an app is hardware accelerated: GPU on an NVIDIA or VA-API device, CPU otherwise.
// For an app card it is where Launch would run it; for a running one, where it runs.
import { Cpu, Zap } from "lucide-vue-next";

import type { DeviceKind } from "../api";

defineProps<{
  kind: DeviceKind;
  /** The device's name, for the tooltip. */
  name?: string;
  /** "would run" on a card, "runs" on a live environment. */
  tense?: "would" | "is";
}>();
</script>

<template>
  <span
    class="inline-flex shrink-0 items-center gap-1 rounded-md border px-1.5 py-px text-2xs font-medium tracking-wide uppercase"
    :class="kind !== 'cpu' ? 'border-accent/40 bg-accent-soft text-accent' : 'border-line text-ink-3'"
    :title="
      kind !== 'cpu'
        ? `Hardware accelerated on ${name ?? 'the GPU'}`
        : tense === 'is'
          ? 'No GPU: it runs on the CPU'
          : 'No GPU where it would launch: runs on the CPU'
    "
  >
    <component :is="kind !== 'cpu' ? Zap : Cpu" class="size-3" aria-hidden="true" />{{ kind !== "cpu" ? "GPU" : "CPU" }}
  </span>
</template>

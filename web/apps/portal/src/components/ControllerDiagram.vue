<script setup lang="ts">
import type { ControllerState, ControllerType } from "@cha/player";
import { computed } from "vue";

// A controller in the standard layout, as it is held right now. Decoration:
// the page beside it says the same in text.
const props = defineProps<{ state: ControllerState; type: ControllerType }>();

const held = (i: number) => (props.state.buttons[i] ?? 0) > 0.5;
const level = (i: number) => Math.min(1, Math.max(0, props.state.buttons[i] ?? 0));
const stick = (axis: number, cx: number, cy: number) => ({
  x: cx + (props.state.axes[axis] ?? 0) * 17,
  y: cy + (props.state.axes[axis + 1] ?? 0) * 17,
});
const left = computed(() => stick(0, 100, 95));
const right = computed(() => stick(2, 230, 150));

const FACE: Record<ControllerType, [string, string, string, string]> = {
  // south, east, west, north
  xbox: ["A", "B", "X", "Y"],
  playstation: ["✕", "○", "□", "△"],
  switch: ["B", "A", "Y", "X"],
  steam: ["A", "B", "X", "Y"],
  generic: ["A", "B", "X", "Y"],
};
const face = computed(() => FACE[props.type]);

const on = "fill-accent stroke-accent";
const off = "fill-panel stroke-line";
</script>

<template>
  <svg viewBox="0 0 360 215" class="w-full max-w-sm" aria-hidden="true" focusable="false">
    <!-- Triggers, as bars that fill -->
    <rect x="50" y="2" width="60" height="9" rx="3" class="fill-panel stroke-line" />
    <rect x="50" y="2" :width="60 * level(6)" height="9" rx="3" class="fill-accent" />
    <rect x="250" y="2" width="60" height="9" rx="3" class="fill-panel stroke-line" />
    <rect x="250" y="2" :width="60 * level(7)" height="9" rx="3" class="fill-accent" />
    <!-- Shoulders -->
    <rect x="50" y="16" width="60" height="14" rx="5" :class="held(4) ? on : off" />
    <rect x="250" y="16" width="60" height="14" rx="5" :class="held(5) ? on : off" />
    <!-- Body -->
    <rect x="30" y="38" width="300" height="165" rx="62" class="fill-panel-2 stroke-line" />

    <!-- Left stick -->
    <circle cx="100" cy="95" r="28" :class="held(10) ? on : 'fill-panel stroke-line'" />
    <circle :cx="left.x" :cy="left.y" r="8" class="fill-ink" />
    <!-- Right stick -->
    <circle cx="230" cy="150" r="28" :class="held(11) ? on : 'fill-panel stroke-line'" />
    <circle :cx="right.x" :cy="right.y" r="8" class="fill-ink" />

    <!-- D-pad -->
    <rect x="133" y="126" width="14" height="14" rx="2" :class="held(12) ? on : off" />
    <rect x="133" y="154" width="14" height="14" rx="2" :class="held(13) ? on : off" />
    <rect x="119" y="140" width="14" height="14" rx="2" :class="held(14) ? on : off" />
    <rect x="147" y="140" width="14" height="14" rx="2" :class="held(15) ? on : off" />

    <!-- Face buttons -->
    <g class="text-2xs font-semibold" text-anchor="middle">
      <circle cx="260" cy="115" r="10" :class="held(0) ? on : off" />
      <circle cx="280" cy="95" r="10" :class="held(1) ? on : off" />
      <circle cx="240" cy="95" r="10" :class="held(2) ? on : off" />
      <circle cx="260" cy="75" r="10" :class="held(3) ? on : off" />
      <text x="260" y="118.5" :class="held(0) ? 'fill-on-accent' : 'fill-ink-3'">{{ face[0] }}</text>
      <text x="280" y="98.5" :class="held(1) ? 'fill-on-accent' : 'fill-ink-3'">{{ face[1] }}</text>
      <text x="240" y="98.5" :class="held(2) ? 'fill-on-accent' : 'fill-ink-3'">{{ face[2] }}</text>
      <text x="260" y="78.5" :class="held(3) ? 'fill-on-accent' : 'fill-ink-3'">{{ face[3] }}</text>
    </g>

    <!-- Back, start, guide -->
    <circle cx="163" cy="95" r="6" :class="held(8) ? on : off" />
    <circle cx="197" cy="95" r="6" :class="held(9) ? on : off" />
    <circle cx="180" cy="68" r="9" :class="held(16) ? on : off" />
  </svg>
</template>

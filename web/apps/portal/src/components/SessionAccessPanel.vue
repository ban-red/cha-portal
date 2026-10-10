<script setup lang="ts">
import { computed } from "vue";
import { freeSlot, watcherLabel, type Watcher } from "@cha/player";

import { guestPrompt } from "../shares";

// The corner card for people in a public or free environment. A joiner (not the owner or an admin)
// starts as a viewer and sees where it stands: watching, asking, or on a gamepad. The owner or admin
// holding the controls sees who asks for a gamepad and the pads they granted, and answers here
// (the toolbar's "Hand controls" answers a request for the controls).
const props = defineProps<{
  /** The owner or an admin. */
  host: boolean;
  hasControl: boolean;
  player?: number;
  canTake: boolean;
  request?: "control" | "pad";
  owner: string;
  watchers: readonly Watcher[];
}>();
const emit = defineEmits<{
  take: [];
  askControl: [];
  askPad: [];
  cancel: [];
  givePad: [id: number, slot: number];
  revokePad: [id: number];
}>();

const prompt = computed(() => guestPrompt(props.player, props.canTake, props.request, props.owner));
const slot = computed(() => freeSlot(props.watchers));
const asks = computed(() => props.watchers.filter((w) => w.request === "pad"));
const granted = computed(() => props.watchers.filter((w) => w.role === "player" && w.granted));
const showGuest = computed(() => !props.host && !props.hasControl);
const showHost = computed(() => props.host && props.hasControl && (asks.value.length > 0 || granted.value.length > 0));
</script>

<template>
  <div
    v-if="showGuest || showHost"
    class="absolute bottom-4 left-4 z-20 w-80 max-w-[calc(100%-2rem)] space-y-2 rounded-xl border border-line bg-panel/90 p-3 text-sm shadow-lg backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none"
    role="region"
    aria-label="Sharing"
  >
    <template v-if="showGuest">
      <p role="status">{{ prompt.text }}</p>
      <div v-if="prompt.kind !== 'player'" class="flex flex-wrap gap-2">
        <button v-if="prompt.kind === 'take'" class="btn-primary min-h-8 px-3 text-xs" @click="emit('take')">Take control</button>
        <template v-else-if="prompt.kind === 'ask'">
          <button class="btn-primary min-h-8 px-3 text-xs" @click="emit('askControl')">Ask for the controls</button>
          <button class="btn-ghost min-h-8 px-3 text-xs" @click="emit('askPad')">Add my gamepad</button>
        </template>
        <button v-else class="btn-ghost min-h-8 px-3 text-xs" @click="emit('cancel')">Cancel</button>
      </div>
    </template>
    <template v-else>
      <ul class="space-y-1.5">
        <li v-for="w in asks" :key="w.id" class="flex flex-wrap items-center justify-between gap-2">
          <span class="min-w-0 truncate">{{ watcherLabel(w) }}</span>
          <button
            v-if="slot !== null"
            class="btn-primary min-h-7 px-2 text-xs"
            @click="emit('givePad', w.id, slot)"
          >
            Allow as player {{ slot + 1 }}
          </button>
          <span v-else class="text-xs text-ink-3">All gamepad slots are in use</span>
        </li>
        <li v-for="w in granted" :key="w.id" class="flex flex-wrap items-center justify-between gap-2">
          <span class="min-w-0 truncate">{{ watcherLabel(w) }}</span>
          <button class="btn-ghost min-h-7 px-2 text-xs" @click="emit('revokePad', w.id)">Take back</button>
        </li>
      </ul>
    </template>
  </div>
</template>

<script setup lang="ts">
// One app of a Moonlight host, as a card or a list row: name, icon and a Launch button
// (Connect once the user's own environment of it runs). No controller, frame rate or
// storage settings: the host decides those.
import { Gamepad2, MonitorPlay, Play } from "lucide-vue-next";
import { computed } from "vue";

import type { Environment, MoonlightApp } from "../api";

const props = defineProps<{
  app: MoonlightApp;
  view: "grid" | "list";
  instance?: Environment;
  /** A launch of this app is under way. */
  busy: boolean;
  disabled: boolean;
  /** Why it can't launch, in words. */
  reason: string | null;
}>();
const emit = defineEmits<{ launch: [] }>();

const starting = computed(() => props.instance?.state === "starting");
const open = computed(() => props.instance?.state === "running");
const TILE = "grid place-items-center rounded-lg bg-accent-soft text-accent";
const BTN = "min-h-9 w-full pointer-coarse:min-h-11";
</script>

<template>
  <article class="card card-lift @container" :aria-label="app.name">
    <div
      class="flex gap-3 p-4"
      :class="view === 'grid' ? 'h-full flex-col' : 'flex-wrap items-center @min-[34rem]:flex-nowrap'"
    >
      <div class="flex min-w-0 items-center gap-3" :class="view === 'list' && 'flex-1'">
        <div class="size-11 shrink-0" :class="TILE"><Gamepad2 class="size-6" aria-hidden="true" /></div>
        <div class="min-w-0">
          <h4 class="truncate text-base leading-6 font-semibold" :title="app.name">{{ app.name }}</h4>
          <p v-if="app.hdr" class="text-xs text-ink-3">HDR</p>
        </div>
      </div>
      <div :class="view === 'grid' ? 'mt-auto' : 'w-full @min-[34rem]:w-44'">
        <RouterLink v-if="open" :to="{ name: 'session', params: { id: instance!.id } }" class="btn-primary" :class="BTN">
          <MonitorPlay class="size-4" aria-hidden="true" />
          Connect
        </RouterLink>
        <button v-else-if="starting" type="button" class="btn-ghost" :class="BTN" disabled>Starting…</button>
        <button
          v-else
          type="button"
          class="btn-primary"
          :class="BTN"
          :disabled="disabled || busy"
          :aria-describedby="reason ? `${app.templateId}-reason` : undefined"
          @click="emit('launch')"
        >
          <Play class="size-4" aria-hidden="true" />
          {{ busy ? "Launching…" : "Launch" }}
        </button>
        <p v-if="reason && !instance" :id="`${app.templateId}-reason`" class="mt-1.5 text-xs text-ink-3">{{ reason }}</p>
      </div>
    </div>
  </article>
</template>

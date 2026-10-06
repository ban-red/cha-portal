<script setup lang="ts">
// One app of the catalog, as a card or a list row. The card lays itself out by its own
// width (a container query): icon beside the title when it is wide, stacked when narrow;
// the Controller and Frame rate selects sit side by side only when there is room for their
// full option text.
import { Activity, AppWindow, Database, Gamepad2, Globe, Monitor, Pin } from "lucide-vue-next";
import { computed } from "vue";

import type { AppSettings, ControllerApp, PlacementChoice, Placements, Template } from "../api";
import { FPS_CHOICES } from "../appFps";
import { KINDS, kindLabel } from "../controllerKinds";
import FormError from "./FormError.vue";
import LaunchButton from "./LaunchButton.vue";

const props = defineProps<{
  template: Template;
  view: "grid" | "list";
  pinned: boolean;
  /** The app keeps the user's data between launches. */
  saved: boolean;
  /** The controller choice, when the server offers it and the user can launch. */
  controller?: ControllerApp;
  fps?: AppSettings;
  controllerError?: string | null;
  fpsError?: string | null;
  /** An environment of this app is live: changes apply from the next launch. */
  running: boolean;
  placements?: Placements;
  busy: boolean;
  disabled: boolean;
}>();
const emit = defineEmits<{
  launch: [choice: PlacementChoice | null];
  pin: [];
  chooseController: [event: Event];
  chooseFps: [event: Event];
}>();

const ICONS = { browser: Globe, desktop: Monitor, gaming: Gamepad2, test: Activity } as const;
const icon = computed(() => ICONS[props.template.class as keyof typeof ICONS] ?? AppWindow);

const t = computed(() => props.template);
const controllerText = computed(() =>
  props.controller ? kindLabel(props.controller.kind ?? props.controller.default) : null,
);
const fpsText = computed(() => (props.fps ? `${props.fps.fps ?? props.fps.defaultFps} fps` : null));

const SELECT = "field min-h-9 pointer-coarse:min-h-11 truncate";
</script>

<template>
  <!-- Grid: a card and a container. -->
  <article v-if="view === 'grid'" class="card card-lift @container" :aria-label="t.name">
    <div class="flex h-full flex-col p-4 @min-[26rem]:p-5">
      <div
        class="grid grid-cols-[minmax(0,1fr)_auto] items-start gap-x-4 gap-y-3 @min-[26rem]:grid-cols-[auto_minmax(0,1fr)_auto]"
      >
        <div class="col-start-1 row-start-1 grid size-14 place-items-center rounded-lg bg-accent-soft text-accent">
          <component :is="icon" class="size-7" aria-hidden="true" />
        </div>
        <div class="col-span-2 col-start-1 row-start-2 min-w-0 @min-[26rem]:col-span-1 @min-[26rem]:col-start-2 @min-[26rem]:row-start-1">
          <h3 class="text-xl leading-7 font-semibold tracking-tight">{{ t.name }}</h3>
          <p class="mt-1 line-clamp-3 text-sm text-ink-2" :title="t.description">{{ t.description }}</p>
        </div>
        <div class="col-start-2 row-start-1 flex items-center gap-2 @min-[26rem]:col-start-3">
          <RouterLink
            v-if="saved"
            :to="{ name: 'storage' }"
            title="Data kept between launches"
            class="inline-flex items-center gap-1 rounded-full border border-line-strong px-2 py-0.5 text-2xs text-ink-3 transition hover:border-ink-3 hover:text-ink-2"
          >
            <Database class="size-3" aria-hidden="true" />
            Saved<span class="sr-only">: data kept between launches. Open storage settings.</span>
          </RouterLink>
          <button
            type="button"
            class="inline-flex size-9 items-center justify-center rounded-lg transition hover:bg-panel-2 pointer-coarse:size-11"
            :class="pinned ? 'text-accent' : 'text-ink-3 hover:text-ink'"
            :aria-pressed="pinned"
            :aria-label="`Pin ${t.name}`"
            :title="pinned ? 'Pinned' : `Pin ${t.name}`"
            @click="emit('pin')"
          >
            <Pin class="size-5" :fill="pinned ? 'currentColor' : 'none'" aria-hidden="true" />
          </button>
        </div>
      </div>

      <div v-if="controller || fps" class="mt-4 border-t border-line pt-4">
        <div class="grid grid-cols-1 gap-3 @min-[26rem]:grid-cols-2">
          <div v-if="controller" class="min-w-0">
            <label :for="`${t.id}-controller`" class="mb-1 block text-xs text-ink-3">Controller</label>
            <select
              :id="`${t.id}-controller`"
              :value="controller.kind ?? ''"
              :class="SELECT"
              title="The controller this app sees, from its next launch"
              @change="emit('chooseController', $event)"
            >
              <option value="">Default ({{ kindLabel(controller.default) }})</option>
              <option v-for="k in KINDS" :key="k.kind" :value="k.kind">{{ k.label }}</option>
            </select>
          </div>
          <div v-if="fps" class="min-w-0">
            <label :for="`${t.id}-fps`" class="mb-1 block text-xs text-ink-3">Frame rate</label>
            <select
              :id="`${t.id}-fps`"
              :value="fps.fps ?? ''"
              :class="SELECT"
              title="Your screen needs to refresh this fast for it to show; 120 needs a 120 Hz display. From the next launch"
              @change="emit('chooseFps', $event)"
            >
              <option value="">Default ({{ fps.defaultFps }} fps)</option>
              <option v-for="f in FPS_CHOICES" :key="f" :value="f">{{ f }} fps</option>
            </select>
          </div>
        </div>
        <p v-if="running" class="mt-2 text-xs text-ink-3">
          Running: a change applies when you stop it and launch it again.
        </p>
        <FormError v-if="controllerError" polite class="mt-1" :message="controllerError" />
        <FormError v-if="fpsError" polite class="mt-1" :message="fpsError" />
      </div>

      <div class="mt-auto">
        <LaunchButton
          :id="t.id"
          :placements="placements"
          :busy="busy"
          :disabled="disabled"
          @launch="(choice) => emit('launch', choice)"
        />
      </div>
    </div>
  </article>

  <!-- List: one row, two lines on a narrow screen. -->
  <article v-else class="card card-lift @container" :aria-label="t.name">
    <div
      class="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-3 p-3 [grid-template-areas:'tile_text_pin''meta_meta_launch'] @min-[44rem]:grid-cols-[auto_minmax(0,1fr)_auto_auto_11rem] @min-[44rem]:gap-x-4 @min-[44rem]:[grid-template-areas:'tile_text_meta_pin_launch']"
    >
      <div class="grid size-11 place-items-center rounded-lg bg-accent-soft text-accent [grid-area:tile]">
        <component :is="icon" class="size-6" aria-hidden="true" />
      </div>
      <div class="min-w-0 [grid-area:text]">
        <div class="flex items-center gap-2">
          <h3 class="truncate text-base leading-6 font-semibold">{{ t.name }}</h3>
          <RouterLink
            v-if="saved"
            :to="{ name: 'storage' }"
            title="Data kept between launches"
            class="inline-flex shrink-0 items-center gap-1 rounded-full border border-line-strong px-2 py-0.5 text-2xs text-ink-3 transition hover:border-ink-3 hover:text-ink-2"
          >
            <Database class="size-3" aria-hidden="true" />
            Saved<span class="sr-only">: data kept between launches. Open storage settings.</span>
          </RouterLink>
        </div>
        <p class="truncate text-sm text-ink-2" :title="t.description">{{ t.description }}</p>
      </div>
      <p v-if="controllerText || fpsText" class="min-w-0 truncate text-xs text-ink-3 [grid-area:meta]" title="Change these in the grid view">
        <template v-if="controllerText"><span class="sr-only">Controller: </span>{{ controllerText }}</template>
        <span v-if="controllerText && fpsText" aria-hidden="true"> · </span>
        <template v-if="fpsText"><span class="sr-only">Frame rate: </span>{{ fpsText }}</template>
      </p>
      <button
        type="button"
        class="inline-flex size-9 items-center justify-center justify-self-end rounded-lg transition hover:bg-panel-2 pointer-coarse:size-11 [grid-area:pin]"
        :class="pinned ? 'text-accent' : 'text-ink-3 hover:text-ink'"
        :aria-pressed="pinned"
        :aria-label="`Pin ${t.name}`"
        :title="pinned ? 'Pinned' : `Pin ${t.name}`"
        @click="emit('pin')"
      >
        <Pin class="size-5" :fill="pinned ? 'currentColor' : 'none'" aria-hidden="true" />
      </button>
      <div class="w-44 justify-self-end [grid-area:launch] @min-[44rem]:w-full">
        <LaunchButton
          :id="t.id"
          compact
          :placements="placements"
          :busy="busy"
          :disabled="disabled"
          @launch="(choice) => emit('launch', choice)"
        />
      </div>
    </div>
  </article>
</template>

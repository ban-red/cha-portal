<script setup lang="ts">
// One app of the catalog, as a card or a list row. The card lays itself out by its own
// width (a container query): icon beside the title when it is wide, stacked when narrow;
// the Controller and Frame rate pickers are compact pills that wrap when there is no room.
import { Activity, AppWindow, Copy, Gamepad2, Gauge, Globe, Monitor, MonitorPlay, Pin } from "lucide-vue-next";
import { computed, ref } from "vue";

import { catalogIconUrl, type AppSettings, type ControllerApp, type Environment, type PlacementChoice, type Placements, type StorageApp, type Template } from "../api";
import { FPS_CHOICES } from "../appFps";
import { KINDS, kindLabel } from "../controllerKinds";
import { dataNote } from "../customEnv";
import { autoOption } from "../placements";
import AppStorageMenu from "./AppStorageMenu.vue";
import GpuBadge from "./GpuBadge.vue";
import FormError from "./FormError.vue";
import WarningNote from "./WarningNote.vue";
import LaunchButton from "./LaunchButton.vue";

const props = defineProps<{
  template: Template;
  view: "grid" | "list";
  pinned: boolean;
  /** The app's data setting, when the server offers it. */
  storage?: StorageApp;
  storageError?: string | null;
  /** The controller choice, when the server offers it and the user can launch. */
  controller?: ControllerApp;
  fps?: AppSettings;
  controllerError?: string | null;
  fpsError?: string | null;
  /** An environment of this app is live: changes apply from the next launch. */
  running: boolean;
  /** The app's existing environment (the latest live one), to open instead of launching. */
  instance?: Environment;
  placements?: Placements;
  busy: boolean;
  disabled: boolean;
  /** Offer "Open in Cha Player" (on a Mac). */
  player?: boolean;
  /** Offer "Duplicate" (admins): make a custom environment from this one. */
  duplicable?: boolean;
  /** For a custom environment, the name of the one it is based on. */
  baseName?: string;
}>();
const emit = defineEmits<{
  launch: [choice: PlacementChoice | null];
  pin: [];
  openInPlayer: [];
  duplicate: [];
  setPersistent: [persistent: boolean];
  chooseController: [event: Event];
  chooseFps: [event: Event];
}>();

const ICONS = { browser: Globe, desktop: Monitor, gaming: Gamepad2, test: Activity } as const;
const icon = computed(() => ICONS[props.template.class as keyof typeof ICONS] ?? AppWindow);
// The app's own logo when the catalog has one (and it loads); the generic icon otherwise.
const logoFailed = ref(false);
const logo = computed(() => (props.template.icon && !logoFailed.value ? catalogIconUrl(props.template.id) : null));
const TILE = "grid place-items-center rounded-lg";
const tileTone = computed(() => (logo.value ? "border border-line bg-panel-2" : "bg-accent-soft text-accent"));

const t = computed(() => props.template);
// "custom" marks an admin's copy; "uses Steam's data" when it shares the base's saved data.
const sharesData = computed(() => dataNote(props.template, props.baseName ?? props.template.custom?.base ?? ""));
// The device Launch would pick, if the portal says.
const place = computed(() => autoOption(props.placements));
const controllerText = computed(() =>
  props.controller ? kindLabel(props.controller.kind ?? props.controller.default) : null,
);
const fpsText = computed(() => (props.fps ? `${props.fps.fps ?? props.fps.defaultFps} fps` : null));

// An icon pill showing the current value; the real <select> sits invisibly on top, so the
// native picker (and its keyboard and screen-reader behaviour) stays.
const PILL =
  "relative inline-flex min-h-9 max-w-full items-center gap-1.5 rounded-lg border px-2.5 text-sm transition pointer-coarse:min-h-11 hover:border-accent/60 hover:text-ink focus-within:ring-2 focus-within:ring-accent/50";
const pillTone = (changed: boolean) => (changed ? "border-accent/50 bg-accent-soft text-accent" : "border-line text-ink-2");
const SELECT = "absolute inset-0 size-full cursor-pointer appearance-none opacity-0";
const controllerShort = computed(() => (props.controller ? kindLabel(props.controller.kind ?? props.controller.default) : ""));
</script>

<template>
  <!-- Grid: a card and a container. -->
  <article v-if="view === 'grid'" class="card card-lift @container" :aria-label="t.name">
    <div class="flex h-full flex-col p-4">
      <div
        class="grid grid-cols-[minmax(0,1fr)_auto] items-start gap-x-4 gap-y-3 @min-[26rem]:grid-cols-[auto_minmax(0,1fr)_auto]"
      >
        <div class="col-start-1 row-start-1 size-11" :class="[TILE, tileTone]">
          <img v-if="logo" :src="logo" alt="" class="size-7 object-contain" draggable="false" @error="logoFailed = true" />
          <component :is="icon" v-else class="size-6" aria-hidden="true" />
        </div>
        <div class="col-span-2 col-start-1 row-start-2 min-w-0 @min-[26rem]:col-span-1 @min-[26rem]:col-start-2 @min-[26rem]:row-start-1">
          <h3 class="flex items-center gap-2 text-lg leading-6 font-semibold tracking-tight">
            {{ t.name }}
            <GpuBadge v-if="place" :kind="place.kind" :name="place.label" />
          </h3>
          <p class="mt-0.5 line-clamp-2 text-sm text-ink-2" :title="t.description">{{ t.description }}</p>
          <p v-if="t.custom" class="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-ink-3">
            <span class="rounded-full border border-line-strong px-1.5 text-2xs text-ink-2">custom</span>
            <span v-if="sharesData">{{ sharesData }}</span>
          </p>
        </div>
        <div class="col-start-2 row-start-1 flex items-center gap-2 @min-[26rem]:col-start-3">
          <AppStorageMenu
            v-if="storage"
            :app="storage"
            :error="storageError"
            :disabled="disabled"
            @toggle="(v) => emit('setPersistent', v)"
          />
          <button
            v-if="player && !disabled"
            type="button"
            class="inline-flex size-9 items-center justify-center rounded-lg text-ink-3 transition hover:bg-panel-2 hover:text-ink pointer-coarse:size-11"
            :aria-label="`Open ${t.name} in Cha Player`"
            :title="`Open ${t.name} in Cha Player`"
            @click="emit('openInPlayer')"
          >
            <MonitorPlay class="size-5" aria-hidden="true" />
          </button>
          <button
            v-if="duplicable"
            type="button"
            class="inline-flex size-9 items-center justify-center rounded-lg text-ink-3 transition hover:bg-panel-2 hover:text-ink pointer-coarse:size-11"
            :aria-label="`Duplicate ${t.name}`"
            :title="`Duplicate ${t.name} as a custom environment`"
            @click="emit('duplicate')"
          >
            <Copy class="size-5" aria-hidden="true" />
          </button>
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

      <div v-if="controller || fps" class="mt-3">
        <div class="flex flex-wrap gap-2">
          <div v-if="controller" :class="[PILL, pillTone(!!controller.kind)]">
            <label :for="`${t.id}-controller`" class="sr-only">Controller</label>
            <Gamepad2 class="size-4 shrink-0" aria-hidden="true" />
            <span class="truncate" aria-hidden="true">{{ controllerShort }}</span>
            <select
              :id="`${t.id}-controller`"
              :value="controller.kind ?? ''"
              :class="SELECT"
              title="Controller: the one this app sees, from its next launch"
              @change="emit('chooseController', $event)"
            >
              <option value="">Default ({{ kindLabel(controller.default) }})</option>
              <option v-for="k in KINDS" :key="k.kind" :value="k.kind">{{ k.label }}</option>
            </select>
          </div>
          <div v-if="fps" :class="[PILL, pillTone(fps.fps != null)]">
            <label :for="`${t.id}-fps`" class="sr-only">Frame rate</label>
            <Gauge class="size-4 shrink-0" aria-hidden="true" />
            <span aria-hidden="true">{{ fps.fps ?? fps.defaultFps }} fps</span>
            <select
              :id="`${t.id}-fps`"
              :value="fps.fps ?? ''"
              :class="SELECT"
              title="Frame rate: your screen needs to refresh this fast for it to show; 120 needs a 120 Hz display. From the next launch"
              @change="emit('chooseFps', $event)"
            >
              <option value="">Default ({{ fps.defaultFps }} fps)</option>
              <option v-for="f in FPS_CHOICES" :key="f" :value="f">{{ f }} fps</option>
            </select>
          </div>
        </div>
        <p v-if="running" class="mt-1.5 text-xs text-ink-3">
          Running: a change applies when you stop it and launch it again.
        </p>
        <FormError v-if="controllerError" polite class="mt-1" :message="controllerError" />
        <FormError v-if="fpsError" polite class="mt-1" :message="fpsError" />
      </div>

      <div class="mt-auto pt-3">
        <WarningNote v-if="!instance" class="mb-2" :message="storage?.sharedProblem ?? null" />
        <LaunchButton
          :id="t.id"
          :placements="placements"
          :busy="busy"
          :disabled="disabled"
          :instance="instance"
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
      <div class="size-11 [grid-area:tile]" :class="[TILE, tileTone]">
        <img v-if="logo" :src="logo" alt="" class="size-7 object-contain" draggable="false" @error="logoFailed = true" />
        <component :is="icon" v-else class="size-6" aria-hidden="true" />
      </div>
      <div class="min-w-0 [grid-area:text]">
        <div class="flex items-center gap-2">
          <h3 class="truncate text-base leading-6 font-semibold">{{ t.name }}</h3>
          <span v-if="t.custom" class="shrink-0 rounded-full border border-line-strong px-1.5 text-2xs text-ink-2">custom</span>
          <GpuBadge v-if="place" :kind="place.kind" :name="place.label" />
          <AppStorageMenu
            v-if="storage"
            :app="storage"
            :error="storageError"
            :disabled="disabled"
            @toggle="(v) => emit('setPersistent', v)"
          />
        </div>
        <p class="truncate text-sm text-ink-2" :title="t.description">{{ t.description }}<template v-if="sharesData"> · {{ sharesData }}</template></p>
      </div>
      <p v-if="controllerText || fpsText" class="min-w-0 truncate text-xs text-ink-3 [grid-area:meta]" title="Change these in the grid view">
        <template v-if="controllerText"><span class="sr-only">Controller: </span>{{ controllerText }}</template>
        <span v-if="controllerText && fpsText" aria-hidden="true"> · </span>
        <template v-if="fpsText"><span class="sr-only">Frame rate: </span>{{ fpsText }}</template>
      </p>
      <div class="flex items-center justify-self-end [grid-area:pin]">
        <button
          v-if="player && !disabled"
          type="button"
          class="inline-flex size-9 items-center justify-center rounded-lg text-ink-3 transition hover:bg-panel-2 hover:text-ink pointer-coarse:size-11"
          :aria-label="`Open ${t.name} in Cha Player`"
          :title="`Open ${t.name} in Cha Player`"
          @click="emit('openInPlayer')"
        >
          <MonitorPlay class="size-5" aria-hidden="true" />
        </button>
        <button
          v-if="duplicable"
          type="button"
          class="inline-flex size-9 items-center justify-center rounded-lg text-ink-3 transition hover:bg-panel-2 hover:text-ink pointer-coarse:size-11"
          :aria-label="`Duplicate ${t.name}`"
          :title="`Duplicate ${t.name} as a custom environment`"
          @click="emit('duplicate')"
        >
          <Copy class="size-5" aria-hidden="true" />
        </button>
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
      <div class="w-44 justify-self-end [grid-area:launch] @min-[44rem]:w-full">
        <LaunchButton
          :id="t.id"
          compact
          :placements="placements"
          :busy="busy"
          :disabled="disabled"
          :instance="instance"
          @launch="(choice) => emit('launch', choice)"
        />
      </div>
    </div>
    <WarningNote v-if="!instance" class="mx-3 mb-3" :message="storage?.sharedProblem ?? null" />
  </article>
</template>

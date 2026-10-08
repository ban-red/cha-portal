<script setup lang="ts">
import type { IconName, ToolbarControl, ToolbarModel } from "@cha/ui-spec";
import { onBeforeUnmount, useTemplateRef, watch } from "vue";

import type { DeviceKind } from "../api";
import ControllerMenu from "./ControllerMenu.vue";
import GpuBadge from "./GpuBadge.vue";
import Icon from "./Icon.vue";
import SoundMenu from "./SoundMenu.vue";
import StreamMenu from "./StreamMenu.vue";
import { HOVER_TEXT, ICON_BTN, TONE_TEXT } from "./toolbarClasses";

// The session's toolbar over the picture: a bar of controls, and a thin bar to hover or click when
// it is folded. What it shows (which controls, their icons, words and states, the menus' rows) is
// the `model` from `buildToolbar` (@cha/ui-spec's toolbar.json); this file only draws it. Props in,
// events out: the session view owns the player, the state and what each control does.
defineProps<{
  model: ToolbarModel;
  /** The environment's device, for the GPU badge. */
  gpu: { kind: DeviceKind; name?: string } | null;
}>();
const emit = defineEmits<{
  /** A control was clicked, by its spec id. */
  control: [id: string];
  /** A guest's Hand controls button. */
  hand: [id: string];
  /** A row in an open menu: a choice picked, a button pressed, the slider moved. */
  row: [menu: string, id: string, value?: string | Event, el?: HTMLSelectElement];
  closeMenu: [];
  /** The bar was asked to show. */
  expand: [];
  pointerEnter: [];
  pointerLeave: [];
  hide: [];
  /** The bar's height, for the stats panel to sit below it. */
  height: [px: number];
}>();

const bar = useTemplateRef<HTMLElement>("bar");
let watcher: ResizeObserver | null = null;
watch(bar, (el) => {
  watcher?.disconnect();
  watcher = null;
  if (!el) return;
  emit("height", el.offsetHeight);
  watcher = new ResizeObserver(() => emit("height", el.offsetHeight));
  watcher.observe(el);
});
onBeforeUnmount(() => watcher?.disconnect());

/** The spec names the icon by id; `check-ui-spec` checks every id in toolbar.json exists. */
const iconName = (c: ToolbarControl) => c.icon as IconName;
const iconButton = (c: ToolbarControl) => [ICON_BTN, c.hover_tone !== "none" && HOVER_TEXT[c.hover_tone], c.active && "bg-line/60", TONE_TEXT[c.tone]];
</script>

<template>
  <!-- The toolbar, folded: a thin bar to hover or click -->
  <button
    type="button"
    class="absolute top-0 left-1/2 z-30 -translate-x-1/2 px-6 pt-1.5 pb-3 transition-opacity duration-200"
    :class="model.visible ? 'pointer-events-none opacity-0' : 'opacity-100'"
    :tabindex="model.visible ? -1 : 0"
    :aria-label="model.folded_bar.aria_label"
    :title="model.folded_bar.tooltip"
    @pointerenter="emit('expand')"
    @click="emit('expand')"
    @focus="emit('expand')"
  >
    <span class="block h-1.5 w-24 rounded-full border border-line bg-panel/80 shadow backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none" />
  </button>

  <!-- Toolbar -->
  <div
    ref="bar"
    class="absolute inset-x-2 top-3 z-30 mx-auto flex w-max max-w-[calc(100%-1rem)] origin-top flex-wrap items-center justify-center gap-1 rounded-xl border border-line bg-panel/90 p-1.5 shadow-lg backdrop-blur transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none transition duration-200"
    :class="model.visible ? 'opacity-100' : 'pointer-events-none -translate-y-3 scale-y-50 opacity-0'"
    :inert="!model.visible"
    @pointerenter="emit('pointerEnter')"
    @pointerleave="emit('pointerLeave')"
    @focusin="emit('expand')"
  >
    <template v-for="c in model.controls" :key="c.id">
      <RouterLink v-if="c.id === 'back'" to="/" class="btn-ghost border-0 px-3 py-1.5 text-xs" :title="c.tooltip ?? undefined">{{ c.label }}</RouterLink>
      <GpuBadge v-else-if="c.id === 'gpu' && gpu" :kind="gpu.kind" :name="gpu.name" tense="is" />
      <span v-else-if="c.kind === 'label'" :class="c.id === 'viewers' ? 'px-2 text-xs text-ink-2' : 'max-w-48 truncate px-2 text-sm font-medium'" :title="c.tooltip ?? undefined">{{ c.label }}</span>
      <button
        v-else-if="c.id === 'take-control'"
        class="btn-ghost border-0 px-3 py-1.5 text-xs"
        :class="TONE_TEXT[c.tone]"
        :title="c.tooltip ?? undefined"
        @click="emit('control', c.id)"
      >
        {{ c.label }}
      </button>
      <ul v-else-if="c.kind === 'list'" class="flex flex-wrap items-center gap-x-1" :aria-label="c.aria_label ?? undefined">
        <li v-for="w in c.items" :key="w.id ?? w.text" class="flex items-center gap-1 px-2 text-xs text-ink-2">
          {{ w.text }}
          <button
            v-if="w.action"
            type="button"
            class="btn-ghost min-h-7 border-0 px-2 py-0.5 text-xs text-accent"
            :title="w.action.tooltip"
            @click="emit('hand', w.id!)"
          >
            {{ w.action.label }}
          </button>
        </li>
      </ul>

      <!-- Menus: stream settings, sound, controllers -->
      <div v-else-if="c.kind === 'menu'" class="relative" data-menu @keydown.esc="emit('closeMenu')">
        <button
          :class="iconButton(c)"
          aria-haspopup="true"
          :aria-expanded="c.active"
          :aria-controls="c.menu_dom_id ?? undefined"
          :aria-label="c.aria_label ?? undefined"
          :title="c.tooltip ?? undefined"
          @click="emit('control', c.id)"
        >
          <Icon :name="iconName(c)" class="size-4" />
          <span v-if="c.badge" class="absolute -top-0.5 -right-0.5 grid min-w-3.5 place-items-center rounded-full bg-accent px-1 text-[9px] leading-3.5 font-semibold text-accent-ink">{{ c.badge.text }}</span>
        </button>
        <StreamMenu v-if="c.menu && c.id === 'stream'" :menu="c.menu" @row="(id, v, el) => emit('row', 'stream', id, v, el)" />
        <SoundMenu v-else-if="c.menu && c.id === 'sound'" :menu="c.menu" @row="(id, e) => emit('row', 'sound', id, e)" />
        <ControllerMenu v-else-if="c.menu && c.id === 'controllers'" :menu="c.menu" @row="(id) => emit('row', 'controllers', id)" />
      </div>

      <!-- Buttons and switches -->
      <button
        v-else-if="c.kind === 'toggle' && c.id === 'stats'"
        :class="[iconButton(c), 'w-auto gap-1 px-2']"
        :aria-pressed="c.pressed ?? undefined"
        :aria-label="c.aria_label ?? undefined"
        :title="c.tooltip ?? undefined"
        @click="emit('control', c.id)"
      >
        <Icon :name="iconName(c)" class="size-4" />
        <span v-if="c.badge" class="font-mono text-xs font-semibold" :class="TONE_TEXT[c.badge.tone]" :title="c.badge.tooltip ?? undefined">{{ c.badge.text }}</span>
      </button>
      <button
        v-else
        :class="iconButton(c)"
        :disabled="c.disabled"
        :aria-pressed="c.pressed ?? undefined"
        :aria-label="c.aria_label ?? undefined"
        :title="c.tooltip ?? undefined"
        @click="emit('control', c.id)"
      >
        <Icon :name="iconName(c)" class="size-4" />
      </button>
    </template>

    <!-- Floats on the toolbar's lower edge, in the middle -->
    <button
      type="button"
      class="absolute top-full left-1/2 grid h-5 w-9 -translate-x-1/2 -translate-y-1/2 place-items-center rounded-full border border-line bg-panel text-ink-2 shadow transition hover:text-ink focus-visible:outline-2 focus-visible:outline-focus disabled:cursor-not-allowed disabled:opacity-50"
      :disabled="model.hide_tab.disabled"
      :aria-label="model.hide_tab.aria_label"
      :title="model.hide_tab.tooltip"
      @click="emit('hide')"
    >
      <Icon name="toolbar-fold" class="size-3.5" />
    </button>
  </div>
</template>

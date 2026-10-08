<script setup lang="ts">
// An app card's Launch: the main part launches the best place the portal found,
// named under it; the chevron opens every option (the disallowed ones say why
// and can't be chosen). Without placements (an older server, or still
// loading) it is a plain Launch button.
//
// When the app already has an environment, the main part is Open (to that
// environment) and the menu starts with "Launch new instance".
import { ChevronDown, MonitorPlay, Play } from "lucide-vue-next";
import { computed, nextTick, onBeforeUnmount, ref, watch } from "vue";

import type { Environment, PlacementChoice, Placements } from "../api";
import LaunchProgress from "./LaunchProgress.vue";
import { KIND_LABEL, autoOption, describe, nowhereToRun } from "../placements";

const props = defineProps<{
  /** The template's id, for element ids. */
  id: string;
  placements?: Placements;
  /** A launch of this app is under way. */
  busy: boolean;
  /** Not for this user (a guest). */
  disabled: boolean;
  /** The app's existing environment, if any: Open goes to it once it runs. */
  instance?: Environment;
  /** For a list row: the place line is for screen readers only, and the
   *  menu may be wider than the button. */
  compact?: boolean;
}>();
const emit = defineEmits<{ launch: [choice: PlacementChoice | null] }>();

const auto = computed(() => autoOption(props.placements));
// Nothing allowed anywhere: the main button has nothing to launch.
const nowhere = computed(() => !!props.placements && !props.placements.auto);
// Menu items before the placement options: "Launch new instance".
const lead = computed(() => (props.instance ? 1 : 0));
const menuId = computed(() => `${props.id}-launch-menu`);

const open = ref(false);
const root = ref<HTMLElement | null>(null);
const toggle = ref<HTMLButtonElement | null>(null);
const items = ref<HTMLElement[]>([]);

const isAuto = (o: { node: string; device: string }) =>
  props.placements?.auto?.node === o.node && props.placements.auto.device === o.device;

function setItem(index: number, el: HTMLElement | null) {
  if (el) items.value[index] = el;
  else delete items.value[index];
}

function focusItem(index: number) {
  const list = items.value.filter(Boolean);
  list[(index + list.length) % list.length]?.focus();
}

async function openMenu(focus: "first" | "last" = "first") {
  open.value = true;
  await nextTick();
  const list = items.value.filter(Boolean);
  // The best allowed one first, so Enter picks what Launch would.
  const best = props.placements?.options.findIndex((o) => isAuto(o)) ?? -1;
  focusItem(focus === "last" ? list.length - 1 : props.instance ? 0 : Math.max(best, 0));
}

function closeMenu(returnFocus = false) {
  open.value = false;
  if (returnFocus) toggle.value?.focus();
}

function choose(o: { node: string; device: string; allowed: boolean }) {
  if (!o.allowed) return;
  closeMenu(true);
  emit("launch", { node: o.node, device: o.device });
}

function launchNew() {
  if (nowhere.value) return;
  closeMenu(true);
  emit("launch", null);
}

function onToggleKey(event: KeyboardEvent) {
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    void openMenu(event.key === "ArrowUp" ? "last" : "first");
  }
}

function onMenuKey(event: KeyboardEvent) {
  const list = items.value.filter(Boolean);
  const at = list.indexOf(document.activeElement as HTMLElement);
  const move = (to: number) => {
    event.preventDefault();
    focusItem(to);
  };
  if (event.key === "ArrowDown") move(at + 1);
  else if (event.key === "ArrowUp") move(at - 1);
  else if (event.key === "Home") move(0);
  else if (event.key === "End") move(list.length - 1);
  else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    closeMenu(true);
  } else if (event.key === "Tab") closeMenu();
}

// A click elsewhere closes it.
function onPointerDown(event: PointerEvent) {
  if (open.value && root.value && !root.value.contains(event.target as Node)) closeMenu();
}
watch(open, (isOpen) => {
  if (isOpen) document.addEventListener("pointerdown", onPointerDown);
  else document.removeEventListener("pointerdown", onPointerDown);
});
onBeforeUnmount(() => document.removeEventListener("pointerdown", onPointerDown));
// What it listed may be gone by the next refresh.
watch(
  () => (props.instance ? 1 : 0) + (props.placements?.options.length ?? 0),
  (n) => {
    if (!n) open.value = false;
  },
);
</script>

<template>
  <div ref="root">
    <div class="relative flex">
      <RouterLink
        v-if="instance && instance.state === 'running'"
        :to="{ name: 'session', params: { id: instance.id } }"
        class="btn-primary min-h-11 flex-1 rounded-r-none"
        :aria-describedby="`${id}-launch-on`"
      >
        <MonitorPlay class="size-4" aria-hidden="true" />
        Open
      </RouterLink>
      <button
        v-else-if="instance"
        class="btn-primary min-h-11 flex-1 rounded-r-none"
        disabled
        :aria-describedby="`${id}-launch-on`"
      >
        {{ instance.state === "stopping" ? "Stopping…" : "Starting…" }}
      </button>
      <button
        v-else
        class="btn-primary min-h-11 flex-1"
        :class="placements && 'rounded-r-none'"
        :disabled="disabled || busy || nowhere"
        :aria-describedby="placements ? `${id}-launch-on` : undefined"
        @click="emit('launch', null)"
      >
        <Play class="size-4 fill-current" aria-hidden="true" />
        {{ busy ? "Launching…" : "Launch" }}
      </button>
      <button
        v-if="placements || instance"
        ref="toggle"
        class="btn-primary min-h-11 min-w-11 rounded-l-none border-l border-on-accent/30 px-2.5"
        :disabled="disabled || busy || (!instance && !placements?.options.length)"
        aria-haspopup="menu"
        :aria-expanded="open"
        :aria-controls="menuId"
        :aria-label="instance ? 'More ways to launch' : 'Choose where to launch'"
        :title="instance ? 'Launch new instance, or choose where' : 'Choose where to launch'"
        @click="open ? closeMenu() : openMenu()"
        @keydown="onToggleKey"
      >
        <ChevronDown class="size-5" aria-hidden="true" />
      </button>
      <ul
        v-if="open && (placements || instance)"
        :id="menuId"
        role="menu"
        :aria-label="instance ? 'Launch' : 'Where to launch'"
        class="absolute top-full right-0 left-0 z-20 mt-2 max-h-72 overflow-y-auto rounded-xl border border-line bg-panel p-1 text-left shadow-lg"
        :class="compact && 'left-auto min-w-64'"
        @keydown="onMenuKey"
      >
        <li v-if="instance" role="none">
          <button
            :ref="(el) => setItem(0, el as HTMLElement | null)"
            role="menuitem"
            type="button"
            class="flex w-full items-center gap-2 rounded-lg px-3 py-2 text-left text-sm font-medium transition focus:bg-panel-2 focus:outline-none"
            :class="nowhere ? 'cursor-not-allowed text-ink-3' : 'text-ink hover:bg-panel-2'"
            :aria-disabled="nowhere"
            @click="launchNew"
          >
            <Play class="size-4 fill-current" aria-hidden="true" />
            Launch new instance
          </button>
        </li>
        <li v-if="instance && placements?.options.length" role="none" class="px-3 pt-2 pb-1 text-2xs text-ink-3">
          Or launch a new instance on
        </li>
        <li v-for="(o, i) in placements?.options ?? []" :key="`${o.node}/${o.device}`" role="none">
          <button
            :ref="(el) => setItem(i + lead, el as HTMLElement | null)"
            role="menuitem"
            type="button"
            class="flex w-full flex-col gap-0.5 rounded-lg px-3 py-2 text-left text-sm transition focus:bg-panel-2 focus:outline-none"
            :class="o.allowed ? 'text-ink hover:bg-panel-2' : 'cursor-not-allowed text-ink-3'"
            :aria-disabled="!o.allowed"
            @click="choose(o)"
          >
            <span class="flex items-center gap-2">
              <span class="min-w-0 flex-1 truncate font-medium">{{ o.label }}</span>
              <span v-if="isAuto(o)" class="rounded-full bg-accent-soft px-2 py-0.5 text-2xs text-accent">Best</span>
              <span class="rounded-full border border-line px-2 py-0.5 text-2xs text-ink-3">{{ KIND_LABEL[o.kind] }}</span>
            </span>
            <span class="truncate text-xs text-ink-3">on {{ o.nodeName }}</span>
            <span v-if="o.reason" class="text-xs" :class="o.allowed ? 'text-warn' : 'text-ink-3'">
              {{ o.allowed ? "" : "Not available: " }}{{ o.reason }}
            </span>
            <span v-if="o.note" class="text-xs text-ink-3">{{ o.note }}</span>
          </button>
        </li>
      </ul>
    </div>
    <!-- Where it would run: a dot (ok when somewhere is allowed, warn when nowhere) and always the words. -->
    <p
      v-if="instance"
      :id="`${id}-launch-on`"
      class="flex items-center gap-2 text-xs text-ink-3"
      :class="compact ? 'sr-only' : 'mt-2'"
      aria-live="polite"
    >
      <span class="size-2 shrink-0 rounded-full" :class="instance.state === 'running' ? 'bg-ok' : 'bg-warn'" aria-hidden="true" />
      <span class="min-w-0 truncate">
        {{ instance.state === "running" ? "Running" : instance.state === "stopping" ? "Stopping" : "Starting" }}
        on {{ instance.nodeName ?? "a removed node" }}
      </span>
    </p>
    <p
      v-else-if="placements"
      :id="`${id}-launch-on`"
      class="flex items-center gap-2 text-xs text-ink-3"
      :class="compact ? 'sr-only' : 'mt-2'"
      aria-live="polite"
    >
      <span class="size-2 shrink-0 rounded-full" :class="auto ? 'bg-ok' : 'bg-warn'" aria-hidden="true" />
      <span class="min-w-0 truncate">
        <template v-if="auto">on {{ describe(auto) }}</template>
        <template v-else>{{ nowhereToRun(placements) }}</template>
      </span>
    </p>
    <LaunchProgress v-if="instance?.state === 'starting'" :env="instance" compact class="mt-1.5" />
  </div>
</template>

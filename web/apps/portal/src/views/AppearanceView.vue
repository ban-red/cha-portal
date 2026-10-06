<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";

import SegmentedControl from "../components/SegmentedControl.vue";
import { THEMES, type ThemePrefs } from "../themes";
import { readSystemEnv, resolvePrefs, useTheme } from "../themes/runtime";
import { usePrefsSync } from "../themes/sync";

const { prefs, resolved, setPrefs } = useTheme();
const sync = usePrefsSync();

// What "System" means right now, from the OS alone (not from the user's other choices).
const env = ref(readSystemEnv());
const queries = [
  "(prefers-color-scheme: light)",
  "(prefers-contrast: more)",
  "(prefers-reduced-motion: reduce)",
  "(prefers-reduced-transparency: reduce)",
];
const lists: MediaQueryList[] = [];
const refresh = () => (env.value = readSystemEnv());
onMounted(() => {
  for (const q of queries) {
    const m = matchMedia(q);
    m.addEventListener?.("change", refresh);
    lists.push(m);
  }
});
onBeforeUnmount(() => lists.forEach((m) => m.removeEventListener?.("change", refresh)));

const system = computed(() => resolvePrefs({ ...prefs.value, appearance: "system", contrast: "system", motion: "system", transparency: "system" }, env.value));
const cap = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);

const appearanceOptions = computed(() => [
  { value: "system", label: `System (${cap(system.value.appearance)})` },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
] as const);
const contrastOptions = computed(() => [
  { value: "system", label: `System (${cap(system.value.contrast)})` },
  { value: "standard", label: "Standard" },
  { value: "more", label: "More" },
] as const);
const motionOptions = computed(() => [
  { value: "system", label: `System (${system.value.motion === "reduced" ? "Reduced" : "Full"})` },
  { value: "reduced", label: "Reduced" },
] as const);
const transparencyOptions = computed(() => [
  { value: "system", label: `System (${system.value.transparency === "reduced" ? "Reduced" : "Full"})` },
  { value: "reduced", label: "Reduced" },
] as const);

function set<K extends keyof ThemePrefs>(key: K, value: ThemePrefs[K]) {
  setPrefs({ [key]: value } as Partial<ThemePrefs>);
}

// Theme tiles: a radiogroup with a roving tabindex; arrow keys move and select.
const tiles = ref<HTMLButtonElement[]>([]);
function pickTheme(i: number) {
  const n = THEMES.length;
  const t = THEMES[((i % n) + n) % n]!;
  set("theme", t.id);
  void nextTick(() => tiles.value[THEMES.indexOf(t)]?.focus());
}
function onTileKey(e: KeyboardEvent, i: number) {
  const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
  if (step !== undefined) pickTheme(i + step);
  else if (e.key === "Home") pickTheme(0);
  else if (e.key === "End") pickTheme(THEMES.length - 1);
  else return;
  e.preventDefault();
}
</script>

<template>
  <div class="max-w-3xl space-y-6">
    <p class="text-sm text-ink-2">Changes apply as you pick them and follow you to your other devices.</p>

    <section class="card space-y-4 p-5" aria-labelledby="theme-h">
      <div>
        <h2 id="theme-h" class="text-lg font-semibold tracking-tight">Theme</h2>
        <p class="mt-1 text-sm text-ink-2">The colours of the portal. Each theme has a light and a dark look.</p>
      </div>
      <div role="radiogroup" aria-labelledby="theme-h" class="grid grid-cols-[repeat(auto-fill,minmax(12rem,1fr))] gap-3">
        <button
          v-for="(t, i) in THEMES"
          :key="t.id"
          :ref="(el) => (tiles[i] = el as HTMLButtonElement)"
          type="button"
          role="radio"
          :aria-checked="prefs.theme === t.id"
          :tabindex="prefs.theme === t.id ? 0 : -1"
          class="relative min-h-11 rounded-xl border bg-panel p-2.5 text-left transition hover:bg-panel-2"
          :class="prefs.theme === t.id ? 'border-accent ring-2 ring-accent' : 'border-line-strong'"
          @click="set('theme', t.id)"
          @keydown="onTileKey($event, i)"
        >
          <!-- A miniature in this theme's own colours (scoped selectors in themes/<id>.css),
               following the current appearance and contrast. -->
          <div
            aria-hidden="true"
            :data-theme-preview="t.id"
            :data-appearance="resolved.appearance"
            :data-contrast="resolved.contrast"
            class="flex h-20 overflow-hidden rounded-lg border border-line bg-canvas"
          >
            <div class="w-1/4 space-y-1 border-r border-line bg-panel p-1.5">
              <div class="h-1.5 rounded-full bg-accent"></div>
              <div class="h-1 rounded-full bg-ink-3"></div>
              <div class="h-1 w-3/4 rounded-full bg-ink-3"></div>
            </div>
            <div class="flex flex-1 items-center p-2">
              <div class="w-full space-y-1.5 rounded-md border border-line bg-panel p-1.5">
                <div class="h-1.5 w-2/3 rounded-full bg-ink"></div>
                <div class="h-1 w-full rounded-full bg-ink-3"></div>
                <div class="h-3 w-1/2 rounded bg-accent-fill"></div>
              </div>
            </div>
          </div>
          <div class="mt-2 flex items-center justify-between gap-2 px-0.5">
            <span class="text-sm" :class="prefs.theme === t.id ? 'font-semibold text-ink' : 'text-ink-2'">{{ t.name }}</span>
            <svg
              v-if="prefs.theme === t.id"
              class="size-5 shrink-0 text-accent"
              viewBox="0 0 20 20"
              fill="none"
              stroke="currentColor"
              stroke-width="2"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <circle cx="10" cy="10" r="8" />
              <path d="M6.5 10.5l2.5 2.5 4.5-5" />
            </svg>
          </div>
        </button>
      </div>
    </section>

    <section class="card space-y-4 p-5" aria-labelledby="appearance-h">
      <div>
        <h2 id="appearance-h" class="text-lg font-semibold tracking-tight">Appearance</h2>
        <p class="mt-1 text-sm text-ink-2">Light or dark. System follows your device and changes with it.</p>
      </div>
      <SegmentedControl label="Appearance" :model-value="prefs.appearance" :options="appearanceOptions" @update:model-value="set('appearance', $event)" />
    </section>

    <section class="card space-y-4 p-5" aria-labelledby="contrast-h">
      <div>
        <h2 id="contrast-h" class="text-lg font-semibold tracking-tight">Contrast</h2>
        <p class="mt-1 text-sm text-ink-2">Stronger edges and text that is easier to read.</p>
      </div>
      <SegmentedControl
        label="Contrast"
        describedby="contrast-note"
        :model-value="prefs.contrast"
        :options="contrastOptions"
        @update:model-value="set('contrast', $event)"
      />
      <p id="contrast-note" class="text-xs text-ink-3">More raises text to at least 7:1 against its background.</p>
    </section>

    <section class="card space-y-4 p-5" aria-labelledby="motion-h">
      <div>
        <h2 id="motion-h" class="text-lg font-semibold tracking-tight">Motion</h2>
        <p class="mt-1 text-sm text-ink-2">Reduced turns off sliding and fading, and nothing loops.</p>
      </div>
      <SegmentedControl label="Motion" :model-value="prefs.motion" :options="motionOptions" @update:model-value="set('motion', $event)" />
    </section>

    <section class="card space-y-4 p-5" aria-labelledby="transparency-h">
      <div>
        <h2 id="transparency-h" class="text-lg font-semibold tracking-tight">Transparency</h2>
        <p class="mt-1 text-sm text-ink-2">Reduced swaps see-through panels for solid ones.</p>
      </div>
      <SegmentedControl
        label="Transparency"
        :model-value="prefs.transparency"
        :options="transparencyOptions"
        @update:model-value="set('transparency', $event)"
      />
    </section>

    <p aria-live="polite" class="min-h-5 text-sm" :class="sync.status.value === 'error' ? 'text-danger' : 'text-ink-3'">
      <template v-if="sync.status.value === 'error'">{{ sync.error.value }}</template>
      <template v-else-if="sync.status.value === 'saving'">Saving…</template>
      <template v-else-if="sync.status.value === 'saved'">Saved</template>
    </p>
  </div>
</template>

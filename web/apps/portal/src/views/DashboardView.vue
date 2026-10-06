<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { ArrowUpDown, LayoutGrid, List, Monitor, MonitorPlay, Pin, Search, Square, X } from "lucide-vue-next";
import { computed, nextTick, ref } from "vue";

import { ApiError, api, type Environment, type EnvironmentState, type PlacementChoice, type Template } from "../api";
import AppCard from "../components/AppCard.vue";
import EnvironmentLog from "../components/EnvironmentLog.vue";
import FormError from "../components/FormError.vue";
import SegmentedControl from "../components/SegmentedControl.vue";
import WarningNote from "../components/WarningNote.vue";
import { ago, dateTime } from "../format";
import { useSession } from "../stores/session";
import { useAppFps } from "../appFps";
import { useControllerApps } from "../controllerKinds";
import { PLACEMENTS_KEY, usePlacements } from "../placements";
import { STORAGE_KEY } from "../storage";
import { MAX_PINNED } from "../themes";
import { useTheme } from "../themes/runtime";

const session = useSession();
const queryClient = useQueryClient();

const catalog = useQuery({ queryKey: ["catalog"], queryFn: api.catalog, staleTime: 60_000 });

const busy = (states: EnvironmentState[]) => states.some((s) => s === "starting" || s === "stopping");
// Poll fast while something is starting or stopping, so the change shows promptly.
const environments = useQuery({
  queryKey: ["environments"],
  queryFn: api.environments,
  refetchInterval: (query) => (busy((query.state.data ?? []).map((e) => e.state)) ? 1000 : 5000),
});

// Which apps keep the user's data. Where the server can't say yet (404) nothing is marked.
const storage = useQuery({ queryKey: STORAGE_KEY, queryFn: api.storage, staleTime: 30_000 });
const saved = computed(
  () => new Set((storage.data.value?.apps ?? []).filter((a) => a.persistent).map((a) => a.template)),
);

// Which controller each app sees, chosen right on its card (from its next launch).
const controllerApps = useControllerApps();
// And the frame rate it runs at, the same way.
const appFps = useAppFps();

// Where each app would run, and the best place: refreshed with the nodes' live
// usage while the page is visible. Guests can't launch, so they ask for none.
const placements = usePlacements(computed(() => session.user?.role !== "guest"));

const live = computed(() =>
  (environments.data.value ?? []).filter((e) => e.state !== "destroyed" && e.state !== "failed"),
);
const ended = computed(() =>
  (environments.data.value ?? []).filter((e) => e.state === "destroyed" || e.state === "failed").slice(0, 8),
);

// ---- find, filter, sort, pin ---------------------------------------------------------------

const { prefs, setPrefs } = useTheme();

type Filter = "all" | "apps" | "desktops" | "pinned";
const filter = ref<Filter>("all");
const FILTERS = [
  { value: "all", label: "All" },
  { value: "apps", label: "Applications", icon: LayoutGrid },
  { value: "desktops", label: "Desktops", icon: Monitor },
  { value: "pinned", label: "Pinned", icon: Pin },
] as const;

const view = computed({
  get: () => prefs.value.envView,
  set: (v: "grid" | "list") => setPrefs({ envView: v }),
});
const VIEWS = [
  { value: "grid", label: "Grid view", icon: LayoutGrid },
  { value: "list", label: "List view", icon: List },
] as const;
const sort = computed({
  get: () => prefs.value.envSort,
  set: (v: "name" | "recent") => setPrefs({ envSort: v }),
});

const pinned = computed(() => new Set(prefs.value.pinned));
function togglePin(id: string) {
  const ids = prefs.value.pinned;
  setPrefs({ pinned: ids.includes(id) ? ids.filter((x) => x !== id) : [...ids, id].slice(-MAX_PINNED) });
}

/** The latest launch of each app, from the environments we know of. */
const lastUsed = computed(() => {
  const m = new Map<string, number>();
  for (const e of environments.data.value ?? []) m.set(e.templateId, Math.max(m.get(e.templateId) ?? 0, e.createdAt));
  return m;
});

const fold = (s: string) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();

const query = ref("");
const searchOpen = ref(false); // a phone shows the field only after the icon is pressed
const searchInput = ref<HTMLInputElement | null>(null);
const searchButton = ref<HTMLButtonElement | null>(null);

async function openSearch() {
  searchOpen.value = true;
  await nextTick();
  searchInput.value?.focus();
}
function closeSearch() {
  query.value = "";
  searchOpen.value = false;
  void nextTick(() => searchButton.value?.focus());
}
function clearSearch() {
  query.value = "";
  searchInput.value?.focus();
}
function onSearchEscape() {
  if (query.value) query.value = "";
  else if (searchOpen.value) closeSearch();
}
function onSearchBlur() {
  if (!query.value) searchOpen.value = false;
}

const shown = computed(() => {
  const q = fold(query.value.trim());
  const list = (catalog.data.value ?? []).filter((t) => {
    if (filter.value === "apps" && t.class === "desktop") return false;
    if (filter.value === "desktops" && t.class !== "desktop") return false;
    if (filter.value === "pinned" && !pinned.value.has(t.id)) return false;
    return !q || fold(`${t.name} ${t.description}`).includes(q);
  });
  const byName = (a: Template, b: Template) => a.name.localeCompare(b.name);
  return list.sort((a, b) => {
    const pin = Number(pinned.value.has(b.id)) - Number(pinned.value.has(a.id));
    if (pin) return pin;
    if (sort.value === "recent") {
      const recent = (lastUsed.value.get(b.id) ?? 0) - (lastUsed.value.get(a.id) ?? 0);
      if (recent) return recent;
    }
    return byName(a, b);
  });
});

const error = ref<string | null>(null);
const message = (err: unknown, fallback: string) => (err instanceof ApiError ? err.message : fallback);

const launch = useMutation({
  mutationFn: (v: { template: Template; choice: PlacementChoice | null }) =>
    api.launch(v.template.id, v.choice ?? undefined),
  onMutate: () => (error.value = null),
  onSuccess: () => {
    void queryClient.invalidateQueries({ queryKey: ["environments"] });
    // The new environment changes what each device is worth.
    void queryClient.invalidateQueries({ queryKey: PLACEMENTS_KEY });
  },
  onError: (err) => (error.value = message(err, "Couldn't launch it.")),
});

const stop = useMutation({
  mutationFn: (e: Environment) => api.stopEnvironment(e.id),
  onMutate: () => (error.value = null),
  onSuccess: () => queryClient.invalidateQueries({ queryKey: ["environments"] }),
  onError: (err) => (error.value = message(err, "Couldn't stop it.")),
});

const STATES: Record<EnvironmentState, { text: string; dot: string }> = {
  starting: { text: "Starting", dot: "bg-warn animate-pulse" },
  running: { text: "Running", dot: "bg-ok" },
  stopping: { text: "Stopping", dot: "bg-warn animate-pulse" },
  destroyed: { text: "Ended", dot: "bg-ink-3" },
  failed: { text: "Failed", dot: "bg-danger" },
};
</script>

<template>
  <!-- The search lives in the shell's header. -->
  <Teleport to="#page-actions" defer>
    <div
      class="flex items-center"
      :class="searchOpen && 'max-sm:absolute max-sm:inset-0 max-sm:z-10 max-sm:gap-2 max-sm:bg-canvas max-sm:px-4'"
    >
      <button
        v-show="!searchOpen"
        ref="searchButton"
        type="button"
        class="inline-flex size-9 items-center justify-center rounded-lg border border-line-strong text-ink-2 transition hover:border-ink-3 hover:text-ink pointer-coarse:size-11 sm:hidden"
        aria-label="Search environments"
        @click="openSearch"
      >
        <Search class="size-5" aria-hidden="true" />
      </button>
      <div class="relative" :class="searchOpen ? 'max-sm:flex-1' : 'max-sm:hidden'">
        <label for="env-search" class="sr-only">Search environments</label>
        <Search class="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-ink-3" aria-hidden="true" />
        <input
          id="env-search"
          ref="searchInput"
          v-model="query"
          type="search"
          autocomplete="off"
          placeholder="Search environments…"
          class="field min-h-9 pl-9 pointer-coarse:min-h-11 sm:w-56 lg:w-72"
          @keydown.esc.prevent="onSearchEscape"
          @blur="onSearchBlur"
        />
      </div>
      <button
        v-if="searchOpen"
        type="button"
        class="inline-flex size-9 shrink-0 items-center justify-center rounded-lg text-ink-2 hover:bg-panel-2 hover:text-ink pointer-coarse:size-11 sm:hidden"
        aria-label="Close search"
        @mousedown.prevent
        @click="closeSearch"
      >
        <X class="size-5" aria-hidden="true" />
      </button>
    </div>
  </Teleport>

  <div class="mx-auto max-w-[100rem] space-y-10">
    <section class="space-y-4" aria-labelledby="launch-heading">
      <h2 id="launch-heading" class="sr-only">Launch</h2>

      <div class="flex flex-wrap items-center gap-x-3 gap-y-3">
        <div class="max-w-full min-w-0 max-sm:w-full">
          <SegmentedControl v-model="filter" :options="FILTERS" label="Show" bare />
        </div>
        <div class="ml-auto flex items-center gap-2">
          <div class="relative">
            <label for="env-sort" class="sr-only">Sort environments</label>
            <ArrowUpDown class="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-ink-3" aria-hidden="true" />
            <select id="env-sort" v-model="sort" class="field min-h-9 w-auto pr-8 pl-9 pointer-coarse:min-h-11">
              <option value="name">Sort: Name</option>
              <option value="recent">Sort: Recently used</option>
            </select>
          </div>
          <SegmentedControl v-model="view" :options="VIEWS" label="View" bare icon-only />
        </div>
      </div>

      <p v-if="catalog.isPending.value" class="text-sm text-ink-3">Loading…</p>
      <FormError v-else-if="catalog.isError.value" :message="catalog.error.value?.message ?? 'Failed to load'" />
      <template v-else>
        <p class="sr-only" role="status">{{ shown.length }} {{ shown.length === 1 ? "environment" : "environments" }}</p>
        <div v-if="!shown.length" class="card px-6 py-10 text-center">
          <template v-if="query.trim()">
            <p class="font-medium">No environments match “{{ query.trim() }}”</p>
            <button type="button" class="btn-ghost mt-4 min-h-9 pointer-coarse:min-h-11" @click="clearSearch">
              <X class="size-4" aria-hidden="true" />
              Clear search
            </button>
          </template>
          <template v-else-if="filter === 'pinned'">
            <p class="font-medium">Nothing pinned yet</p>
            <p class="mt-1 text-sm text-ink-2">Use the pin on an app to keep it here, and first in every list.</p>
          </template>
          <template v-else>
            <p class="font-medium">No environments in this view</p>
            <p class="mt-1 text-sm text-ink-2">Try another filter.</p>
          </template>
        </div>
        <div v-else class="@container">
          <ul
            :class="
              view === 'grid'
                ? 'grid grid-cols-[repeat(auto-fill,minmax(16rem,1fr))] gap-4'
                : 'flex flex-col gap-3'
            "
          >
            <li v-for="t in shown" :key="t.id" class="min-w-0">
              <AppCard
                class="h-full"
                :template="t"
                :view="view"
                :pinned="pinned.has(t.id)"
                :saved="saved.has(t.id)"
                :controller="
                  session.user?.role !== 'guest' && !controllerApps.missing.value
                    ? controllerApps.byTemplate.value.get(t.id)
                    : undefined
                "
                :fps="
                  session.user?.role !== 'guest' && !appFps.missing.value ? appFps.byTemplate.value.get(t.id) : undefined
                "
                :controller-error="controllerApps.errors[t.id]"
                :fps-error="appFps.errors[t.id]"
                :running="live.some((e) => e.templateId === t.id)"
                :placements="placements.missing.value ? undefined : placements.byTemplate.value[t.id]"
                :busy="launch.isPending.value && launch.variables.value?.template.id === t.id"
                :disabled="session.user?.role === 'guest'"
                @launch="(choice) => launch.mutate({ template: t, choice })"
                @pin="togglePin(t.id)"
                @choose-controller="controllerApps.choose(controllerApps.byTemplate.value.get(t.id)!, $event)"
                @choose-fps="appFps.choose(appFps.byTemplate.value.get(t.id)!, $event)"
              />
            </li>
          </ul>
        </div>
      </template>
    </section>

    <FormError :message="error" />

    <section class="space-y-4" aria-labelledby="live-heading">
      <h2 id="live-heading" class="text-xl font-semibold tracking-tight">Your environments</h2>
      <p v-if="environments.isPending.value" class="text-sm text-ink-3">Loading…</p>
      <FormError
        v-else-if="environments.isError.value"
        :message="environments.error.value?.message ?? 'Failed to load'"
      />
      <div v-else-if="!live.length" class="card px-6 py-10 text-center">
        <p class="font-medium">Nothing running</p>
        <p class="mt-1 text-sm text-ink-2">Launch one above. It runs on a node until you stop it.</p>
      </div>
      <ul v-else class="card divide-y divide-line">
        <li v-for="e in live" :key="e.id" class="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-3">
          <span class="size-2 shrink-0 rounded-full" :class="STATES[e.state].dot" aria-hidden="true" />
          <div class="min-w-0 flex-1">
            <p class="truncate font-medium">{{ e.templateName }}</p>
            <p class="truncate text-xs text-ink-3" :title="dateTime(e.createdAt)">
              {{ STATES[e.state].text }} on {{ e.nodeName ?? "a removed node" }} · started {{ ago(e.createdAt) }}
            </p>
          </div>
          <RouterLink
            v-if="e.state === 'running'"
            :to="{ name: 'session', params: { id: e.id } }"
            class="btn-primary min-h-9 shrink-0 px-3 pointer-coarse:min-h-11"
          >
            <MonitorPlay class="size-4" aria-hidden="true" />
            Connect
          </RouterLink>
          <button
            class="btn-ghost min-h-9 shrink-0 px-3 hover:border-danger/60 hover:text-danger pointer-coarse:min-h-11"
            :disabled="e.state === 'stopping' || (stop.isPending.value && stop.variables.value?.id === e.id)"
            @click="stop.mutate(e)"
          >
            <Square class="size-4" aria-hidden="true" />
            Stop
          </button>
          <WarningNote :message="e.warning" class="basis-full" />
        </li>
      </ul>
    </section>

    <section v-if="ended.length" class="space-y-4" aria-labelledby="ended-heading">
      <h2 id="ended-heading" class="text-xl font-semibold tracking-tight">Recently ended</h2>
      <ul class="card divide-y divide-line">
        <li v-for="e in ended" :key="e.id" class="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-2.5 text-sm">
          <span class="size-2 shrink-0 rounded-full" :class="STATES[e.state].dot" aria-hidden="true" />
          <span class="w-36 shrink-0 truncate text-ink-2">{{ e.templateName }}</span>
          <span class="min-w-0 flex-1 break-words" :class="e.state === 'failed' ? 'text-danger' : 'text-ink-3'">
            {{ STATES[e.state].text }}<template v-if="e.detail">: {{ e.detail }}</template>
          </span>
          <span class="shrink-0 text-xs text-ink-3" :title="dateTime(e.updatedAt)">{{ ago(e.updatedAt) }}</span>
          <EnvironmentLog :log="e.log" class="basis-full" />
        </li>
      </ul>
    </section>
  </div>
</template>

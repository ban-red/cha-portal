<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, ref } from "vue";

import { ApiError, api, type Environment, type EnvironmentState, type PlacementChoice, type Template } from "../api";
import EnvironmentLog from "../components/EnvironmentLog.vue";
import FormError from "../components/FormError.vue";
import LaunchButton from "../components/LaunchButton.vue";
import WarningNote from "../components/WarningNote.vue";
import { ago, dateTime } from "../format";
import { useSession } from "../stores/session";
import { FPS_CHOICES, useAppFps } from "../appFps";
import { KINDS, kindLabel, useControllerApps } from "../controllerKinds";
import { PLACEMENTS_KEY, usePlacements } from "../placements";
import { STORAGE_KEY } from "../storage";

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

const GLYPHS: Record<string, string> = { browser: "◎", desktop: "▦", test: "▤", gaming: "◈" };
const glyph = (t: Template) => GLYPHS[t.class] ?? "⧉";

const STATES: Record<EnvironmentState, { text: string; dot: string }> = {
  starting: { text: "Starting", dot: "bg-warn animate-pulse" },
  running: { text: "Running", dot: "bg-ok" },
  stopping: { text: "Stopping", dot: "bg-warn animate-pulse" },
  destroyed: { text: "Ended", dot: "bg-ink-3" },
  failed: { text: "Failed", dot: "bg-danger" },
};
</script>

<template>
  <div class="mx-auto max-w-5xl space-y-8">
    <section class="space-y-3">
      <h2 class="text-sm font-medium tracking-wide text-ink-2 uppercase">Launch</h2>
      <p v-if="catalog.isPending.value" class="text-sm text-ink-3">Loading…</p>
      <FormError v-else-if="catalog.isError.value" :message="catalog.error.value?.message ?? 'Failed to load'" />
      <div v-else class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <article v-for="t in catalog.data.value" :key="t.id" class="card flex flex-col p-4">
          <div class="mb-3 flex items-start justify-between gap-2">
            <div class="grid size-10 place-items-center rounded-lg border border-line bg-panel-2 text-xl text-accent">
              {{ glyph(t) }}
            </div>
            <RouterLink
              v-if="saved.has(t.id)"
              :to="{ name: 'storage' }"
              title="Data kept between launches"
              class="inline-flex items-center gap-1 rounded-full border border-line-strong px-2 py-0.5 text-2xs text-ink-3 transition hover:border-ink-3 hover:text-ink-2"
            >
              <svg viewBox="0 0 16 16" class="size-3" fill="none" stroke="currentColor" stroke-width="1.5" aria-hidden="true">
                <ellipse cx="8" cy="4" rx="5" ry="2" />
                <path d="M3 4v4c0 1.1 2.2 2 5 2s5-.9 5-2V4M3 8v4c0 1.1 2.2 2 5 2s5-.9 5-2V8" />
              </svg>
              Saved<span class="sr-only">: data kept between launches. Open storage settings.</span>
            </RouterLink>
          </div>
          <h3 class="font-semibold">{{ t.name }}</h3>
          <p class="mt-1 flex-1 text-sm text-ink-2">{{ t.description }}</p>
          <div
            v-if="
              session.user?.role !== 'guest' &&
              ((!controllerApps.missing.value && controllerApps.byTemplate.value.get(t.id)) ||
                (!appFps.missing.value && appFps.byTemplate.value.get(t.id)))
            "
            class="mt-3"
          >
            <div class="grid grid-cols-2 gap-2">
              <div v-if="!controllerApps.missing.value && controllerApps.byTemplate.value.get(t.id)">
                <label :for="`${t.id}-controller`" class="mb-1 block text-xs text-ink-3">Controller</label>
                <select
                  :id="`${t.id}-controller`"
                  :value="controllerApps.byTemplate.value.get(t.id)?.kind ?? ''"
                  class="field w-full py-1 text-xs"
                  title="The controller this app sees, from its next launch"
                  @change="controllerApps.choose(controllerApps.byTemplate.value.get(t.id)!, $event)"
                >
                  <option value="">Default ({{ kindLabel(controllerApps.byTemplate.value.get(t.id)!.default) }})</option>
                  <option v-for="k in KINDS" :key="k.kind" :value="k.kind">{{ k.label }}</option>
                </select>
              </div>
              <div v-if="!appFps.missing.value && appFps.byTemplate.value.get(t.id)">
                <label :for="`${t.id}-fps`" class="mb-1 block text-xs text-ink-3">Frame rate</label>
                <select
                  :id="`${t.id}-fps`"
                  :value="appFps.byTemplate.value.get(t.id)?.fps ?? ''"
                  class="field w-full py-1 text-xs"
                  title="Your screen needs to refresh this fast for it to show; 120 needs a 120 Hz display. From the next launch"
                  @change="appFps.choose(appFps.byTemplate.value.get(t.id)!, $event)"
                >
                  <option value="">Default ({{ appFps.byTemplate.value.get(t.id)!.defaultFps }} fps)</option>
                  <option v-for="f in FPS_CHOICES" :key="f" :value="f">{{ f }} fps</option>
                </select>
              </div>
            </div>
            <p v-if="live.some((e) => e.templateId === t.id)" class="mt-1 text-2xs text-ink-3">
              Running: a change applies when you stop it and launch it again.
            </p>
            <FormError v-if="controllerApps.errors[t.id]" polite class="mt-1" :message="controllerApps.errors[t.id] ?? null" />
            <FormError v-if="appFps.errors[t.id]" polite class="mt-1" :message="appFps.errors[t.id] ?? null" />
          </div>
          <LaunchButton
            :id="t.id"
            :placements="placements.missing.value ? undefined : placements.byTemplate.value[t.id]"
            :busy="launch.isPending.value && launch.variables.value?.template.id === t.id"
            :disabled="session.user?.role === 'guest'"
            @launch="(choice) => launch.mutate({ template: t, choice })"
          />
        </article>
      </div>
    </section>

    <FormError :message="error" />

    <section class="space-y-3">
      <h2 class="text-sm font-medium tracking-wide text-ink-2 uppercase">Your environments</h2>
      <p v-if="environments.isPending.value" class="text-sm text-ink-3">Loading…</p>
      <FormError
        v-else-if="environments.isError.value"
        :message="environments.error.value?.message ?? 'Failed to load'"
      />
      <div v-else-if="!live.length" class="card px-6 py-10 text-center">
        <p class="font-medium">Nothing running</p>
        <p class="mt-1 text-sm text-ink-2">Launch one above. It runs on a node until you stop it.</p>
      </div>
      <div v-else class="card divide-y divide-line">
        <div v-for="e in live" :key="e.id" class="flex flex-wrap items-center gap-x-4 gap-y-1 px-4 py-3">
          <span class="size-2 shrink-0 rounded-full" :class="STATES[e.state].dot" />
          <div class="min-w-0 flex-1">
            <p class="truncate font-medium">{{ e.templateName }}</p>
            <p class="truncate text-xs text-ink-3" :title="dateTime(e.createdAt)">
              {{ STATES[e.state].text }} on {{ e.nodeName ?? "a removed node" }} · started {{ ago(e.createdAt) }}
            </p>
          </div>
          <RouterLink
            v-if="e.state === 'running'"
            :to="{ name: 'session', params: { id: e.id } }"
            class="btn-primary shrink-0 px-3 py-1 text-xs"
          >
            Connect
          </RouterLink>
          <button
            class="btn-ghost shrink-0 px-3 py-1 text-xs hover:border-danger/60 hover:text-danger"
            :disabled="e.state === 'stopping' || (stop.isPending.value && stop.variables.value?.id === e.id)"
            @click="stop.mutate(e)"
          >
            Stop
          </button>
          <WarningNote :message="e.warning" class="basis-full" />
        </div>
      </div>
    </section>

    <section v-if="ended.length" class="space-y-3">
      <h2 class="text-sm font-medium tracking-wide text-ink-2 uppercase">Recently ended</h2>
      <div class="card divide-y divide-line">
        <div v-for="e in ended" :key="e.id" class="flex flex-wrap items-center gap-x-4 gap-y-2 px-4 py-2.5 text-sm">
          <span class="size-2 shrink-0 rounded-full" :class="STATES[e.state].dot" />
          <span class="w-36 shrink-0 truncate text-ink-2">{{ e.templateName }}</span>
          <span class="min-w-0 flex-1 break-words" :class="e.state === 'failed' ? 'text-danger' : 'text-ink-3'">
            {{ STATES[e.state].text }}<template v-if="e.detail">: {{ e.detail }}</template>
          </span>
          <span class="shrink-0 text-xs text-ink-3" :title="dateTime(e.updatedAt)">{{ ago(e.updatedAt) }}</span>
          <EnvironmentLog :log="e.log" class="basis-full" />
        </div>
      </div>
    </section>
  </div>
</template>

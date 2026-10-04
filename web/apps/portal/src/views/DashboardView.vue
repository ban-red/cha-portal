<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, ref } from "vue";

import { ApiError, api, type Environment, type EnvironmentState, type Template } from "../api";
import FormError from "../components/FormError.vue";
import { ago, dateTime } from "../format";
import { useSession } from "../stores/session";

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

const live = computed(() =>
  (environments.data.value ?? []).filter((e) => e.state !== "destroyed" && e.state !== "failed"),
);
const ended = computed(() =>
  (environments.data.value ?? []).filter((e) => e.state === "destroyed" || e.state === "failed").slice(0, 8),
);

const error = ref<string | null>(null);
const message = (err: unknown, fallback: string) => (err instanceof ApiError ? err.message : fallback);

const launch = useMutation({
  mutationFn: (t: Template) => api.launch(t.id),
  onMutate: () => (error.value = null),
  onSuccess: () => queryClient.invalidateQueries({ queryKey: ["environments"] }),
  onError: (err) => (error.value = message(err, "Couldn't launch it.")),
});

const stop = useMutation({
  mutationFn: (e: Environment) => api.stopEnvironment(e.id),
  onMutate: () => (error.value = null),
  onSuccess: () => queryClient.invalidateQueries({ queryKey: ["environments"] }),
  onError: (err) => (error.value = message(err, "Couldn't stop it.")),
});

const GLYPHS: Record<string, string> = { browser: "◎", desktop: "▦", test: "▤" };
const glyph = (t: Template) => GLYPHS[t.class] ?? "⧉";

const STATES: Record<EnvironmentState, { text: string; dot: string }> = {
  starting: { text: "Starting", dot: "bg-warn animate-pulse" },
  running: { text: "Running", dot: "bg-accent" },
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
          <div class="mb-3 grid size-10 place-items-center rounded-lg border border-line bg-panel-2 text-xl text-accent">
            {{ glyph(t) }}
          </div>
          <h3 class="font-semibold">{{ t.name }}</h3>
          <p class="mt-1 flex-1 text-sm text-ink-2">{{ t.description }}</p>
          <button
            class="btn-primary mt-4"
            :disabled="session.user?.role === 'guest' || (launch.isPending.value && launch.variables.value?.id === t.id)"
            @click="launch.mutate(t)"
          >
            {{ launch.isPending.value && launch.variables.value?.id === t.id ? "Launching…" : "Launch" }}
          </button>
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
              <template v-if="e.streamer">
                · streamer
                <span class="font-mono">{{ e.streamer.host ?? "?" }}:{{ e.streamer.httpPort }}</span>
              </template>
            </p>
          </div>
          <button
            class="btn-ghost shrink-0 px-3 py-1 text-xs hover:border-danger/60 hover:text-danger"
            :disabled="e.state === 'stopping' || (stop.isPending.value && stop.variables.value?.id === e.id)"
            @click="stop.mutate(e)"
          >
            Stop
          </button>
        </div>
      </div>
    </section>

    <section v-if="ended.length" class="space-y-3">
      <h2 class="text-sm font-medium tracking-wide text-ink-2 uppercase">Recently ended</h2>
      <div class="card divide-y divide-line">
        <div v-for="e in ended" :key="e.id" class="flex items-center gap-4 px-4 py-2.5 text-sm">
          <span class="size-2 shrink-0 rounded-full" :class="STATES[e.state].dot" />
          <span class="w-36 shrink-0 truncate text-ink-2">{{ e.templateName }}</span>
          <span class="min-w-0 flex-1 truncate" :class="e.state === 'failed' ? 'text-danger' : 'text-ink-3'">
            {{ STATES[e.state].text }}<template v-if="e.detail">: {{ e.detail }}</template>
          </span>
          <span class="shrink-0 text-xs text-ink-3" :title="dateTime(e.updatedAt)">{{ ago(e.updatedAt) }}</span>
        </div>
      </div>
    </section>
  </div>
</template>

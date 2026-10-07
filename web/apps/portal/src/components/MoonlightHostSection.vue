<script setup lang="ts">
// One adopted Moonlight host as a dashboard section: its name, where it is reached from,
// who is using it, and its apps as cards.
import { computed } from "vue";

import type { AdoptedHost, Environment } from "../api";
import { launchState, visibleApps } from "../moonlight";
import MoonlightAppCard from "./MoonlightAppCard.vue";

const props = defineProps<{
  host: AdoptedHost;
  view: "grid" | "list";
  query: string;
  sort: "name" | "recent";
  lastUsed: ReadonlyMap<string, number>;
  live: ReadonlyMap<string, Environment>;
  fold: (s: string) => string;
  /** Show every app, whatever the search (the host's own name matched). */
  all: boolean;
  guest: boolean;
  /** Template id being launched now. */
  launching: string | null;
}>();
const emit = defineEmits<{ launch: [templateId: string] }>();

const apps = computed(() =>
  visibleApps(props.host, props.all ? "" : props.query, props.fold, props.sort, props.lastUsed),
);
const headingId = computed(() => `moonlight-${props.host.id}`);
</script>

<template>
  <section class="space-y-3" :aria-labelledby="headingId">
    <div>
      <h2 :id="headingId" class="text-xl font-semibold tracking-tight">{{ host.name }}</h2>
      <p class="text-sm text-ink-3">
        Moonlight host on {{ host.nodeName }}
        <span v-if="!host.online" class="text-warn"> · Offline</span>
        <span v-if="host.busy"> · In use by {{ host.busy.owner }}</span>
      </p>
    </div>
    <p v-if="!host.apps.length" class="text-sm text-ink-2">No apps reported by this host.</p>
    <p v-else-if="!apps.length" class="text-sm text-ink-2">No apps here match your search.</p>
    <div v-else class="@container">
      <ul
        :class="
          view === 'grid' ? 'grid grid-cols-[repeat(auto-fill,minmax(16rem,1fr))] gap-4' : 'flex flex-col gap-3'
        "
      >
        <li v-for="a in apps" :key="a.templateId" class="min-w-0">
          <MoonlightAppCard
            class="h-full"
            :app="a"
            :view="view"
            :instance="launchState(host, a, live, guest).instance"
            :disabled="launchState(host, a, live, guest).disabled"
            :reason="launchState(host, a, live, guest).reason"
            :busy="launching === a.templateId"
            @launch="emit('launch', a.templateId)"
          />
        </li>
      </ul>
    </div>
  </section>
</template>

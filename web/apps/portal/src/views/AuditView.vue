<script setup lang="ts">
import { useQuery } from "@tanstack/vue-query";
import { RefreshCw, TriangleAlert } from "lucide-vue-next";
import { computed } from "vue";

import { api } from "../api";
import FormError from "../components/FormError.vue";
import { dateTime } from "../format";

const audit = useQuery({ queryKey: ["audit"], queryFn: api.audit });
const users = useQuery({ queryKey: ["users"], queryFn: api.users });
const names = computed(() => new Map((users.data.value ?? []).map((u) => [u.id, u.username])));

function detail(raw: string | null): string {
  if (!raw) return "";
  try {
    return Object.entries(JSON.parse(raw) as Record<string, unknown>)
      .map(([k, v]) => `${k}: ${String(v)}`)
      .join(" · ");
  } catch {
    return raw;
  }
}

// Failures read warm and carry an icon; setup is the accent (it is the brand's moment); the rest are plain.
const failed = (action: string) => action.endsWith(".failed") || action.includes("bad_");
const tone = (action: string) => (failed(action) ? "text-warn" : action.startsWith("setup") ? "text-accent" : "text-ink");
</script>

<template>
  <div class="max-w-5xl space-y-6">
    <div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-3">
      <p class="min-w-0 text-sm text-ink-2">Sign-ins, setup and admin actions, newest first (last 200).</p>
      <button type="button" class="btn-ghost" :disabled="audit.isFetching.value" @click="audit.refetch()">
        <RefreshCw class="size-4" :class="audit.isFetching.value && 'animate-spin'" aria-hidden="true" />
        Refresh
      </button>
    </div>
    <!-- Not a table: each entry is a row of its own that stacks (time, action, address) below sm,
         so nothing scrolls sideways. -->
    <div class="card overflow-hidden">
      <p v-if="audit.isPending.value" class="p-6 text-sm text-ink-3">Loading…</p>
      <FormError v-else-if="audit.isError.value" class="m-4" :message="audit.error.value?.message ?? 'Failed to load'" />
      <p v-else-if="!audit.data.value?.length" class="p-6 text-sm text-ink-3">Nothing yet.</p>
      <ul v-else class="divide-y divide-line/60">
        <li v-for="e in audit.data.value" :key="e.id" class="grid gap-1 px-4 py-3 text-sm sm:grid-cols-[11rem_minmax(0,1fr)_auto] sm:gap-4 sm:px-5">
          <span class="font-mono text-xs text-ink-3 sm:pt-0.5">{{ dateTime(e.at) }}</span>
          <span class="min-w-0 break-words">
            <TriangleAlert v-if="failed(e.action)" class="mr-1 -mt-0.5 inline size-4 text-warn" aria-hidden="true" />
            <span class="font-medium" :class="tone(e.action)">{{ e.action }}</span>
            <span v-if="e.actorId" class="text-ink-2"> by {{ names.get(e.actorId) ?? e.actorId.slice(0, 8) }}</span>
            <span v-if="e.detail" class="block text-xs text-ink-3">{{ detail(e.detail) }}</span>
          </span>
          <span class="font-mono text-xs text-ink-3">{{ e.ip ?? "" }}</span>
        </li>
      </ul>
    </div>
  </div>
</template>

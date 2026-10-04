<script setup lang="ts">
import { useQuery } from "@tanstack/vue-query";
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

const tone = (action: string) =>
  action.endsWith(".failed") || action.includes("bad_") ? "text-warn" : action.startsWith("setup") ? "text-accent" : "text-ink";
</script>

<template>
  <div class="mx-auto max-w-5xl space-y-6">
    <div class="flex items-center justify-between gap-4">
      <p class="text-sm text-ink-2">Sign-ins, setup and admin actions, newest first (last 200).</p>
      <button class="btn-ghost" :disabled="audit.isFetching.value" @click="audit.refetch()">Refresh</button>
    </div>
    <div class="card overflow-hidden">
      <p v-if="audit.isPending.value" class="p-6 text-sm text-ink-3">Loading…</p>
      <FormError v-else-if="audit.isError.value" class="m-4" :message="audit.error.value?.message ?? 'Failed to load'" />
      <p v-else-if="!audit.data.value?.length" class="p-6 text-sm text-ink-3">Nothing yet.</p>
      <ul v-else class="divide-y divide-line/60">
        <li v-for="e in audit.data.value" :key="e.id" class="grid gap-1 px-5 py-3 text-sm sm:grid-cols-[11rem_1fr_auto] sm:gap-4">
          <span class="font-mono text-xs text-ink-3 sm:pt-0.5">{{ dateTime(e.at) }}</span>
          <span>
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

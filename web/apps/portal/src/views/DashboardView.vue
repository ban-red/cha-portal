<script setup lang="ts">
import { useSession } from "../stores/session";

const session = useSession();
</script>

<template>
  <div class="mx-auto max-w-5xl space-y-6">
    <p class="text-ink-2">
      Welcome, <span class="text-ink">{{ session.user?.displayName }}</span>. Your environments will appear here.
    </p>
    <div class="card flex flex-col items-center px-6 py-16 text-center">
      <div class="mb-4 grid size-12 place-items-center rounded-xl border border-line bg-panel-2 text-2xl">⧉</div>
      <h2 class="text-base font-semibold">No environments yet</h2>
      <p class="mt-2 max-w-md text-sm text-ink-2">
        Environments run on Cha Nodes: GPU machines that join this portal.
        <template v-if="session.isAdmin">Adding nodes comes next in Phase 1.</template>
        <template v-else>Ask an admin to add a node and grant you templates.</template>
      </p>
    </div>
  </div>
</template>

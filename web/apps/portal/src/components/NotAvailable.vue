<script setup lang="ts">
import { Info } from "lucide-vue-next";

withDefaults(defineProps<{ /** What the server lacks, e.g. "app data settings". */ what?: string }>(), { what: "app data settings" });
</script>

<template>
  <div class="card flex flex-col items-center px-6 py-10 text-center">
    <Info class="mb-3 size-6 text-ink-3" aria-hidden="true" />
    <p class="text-base font-medium">Not available yet</p>
    <p class="mt-1 text-sm text-ink-2">This server doesn't support {{ what }} yet.</p>
  </div>
</template>

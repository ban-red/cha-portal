<script setup lang="ts">
import { TriangleAlert } from "lucide-vue-next";

// Something to know about a running environment that isn't an error, as the
// node reported it: a status, so it doesn't interrupt a screen reader. The icon
// keeps it from being told by colour alone.
defineProps<{ message: string | null }>();
</script>

<template>
  <p v-if="message" role="status" class="flex items-start gap-2 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-sm text-warn">
    <TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
    <span class="min-w-0 break-words">{{ message }}</span>
  </p>
</template>

<script setup lang="ts">
import { CircleAlert } from "lucide-vue-next";

// `polite`: for a message shown inside a live region the page already has
// (`aria-live="polite"`), instead of announcing itself as an alert. The icon keeps the
// message from being told by colour alone.
defineProps<{ message: string | null; polite?: boolean }>();
</script>

<template>
  <p
    v-if="message"
    :role="polite ? undefined : 'alert'"
    class="flex items-start gap-2 rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-sm text-danger"
  >
    <CircleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
    <span class="min-w-0 break-words">{{ message }}</span>
  </p>
</template>

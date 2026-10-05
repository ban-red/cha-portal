<script setup lang="ts">
// `polite`: for a message shown inside a live region the page already has
// (`aria-live="polite"`), instead of announcing itself as an alert.
defineProps<{ message: string | null; polite?: boolean }>();
</script>

<template>
  <p
    v-if="message"
    :role="polite ? undefined : 'alert'"
    class="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-sm text-danger"
  >
    {{ message }}
  </p>
</template>

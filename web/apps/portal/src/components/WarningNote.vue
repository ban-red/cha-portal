<script setup lang="ts">
// Something to know about a running environment that isn't an error, as the
// node reported it: a status, so it doesn't interrupt a screen reader.
defineProps<{ message: string | null }>();
</script>

<template>
  <p v-if="message" role="status" class="rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-sm text-warn">
    {{ message }}
  </p>
</template>

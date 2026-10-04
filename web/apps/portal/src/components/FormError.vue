<script setup lang="ts">
defineProps<{ message: string | null }>();
</script>

<template>
  <p
    v-if="message"
    role="alert"
    class="rounded-lg border border-danger/30 bg-danger/10 px-3 py-2 text-sm text-danger"
  >
    {{ message }}
  </p>
</template>

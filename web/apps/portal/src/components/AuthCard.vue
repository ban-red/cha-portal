<script setup lang="ts">
import BrandMark from "./BrandMark.vue";

defineProps<{ title: string; subtitle?: string }>();
</script>

<template>
  <div class="flex min-h-full items-center justify-center px-4 py-12">
    <div class="w-full max-w-sm">
      <div class="mb-8 flex flex-col items-center text-center">
        <BrandMark class="mb-4 size-12" />
        <h1 class="text-xl font-semibold tracking-tight">{{ title }}</h1>
        <p v-if="subtitle" class="mt-1.5 text-sm text-ink-2">{{ subtitle }}</p>
      </div>
      <div class="card p-6">
        <slot />
      </div>
      <p class="mt-6 text-center text-xs text-ink-3">Cha Portal · self-hosted remote environments</p>
    </div>
  </div>
</template>

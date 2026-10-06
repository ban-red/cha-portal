<script setup lang="ts">
// The frame for sign-in and first-run setup: one card, centred on every screen size, under
// the brand mark. It sits outside the shell, so it brings its own <main> and safe-area padding.
import BrandMark from "./BrandMark.vue";

defineProps<{ title: string; subtitle?: string }>();
</script>

<template>
  <main
    class="flex min-h-dvh items-center justify-center px-4 py-10 pt-[max(2.5rem,env(safe-area-inset-top))] pr-[max(1rem,env(safe-area-inset-right))] pb-[max(2.5rem,env(safe-area-inset-bottom))] pl-[max(1rem,env(safe-area-inset-left))]"
  >
    <div class="w-full max-w-sm">
      <div class="mb-8 flex flex-col items-center text-center">
        <BrandMark class="mb-4 size-12" />
        <h1 class="text-2xl font-semibold tracking-tight">{{ title }}</h1>
        <p v-if="subtitle" class="mt-1.5 text-sm text-ink-2">{{ subtitle }}</p>
      </div>
      <div class="card p-6">
        <slot />
      </div>
      <p class="mt-6 text-center text-xs text-ink-3">Cha Portal · self-hosted remote environments</p>
    </div>
  </main>
</template>

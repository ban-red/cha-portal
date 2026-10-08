<script setup lang="ts">
import { Check, Copy } from "lucide-vue-next";
import { ref } from "vue";

// A link just made, shown once, with Copy (ADR 0014: the token is never shown again).
const props = defineProps<{ url: string; label: string }>();

const copied = ref(false);
const failed = ref(false);

async function copy() {
  copied.value = false;
  failed.value = false;
  try {
    await navigator.clipboard.writeText(props.url);
    copied.value = true;
  } catch {
    // No clipboard permission, or an insecure page: the field is selectable.
    failed.value = true;
  }
}
</script>

<template>
  <div class="space-y-1">
    <div class="flex gap-2">
      <input
        class="field font-mono text-xs"
        readonly
        :value="url"
        :aria-label="label"
        @focus="($event.target as HTMLInputElement).select()"
      />
      <button type="button" class="btn-primary min-h-9 shrink-0 px-3" @click="copy">
        <Check v-if="copied" class="size-4" aria-hidden="true" />
        <Copy v-else class="size-4" aria-hidden="true" />
        {{ copied ? "Copied" : "Copy" }}
      </button>
    </div>
    <p class="text-xs text-ink-3">This is the only time the link is shown.</p>
    <p v-if="failed" role="status" class="text-xs text-warn">Couldn't copy; select the link and copy it.</p>
  </div>
</template>

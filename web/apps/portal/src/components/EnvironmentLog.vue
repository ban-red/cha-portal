<script setup lang="ts">
import { ref } from "vue";

// What an environment's containers last logged when it died: its node removes
// them, so this is all there is. Shown to its owner and admins (the portal
// sends it to no one else).
const props = defineProps<{ log: string[] | null }>();

const copied = ref(false);
let copiedTimer: ReturnType<typeof setTimeout> | undefined;

async function copy() {
  try {
    await navigator.clipboard.writeText(props.log?.join("\n") ?? "");
    copied.value = true;
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied.value = false), 2000);
  } catch {
    // No clipboard here (an insecure origin, a denied permission): the text is selectable.
  }
}
</script>

<template>
  <details v-if="log?.length" class="group text-left text-sm">
    <summary class="w-fit cursor-pointer text-xs text-ink-2 select-none hover:text-ink">
      <span class="group-open:hidden">Show log</span><span class="hidden group-open:inline">Hide log</span>
    </summary>
    <div class="relative mt-2">
      <button type="button" class="btn-ghost absolute top-1 right-3 px-2 py-0.5 text-xs" @click="copy">
        {{ copied ? "Copied" : "Copy" }}
      </button>
      <pre
        class="max-h-64 overflow-auto rounded-lg border border-line bg-canvas p-3 font-mono text-xs leading-relaxed whitespace-pre-wrap break-words text-ink-2"
        tabindex="0"
        aria-label="Environment log"
        >{{ log.join("\n") }}</pre
      >
    </div>
  </details>
</template>

<script setup lang="ts">
import { recordingMarkdown, type RecordingSummary } from "@cha/player";
import { Check, Copy, X } from "lucide-vue-next";
import { computed, ref, useId, useTemplateRef, watch } from "vue";

// The result of a "Record 30 s" run, on the native <dialog> (focus trap, Esc, a scrim
// behind). Focus starts on Close and goes back to the opener afterwards (the browser does
// that; if it leaves focus on <body>, this does). Two buttons copy the result for the Phase 2
// exit pass: JSON, or a Markdown table.
const props = defineProps<{ summary: RecordingSummary | null }>();
const emit = defineEmits<{ close: [] }>();

const dialog = useTemplateRef<HTMLDialogElement>("dialog");
const closeButton = useTemplateRef<HTMLButtonElement>("closeButton");
const titleId = useId();
const markdown = computed(() => (props.summary ? recordingMarkdown(props.summary) : ""));
const json = computed(() => (props.summary ? JSON.stringify(props.summary, null, 2) : ""));
const copied = ref<string | null>(null);
const copyFailed = ref(false);
let opener: HTMLElement | null = null;

watch(
  () => props.summary,
  (summary) => {
    const el = dialog.value;
    if (!el) return;
    copied.value = null;
    if (summary && !el.open) {
      opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      el.showModal();
      closeButton.value?.focus();
    } else if (!summary && el.open) {
      el.close();
    }
  },
  { flush: "post" },
);

function onClose() {
  emit("close");
  const to = opener;
  opener = null;
  if (to?.isConnected && (document.activeElement === document.body || document.activeElement === null)) to.focus();
}

async function copy(what: "JSON" | "Markdown", text: string) {
  try {
    await navigator.clipboard.writeText(text);
    copyFailed.value = false;
    copied.value = `Copied as ${what}`;
  } catch {
    // No clipboard permission, or an insecure page: the text is selectable below.
    copyFailed.value = true;
    copied.value = "Couldn't copy; select the text and copy it";
  }
}
</script>

<template>
  <dialog
    ref="dialog"
    :aria-labelledby="titleId"
    class="m-auto w-[min(40rem,calc(100%-2rem))] rounded-xl border border-line bg-panel p-0 text-ink backdrop:bg-scrim"
    @click.self="emit('close')"
    @close="onClose"
  >
    <div class="space-y-3 p-5">
      <h2 :id="titleId" class="text-lg font-semibold tracking-tight">Measurement</h2>
      <pre
        class="max-h-[50vh] overflow-auto rounded-lg border border-line bg-canvas p-3 font-mono text-xs leading-5 whitespace-pre-wrap text-ink-2"
        tabindex="0"
        aria-label="Measurement result"
        >{{ markdown }}</pre
      >
      <div class="flex flex-wrap items-center justify-end gap-2">
        <span class="mr-auto inline-flex items-center gap-1.5 text-xs" :class="copyFailed ? 'text-warn' : 'text-ink-2'" role="status">
          <template v-if="copied">
            <Check v-if="!copyFailed" class="size-4 shrink-0 text-ok" aria-hidden="true" />
            {{ copied }}
          </template>
        </span>
        <button type="button" class="btn-ghost" @click="copy('JSON', json)">
          <Copy class="size-4" aria-hidden="true" />
          Copy as JSON
        </button>
        <button type="button" class="btn-ghost" @click="copy('Markdown', markdown)">
          <Copy class="size-4" aria-hidden="true" />
          Copy as Markdown
        </button>
        <button ref="closeButton" type="button" class="btn-primary" @click="emit('close')">
          <X class="size-4" aria-hidden="true" />
          Close
        </button>
      </div>
    </div>
  </dialog>
</template>

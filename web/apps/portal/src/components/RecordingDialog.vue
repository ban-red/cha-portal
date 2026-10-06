<script setup lang="ts">
import { recordingMarkdown, type RecordingSummary } from "@cha/player";
import { computed, ref, useId, useTemplateRef, watch } from "vue";

// The result of a "Record 30 s" run, on the native <dialog> (focus trap, Esc, focus back on
// the opener). Two buttons copy it for the Phase 2 exit pass: JSON, or a Markdown table.
const props = defineProps<{ summary: RecordingSummary | null }>();
const emit = defineEmits<{ close: [] }>();

const dialog = useTemplateRef<HTMLDialogElement>("dialog");
const titleId = useId();
const markdown = computed(() => (props.summary ? recordingMarkdown(props.summary) : ""));
const json = computed(() => (props.summary ? JSON.stringify(props.summary, null, 2) : ""));
const copied = ref<string | null>(null);

watch(
  () => props.summary,
  (summary) => {
    const el = dialog.value;
    if (!el) return;
    copied.value = null;
    if (summary && !el.open) el.showModal();
    else if (!summary && el.open) el.close();
  },
  { flush: "post" },
);

async function copy(what: "JSON" | "Markdown", text: string) {
  try {
    await navigator.clipboard.writeText(text);
    copied.value = `Copied as ${what}`;
  } catch {
    // No clipboard permission, or an insecure page: the text is selectable below.
    copied.value = "Couldn't copy; select the text and copy it";
  }
}
</script>

<template>
  <dialog
    ref="dialog"
    :aria-labelledby="titleId"
    class="m-auto w-[min(40rem,calc(100%-2rem))] rounded-xl border border-line bg-panel p-0 text-ink backdrop:bg-black/60"
    @click.self="emit('close')"
    @close="emit('close')"
  >
    <div class="space-y-3 p-5">
      <h2 :id="titleId" class="text-base font-semibold tracking-tight">Measurement</h2>
      <pre class="max-h-[50vh] overflow-auto rounded-lg border border-line bg-canvas p-3 font-mono text-[11px] leading-5 whitespace-pre-wrap text-ink-2" tabindex="0">{{ markdown }}</pre>
      <div class="flex flex-wrap items-center justify-end gap-2">
        <span class="mr-auto text-xs text-ink-3" role="status">{{ copied }}</span>
        <button type="button" class="btn-ghost" @click="copy('JSON', json)">Copy as JSON</button>
        <button type="button" class="btn-ghost" @click="copy('Markdown', markdown)">Copy as Markdown</button>
        <button type="button" class="btn-ghost" @click="emit('close')">Close</button>
      </div>
    </div>
  </dialog>
</template>

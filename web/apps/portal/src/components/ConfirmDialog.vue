<script setup lang="ts">
import { useId, useTemplateRef, watch } from "vue";

import FormError from "./FormError.vue";

// A modal confirmation on the native <dialog>: the browser traps focus, makes
// the page behind inert, closes on Esc and puts focus back on the button that
// opened it (`closed` lets the parent do that itself where the browser doesn't,
// e.g. Safari doesn't focus a button on click). Focus starts on Cancel, so
// Enter never confirms by accident.
const props = defineProps<{
  open: boolean;
  title: string;
  confirmLabel: string;
  /** Working on it: the buttons and Esc do nothing until it finishes. */
  busy?: boolean;
  error?: string | null;
}>();
const emit = defineEmits<{ confirm: []; cancel: []; closed: [] }>();

const dialog = useTemplateRef<HTMLDialogElement>("dialog");
const cancelButton = useTemplateRef<HTMLButtonElement>("cancelButton");
const titleId = useId();
const bodyId = useId();

watch(
  () => props.open,
  (open) => {
    const el = dialog.value;
    if (!el) return;
    if (open && !el.open) {
      el.showModal();
      cancelButton.value?.focus();
    } else if (!open && el.open) {
      el.close();
    }
  },
  { flush: "post" },
);

function cancel() {
  if (!props.busy) emit("cancel");
}

function confirm() {
  if (!props.busy) emit("confirm");
}
</script>

<template>
  <dialog
    ref="dialog"
    :aria-labelledby="titleId"
    :aria-describedby="bodyId"
    class="m-auto w-[min(28rem,calc(100%-2rem))] rounded-xl border border-line bg-panel p-0 text-ink backdrop:bg-black/60"
    @cancel.prevent="cancel"
    @click.self="cancel"
    @close="emit('closed')"
  >
    <form class="space-y-4 p-5" @submit.prevent="confirm">
      <h2 :id="titleId" class="text-base font-semibold tracking-tight">{{ title }}</h2>
      <div :id="bodyId" class="space-y-2 text-sm text-ink-2">
        <slot />
      </div>
      <FormError :message="error ?? null" />
      <div class="flex flex-wrap justify-end gap-2">
        <!-- aria-disabled rather than disabled, so focus isn't dropped mid-request. -->
        <button
          ref="cancelButton"
          type="button"
          class="btn-ghost aria-disabled:cursor-not-allowed aria-disabled:opacity-50"
          :aria-disabled="busy || undefined"
          @click="cancel"
        >
          Cancel
        </button>
        <button
          type="submit"
          class="btn border border-danger/50 bg-danger/10 text-danger hover:bg-danger/20 aria-disabled:cursor-not-allowed aria-disabled:opacity-50"
          :aria-disabled="busy || undefined"
        >
          {{ confirmLabel }}
        </button>
      </div>
    </form>
  </dialog>
</template>

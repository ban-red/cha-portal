<script setup lang="ts">
import { TriangleAlert } from "lucide-vue-next";
import { useId, useTemplateRef, watch } from "vue";

import FormError from "./FormError.vue";

// A modal confirmation on the native <dialog>: the browser traps focus, makes
// the page behind inert, closes on Esc and puts focus back on the button that
// opened it (`closed` lets the parent do that itself where the browser doesn't,
// e.g. Safari doesn't focus a button on click; as a fallback this component also
// refocuses the opener it saw). Focus starts on Cancel, so Enter never confirms by
// accident. The confirm button is a solid danger button (btn-danger) unless
// `tone="default"`, which makes it the primary button.
const props = withDefaults(
  defineProps<{
    open: boolean;
    title: string;
    confirmLabel: string;
    /** Working on it: the buttons and Esc do nothing until it finishes. */
    busy?: boolean;
    error?: string | null;
    tone?: "danger" | "default";
  }>(),
  { tone: "danger" },
);
const emit = defineEmits<{ confirm: []; cancel: []; closed: [] }>();

const dialog = useTemplateRef<HTMLDialogElement>("dialog");
const cancelButton = useTemplateRef<HTMLButtonElement>("cancelButton");
const titleId = useId();
const bodyId = useId();
let opener: HTMLElement | null = null;

watch(
  () => props.open,
  (open) => {
    const el = dialog.value;
    if (!el) return;
    if (open && !el.open) {
      opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      el.showModal();
      cancelButton.value?.focus();
    } else if (!open && el.open) {
      el.close();
    }
  },
  { flush: "post" },
);

function onClose() {
  emit("closed");
  // The browser normally does this; where it doesn't (focus fell to <body>), do it here.
  const to = opener;
  opener = null;
  if (to?.isConnected && (document.activeElement === document.body || document.activeElement === null)) to.focus();
}

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
    class="m-auto w-[min(28rem,calc(100%-2rem))] rounded-xl border border-line bg-panel p-0 text-ink backdrop:bg-scrim"
    @cancel.prevent="cancel"
    @click.self="cancel"
    @close="onClose"
  >
    <form class="space-y-4 p-5" @submit.prevent="confirm">
      <div class="flex items-start gap-3">
        <TriangleAlert v-if="tone === 'danger'" class="mt-0.5 size-5 shrink-0 text-danger" aria-hidden="true" />
        <h2 :id="titleId" class="min-w-0 text-lg leading-snug font-semibold tracking-tight">{{ title }}</h2>
      </div>
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
          :class="tone === 'danger' ? 'btn-danger' : 'btn-primary'"
          class="aria-disabled:cursor-not-allowed aria-disabled:opacity-50"
          :aria-disabled="busy || undefined"
        >
          {{ confirmLabel }}
        </button>
      </div>
    </form>
  </dialog>
</template>

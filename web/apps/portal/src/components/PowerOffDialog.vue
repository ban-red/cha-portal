<script setup lang="ts">
import { onBeforeUnmount, ref, useId, useTemplateRef, watch } from "vue";

// "Powering off in 5…": a modal on the native <dialog> that stops the app when the countdown
// reaches zero, unless it is cancelled (button, Esc, or a click outside) first.
const props = defineProps<{ open: boolean; name: string; seconds?: number; failed?: string | null }>();
const emit = defineEmits<{ cancel: []; confirm: [] }>();

const dialog = useTemplateRef<HTMLDialogElement>("dialog");
const titleId = useId();
const total = () => props.seconds ?? 5;
const left = ref(total());
/** Stopping has been asked for: the countdown is over and the answer is awaited. */
const stopping = ref(false);
let timer: ReturnType<typeof setInterval> | undefined;

function stopTimer() {
  clearInterval(timer);
  timer = undefined;
}

watch(
  () => props.open,
  (open) => {
    const el = dialog.value;
    stopTimer();
    if (!el) return;
    if (!open) {
      if (el.open) el.close();
      return;
    }
    left.value = total();
    stopping.value = false;
    if (!el.open) el.showModal();
    timer = setInterval(() => {
      left.value -= 1;
      if (left.value > 0) return;
      stopTimer();
      stopping.value = true;
      emit("confirm");
    }, 1000);
  },
  { flush: "post" },
);

// A failure puts the dialog back in a state the user can leave.
watch(
  () => props.failed,
  (failed) => {
    if (failed) stopping.value = false;
  },
);

function cancel() {
  stopTimer();
  emit("cancel");
}
onBeforeUnmount(stopTimer);
</script>

<template>
  <dialog
    ref="dialog"
    :aria-labelledby="titleId"
    class="m-auto w-[min(24rem,calc(100%-2rem))] rounded-xl border border-line bg-panel p-0 text-ink backdrop:bg-scrim"
    @click.self="cancel"
    @cancel.prevent="cancel"
  >
    <div class="space-y-4 p-5 text-center">
      <h2 :id="titleId" class="text-base font-semibold tracking-tight" role="status">
        <template v-if="failed">Couldn't power off {{ name }}</template>
        <template v-else-if="stopping">Powering off {{ name }}…</template>
        <template v-else>Powering off {{ name }} in {{ left }}…</template>
      </h2>
      <div v-if="!failed && !stopping" class="h-1 overflow-hidden rounded-full bg-line" aria-hidden="true">
        <div class="h-full bg-danger transition-[width] duration-1000 ease-linear" :style="{ width: `${(left / total()) * 100}%` }" />
      </div>
      <p v-if="failed" class="text-sm text-danger">{{ failed }}</p>
      <p v-else class="text-sm text-ink-2">The app stops and its session ends. Saved data stays.</p>
      <div class="flex justify-center">
        <button type="button" class="btn-ghost" :disabled="stopping && !failed" @click="cancel">{{ failed ? "Close" : "Cancel" }}</button>
      </div>
    </div>
  </dialog>
</template>

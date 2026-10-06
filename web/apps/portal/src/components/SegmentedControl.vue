<script setup lang="ts" generic="T extends string">
// A radiogroup of short options, one visible label. Arrow keys move and select (and wrap),
// Home/End jump; only the selected segment is in the tab order. The selected segment is
// shown by an accent-soft fill, a heavier weight and an accent edge, never colour alone.
import { nextTick, ref, useId } from "vue";

const props = defineProps<{
  modelValue: T;
  options: readonly { value: T; label: string }[];
  /** The visible label above the control. */
  label: string;
  /** Extra text for screen readers, tied with aria-describedby. */
  describedby?: string;
}>();
const emit = defineEmits<{ "update:modelValue": [value: T] }>();

const labelId = useId();
const buttons = ref<HTMLButtonElement[]>([]);

function pick(i: number) {
  const n = props.options.length;
  const next = props.options[((i % n) + n) % n]!;
  emit("update:modelValue", next.value);
  // By value: the options may be a fresh array after the change re-renders the parent.
  void nextTick(() => buttons.value[props.options.findIndex((o) => o.value === next.value)]?.focus());
}

function onKey(e: KeyboardEvent, i: number) {
  const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
  if (step !== undefined) pick(i + step);
  else if (e.key === "Home") pick(0);
  else if (e.key === "End") pick(props.options.length - 1);
  else return;
  e.preventDefault();
}
</script>

<template>
  <div>
    <p :id="labelId" class="label">{{ label }}</p>
    <div
      role="radiogroup"
      :aria-labelledby="labelId"
      :aria-describedby="describedby"
      class="inline-flex max-w-full flex-wrap gap-1 rounded-lg border border-line-strong bg-canvas p-1 max-sm:flex max-sm:w-full"
    >
      <button
        v-for="(o, i) in options"
        :key="o.value"
        :ref="(el) => (buttons[i] = el as HTMLButtonElement)"
        type="button"
        role="radio"
        :aria-checked="o.value === modelValue"
        :tabindex="o.value === modelValue ? 0 : -1"
        class="inline-flex min-h-8 items-center justify-center rounded-md border px-3 text-sm transition pointer-coarse:min-h-11 max-sm:flex-1"
        :class="
          o.value === modelValue
            ? 'border-accent bg-accent-soft font-semibold text-ink'
            : 'border-transparent text-ink-2 hover:bg-panel-2 hover:text-ink'
        "
        @click="emit('update:modelValue', o.value)"
        @keydown="onKey($event, i)"
      >
        {{ o.label }}
      </button>
    </div>
  </div>
</template>

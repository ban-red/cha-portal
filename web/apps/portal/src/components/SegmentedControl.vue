<script setup lang="ts" generic="T extends string">
// A radiogroup of short options, one visible label. Arrow keys move and select (and wrap),
// Home/End jump; only the selected segment is in the tab order. The selected segment is
// shown by an accent-soft fill, a heavier weight and an accent edge, never colour alone.
import { nextTick, ref, useId, type Component } from "vue";

const props = defineProps<{
  modelValue: T;
  options: readonly { value: T; label: string; icon?: Component }[];
  /** The label above the control (read by screen readers even when `bare`). */
  label: string;
  /** For toolbars: the label is for screen readers only, the group never wraps or fills the
   *  width, and it scrolls sideways when it doesn't fit. */
  bare?: boolean;
  /** Show only the icons; each label stays as the accessible name and tooltip. */
  iconOnly?: boolean;
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
  <div :class="bare && 'max-w-full min-w-0'">
    <p :id="labelId" :class="bare ? 'sr-only' : 'label'">{{ label }}</p>
    <div
      role="radiogroup"
      :aria-labelledby="labelId"
      :aria-describedby="describedby"
      class="inline-flex max-w-full gap-1 rounded-lg border border-line-strong bg-canvas p-1"
      :class="bare ? 'overflow-x-auto' : 'flex-wrap max-sm:flex max-sm:w-full'"
    >
      <button
        v-for="(o, i) in options"
        :key="o.value"
        :ref="(el) => (buttons[i] = el as HTMLButtonElement)"
        type="button"
        role="radio"
        :aria-checked="o.value === modelValue"
        :tabindex="o.value === modelValue ? 0 : -1"
        :title="iconOnly ? o.label : undefined"
        class="inline-flex min-h-8 shrink-0 items-center justify-center gap-1.5 rounded-md border text-sm whitespace-nowrap transition pointer-coarse:min-h-11"
        :class="[
          iconOnly ? 'min-w-8 px-2 pointer-coarse:min-w-11' : 'px-3',
          bare ? 'focus-visible:-outline-offset-2' : 'max-sm:flex-1',
          o.value === modelValue
            ? 'border-accent bg-accent-soft font-semibold text-ink'
            : 'border-transparent text-ink-2 hover:bg-panel-2 hover:text-ink',
        ]"
        @click="emit('update:modelValue', o.value)"
        @keydown="onKey($event, i)"
      >
        <component :is="o.icon" v-if="o.icon" class="size-4 shrink-0" aria-hidden="true" />
        <span :class="iconOnly && o.icon && 'sr-only'">{{ o.label }}</span>
      </button>
    </div>
  </div>
</template>

<script setup lang="ts">
// A labelled-from-outside on/off switch: pass `aria-labelledby` (and
// `aria-describedby`) from the parent. While disabled it stays focusable, so
// its hint is still read out, but ignores activation.
const props = defineProps<{ modelValue: boolean; disabled?: boolean }>();
const emit = defineEmits<{ "update:modelValue": [value: boolean] }>();

function toggle() {
  if (!props.disabled) emit("update:modelValue", !props.modelValue);
}
</script>

<template>
  <button
    type="button"
    role="switch"
    :aria-checked="modelValue"
    :aria-disabled="disabled || undefined"
    class="relative inline-flex h-6 w-11 shrink-0 items-center rounded-full border transition before:absolute before:-inset-2 before:content-['']
      motion-reduce:transition-none"
    :class="[
      modelValue ? 'border-accent bg-accent' : 'border-line bg-panel-2',
      disabled ? 'cursor-not-allowed opacity-50' : 'cursor-pointer hover:brightness-110',
    ]"
    @click="toggle"
  >
    <span
      class="inline-block size-4 rounded-full transition-transform motion-reduce:transition-none"
      :class="modelValue ? 'translate-x-[1.375rem] bg-accent-ink' : 'translate-x-1 bg-ink-3'"
    />
  </button>
</template>

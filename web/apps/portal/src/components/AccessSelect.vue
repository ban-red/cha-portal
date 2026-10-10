<script setup lang="ts">
import { useId } from "vue";

import { SESSION_ACCESS, accessNote, type SessionAccess } from "../shares";

// Who besides the owner and admins may join a session: private, public or free.
defineProps<{ modelValue: SessionAccess; label: string; disabled?: boolean }>();
const emit = defineEmits<{ "update:modelValue": [value: SessionAccess] }>();
const noteId = useId();
</script>

<template>
  <label class="flex items-center gap-2 text-xs text-ink-2">
    <span>{{ label }}</span>
    <select
      class="input min-h-8 py-0.5 text-xs pointer-coarse:min-h-11"
      :value="modelValue"
      :disabled="disabled"
      :aria-describedby="noteId"
      @change="emit('update:modelValue', ($event.target as HTMLSelectElement).value as SessionAccess)"
    >
      <option v-for="a in SESSION_ACCESS" :key="a.value" :value="a.value">{{ a.label }}</option>
    </select>
    <span :id="noteId" class="sr-only">{{ accessNote(modelValue) }}</span>
  </label>
</template>

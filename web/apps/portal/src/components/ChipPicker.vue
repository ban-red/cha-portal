<script setup lang="ts">
import { Plus, X } from "lucide-vue-next";
import { ref, useId } from "vue";

// Chosen values as chips. `choices` are what nodes allow (toggle them); `free` adds a field for
// typing anything (for a node in full mode). `problem` says why a typed value is refused.
const props = defineProps<{
  label: string;
  selected: string[];
  choices: string[];
  free: boolean;
  placeholder?: string;
  problem?: (value: string) => string | null;
}>();
const emit = defineEmits<{ "update:selected": [value: string[]] }>();

const typed = ref("");
const error = ref<string | null>(null);
const inputId = useId();

function toggle(v: string) {
  emit("update:selected", props.selected.includes(v) ? props.selected.filter((x) => x !== v) : [...props.selected, v]);
}
function add() {
  const v = typed.value.trim();
  if (!v) return;
  error.value = props.problem?.(v) ?? null;
  if (error.value) return;
  if (!props.selected.includes(v)) emit("update:selected", [...props.selected, v]);
  typed.value = "";
}
const CHIP = "inline-flex min-h-7 items-center gap-1 rounded-full border px-2.5 font-mono text-xs transition pointer-coarse:min-h-9";
</script>

<template>
  <div class="space-y-2">
    <div v-if="selected.length" class="flex flex-wrap gap-1.5" role="list" :aria-label="`${label}, chosen`">
      <span v-for="v in selected" :key="v" role="listitem" :class="CHIP" class="border-accent/50 bg-accent-soft text-accent">
        {{ v }}
        <button type="button" class="rounded-full hover:text-ink" :aria-label="`Remove ${v}`" @click="toggle(v)">
          <X class="size-3.5" aria-hidden="true" />
        </button>
      </span>
    </div>
    <div v-if="choices.some((c) => !selected.includes(c))" class="flex flex-wrap items-center gap-1.5">
      <span class="text-xs text-ink-3">Nodes allow:</span>
      <button
        v-for="c in choices.filter((c) => !selected.includes(c))"
        :key="c"
        type="button"
        :class="CHIP"
        class="border-line-strong text-ink-2 hover:border-ink-3 hover:text-ink"
        :aria-label="`Add ${c}`"
        @click="toggle(c)"
      >
        <Plus class="size-3" aria-hidden="true" />{{ c }}
      </button>
    </div>
    <p v-else-if="!choices.length && !free" class="text-xs text-ink-3">No node names any.</p>
    <div v-if="free" class="flex gap-2">
      <label :for="inputId" class="sr-only">Add a {{ label }}</label>
      <input
        :id="inputId"
        v-model="typed"
        class="field font-mono"
        :placeholder="placeholder"
        autocomplete="off"
        spellcheck="false"
        @keydown.enter.prevent="add"
      />
      <button type="button" class="btn-ghost shrink-0" @click="add">Add</button>
    </div>
    <p v-if="error" class="text-xs text-danger" role="alert">{{ error }}</p>
  </div>
</template>

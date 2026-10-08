<script setup lang="ts">
import { ref, useId, watch } from "vue";
import { useRouter } from "vue-router";

import type { Template } from "../api";
import { SHARE_DATA_EXPLAIN, newCustomQuery, slugProblem, suggestSlug } from "../customEnv";
import ConfirmDialog from "./ConfirmDialog.vue";

// "Duplicate" on a template (admins): a name for the new id, and an explicit choice about its
// saved data (ADR 0021). Continues in the editor, where it is saved.
const props = defineProps<{ template: Pick<Template, "id" | "name"> | null }>();
const emit = defineEmits<{ close: [] }>();

const router = useRouter();
const uid = useId();
const slug = ref("");
const share = ref<"own" | "base">("own");
const error = ref<string | null>(null);

watch(
  () => props.template,
  (t) => {
    if (!t) return;
    slug.value = suggestSlug(t.name);
    share.value = "own";
    error.value = null;
  },
);

function go() {
  const t = props.template;
  if (!t) return;
  const s = slug.value.trim();
  error.value = slugProblem(s);
  if (error.value) return;
  void router.push({ name: "custom-new", query: newCustomQuery(t.id, s, share.value === "base") });
  emit("close");
}
</script>

<template>
  <ConfirmDialog
    :open="!!template"
    :title="`Duplicate ${template?.name ?? ''}`"
    confirm-label="Continue"
    tone="default"
    :error="error"
    @cancel="emit('close')"
    @confirm="go"
  >
    <p>Saves a copy of {{ template?.name }} under a name of its own. You change what you like in the next step; what you leave alone follows {{ template?.name }}.</p>
    <div>
      <label :for="`${uid}-slug`" class="label">Id</label>
      <div class="flex items-center gap-1 font-mono text-sm">
        <span class="text-ink-3">custom.</span>
        <input :id="`${uid}-slug`" v-model="slug" class="field flex-1" autocomplete="off" spellcheck="false" maxlength="40" />
      </div>
    </div>
    <fieldset class="space-y-2">
      <legend class="label">Saved data</legend>
      <label class="flex items-start gap-2 rounded-lg border border-line p-3 has-checked:border-accent/60 has-checked:bg-accent-soft">
        <input v-model="share" type="radio" :name="`${uid}-data`" value="own" class="mt-0.5 accent-[var(--cha-accent)]" />
        <span class="text-ink">Its own saved data</span>
      </label>
      <label class="flex items-start gap-2 rounded-lg border border-line p-3 has-checked:border-accent/60 has-checked:bg-accent-soft">
        <input v-model="share" type="radio" :name="`${uid}-data`" value="base" class="mt-0.5 accent-[var(--cha-accent)]" />
        <span class="text-ink">Share {{ template?.name }}'s saved data (home, library, shared folder)</span>
      </label>
      <p class="text-xs text-ink-3">{{ SHARE_DATA_EXPLAIN }}</p>
    </fieldset>
  </ConfirmDialog>
</template>

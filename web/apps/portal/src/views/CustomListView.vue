<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Package, Pencil, TriangleAlert } from "lucide-vue-next";
import { ref } from "vue";

import { api, catalogIconUrl, type CustomTemplate } from "../api";
import { errorText } from "../catalogs";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import FormError from "../components/FormError.vue";
import { CUSTOM_KEY, summaryLine } from "../customEnv";
import { ago, dateTime } from "../format";

const queryClient = useQueryClient();
const customs = useQuery({ queryKey: CUSTOM_KEY, queryFn: api.customTemplates });

const target = ref<CustomTemplate | null>(null);
const removeError = ref<string | null>(null);
const remove = useMutation({
  mutationFn: (c: CustomTemplate) => api.deleteCustomTemplate(c.id),
  onMutate: () => (removeError.value = null),
  onSuccess: () => {
    target.value = null;
    void queryClient.invalidateQueries({ queryKey: CUSTOM_KEY });
    void queryClient.invalidateQueries({ queryKey: ["catalog"] });
  },
  // 409 while one is live: the server says which.
  onError: (err) => (removeError.value = errorText(err, "Couldn't delete it.")),
});

function confirmRemove(c: CustomTemplate) {
  removeError.value = null;
  target.value = c;
}
</script>

<template>
  <div class="max-w-4xl space-y-6">
    <div class="space-y-1 text-sm text-ink-2">
      <p>
        A custom environment is a copy of an app with a few things changed: its image, memory, variables, and on nodes that allow it, folders, ports and capabilities. What it doesn't change follows the app it is based on.
      </p>
      <p>
        To make one, use <span class="font-medium text-ink">Duplicate</span> on an environment's card on the
        <RouterLink to="/" class="text-accent underline underline-offset-2 hover:text-ink">Environments</RouterLink> page, or on an app in
        <RouterLink to="/admin/catalogs" class="text-accent underline underline-offset-2 hover:text-ink">Catalogs</RouterLink>.
      </p>
    </div>

    <p v-if="customs.isPending.value" class="text-sm text-ink-3">Loading…</p>
    <FormError v-else-if="customs.isError.value" :message="customs.error.value?.message ?? 'Failed to load'" />
    <div v-else-if="!customs.data.value?.length" class="card px-6 py-10 text-center">
      <p class="text-base font-medium">No custom environments</p>
      <p class="mt-1 text-sm text-ink-2">Duplicate an environment to make the first.</p>
    </div>

    <ul v-else class="card divide-y divide-line">
      <li v-for="c in customs.data.value" :key="c.id" class="flex flex-wrap items-start gap-x-4 gap-y-2 px-4 py-3 sm:px-5">
        <div class="flex size-10 shrink-0 items-center justify-center rounded-lg bg-canvas">
          <img v-if="c.hasIcon" :src="`${catalogIconUrl(c.id)}?v=${c.updatedAt}`" alt="" class="size-7 object-contain" />
          <Package v-else class="size-5 text-ink-3" aria-hidden="true" />
        </div>
        <div class="min-w-0 flex-1 basis-64">
          <p class="text-sm font-medium">
            {{ c.overrides.name || c.baseName }} <span class="ml-1 font-mono text-xs font-normal text-ink-3">{{ c.id }}</span>
          </p>
          <p class="text-xs text-ink-2">{{ summaryLine(c) }}</p>
          <p v-if="c.unavailable" class="mt-1 flex items-start gap-1.5 text-xs text-warn">
            <TriangleAlert class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
            <span class="min-w-0 break-words">Not available: {{ c.unavailable }}</span>
          </p>
          <p class="mt-0.5 text-2xs text-ink-3" :title="dateTime(c.updatedAt)">Changed {{ ago(c.updatedAt) }}</p>
        </div>
        <div class="flex gap-2">
          <RouterLink :to="{ name: 'custom-edit', params: { id: c.id } }" class="btn-ghost px-3 py-1 text-xs">
            <Pencil class="size-3.5" aria-hidden="true" />Edit<span class="sr-only"> {{ c.overrides.name || c.id }}</span>
          </RouterLink>
          <button type="button" class="btn-ghost px-3 py-1 text-xs hover:border-danger/60 hover:text-danger" @click="confirmRemove(c)">
            Delete<span class="sr-only"> {{ c.overrides.name || c.id }}</span>
          </button>
        </div>
      </li>
    </ul>

    <ConfirmDialog
      :open="!!target"
      :title="`Delete ${target?.overrides.name || target?.id || ''}?`"
      :confirm-label="remove.isPending.value ? 'Deleting…' : 'Delete'"
      :busy="remove.isPending.value"
      :error="removeError"
      @cancel="target = null"
      @confirm="target && remove.mutate(target)"
    >
      <p>It leaves the dashboard. It can't be deleted while it is running.</p>
    </ConfirmDialog>
  </div>
</template>

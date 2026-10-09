<script setup lang="ts">
// What one user may launch: the nodes they are limited to, how many environments they can run
// at once, and single apps shared from a node that is otherwise off limits. The access form
// has a Save button; grants take effect at once.
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Plus, Trash2 } from "lucide-vue-next";
import { computed, ref, useId, watch } from "vue";

import { ApiError, api, type User } from "../api";
import { accessBody, accessChanged, formFromAccess, grantLabel, hasGrant, limitHint, toggleNode, MAX_INSTANCES_LIMIT, type AccessForm } from "../userAccess";
import FormError from "./FormError.vue";
import ToggleSwitch from "./ToggleSwitch.vue";

const props = defineProps<{ user: User }>();

const queryClient = useQueryClient();
const id = useId();
const key = computed(() => ["user-access", props.user.id]);

const access = useQuery({ queryKey: key, queryFn: () => api.userAccess(props.user.id) });
const nodes = useQuery({ queryKey: ["nodes"], queryFn: api.nodes, staleTime: 30_000 });
const catalog = useQuery({ queryKey: ["catalog"], queryFn: api.catalog, staleTime: 60_000 });

const form = ref<AccessForm>({ nodeRestricted: false, nodeIds: [], maxInstances: "" });
const saved = computed(() => (access.data.value ? formFromAccess(access.data.value) : null));
// Load the server's values into the form when they arrive or change, unless the admin is mid-edit.
watch(
  saved,
  (now, before) => {
    if (!now) return;
    if (!before || !accessChanged(form.value, before)) form.value = { ...now, nodeIds: [...now.nodeIds] };
  },
  { immediate: true },
);
const dirty = computed(() => !!saved.value && accessChanged(form.value, saved.value));

const message = (err: unknown, fallback: string) => (err instanceof ApiError && err.message ? err.message : fallback);

const saveError = ref<string | null>(null);
const savedNote = ref(false);
const save = useMutation({
  mutationFn: () => {
    const body = accessBody(form.value);
    if (!body.ok) throw new Error(body.error);
    return api.setUserAccess(props.user.id, body.body);
  },
  onMutate: () => {
    saveError.value = null;
    savedNote.value = false;
  },
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: key.value });
    await queryClient.invalidateQueries({ queryKey: ["users"] });
    savedNote.value = true;
  },
  onError: (err) => (saveError.value = err instanceof Error && !(err instanceof ApiError) ? err.message : message(err, "Couldn't save.")),
});

// ---- grants ----------------------------------------------------------------------------

const grantNode = ref("");
const grantTemplate = ref("");
const grantError = ref<string | null>(null);
const templates = computed(() => [...(catalog.data.value ?? [])].sort((a, b) => a.name.localeCompare(b.name)));
const nodeList = computed(() => nodes.data.value ?? []);
const duplicate = computed(() => !!access.data.value && hasGrant(access.data.value.grants, grantNode.value, grantTemplate.value));

const addGrant = useMutation({
  mutationFn: () => api.addGrant(props.user.id, { nodeId: grantNode.value, templateId: grantTemplate.value }),
  onMutate: () => (grantError.value = null),
  onSuccess: async () => {
    grantTemplate.value = "";
    await queryClient.invalidateQueries({ queryKey: key.value });
  },
  onError: (err) => (grantError.value = message(err, "Couldn't add the grant.")),
});
const removeGrant = useMutation({
  mutationFn: (grantId: string) => api.removeGrant(props.user.id, grantId),
  onMutate: () => (grantError.value = null),
  onSuccess: () => queryClient.invalidateQueries({ queryKey: key.value }),
  onError: (err) => (grantError.value = message(err, "Couldn't remove the grant.")),
});
</script>

<template>
  <div class="space-y-6 py-1">
    <p v-if="access.isPending.value" class="text-sm text-ink-3">Loading…</p>
    <FormError v-else-if="access.isError.value" :message="access.error.value?.message ?? 'Failed to load'" />
    <template v-else-if="access.data.value">
      <form class="space-y-4" @submit.prevent="save.mutate()">
        <div class="flex items-start gap-3">
          <ToggleSwitch
            :model-value="form.nodeRestricted"
            :aria-labelledby="`${id}-restrict`"
            :aria-describedby="`${id}-restrict-hint`"
            @update:model-value="form.nodeRestricted = $event"
          />
          <div class="min-w-0">
            <p :id="`${id}-restrict`" class="text-sm font-medium">Only allow selected nodes</p>
            <p :id="`${id}-restrict-hint`" class="text-xs text-ink-3">Off: this user can launch on any node.</p>
          </div>
        </div>

        <fieldset v-if="form.nodeRestricted" class="space-y-1">
          <legend class="label">Nodes this user may use</legend>
          <p v-if="nodes.isPending.value" class="text-sm text-ink-3">Loading nodes…</p>
          <FormError v-else-if="nodes.isError.value" :message="nodes.error.value?.message ?? 'Failed to load'" />
          <p v-else-if="!nodeList.length" class="text-sm text-ink-3">No nodes are enrolled yet.</p>
          <ul v-else class="grid gap-x-4 gap-y-1 sm:grid-cols-2">
            <li v-for="n in nodeList" :key="n.id">
              <label class="flex min-h-9 items-center gap-2 text-sm pointer-coarse:min-h-11">
                <input
                  type="checkbox"
                  class="size-4 accent-[var(--cha-accent-fill)]"
                  :checked="form.nodeIds.includes(n.id)"
                  @change="form.nodeIds = toggleNode(form.nodeIds, n.id, ($event.target as HTMLInputElement).checked)"
                />
                <span class="min-w-0 truncate">{{ n.name }}</span>
                <span v-if="!n.online" class="text-xs text-ink-3">offline</span>
              </label>
            </li>
          </ul>
          <p v-if="!form.nodeIds.length && nodeList.length" class="text-xs text-warn">With none ticked this user can only launch what is shared below.</p>
        </fieldset>

        <div class="max-w-xs">
          <label class="label" :for="`${id}-max`">Max active instances</label>
          <input
            :id="`${id}-max`"
            v-model="form.maxInstances"
            class="field"
            type="number"
            inputmode="numeric"
            min="1"
            :max="MAX_INSTANCES_LIMIT"
            step="1"
            :placeholder="`Default (${access.data.value.effectiveMax})`"
            :aria-describedby="`${id}-max-hint`"
          />
          <p :id="`${id}-max-hint`" class="mt-1.5 text-xs text-ink-3">
            Leave empty for the portal's default. {{ limitHint(access.data.value) }}
          </p>
        </div>

        <FormError :message="saveError" />
        <div class="flex flex-wrap items-center gap-3">
          <button type="submit" class="btn-primary max-sm:w-full" :disabled="save.isPending.value || !dirty">
            {{ save.isPending.value ? "Saving…" : "Save access" }}
          </button>
          <p v-if="savedNote && !dirty" role="status" class="text-sm text-ink-2">Saved.</p>
        </div>
      </form>

      <section class="space-y-3 border-t border-line pt-5" :aria-labelledby="`${id}-grants`">
        <div>
          <h3 :id="`${id}-grants`" class="text-sm font-semibold">Shared apps</h3>
          <p class="text-xs text-ink-3">An app on a node this user is otherwise kept off. It shows under “Shared with you” on their dashboard.</p>
        </div>
        <p v-if="!access.data.value.grants.length" class="text-sm text-ink-3">Nothing shared.</p>
        <ul v-else class="divide-y divide-line rounded-lg border border-line">
          <li v-for="g in access.data.value.grants" :key="g.id" class="flex items-center gap-3 px-3 py-2">
            <p class="min-w-0 flex-1 text-sm break-words">
              <span class="font-medium">{{ grantLabel(g, nodeList, templates).app }}</span>
              <span class="text-ink-2"> on {{ grantLabel(g, nodeList, templates).node }}</span>
            </p>
            <button
              type="button"
              class="inline-flex size-9 shrink-0 items-center justify-center rounded-lg text-ink-3 transition hover:bg-panel-2 hover:text-danger pointer-coarse:size-11"
              :aria-label="`Stop sharing ${grantLabel(g, nodeList, templates).text}`"
              :title="`Stop sharing ${grantLabel(g, nodeList, templates).text}`"
              :disabled="removeGrant.isPending.value && removeGrant.variables.value === g.id"
              @click="removeGrant.mutate(g.id)"
            >
              <Trash2 class="size-4" aria-hidden="true" />
            </button>
          </li>
        </ul>

        <form class="grid gap-3 sm:grid-cols-[1fr_1fr_auto] sm:items-end" @submit.prevent="addGrant.mutate()">
          <div>
            <label class="label" :for="`${id}-gnode`">Node</label>
            <select :id="`${id}-gnode`" v-model="grantNode" class="field" required>
              <option value="" disabled>Choose a node</option>
              <option v-for="n in nodeList" :key="n.id" :value="n.id">{{ n.name }}</option>
            </select>
          </div>
          <div>
            <label class="label" :for="`${id}-gapp`">App</label>
            <select :id="`${id}-gapp`" v-model="grantTemplate" class="field" required>
              <option value="" disabled>Choose an app</option>
              <option v-for="t in templates" :key="t.id" :value="t.id">{{ t.name }}</option>
            </select>
          </div>
          <button type="submit" class="btn-ghost max-sm:w-full" :disabled="!grantNode || !grantTemplate || duplicate || addGrant.isPending.value">
            <Plus class="size-4" aria-hidden="true" />
            Share app
          </button>
        </form>
        <p v-if="duplicate" class="text-xs text-ink-3">Already shared.</p>
        <FormError :message="grantError" />
      </section>
    </template>
  </div>
</template>

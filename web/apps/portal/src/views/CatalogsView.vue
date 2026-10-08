<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Copy, Package, RefreshCw, ShieldCheck, TriangleAlert } from "lucide-vue-next";
import { computed, reactive, ref } from "vue";

import { api, catalogIconUrl, type CatalogTemplateView, type CatalogView } from "../api";
import { CATALOGS_KEY, IMAGE_SPEC_URL, addBody, approvalGrant, availability, errorText, needsApproval, sourceLabel } from "../catalogs";
import DuplicateDialog from "../components/DuplicateDialog.vue";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import FormError from "../components/FormError.vue";
import SegmentedControl from "../components/SegmentedControl.vue";
import { ago, dateTime } from "../format";

const queryClient = useQueryClient();
const catalogs = useQuery({ queryKey: CATALOGS_KEY, queryFn: api.adminCatalogs });
const list = computed(() => catalogs.data.value?.catalogs ?? []);

// What changed here may change what the dashboard offers.
function refreshLists() {
  void queryClient.invalidateQueries({ queryKey: CATALOGS_KEY });
  void queryClient.invalidateQueries({ queryKey: ["catalog"] });
}

// ---- Add ----

const mode = ref<"url" | "paste">("url");
const modeOptions = [
  { value: "url", label: "From a URL" },
  { value: "paste", label: "Paste JSON" },
] as const;
const url = ref("");
const pasted = ref("");
const slug = ref("");
const addError = ref<string | null>(null);

const add = useMutation({
  mutationFn: () => api.addCatalog(addBody(mode.value, slug.value, mode.value === "url" ? url.value : pasted.value)),
  onMutate: () => (addError.value = null),
  onSuccess: () => {
    url.value = "";
    pasted.value = "";
    slug.value = "";
    refreshLists();
  },
  onError: (err) => (addError.value = errorText(err, "Couldn't add the catalog.")),
});

// ---- Per catalog: refresh, approve, remove ----

/** A failed action, per catalog, in a live region on its card. */
const errors = reactive<Record<string, string>>({});
/** Catalogs whose pasted-document box for a refresh is open, and what is in it. */
const pasting = reactive<Record<string, string>>({});

const refresh = useMutation({
  mutationFn: (v: { c: CatalogView; document?: string }) => api.refreshCatalog(v.c.slug, v.document),
  onMutate: ({ c }) => delete errors[c.slug],
  onSuccess: (_, { c }) => {
    delete pasting[c.slug];
    refreshLists();
  },
  onError: (err, { c }) => {
    errors[c.slug] = errorText(err, "Couldn't refresh the catalog.");
    // A failed fetch is recorded on the catalog too.
    refreshLists();
  },
});

function startRefresh(c: CatalogView) {
  if (c.url) refresh.mutate({ c });
  else if (c.slug in pasting) delete pasting[c.slug];
  else pasting[c.slug] = "";
}

const approve = useMutation({
  mutationFn: (v: { c: CatalogView; t: CatalogTemplateView; approved: boolean }) =>
    api.approveCatalogTemplate(v.c.slug, v.t.app, v.approved),
  onMutate: ({ c }) => delete errors[c.slug],
  onSuccess: refreshLists,
  onError: (err, { c }) => (errors[c.slug] = errorText(err, "Couldn't change the approval.")),
});

const duplicating = ref<{ id: string; name: string } | null>(null);

const removeTarget = ref<CatalogView | null>(null);
const removeError = ref<string | null>(null);
const remove = useMutation({
  mutationFn: (c: CatalogView) => api.removeCatalog(c.slug),
  onMutate: () => (removeError.value = null),
  onSuccess: () => {
    removeTarget.value = null;
    refreshLists();
  },
  onError: (err) => (removeError.value = errorText(err, "Couldn't remove the catalog.")),
});

function confirmRemove(c: CatalogView) {
  removeError.value = null;
  removeTarget.value = c;
}
</script>

<template>
  <div class="max-w-4xl space-y-6">
    <div class="space-y-1 text-sm text-ink-2">
      <p>
        A catalog is a list of apps, each an image from a registry. Adding one makes its
        <span class="font-mono">standard</span> apps available to everyone; apps that need a browser sandbox or Steam wait for your approval.
      </p>
      <p>
        Authors: see the
        <a :href="IMAGE_SPEC_URL" target="_blank" rel="noopener" class="text-accent underline underline-offset-2 hover:text-ink">image and catalog spec</a>.
      </p>
      <p>
        To change an app a little instead (a bigger <span class="font-mono">/dev/shm</span>, another image tag, a variable), duplicate it into a
        <RouterLink to="/admin/custom" class="text-accent underline underline-offset-2 hover:text-ink">custom environment</RouterLink>.
      </p>
    </div>

    <!-- Add -->
    <form class="card space-y-4 p-4 sm:p-5" @submit.prevent="add.mutate()">
      <h2 class="text-base font-semibold">Add a catalog</h2>
      <SegmentedControl v-model="mode" label="Source" :options="modeOptions" />
      <div v-if="mode === 'url'">
        <label for="cat-url" class="label">Catalog URL</label>
        <input id="cat-url" v-model="url" type="url" required class="field" placeholder="https://example.com/catalog.json" autocomplete="off" />
      </div>
      <div v-else>
        <label for="cat-doc" class="label">Catalog JSON</label>
        <textarea
          id="cat-doc"
          v-model="pasted"
          required
          rows="8"
          spellcheck="false"
          class="field font-mono"
          placeholder='{ "version": 1, "templates": [ … ] }'
        />
      </div>
      <div class="sm:max-w-xs">
        <label for="cat-slug" class="label">Name in the portal (optional)</label>
        <input id="cat-slug" v-model="slug" class="field font-mono" placeholder="from the document's id" autocomplete="off" spellcheck="false" aria-describedby="cat-slug-hint" />
        <p id="cat-slug-hint" class="mt-1 text-xs text-ink-3">
          Lowercase letters, digits and hyphens. Its apps get ids like <span class="font-mono">{{ slug.trim() || "name" }}.app</span>.
        </p>
      </div>
      <FormError :message="addError" />
      <button type="submit" class="btn-primary" :disabled="add.isPending.value">
        {{ add.isPending.value ? "Adding…" : "Add catalog" }}
      </button>
    </form>

    <!-- List -->
    <p v-if="catalogs.isPending.value" class="text-sm text-ink-3">Loading…</p>
    <FormError v-else-if="catalogs.isError.value" :message="catalogs.error.value?.message ?? 'Failed to load'" />
    <div v-else-if="!list.length" class="card px-6 py-10 text-center">
      <p class="text-base font-medium">No catalogs loaded</p>
      <p class="mt-1 text-sm text-ink-2">Only the built-in apps are offered.</p>
    </div>

    <ul v-else class="space-y-4">
      <li v-for="c in list" :key="c.slug" class="card">
        <div class="flex flex-wrap items-start justify-between gap-3 px-4 py-4 sm:px-5">
          <div class="min-w-0">
            <h2 class="text-base font-semibold">
              {{ c.name }} <span class="ml-1 font-mono text-sm font-normal text-ink-3">{{ c.slug }}</span>
            </h2>
            <p class="mt-0.5 text-sm break-all text-ink-2">
              <span class="text-ink-3">Source:</span> {{ sourceLabel(c) }}
            </p>
            <p class="mt-0.5 text-sm text-ink-2">
              <span class="text-ink-3">Fetched</span>{{ " " }}
              <time :datetime="new Date(c.fetchedAt * 1000).toISOString()" :title="dateTime(c.fetchedAt)">{{ ago(c.fetchedAt) }}</time>
              · {{ availability(c) }}
            </p>
          </div>
          <div class="flex flex-wrap gap-2">
            <button
              type="button"
              class="btn-ghost"
              :disabled="refresh.isPending.value"
              :aria-expanded="c.url ? undefined : c.slug in pasting"
              @click="startRefresh(c)"
            >
              <RefreshCw class="size-4" aria-hidden="true" />
              {{ refresh.isPending.value && refresh.variables.value?.c.slug === c.slug ? "Refreshing…" : "Refresh" }}
            </button>
            <button type="button" class="btn-ghost" @click="confirmRemove(c)">Remove</button>
          </div>
        </div>

        <div v-if="c.slug in pasting" class="space-y-3 border-t border-line px-4 py-4 sm:px-5">
          <label :for="`doc-${c.slug}`" class="label">The new document for {{ c.name }}</label>
          <textarea :id="`doc-${c.slug}`" v-model="pasting[c.slug]" rows="6" spellcheck="false" class="field font-mono" />
          <div class="flex gap-2">
            <button
              type="button"
              class="btn-primary"
              :disabled="refresh.isPending.value || !pasting[c.slug]?.trim()"
              @click="refresh.mutate({ c, document: pasting[c.slug] })"
            >
              Refresh from this
            </button>
            <button type="button" class="btn-ghost" @click="delete pasting[c.slug]">Cancel</button>
          </div>
        </div>

        <div class="space-y-2 px-4 empty:hidden sm:px-5" aria-live="polite">
          <FormError v-if="errors[c.slug]" polite :message="errors[c.slug] ?? null" class="mb-3" />
          <p v-if="c.lastError" class="mb-3 flex items-start gap-2 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-sm text-warn">
            <TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
            <span class="min-w-0 break-words">
              Last refresh failed<template v-if="c.lastErrorAt"> ({{ ago(c.lastErrorAt) }})</template>, so the previous document stays in use: {{ c.lastError }}
            </span>
          </p>
        </div>

        <ul class="divide-y divide-line border-t border-line">
          <li v-for="t in c.templates" :key="t.id" class="flex flex-wrap items-start gap-x-4 gap-y-2 px-4 py-3 sm:px-5">
            <div class="flex size-10 shrink-0 items-center justify-center rounded-lg bg-canvas">
              <img v-if="t.hasIcon" :src="catalogIconUrl(t.id)" alt="" class="size-7" />
              <Package v-else class="size-5 text-ink-3" aria-hidden="true" />
            </div>
            <div class="min-w-0 flex-1 basis-64">
              <p class="text-sm font-medium">
                {{ t.name }} <span class="ml-1 font-mono text-xs font-normal text-ink-3">{{ t.id }}</span>
              </p>
              <p class="font-mono text-xs break-all text-ink-3">{{ t.image }}</p>
              <p class="mt-0.5 text-xs text-ink-2">
                Profile <span class="font-mono">{{ t.security }}</span>
                <template v-if="t.available"> ·
                  <span class="text-ok">Available</span>
                </template>
                <template v-else> ·
                  <span class="text-warn">Not available: {{ t.unavailableReason ?? "unknown" }}</span>
                </template>
              </p>
              <p v-if="t.iconError" class="mt-0.5 text-xs text-warn">Logo not loaded: {{ t.iconError }}</p>
              <p v-if="needsApproval(t)" :id="`grant-${t.id}`" class="mt-1 flex items-start gap-1.5 text-xs text-ink-3">
                <ShieldCheck class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
                <span>
                  Approval {{ approvalGrant(t.security) }}. It applies to images from <span class="font-mono">{{ t.imageHost }}</span>; a refresh that changes the profile or the host clears it.
                </span>
              </p>
            </div>
            <button
              v-if="t.available"
              type="button"
              class="btn-ghost"
              @click="duplicating = { id: t.id, name: t.name }"
            >
              <Copy class="size-4" aria-hidden="true" />Duplicate<span class="sr-only"> {{ t.name }}</span>
            </button>
            <button
              v-if="needsApproval(t)"
              type="button"
              :class="t.approved ? 'btn-ghost' : 'btn-primary'"
              :aria-describedby="`grant-${t.id}`"
              :disabled="approve.isPending.value"
              @click="approve.mutate({ c, t, approved: !t.approved })"
            >
              {{ t.approved ? "Revoke" : "Approve" }}<span class="sr-only"> {{ t.name }}</span>
            </button>
          </li>
          <li v-if="!c.templates.length" class="px-4 py-3 text-sm text-ink-3 sm:px-5">This catalog has no apps.</li>
        </ul>
      </li>
    </ul>

    <DuplicateDialog :template="duplicating" @close="duplicating = null" />

    <ConfirmDialog
      :open="!!removeTarget"
      :title="`Remove ${removeTarget?.name ?? ''}?`"
      :confirm-label="remove.isPending.value ? 'Removing…' : 'Remove'"
      :busy="remove.isPending.value"
      :error="removeError"
      @cancel="removeTarget = null"
      @confirm="removeTarget && remove.mutate(removeTarget)"
    >
      <p>
        Its apps leave the dashboard and its approvals are dropped. People's settings and saved data for them are kept, and come back if you add a catalog under the same name again.
      </p>
    </ConfirmDialog>
  </div>
</template>

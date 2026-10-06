<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { RotateCcw } from "lucide-vue-next";
import { computed, onBeforeUnmount, reactive, ref } from "vue";

import { ApiError, api, type StorageApp } from "../api";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import FormError from "../components/FormError.vue";
import NotAvailable from "../components/NotAvailable.vue";
import ToggleSwitch from "../components/ToggleSwitch.vue";
import { useSession } from "../stores/session";
import { STORAGE_KEY, dataPath, notAvailable, patchApp, sharedNote, sharedPath } from "../storage";

const session = useSession();
const queryClient = useQueryClient();

// Apps whose change is still being saved; polling waits so it can't undo them.
const pending = reactive(new Set<string>());
// `live` comes from the server, so poll to notice an app starting or stopping.
const storage = useQuery({
  queryKey: STORAGE_KEY,
  queryFn: api.storage,
  refetchInterval: () => (pending.size ? false : 5000),
});
const apps = computed(() => storage.data.value?.apps ?? []);
const root = computed(() => storage.data.value?.root ?? "");

// Apps whose shared data has parts each user keeps apart (Steam's Proton
// prefixes): those live in the user's own data, so without it nothing is shared.
const catalog = useQuery({ queryKey: ["catalog"], queryFn: api.catalog, staleTime: 60_000 });
const sharesOnlyWhenKept = (app: StorageApp) =>
  !!catalog.data.value?.find((t) => t.id === app.template)?.shared?.perUser.length;

// Per app: a failed save, or a finished reset. One live region per row announces both.
const errors = reactive<Record<string, string>>({});
const notes = reactive<Record<string, string>>({});
const noteTimers = new Set<ReturnType<typeof setTimeout>>();
onBeforeUnmount(() => noteTimers.forEach(clearTimeout));

function failText(err: unknown, what: string, name: string): string {
  if (err instanceof ApiError) {
    if (err.status === 409) return `${name} is running. Stop it first.`;
    if (err.status === 404) return "Not available on this server yet.";
    if (err.message) return `${what}: ${err.message}`;
  }
  return `${what}.`;
}

// ---- Keep data between launches ----

const setPersistent = useMutation({
  mutationFn: (v: { app: StorageApp; persistent: boolean }) => api.setStoragePersistent(v.app.template, v.persistent),
  onMutate: async ({ app, persistent }) => {
    delete errors[app.template];
    delete notes[app.template];
    pending.add(app.template);
    await queryClient.cancelQueries({ queryKey: STORAGE_KEY });
    patchApp<StorageApp>(queryClient, STORAGE_KEY, app.template, { persistent });
    return { was: app.persistent };
  },
  onSuccess: (updated, { app }) => patchApp<StorageApp>(queryClient, STORAGE_KEY, app.template, updated),
  onError: (err, { app }, ctx) => {
    patchApp<StorageApp>(queryClient, STORAGE_KEY, app.template, {
      persistent: ctx?.was ?? app.persistent,
      ...(err instanceof ApiError && err.status === 409 ? { live: true } : {}),
    });
    errors[app.template] = failText(err, "Couldn't save the change", app.name);
  },
  onSettled: (_data, _err, { app }) => {
    pending.delete(app.template);
    if (!pending.size) void queryClient.invalidateQueries({ queryKey: STORAGE_KEY });
  },
});

const toggling = (app: StorageApp) => pending.has(app.template);

function onToggle(app: StorageApp, persistent: boolean) {
  if (app.live || toggling(app)) return;
  setPersistent.mutate({ app, persistent });
}

function hint(app: StorageApp): string {
  const parts: string[] = [];
  if (app.persistent !== app.default) parts.push(`Default: ${app.default ? "on" : "off"}`);
  if (app.live) parts.push("Stop it to change this");
  return parts.join(" · ");
}

// ---- Reset ----

const resetTarget = ref<StorageApp | null>(null);
const resetError = ref<string | null>(null);

const reset = useMutation({
  mutationFn: (app: StorageApp) => api.resetStorage(app.template),
  onMutate: () => (resetError.value = null),
  onSuccess: (_data, app) => {
    resetTarget.value = null;
    delete errors[app.template];
    notes[app.template] = `${app.name} data reset.`;
    noteTimers.add(setTimeout(() => delete notes[app.template], 8000));
  },
  onError: (err, app) => {
    resetError.value = failText(err, "The node couldn't reset the data", app.name);
    if (err instanceof ApiError && err.status === 409) void queryClient.invalidateQueries({ queryKey: STORAGE_KEY });
  },
});

// Back to the button that asked, whichever way the dialog closed.
let resetOpener = "";

function askReset(app: StorageApp) {
  resetError.value = null;
  resetTarget.value = app;
  resetOpener = `${app.template}-reset`;
}

function afterReset() {
  document.getElementById(resetOpener)?.focus();
}

/** What a reset removes, for the confirmation. */
function resetDetail(app: StorageApp): string {
  return app.template === "steam" ? "the client, your login and settings" : "its settings, logins and files";
}

function resetKeeps(app: StorageApp): string | null {
  if (app.sharedAccess === "none") return null;
  return app.template === "steam" ? "Games installed in the shared library stay." : "Data shared with other users stays.";
}
</script>

<template>
  <div class="max-w-3xl space-y-6">
    <div class="space-y-1 text-sm text-ink-2">
      <p>
        Choose which apps keep their data between launches. An app that doesn't keep its data starts fresh every time.
        <template v-if="root">
          Saved data is stored on the node under <code class="font-mono text-ink">{{ root }}</code
          >.
        </template>
      </p>
      <p v-if="session.isAdmin">
        Defaults and shared data for everyone are under
        <RouterLink to="/admin/storage" class="text-accent underline underline-offset-2 hover:text-ink">App data</RouterLink>.
      </p>
    </div>

    <p v-if="storage.isPending.value" class="text-sm text-ink-3">Loading…</p>
    <NotAvailable v-else-if="notAvailable(storage.error.value)" />
    <FormError v-else-if="storage.isError.value" :message="storage.error.value?.message ?? 'Failed to load'" />
    <div v-else-if="!apps.length" class="card px-6 py-10 text-center">
      <p class="text-base font-medium">No apps</p>
      <p class="mt-1 text-sm text-ink-2">Nothing here keeps data yet.</p>
    </div>

    <ul v-else class="card divide-y divide-line">
      <li v-for="app in apps" :key="app.template" class="px-4 py-4 sm:px-5">
        <h2 :id="`${app.template}-name`" class="text-base font-semibold">{{ app.name }}</h2>
        <p class="mt-0.5 font-mono text-xs break-all text-ink-3">{{ dataPath(root, app.template) }}</p>
        <template v-if="app.sharedAccess !== 'none'">
          <p class="mt-1 text-xs text-ink-2">
            {{ sharedNote(app.template, app.sharedAccess)
            }}<template v-if="sharesOnlyWhenKept(app)">, while you keep your data</template>
          </p>
          <p class="font-mono text-xs break-all text-ink-3">{{ sharedPath(root, app) }}</p>
        </template>

        <div class="mt-3 flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
          <div class="flex items-start gap-3">
            <ToggleSwitch
              :model-value="app.persistent"
              :disabled="app.live"
              :aria-labelledby="`${app.template}-name ${app.template}-label`"
              :aria-describedby="hint(app) ? `${app.template}-hint` : undefined"
              @update:model-value="onToggle(app, $event)"
            />
            <div class="min-w-0 text-sm">
              <p :id="`${app.template}-label`">Keep data between launches</p>
              <p v-if="hint(app)" :id="`${app.template}-hint`" class="text-xs text-ink-3">{{ hint(app) }}</p>
            </div>
          </div>
          <button
            :id="`${app.template}-reset`"
            type="button"
            class="btn-ghost shrink-0 px-3 py-1.5 hover:border-danger/60 hover:text-danger"
            :disabled="app.live"
            :title="app.live ? 'Stop it to reset its data' : undefined"
            :aria-labelledby="`${app.template}-reset ${app.template}-name`"
            @click="askReset(app)"
          >
            <RotateCcw class="size-4" aria-hidden="true" />
            Reset data…
          </button>
        </div>

        <div aria-live="polite">
          <FormError v-if="errors[app.template]" polite class="mt-3" :message="errors[app.template] ?? null" />
          <p v-else-if="notes[app.template]" class="mt-3 text-xs text-ink-2">{{ notes[app.template] }}</p>
        </div>
      </li>
    </ul>

    <ConfirmDialog
      :open="!!resetTarget"
      :title="`Reset ${resetTarget?.name ?? ''} data?`"
      :confirm-label="reset.isPending.value ? 'Resetting…' : 'Reset data'"
      :busy="reset.isPending.value"
      :error="resetError"
      @cancel="resetTarget = null"
      @closed="afterReset"
      @confirm="resetTarget && reset.mutate(resetTarget)"
    >
      <template v-if="resetTarget">
        <p>
          This deletes {{ resetTarget.name }}'s saved data for your account: {{ resetDetail(resetTarget) }}. It can't
          be undone.
        </p>
        <p v-if="resetKeeps(resetTarget)">{{ resetKeeps(resetTarget) }}</p>
      </template>
    </ConfirmDialog>
  </div>
</template>

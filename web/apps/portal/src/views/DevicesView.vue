<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Laptop, Trash2 } from "lucide-vue-next";
import { computed, ref } from "vue";

import { ApiError, api, type PlayerDevice } from "../api";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import FormError from "../components/FormError.vue";
import { ago } from "../format";

const queryClient = useQueryClient();
const KEY = ["player-devices"] as const;

const devices = useQuery({ queryKey: KEY, queryFn: api.playerDevices });
const list = computed(() => devices.data.value ?? []);

const target = ref<PlayerDevice | null>(null);
const error = ref<string | null>(null);
const revoke = useMutation({
  mutationFn: (d: PlayerDevice) => api.revokePlayerDevice(d.id),
  onMutate: () => (error.value = null),
  onSuccess: () => {
    target.value = null;
    void queryClient.invalidateQueries({ queryKey: KEY });
  },
  onError: (err) => {
    error.value = err instanceof ApiError && err.message ? err.message : "Couldn't revoke the device.";
    if (err instanceof ApiError && err.status === 404) void queryClient.invalidateQueries({ queryKey: KEY });
  },
});
</script>

<template>
  <div class="max-w-3xl space-y-6">
    <section class="card space-y-3 px-4 py-4 sm:px-5" aria-labelledby="devices-title">
      <div>
        <h2 id="devices-title" class="text-base font-semibold">Cha Player devices</h2>
        <p class="mt-1 text-sm text-ink-2">
          Installs of Cha Player signed in as you. Revoke one and it has to sign in again.
          To add one, use “Open in Cha Player” or type its code at
          <RouterLink to="/link" class="underline hover:text-ink">/link</RouterLink>.
        </p>
      </div>
      <FormError v-if="devices.isError.value" :message="devices.error.value?.message ?? 'Failed to load'" />
      <p v-else-if="devices.isPending.value" class="text-sm text-ink-3">Loading…</p>
      <p v-else-if="!list.length" class="text-sm text-ink-3">No devices yet.</p>
      <ul v-else class="divide-y divide-line">
        <li v-for="d in list" :key="d.id" class="flex items-center justify-between gap-3 py-3 first:pt-0 last:pb-0">
          <div class="flex min-w-0 items-center gap-3">
            <Laptop class="size-5 shrink-0 text-ink-3" aria-hidden="true" />
            <div class="min-w-0">
              <p class="truncate text-sm font-medium">{{ d.name }}</p>
              <p class="text-xs text-ink-3">
                added {{ ago(d.createdAt) }} · last used {{ ago(d.lastUsedAt) }}<template v-if="d.lastIp">
                  from <span class="font-mono">{{ d.lastIp }}</span></template>
              </p>
            </div>
          </div>
          <button
            type="button"
            class="btn-ghost shrink-0 px-3 py-1.5 hover:border-danger/60 hover:text-danger"
            @click="
              error = null;
              target = d;
            "
          >
            <Trash2 class="size-4" aria-hidden="true" />
            Revoke<span class="sr-only"> {{ d.name }}</span>
          </button>
        </li>
      </ul>
    </section>

    <ConfirmDialog
      :open="!!target"
      :title="`Revoke ${target?.name ?? ''}?`"
      :confirm-label="revoke.isPending.value ? 'Revoking…' : 'Revoke'"
      :busy="revoke.isPending.value"
      :error="error"
      @cancel="target = null"
      @confirm="target && revoke.mutate(target)"
    >
      <p>Revoke {{ target?.name }}? Cha Player on it is signed out and has to sign in again.</p>
    </ConfirmDialog>
  </div>
</template>

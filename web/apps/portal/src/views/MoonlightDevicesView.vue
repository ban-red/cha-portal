<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Check, Copy, Trash2 } from "lucide-vue-next";
import { computed, onBeforeUnmount, reactive, ref } from "vue";

import { ApiError, api, type GamestreamDevice } from "../api";
import ConfirmDialog from "../components/ConfirmDialog.vue";
import FormError from "../components/FormError.vue";
import { ago } from "../format";
import { cleanPin, expiresIn, hostAddress, pairErrorText, validPin } from "../moonlightDevices";
import { notAvailable } from "../storage";
import { useSession } from "../stores/session";

const session = useSession();
const queryClient = useQueryClient();
const DEVICES_KEY = ["gamestream", "devices"] as const;

// Ticks so "expires in N s" counts down between polls.
const now = ref(Date.now());
const tick = setInterval(() => (now.value = Date.now()), 1000);
onBeforeUnmount(() => {
  clearInterval(tick);
  clearTimeout(copiedTimer);
  noteTimers.forEach(clearTimeout);
});

const hosts = useQuery({ queryKey: ["gamestream", "hosts"], queryFn: api.gamestreamHosts, retry: false });
// Polls only while the tab is visible (TanStack's default).
const pairing = useQuery({
  queryKey: ["gamestream", "pairing"],
  queryFn: api.gamestreamPairing,
  refetchInterval: 2000,
  retry: false,
});
const devices = useQuery({ queryKey: DEVICES_KEY, queryFn: api.gamestreamDevices, retry: false });

const unavailable = computed(() => [hosts, pairing, devices].some((q) => notAvailable(q.error.value)));
const requests = computed(() => pairing.data.value?.requests ?? []);
const deviceList = computed(() => devices.data.value?.devices ?? []);
const hostList = computed(() => hosts.data.value?.hosts ?? []);

// ---- How to connect ----

const copied = ref<string | null>(null);
let copiedTimer: ReturnType<typeof setTimeout> | undefined;
async function copy(key: string, text: string) {
  try {
    await navigator.clipboard.writeText(text);
    copied.value = key;
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied.value = null), 2000);
  } catch {
    // No clipboard outside a secure context: the address is on screen to select.
  }
}

// ---- Waiting to pair ----

const pins = reactive<Record<string, string>>({});
const busy = reactive(new Set<string>());
const errors = reactive<Record<string, string>>({});
const notes = reactive<Record<string, string>>({});
const noteTimers = new Set<ReturnType<typeof setTimeout>>();

function onPinInput(id: string, value: string) {
  pins[id] = cleanPin(value);
  delete errors[id];
}

async function pair(id: string, deviceName: string) {
  if (busy.has(id)) return;
  const pin = validPin(pins[id] ?? "");
  if (!pin) {
    errors[id] = pairErrorText(null, "bad_pin", null);
    return;
  }
  delete errors[id];
  busy.add(id);
  try {
    await api.gamestreamPair(id, pin);
    delete pins[id];
    notes[id] = `Paired ${deviceName}.`;
    noteTimers.add(setTimeout(() => delete notes[id], 8000));
    void queryClient.invalidateQueries({ queryKey: DEVICES_KEY });
  } catch (err) {
    errors[id] =
      err instanceof ApiError ? pairErrorText(err.status, err.code, err.message) : pairErrorText(null, null, null);
    if (err instanceof ApiError && (err.status === 404 || err.status === 403 || err.status === 504)) delete pins[id];
    void queryClient.invalidateQueries({ queryKey: ["gamestream", "pairing"] });
  } finally {
    busy.delete(id);
    void queryClient.invalidateQueries({ queryKey: ["gamestream", "pairing"] });
  }
}

// ---- Paired devices ----

const removeTarget = ref<GamestreamDevice | null>(null);
const removeError = ref<string | null>(null);
const remove = useMutation({
  mutationFn: (d: GamestreamDevice) => api.removeGamestreamDevice(d.id),
  onMutate: () => (removeError.value = null),
  onSuccess: () => {
    removeTarget.value = null;
    void queryClient.invalidateQueries({ queryKey: DEVICES_KEY });
  },
  onError: (err) => {
    removeError.value = err instanceof ApiError && err.message ? err.message : "Couldn't remove the device.";
    if (notAvailable(err) || (err instanceof ApiError && err.status === 404)) {
      void queryClient.invalidateQueries({ queryKey: DEVICES_KEY });
    }
  },
});
const showOwner = (d: GamestreamDevice) => d.owner.id !== session.user?.id;
</script>

<template>
  <div class="max-w-3xl space-y-6">
    <p v-if="unavailable" class="text-sm text-ink-3">Moonlight isn't available on this portal.</p>

    <template v-else>
      <section class="card space-y-3 px-4 py-4 sm:px-5" aria-labelledby="ml-connect">
        <h2 id="ml-connect" class="text-base font-semibold">How to connect</h2>
        <ol class="list-inside list-decimal space-y-1 text-sm text-ink-2">
          <li>
            In Moonlight, add a PC. A node usually appears by itself; otherwise add its address (with the port if it
            isn't 47989).
          </li>
          <li>Pick it. Moonlight shows a PIN.</li>
          <li>Type the PIN below.</li>
        </ol>
        <ul v-if="hostList.length" class="divide-y divide-line rounded-lg border border-line">
          <li v-for="h in hostList" :key="h.nodeId" class="flex items-center justify-between gap-3 px-3 py-2 text-sm">
            <span class="min-w-0">
              <span class="font-medium">{{ h.nodeName }}</span>
              <span class="text-ink-3"> — </span>
              <span class="font-mono break-all text-ink-2">{{ hostAddress(h) }}</span>
            </span>
            <button type="button" class="btn-ghost shrink-0 px-3 py-1 text-xs" @click="copy(h.nodeId, hostAddress(h))">
              <Check v-if="copied === h.nodeId" class="size-3.5" aria-hidden="true" />
              <Copy v-else class="size-3.5" aria-hidden="true" />
              {{ copied === h.nodeId ? "Copied" : "Copy" }}
              <span class="sr-only"> address of {{ h.nodeName }}</span>
            </button>
          </li>
        </ul>
        <p v-else-if="hosts.isSuccess.value" class="text-sm text-ink-3">
          No node has Moonlight turned on (<code class="font-mono">CHA_GAMESTREAM</code> on the node).
        </p>
        <p class="text-sm text-ink-2">Moonlight lists the apps the node can run; picking one starts it there, or resumes your copy if it's already running.</p>
      </section>

      <section class="card space-y-3 px-4 py-4 sm:px-5" aria-labelledby="ml-waiting">
        <h2 id="ml-waiting" class="text-base font-semibold">Waiting to pair</h2>
        <FormError v-if="pairing.isError.value" :message="pairing.error.value?.message ?? 'Failed to load'" />
        <p v-else-if="pairing.isPending.value" class="text-sm text-ink-3">Loading…</p>
        <p v-else-if="!requests.length" class="text-sm text-ink-3">
          No device is asking to pair. Start pairing in Moonlight and it appears here.
        </p>
        <ul v-else class="divide-y divide-line">
          <li v-for="r in requests" :key="r.id" class="space-y-2 py-3 first:pt-0 last:pb-0">
            <div>
              <p class="text-sm font-medium">{{ r.deviceName }}</p>
              <p class="text-xs text-ink-3">
                on {{ r.nodeName }} · <span class="font-mono">{{ r.address }}</span> · expires
                {{ expiresIn(r.expiresAt, now) }}
              </p>
            </div>
            <form class="flex flex-wrap items-center gap-2" @submit.prevent="pair(r.id, r.deviceName)">
              <label :for="`pin-${r.id}`" class="sr-only">PIN shown on {{ r.deviceName }}</label>
              <input
                :id="`pin-${r.id}`"
                :value="pins[r.id] ?? ''"
                class="field w-24 text-center font-mono tracking-widest"
                inputmode="numeric"
                autocomplete="one-time-code"
                maxlength="4"
                placeholder="0000"
                :disabled="busy.has(r.id)"
                @input="onPinInput(r.id, ($event.target as HTMLInputElement).value)"
              />
              <button type="submit" class="btn-primary px-4 py-2" :disabled="busy.has(r.id) || (pins[r.id] ?? '').length !== 4">
                {{ busy.has(r.id) ? "Pairing…" : "Pair" }}
              </button>
            </form>
            <div aria-live="polite">
              <FormError v-if="errors[r.id]" polite :message="errors[r.id] ?? null" />
              <p v-else-if="busy.has(r.id)" class="text-xs text-ink-3">This can take up to 20 seconds.</p>
            </div>
          </li>
        </ul>
        <div aria-live="polite">
          <p v-for="(text, id) in notes" :key="id" class="text-sm text-ok">{{ text }}</p>
        </div>
      </section>

      <section class="card space-y-3 px-4 py-4 sm:px-5" aria-labelledby="ml-devices">
        <h2 id="ml-devices" class="text-base font-semibold">Paired devices</h2>
        <FormError v-if="devices.isError.value" :message="devices.error.value?.message ?? 'Failed to load'" />
        <p v-else-if="devices.isPending.value" class="text-sm text-ink-3">Loading…</p>
        <p v-else-if="!deviceList.length" class="text-sm text-ink-3">No paired devices yet.</p>
        <ul v-else class="divide-y divide-line">
          <li v-for="d in deviceList" :key="d.id" class="flex items-center justify-between gap-3 py-3 first:pt-0 last:pb-0">
            <div class="min-w-0">
              <p class="truncate text-sm font-medium">{{ d.name }}</p>
              <p class="text-xs text-ink-3">
                on {{ d.nodeName }} · paired {{ ago(d.pairedAt) }}<template v-if="showOwner(d)"> · {{ d.owner.name }}</template>
              </p>
            </div>
            <button
              :id="`remove-${d.id}`"
              type="button"
              class="btn-ghost shrink-0 px-3 py-1.5 hover:border-danger/60 hover:text-danger"
              @click="
                removeError = null;
                removeTarget = d;
              "
            >
              <Trash2 class="size-4" aria-hidden="true" />
              Remove<span class="sr-only"> {{ d.name }}</span>
            </button>
          </li>
        </ul>
      </section>
    </template>

    <ConfirmDialog
      :open="!!removeTarget"
      :title="`Remove ${removeTarget?.name ?? ''}?`"
      :confirm-label="remove.isPending.value ? 'Removing…' : 'Remove'"
      :busy="remove.isPending.value"
      :error="removeError"
      @cancel="removeTarget = null"
      @confirm="removeTarget && remove.mutate(removeTarget)"
    >
      <p>Remove {{ removeTarget?.name }}? It will need to pair again.</p>
    </ConfirmDialog>
  </div>
</template>

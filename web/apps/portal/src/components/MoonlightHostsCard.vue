<script setup lang="ts">
// Admin: Moonlight hosts (Sunshine/Apollo) the nodes see on their LAN, and the adopted ones.
// Adopting pairs a node with the host by a PIN typed into the host's own web UI.
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Check, ExternalLink, LoaderCircle, RefreshCw, Trash2 } from "lucide-vue-next";
import { computed, ref, watch } from "vue";

import { ApiError, api, type AdoptedHost, type FoundMoonlightHost } from "../api";
import { ago, dateTime } from "../format";
import { MOONLIGHT_FOUND_KEY, MOONLIGHT_HOSTS_KEY, isMissing } from "../moonlight";
import ConfirmDialog from "./ConfirmDialog.vue";
import FormError from "./FormError.vue";

const queryClient = useQueryClient();
const retry = (count: number, err: unknown) => !isMissing(err) && count < 2;

const found = useQuery({
  queryKey: MOONLIGHT_FOUND_KEY,
  queryFn: api.foundMoonlightHosts,
  refetchInterval: 5000,
  refetchIntervalInBackground: false,
  retry,
});
const adopted = useQuery({
  queryKey: MOONLIGHT_HOSTS_KEY,
  queryFn: api.moonlightHosts,
  refetchInterval: 10_000,
  refetchIntervalInBackground: false,
  retry,
});
// An older server has no Moonlight endpoints: hide the card.
const missing = computed(() => isMissing(found.error.value) || isMissing(adopted.error.value));
const foundHosts = computed(() => found.data.value?.hosts ?? []);
const adoptedHosts = computed(() => adopted.data.value?.hosts ?? []);
// A host that was just adopted can linger in the found list until the next poll.
const adoptedNames = computed(() => new Set(adoptedHosts.value.map((h) => h.name)));

function done() {
  void queryClient.invalidateQueries({ queryKey: ["moonlight"] });
}
const errorText = (err: unknown, fallback: string) => (err instanceof ApiError && err.message ? err.message : fallback);

// ---- Adopting ----

interface Pairing {
  key: string;
  hostName: string;
  pin: string;
  pinUrl: string;
}
const pairing = ref<Pairing | null>(null);
const adoptingKey = ref<string | null>(null);
const adoptNote = ref<string | null>(null);
const adoptError = ref<string | null>(null);
const failure = ref<string | null>(null);

const adopt = useMutation({
  mutationFn: (h: FoundMoonlightHost) => {
    adoptingKey.value = h.key;
    return api.adoptMoonlightHost(h.key);
  },
  onMutate: () => {
    adoptError.value = null;
    adoptNote.value = null;
    failure.value = null;
  },
  onSuccess: (res, h) => {
    if (res.status === "adopted") {
      adoptNote.value = `Adopted ${res.host.name}. It is on the dashboard now.`;
      pairing.value = null;
      done();
    } else {
      pairing.value = { key: h.key, hostName: res.hostName, pin: res.pin, pinUrl: res.pinUrl };
    }
  },
  onError: (err, h) => {
    adoptError.value =
      err instanceof ApiError && err.code === "already_adopted"
        ? `${h.name} is already adopted.`
        : err instanceof ApiError && err.code === "node_unreachable"
          ? `${h.nodeName} didn't answer. Check that its agent is running.`
          : err instanceof ApiError && err.code === "not_found"
            ? `${h.name} stopped being visible. It may have gone offline.`
            : errorText(err, "Couldn't adopt the host.");
    if (err instanceof ApiError && err.code === "already_adopted") done();
  },
});

// While a PIN is out, ask every 2 s whether it was entered; the node gives up after 5 minutes.
const pairingKey = computed(() => pairing.value?.key ?? null);
const poll = useQuery({
  queryKey: computed(() => ["moonlight", "pairing", pairingKey.value]),
  queryFn: () => api.moonlightPairing(pairingKey.value!),
  enabled: computed(() => !!pairingKey.value && !failure.value),
  refetchInterval: 2000,
  refetchIntervalInBackground: false,
  gcTime: 0,
  retry: false,
});
watch(
  () => poll.data.value,
  (st) => {
    if (!st || !pairing.value) return;
    if (st.status === "adopted") {
      adoptNote.value = `Adopted ${st.host?.name ?? pairing.value.hostName}. It is on the dashboard now.`;
      pairing.value = null;
      done();
    } else if (st.status === "failed") {
      failure.value = st.message || "Pairing didn't finish.";
    }
  },
);
watch(
  () => poll.error.value,
  (err) => {
    if (err && pairing.value) failure.value = errorText(err, "Lost track of the pairing.");
  },
);

function cancelPairing() {
  pairing.value = null;
  failure.value = null;
}
function tryAgain() {
  const h = foundHosts.value.find((f) => f.key === pairing.value?.key);
  failure.value = null;
  pairing.value = null;
  if (h) adopt.mutate(h);
}

// ---- Adopted hosts ----

const rowError = ref<string | null>(null);
const refreshing = ref<string | null>(null);
const refresh = useMutation({
  mutationFn: (h: AdoptedHost) => {
    refreshing.value = h.id;
    return api.refreshMoonlightHost(h.id);
  },
  onMutate: () => (rowError.value = null),
  onSuccess: done,
  onError: (err) => (rowError.value = errorText(err, "Couldn't refresh the apps.")),
  onSettled: () => (refreshing.value = null),
});

const removeTarget = ref<AdoptedHost | null>(null);
const removeError = ref<string | null>(null);
const remove = useMutation({
  mutationFn: (h: AdoptedHost) => api.removeMoonlightHost(h.id),
  onMutate: () => (removeError.value = null),
  onSuccess: () => {
    removeTarget.value = null;
    done();
  },
  onError: (err) =>
    (removeError.value =
      err instanceof ApiError && err.code === "host_busy"
        ? "Someone is streaming from it. Try again when they stop."
        : errorText(err, "Couldn't remove the host.")),
});
</script>

<template>
  <section v-if="!missing && !found.isPending.value" class="card max-w-3xl space-y-4 p-5" aria-labelledby="moonlight-h">
    <div>
      <h2 id="moonlight-h" class="text-sm font-semibold">Moonlight hosts</h2>
      <p class="mt-1 text-xs text-ink-3">
        Gaming PCs running Sunshine or Apollo that a node can see. Adopt one and everyone gets its apps on the dashboard.
      </p>
    </div>

    <p v-if="adoptNote" class="flex items-center gap-2 text-sm text-ok" role="status">
      <Check class="size-4 shrink-0" aria-hidden="true" />{{ adoptNote }}
    </p>

    <!-- Pairing in progress -->
    <div v-if="pairing" class="space-y-3 rounded-lg border border-line bg-canvas p-4" role="group" aria-label="Pairing">
      <template v-if="!failure">
        <p class="text-sm text-ink-2">Enter this PIN on {{ pairing.hostName }}:</p>
        <p class="font-mono text-4xl font-semibold tracking-[0.3em] text-ink" aria-label="PIN">{{ pairing.pin }}</p>
        <div class="flex flex-wrap items-center gap-2">
          <a :href="pairing.pinUrl" target="_blank" rel="noopener" class="btn-ghost px-3 py-1 text-xs">
            <ExternalLink class="size-3.5" aria-hidden="true" />
            Open the host's PIN page
          </a>
        </div>
        <p class="text-xs text-ink-3">
          The host's web UI may warn about its self-signed certificate; that is expected, continue past it.
        </p>
        <p class="flex items-center gap-2 text-sm text-ink-2" role="status">
          <LoaderCircle class="size-4 shrink-0 animate-spin text-accent" aria-hidden="true" />
          Waiting for the PIN…
        </p>
        <button type="button" class="btn-ghost px-3 py-1 text-xs" @click="cancelPairing">Cancel</button>
      </template>
      <template v-else>
        <FormError :message="failure" />
        <div class="flex gap-2">
          <button type="button" class="btn-primary px-3 py-1 text-xs" :disabled="adopt.isPending.value" @click="tryAgain">
            Try again
          </button>
          <button type="button" class="btn-ghost px-3 py-1 text-xs" @click="cancelPairing">Cancel</button>
        </div>
      </template>
    </div>

    <FormError :message="adoptError" />

    <div>
      <h3 class="text-xs font-semibold tracking-wide text-ink-3 uppercase">Found on your network</h3>
      <p v-if="found.isError.value" class="mt-2 text-sm text-ink-3">Couldn't check for Moonlight hosts.</p>
      <p v-else-if="!foundHosts.length" class="mt-2 text-sm text-ink-3">
        No Moonlight hosts seen. Start Sunshine or Apollo on a PC on the same network as a node.
      </p>
      <ul v-else class="mt-2 divide-y divide-line">
        <li v-for="h in foundHosts" :key="h.key" class="flex flex-wrap items-center gap-x-4 gap-y-2 py-3 first:pt-0 last:pb-0">
          <div class="min-w-0 flex-1">
            <p class="truncate text-sm font-medium">{{ h.name }}</p>
            <p class="truncate text-xs text-ink-2">{{ h.address }} · via {{ h.nodeName }}</p>
          </div>
          <span v-if="adoptedNames.has(h.name)" class="text-xs text-ink-3">Adopted</span>
          <button
            v-else
            type="button"
            class="btn-primary shrink-0 px-3 py-1 text-xs"
            :aria-label="`Adopt ${h.name}`"
            :disabled="adopt.isPending.value || pairing?.key === h.key"
            @click="adopt.mutate(h)"
          >
            {{ adopt.isPending.value && adoptingKey === h.key ? "Adopting…" : "Adopt" }}
          </button>
        </li>
      </ul>
    </div>

    <div>
      <h3 class="text-xs font-semibold tracking-wide text-ink-3 uppercase">Adopted</h3>
      <p v-if="!adoptedHosts.length" class="mt-2 text-sm text-ink-3">None yet.</p>
      <ul v-else class="mt-2 divide-y divide-line">
        <li v-for="h in adoptedHosts" :key="h.id" class="flex flex-wrap items-center gap-x-4 gap-y-2 py-3 first:pt-0 last:pb-0">
          <div class="min-w-0 flex-1">
            <p class="truncate text-sm font-medium">
              {{ h.name }}<span v-if="!h.online" class="font-normal text-warn"> · Offline</span>
            </p>
            <p class="truncate text-xs text-ink-2">
              {{ h.apps.length }} {{ h.apps.length === 1 ? "app" : "apps" }} ·
              <span :title="h.appsAt ? dateTime(h.appsAt) : undefined">{{
                h.appsAt ? `apps updated ${ago(h.appsAt)}` : "apps not fetched yet"
              }}</span>
              · via {{ h.nodeName }}
            </p>
          </div>
          <button
            type="button"
            class="btn-ghost shrink-0 px-3 py-1 text-xs"
            :aria-label="`Refresh apps of ${h.name}`"
            :disabled="refresh.isPending.value"
            @click="refresh.mutate(h)"
          >
            <RefreshCw class="size-3.5" :class="refreshing === h.id && 'animate-spin'" aria-hidden="true" />
            Refresh
          </button>
          <button
            type="button"
            class="btn-ghost shrink-0 px-3 py-1 text-xs hover:border-danger/60 hover:text-danger"
            :aria-label="`Remove ${h.name}`"
            @click="(removeError = null), (removeTarget = h)"
          >
            <Trash2 class="size-3.5" aria-hidden="true" />
            Remove
          </button>
        </li>
      </ul>
      <FormError :message="rowError" class="mt-2" />
    </div>

    <ConfirmDialog
      :open="!!removeTarget"
      :title="`Remove ${removeTarget?.name ?? ''}?`"
      confirm-label="Remove"
      :busy="remove.isPending.value"
      :error="removeError"
      @cancel="removeTarget = null"
      @confirm="removeTarget && remove.mutate(removeTarget)"
    >
      <p>
        Its apps leave everyone's dashboard. The node stays paired with the host; remove the portal's client from the
        host's own Moonlight client list to unpair it.
      </p>
    </ConfirmDialog>
  </section>
</template>

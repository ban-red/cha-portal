<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Check, Copy, Plus, TriangleAlert, X } from "lucide-vue-next";
import { computed, nextTick, ref } from "vue";

import { ApiError, api, type NodeInfo } from "../api";
import FormError from "../components/FormError.vue";
import NodeUsage from "../components/NodeUsage.vue";
import { normalizePairingCode } from "../pairingCode";
import { ago, clockTime, dateTime, megabytes } from "../format";

const queryClient = useQueryClient();
// Status and usage come from the live node channel; poll so they stay
// current: every few seconds while the page is visible (the usage changes
// that fast), and faster while a join token is out so the new node appears
// promptly. A hidden tab doesn't poll.
const issued = ref<{ token: string; expiresAt: number } | null>(null);
const nodes = useQuery({
  queryKey: ["nodes"],
  queryFn: api.nodes,
  refetchInterval: () => (issued.value ? 2000 : 3000),
  refetchIntervalInBackground: false,
});

// ---- Adding a node ----

const showAdd = ref(false);
const label = ref("");
const joinError = ref<string | null>(null);
const copied = ref(false);
const commandEl = ref<HTMLElement | null>(null);

const portalUrl = window.location.origin;
const onLocalhost = ["localhost", "127.0.0.1", "[::1]"].includes(window.location.hostname);
const command = computed(() =>
  issued.value ? `cha-node \\\n  --portal-url ${portalUrl} \\\n  --join-token ${issued.value.token}` : "",
);

const createToken = useMutation({
  mutationFn: () => api.createJoinToken({ label: label.value || undefined }),
  onSuccess: (data) => {
    issued.value = data;
    copied.value = false;
  },
  onError: (err) => {
    joinError.value = err instanceof ApiError ? err.message : "Couldn't create a join token.";
  },
});

function toggleAdd() {
  showAdd.value = !showAdd.value;
  issued.value = null;
  joinError.value = null;
  label.value = "";
}

async function copyCommand() {
  try {
    await navigator.clipboard.writeText(command.value);
    copied.value = true;
  } catch {
    // No clipboard outside a secure context: select the text instead.
    if (commandEl.value) window.getSelection()?.selectAllChildren(commandEl.value);
  }
}

// ---- Found on your network ----

// Unclaimed agents advertise themselves; an admin claims one with the code in
// its log. An older server without the endpoint answers 404: hide the section.
const discovered = useQuery({
  queryKey: ["nodes", "discovered"],
  queryFn: api.discoveredNodes,
  refetchInterval: 3000,
  refetchIntervalInBackground: false,
  retry: (count, err) => !(err instanceof ApiError && err.status === 404) && count < 2,
});
const foundNodes = computed(() => discovered.data.value?.nodes ?? []);
// Two found nodes with one name: one may be posing as the other, and would
// learn the code if given it. The fingerprint in the node's log tells them apart.
const sharedNames = computed(() => {
  const seen = new Map<string, number>();
  for (const n of foundNodes.value) seen.set(n.name, (seen.get(n.name) ?? 0) + 1);
  return new Set([...seen].filter(([, count]) => count > 1).map(([name]) => name));
});
const showFound = computed(
  () => !(discovered.error.value instanceof ApiError && discovered.error.value.status === 404) && !discovered.isPending.value,
);

const claiming = ref<string | null>(null);
const codeInput = ref("");
const claimError = ref<string | null>(null);
const claimNote = ref<string | null>(null);
const codeEl = ref<HTMLInputElement[] | HTMLInputElement | null>(null);

async function openClaim(id: string) {
  claiming.value = id;
  codeInput.value = "";
  claimError.value = null;
  claimNote.value = null;
  await nextTick();
  const el = Array.isArray(codeEl.value) ? codeEl.value[0] : codeEl.value;
  el?.focus();
}

function closeClaim() {
  claiming.value = null;
  claimError.value = null;
}

const claim = useMutation({
  mutationFn: (v: { id: string; code: string }) => api.claimNode(v),
  onSuccess: (data) => {
    claimNote.value = `Claimed ${data.name}. It is connecting now.`;
    closeClaim();
    queryClient.invalidateQueries({ queryKey: ["nodes"] });
  },
  onError: (err) => {
    if (err instanceof ApiError && err.code === "wrong_code") {
      claimError.value = "That code doesn't match. Check the node's log; after five wrong codes it makes a new one.";
    } else if (err instanceof ApiError && err.code === "not_found") {
      claimError.value = "That node stopped advertising. Check that its agent is running.";
    } else {
      claimError.value = err instanceof ApiError ? err.message : "Couldn't claim the node.";
    }
  },
});

function submitClaim() {
  if (!claiming.value) return;
  claimError.value = null;
  const code = normalizePairingCode(codeInput.value);
  if (!code) {
    claimError.value = "Enter the 8-digit code from the node's log, like 4821-9375.";
    return;
  }
  claim.mutate({ id: claiming.value, code });
}

// ---- Node actions ----

const pings = ref<Record<string, string>>({});
const actionError = ref<string | null>(null);

async function ping(node: NodeInfo) {
  pings.value[node.id] = "…";
  try {
    const { rttMs } = await api.pingNode(node.id);
    pings.value[node.id] = `${rttMs.toFixed(1)} ms`;
  } catch (err) {
    pings.value[node.id] = err instanceof ApiError ? err.message : "failed";
  }
}

const remove = useMutation({
  mutationFn: (node: NodeInfo) => api.removeNode(node.id),
  onSuccess: () => queryClient.invalidateQueries({ queryKey: ["nodes"] }),
  onError: (err) => {
    actionError.value = err instanceof ApiError ? err.message : "Couldn't remove the node.";
  },
});

function confirmRemove(node: NodeInfo) {
  actionError.value = null;
  const ok = window.confirm(
    `Remove ${node.name}? It disconnects now and needs a new join token to come back.`,
  );
  if (ok) remove.mutate(node);
}

function status(node: NodeInfo): { text: string; dot: string } {
  if (node.online) return { text: `Online since ${clockTime(node.connectedAt ?? 0)}`, dot: "bg-ok" };
  if (node.lastSeenAt) return { text: `Offline · seen ${ago(node.lastSeenAt)}`, dot: "bg-ink-3" };
  return { text: "Waiting for its first connection", dot: "bg-warn" };
}
</script>

<template>
  <div class="max-w-5xl space-y-6">
    <div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-3">
      <p class="min-w-0 text-sm text-ink-2">Machines that run environments. Each runs the <code class="font-mono text-ink">cha-node</code> agent.</p>
      <button type="button" class="shrink-0" :class="showAdd ? 'btn-ghost' : 'btn-primary'" :aria-expanded="showAdd" aria-controls="add-node" @click="toggleAdd">
        <X v-if="showAdd" class="size-4" aria-hidden="true" />
        <Plus v-else class="size-4" aria-hidden="true" />
        {{ showAdd ? "Close" : "Add node" }}
      </button>
    </div>

    <section v-if="showAdd" id="add-node" class="card max-w-3xl space-y-4 p-5" aria-label="Add a node">
      <template v-if="!issued">
        <p class="text-sm text-ink-2">
          Create a one-time join token, then run the agent on the new machine with it. The node keeps its own key
          afterwards; the token isn't needed again.
        </p>
        <form class="flex flex-col gap-3 sm:flex-row sm:items-end" @submit.prevent="(joinError = null), createToken.mutate()">
          <div class="flex-1">
            <label class="label" for="j-label">Label <span class="font-normal text-ink-3">(optional, for the audit log)</span></label>
            <input id="j-label" v-model="label" class="field" maxlength="64" autocomplete="off" placeholder="e.g. rack GPU box" />
          </div>
          <button type="submit" class="btn-primary max-sm:w-full" :disabled="createToken.isPending.value">
            {{ createToken.isPending.value ? "Creating…" : "Create join token" }}
          </button>
        </form>
        <FormError :message="joinError" />
      </template>

      <template v-else>
        <p class="text-sm text-ink-2">
          Run this on the node. The token works once and expires at
          <span class="text-ink">{{ clockTime(issued.expiresAt) }}</span>; it isn't shown again.
        </p>
        <div class="flex items-start gap-2 rounded-lg border border-line bg-canvas p-3">
          <code ref="commandEl" class="min-w-0 flex-1 font-mono text-xs leading-relaxed break-all whitespace-pre-wrap text-ink">{{ command }}</code>
          <button type="button" class="btn-ghost shrink-0 px-3 py-1 text-xs" @click="copyCommand">
            <Check v-if="copied" class="size-3.5 text-ok" aria-hidden="true" />
            <Copy v-else class="size-3.5" aria-hidden="true" />
            {{ copied ? "Copied" : "Copy" }}
          </button>
        </div>
        <p v-if="onLocalhost" class="flex items-start gap-2 text-xs text-warn">
          <TriangleAlert class="size-4 shrink-0" aria-hidden="true" />
          <span>You're browsing the portal on localhost: if the node is another machine, replace the URL with one it can reach.</span>
        </p>
        <p class="text-xs text-ink-3">
          Add <code class="font-mono">--name</code> to choose its name here (it defaults to the hostname), and
          <code class="font-mono">--state-dir</code> to keep its identity somewhere other than
          <code class="font-mono">/var/lib/cha-node</code>. It shows up below once it connects.
        </p>
      </template>
    </section>

    <section v-if="showFound" class="card max-w-3xl space-y-3 p-5" aria-labelledby="found-h">
      <h2 id="found-h" class="text-sm font-semibold">Found on your network</h2>
      <p v-if="claimNote" class="flex items-center gap-2 text-sm text-ok" role="status">
        <Check class="size-4 shrink-0" aria-hidden="true" />{{ claimNote }}
      </p>
      <p v-if="discovered.data.value && !discovered.data.value.enabled" class="text-sm text-ink-3">
        Node discovery is turned off on this portal.
      </p>
      <p v-else-if="discovered.isError.value" class="text-sm text-ink-3">Couldn't check for nodes on the network.</p>
      <p v-else-if="!foundNodes.length" class="text-sm text-ink-3">
        No unclaimed nodes on this network. Start a node's agent without a join token and it appears here, or add one
        with a token.
      </p>
      <ul v-else class="divide-y divide-line">
        <li v-for="n in foundNodes" :key="n.id" class="py-3 first:pt-0 last:pb-0">
          <div class="flex flex-wrap items-center gap-x-4 gap-y-2">
            <div class="min-w-0 flex-1">
              <p class="truncate text-sm font-medium">{{ n.name }}</p>
              <p class="truncate text-xs text-ink-2">
                <span v-if="n.gpu">{{ n.gpu }} · </span>{{ n.addresses[0] ?? "no address" }}
              </p>
              <p class="mt-0.5 truncate font-mono text-2xs text-ink-3" :title="n.fingerprint">{{ n.fingerprint }}</p>
            </div>
            <button
              v-if="claiming !== n.id"
              type="button"
              class="btn-primary shrink-0 px-3 py-1 text-xs"
              :aria-label="`Claim ${n.name}`"
              @click="openClaim(n.id)"
            >
              Claim
            </button>
          </div>
          <form v-if="claiming === n.id" class="mt-3 space-y-3" @submit.prevent="submitClaim" @keydown.esc.prevent="closeClaim">
            <div>
              <label class="label" :for="`claim-code-${n.id}`">Pairing code for {{ n.name }}</label>
              <div class="flex flex-col gap-2 sm:flex-row">
                <input
                  :id="`claim-code-${n.id}`"
                  ref="codeEl"
                  v-model="codeInput"
                  class="field flex-1 font-mono sm:max-w-48"
                  inputmode="numeric"
                  autocomplete="one-time-code"
                  placeholder="0000-0000"
                  maxlength="12"
                  :aria-describedby="`claim-help-${n.id}`"
                />
                <button type="submit" class="btn-primary max-sm:w-full" :disabled="claim.isPending.value">
                  {{ claim.isPending.value ? "Claiming…" : "Claim" }}
                </button>
                <button type="button" class="btn-ghost max-sm:w-full" @click="closeClaim">Cancel</button>
              </div>
              <p :id="`claim-help-${n.id}`" class="mt-1.5 text-xs text-ink-3">
                The code is in the node's log (<code class="font-mono">docker compose logs agent</code>), next to the
                fingerprint: check it matches <span class="font-mono">{{ n.fingerprint }}</span> before you claim.
              </p>
              <p v-if="sharedNames.has(n.name)" class="mt-1.5 flex items-center gap-1.5 text-xs text-warn" role="note">
                <TriangleAlert class="size-3.5 shrink-0" aria-hidden="true" />More than one node here is called
                {{ n.name }}: claim only the one whose fingerprint matches its log.
              </p>
            </div>
            <FormError :message="claimError" />
          </form>
        </li>
      </ul>
    </section>

    <FormError :message="actionError" />

    <p v-if="nodes.isPending.value" class="text-sm text-ink-3">Loading…</p>
    <FormError v-else-if="nodes.isError.value" :message="nodes.error.value?.message ?? 'Failed to load'" />
    <div v-else-if="!nodes.data.value?.length" class="card p-8 text-center">
      <p class="font-medium">No nodes yet</p>
      <p class="mt-1 text-sm text-ink-2">Add a machine with a GPU to start running environments on it.</p>
    </div>

    <div v-else class="grid gap-4 lg:grid-cols-2">
      <article v-for="node in nodes.data.value" :key="node.id" class="card flex flex-col p-5">
        <header class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <h2 class="truncate text-base font-semibold">{{ node.name }}</h2>
            <p class="mt-1 flex items-center gap-2 text-xs text-ink-2">
              <span class="size-2 shrink-0 rounded-full" :class="status(node).dot" aria-hidden="true" />
              {{ status(node).text }}
            </p>
          </div>
          <span v-if="node.agentVersion" class="shrink-0 rounded-full border border-line px-2 py-0.5 font-mono text-2xs text-ink-3">
            v{{ node.agentVersion }}
          </span>
        </header>

        <dl v-if="node.inventory" class="mt-4 grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-1.5 text-sm">
          <dt class="text-ink-3">Host</dt>
          <dd class="truncate">{{ node.inventory.hostname }} · {{ node.inventory.os }} · {{ node.inventory.arch }}</dd>
          <dt class="text-ink-3">CPU / RAM</dt>
          <dd>{{ node.inventory.cpus }} threads · {{ megabytes(node.inventory.memoryMb) }}</dd>
          <dt class="text-ink-3">GPUs</dt>
          <dd>
            <p v-if="!node.inventory.gpus.length" class="text-ink-3">None found</p>
            <div v-for="gpu in node.inventory.gpus" :key="gpu.renderNode ?? gpu.name" class="mb-1 last:mb-0">
              <p>
                {{ gpu.name }}<span v-if="gpu.memoryMb" class="text-ink-2"> · {{ megabytes(gpu.memoryMb) }}</span>
              </p>
              <p class="mt-0.5 flex flex-wrap items-center gap-1.5 text-xs text-ink-3">
                <span v-if="gpu.driver">driver {{ gpu.driver }}</span>
                <span
                  v-for="enc in gpu.encoders"
                  :key="enc"
                  class="rounded border border-accent/30 px-1.5 font-mono text-2xs text-accent uppercase"
                  >{{ enc }}</span
                >
              </p>
            </div>
          </dd>
          <dt class="text-ink-3">Addresses</dt>
          <dd class="font-mono text-xs leading-5 break-all text-ink-2">{{ node.inventory.addresses.join(", ") || "—" }}</dd>
        </dl>
        <p v-else class="mt-4 text-sm text-ink-3">No inventory reported yet.</p>

        <NodeUsage v-if="node.usage" :usage="node.usage" />
        <p v-else-if="node.online" class="mt-4 text-xs text-ink-3">No live data</p>

        <footer class="mt-5 flex flex-wrap items-center gap-2 border-t border-line pt-4">
          <span class="mr-auto min-w-0 text-xs text-ink-3" :title="dateTime(node.enrolledAt)">Enrolled {{ ago(node.enrolledAt) }}</span>
          <span v-if="pings[node.id]" class="text-xs text-ink-2">{{ pings[node.id] }}</span>
          <button type="button" class="btn-ghost px-3 py-1 text-xs" :disabled="!node.online" @click="ping(node)">Ping</button>
          <button
            type="button"
            class="btn-ghost px-3 py-1 text-xs hover:border-danger/60 hover:text-danger"
            :disabled="remove.isPending.value"
            @click="confirmRemove(node)"
          >
            Remove
          </button>
        </footer>
      </article>
    </div>
  </div>
</template>

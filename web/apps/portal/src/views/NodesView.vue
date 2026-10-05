<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { computed, ref } from "vue";

import { ApiError, api, type NodeInfo } from "../api";
import FormError from "../components/FormError.vue";
import NodeUsage from "../components/NodeUsage.vue";
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
  if (node.online) return { text: `Online since ${clockTime(node.connectedAt ?? 0)}`, dot: "bg-accent" };
  if (node.lastSeenAt) return { text: `Offline · seen ${ago(node.lastSeenAt)}`, dot: "bg-ink-3" };
  return { text: "Waiting for its first connection", dot: "bg-warn" };
}
</script>

<template>
  <div class="mx-auto max-w-5xl space-y-6">
    <div class="flex items-center justify-between gap-4">
      <p class="text-sm text-ink-2">Machines that run environments. Each runs the <code class="font-mono text-ink">cha-node</code> agent.</p>
      <button class="shrink-0" :class="showAdd ? 'btn-ghost' : 'btn-primary'" @click="toggleAdd">{{ showAdd ? "Close" : "Add node" }}</button>
    </div>

    <section v-if="showAdd" class="card space-y-4 p-5">
      <template v-if="!issued">
        <p class="text-sm text-ink-2">
          Create a one-time join token, then run the agent on the new machine with it. The node keeps its own key
          afterwards; the token isn't needed again.
        </p>
        <form class="flex flex-col gap-3 sm:flex-row sm:items-end" @submit.prevent="(joinError = null), createToken.mutate()">
          <div class="flex-1">
            <label class="label" for="j-label">Label <span class="normal-case text-ink-3">(optional, for the audit log)</span></label>
            <input id="j-label" v-model="label" class="field" maxlength="64" autocomplete="off" placeholder="e.g. rack GPU box" />
          </div>
          <button type="submit" class="btn-primary" :disabled="createToken.isPending.value">
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
          <button class="btn-ghost shrink-0 px-3 py-1 text-xs" @click="copyCommand">{{ copied ? "Copied" : "Copy" }}</button>
        </div>
        <p v-if="onLocalhost" class="text-xs text-warn">
          You're browsing the portal on localhost: if the node is another machine, replace the URL with one it can reach.
        </p>
        <p class="text-xs text-ink-3">
          Add <code class="font-mono">--name</code> to choose its name here (it defaults to the hostname), and
          <code class="font-mono">--state-dir</code> to keep its identity somewhere other than
          <code class="font-mono">/var/lib/cha-node</code>. It shows up below once it connects.
        </p>
      </template>
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
            <h2 class="truncate font-semibold">{{ node.name }}</h2>
            <p class="mt-1 flex items-center gap-2 text-xs text-ink-2">
              <span class="size-2 shrink-0 rounded-full" :class="status(node).dot" />
              {{ status(node).text }}
            </p>
          </div>
          <span v-if="node.agentVersion" class="shrink-0 rounded-full border border-line px-2 py-0.5 font-mono text-[11px] text-ink-3">
            v{{ node.agentVersion }}
          </span>
        </header>

        <dl v-if="node.inventory" class="mt-4 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1.5 text-sm">
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
                  class="rounded border border-accent/30 px-1.5 font-mono text-[10px] text-accent uppercase"
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

        <footer class="mt-5 flex items-center gap-2 border-t border-line pt-4">
          <span class="mr-auto text-xs text-ink-3" :title="dateTime(node.enrolledAt)">Enrolled {{ ago(node.enrolledAt) }}</span>
          <span v-if="pings[node.id]" class="text-xs text-ink-2">{{ pings[node.id] }}</span>
          <button class="btn-ghost px-3 py-1 text-xs" :disabled="!node.online" @click="ping(node)">Ping</button>
          <button
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

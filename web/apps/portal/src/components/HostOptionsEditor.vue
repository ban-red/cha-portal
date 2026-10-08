<script setup lang="ts">
import { Check, Plus, TriangleAlert, Trash2, X } from "lucide-vue-next";
import { computed, useId } from "vue";

import type { HostMount, HostOptions, HostPort, MountSource, NetworkFs, NodeHostOptions, PortProtocol } from "../api";
import {
  MODE_LABEL,
  allowedChoices,
  anyFull,
  checkShape,
  isEmptyHost,
  mountChoices,
  noNodeAccepts,
  nodeVerdicts,
  policySummary,
  portHint,
  validCapName,
} from "../hostOptions";
import ChipPicker from "./ChipPicker.vue";
import FormError from "./FormError.vue";

// The host options of a custom environment (ADR 0021): mounts, ports, capabilities and devices
// that each node allows or not. Beside the editors, every node says whether it would accept
// the options as they stand now (the same checks the portal and the node run).
const props = defineProps<{
  nodes: NodeHostOptions[];
  /** The environment sets variables of its own, which an older agent can't pass on. */
  needsEnv: boolean;
}>();
const model = defineModel<HostOptions>({ required: true });

const uid = useId();
const h = computed(() => model.value);

function update(fn: (next: HostOptions) => void) {
  const next: HostOptions = JSON.parse(JSON.stringify(model.value));
  fn(next);
  model.value = next;
}

const mounts = computed(() => h.value.mounts ?? []);
const ports = computed(() => h.value.ports ?? []);
const choices = computed(() => mountChoices(props.nodes));
const full = computed(() => anyFull(props.nodes));
const verdicts = computed(() => nodeVerdicts(props.nodes, h.value, props.needsEnv));
const shapeProblem = computed(() => checkShape(h.value));
const nobody = computed(() => !isEmptyHost(h.value) && noNodeAccepts(verdicts.value));
const nodeByName = computed(() => new Map(props.nodes.map((n) => [n.nodeId, n])));
const ranges = computed(() => portHint(props.nodes));
const anyAllows = computed(() => props.nodes.some((n) => n.policy && n.policy.mode !== "off"));

// ---- Mounts ----

type Kind = MountSource["kind"];
const KINDS: { value: Kind; label: string; full: boolean }[] = [
  { value: "named", label: "Named mount", full: false },
  { value: "path", label: "Host path", full: true },
  { value: "network", label: "Network share", full: true },
];
// A kind is offered when a node in full mode could take it, or when the mount already uses it.
const kindsFor = (m: HostMount) => KINDS.filter((k) => !k.full || full.value || k.value === m.source.kind);

function blankSource(kind: Kind): MountSource {
  return kind === "named" ? { kind, name: "" } : kind === "path" ? { kind, path: "" } : { kind, fsType: "nfs", device: "", options: "" };
}
const addMount = () => update((n) => (n.mounts = [...(n.mounts ?? []), { source: blankSource("named"), target: "", readOnly: false }]));
const removeMount = (i: number) => update((n) => n.mounts!.splice(i, 1));
const setKind = (i: number, kind: Kind) => update((n) => (n.mounts![i]!.source = blankSource(kind)));
const setSource = (i: number, patch: Partial<MountSource>) => update((n) => Object.assign(n.mounts![i]!.source, patch));
const setTarget = (i: number, target: string) => update((n) => (n.mounts![i]!.target = target));
const setRo = (i: number, ro: boolean) => update((n) => (n.mounts![i]!.readOnly = ro));
const value = (e: Event) => (e.target as HTMLInputElement | HTMLSelectElement).value;
const checked = (e: Event) => (e.target as HTMLInputElement).checked;

function readOnlyNote(m: HostMount): string | null {
  if (m.source.kind !== "named") return null;
  const name = m.source.name;
  const c = choices.value.find((x) => x.name === name);
  if (!c?.readOnlyOn.length) return null;
  return c.readOnlyOn.length === c.nodes.length ? `Read-only on ${c.readOnlyOn.join(", ")}.` : `Read-only on ${c.readOnlyOn.join(", ")}; the others allow writing.`;
}

// ---- Ports ----

const addPort = () => update((n) => (n.ports = [...(n.ports ?? []), { container: 0, protocol: "tcp" }]));
const removePort = (i: number) => update((n) => n.ports!.splice(i, 1));
function setPort(i: number, patch: Partial<HostPort>) {
  update((n) => Object.assign(n.ports![i]!, patch));
}
const portNumber = (e: Event) => {
  const v = value(e).trim();
  return v === "" ? undefined : Number(v);
};

// ---- Capabilities, devices, full-only ----

const setList = (key: "capAdd" | "devices" | "securityOpt", list: string[]) => update((n) => (n[key] = list));
const capProblem = (v: string) => (validCapName(v) ? null : "A capability is upper-case without CAP_, like SYS_NICE.");
const devProblem = (v: string) => (v.startsWith("/dev/") && !v.includes("..") ? null : "A device path starts with /dev/.");
const devChoices = computed(() => allowedChoices(props.nodes, "devices"));
const capChoices = computed(() => allowedChoices(props.nodes, "caps"));
const CTRL = "min-h-9 pointer-coarse:min-h-11";
</script>

<template>
  <div class="space-y-6">
    <!-- What each node allows, and whether it would take these options. -->
    <section class="space-y-2" :aria-labelledby="`${uid}-nodes`">
      <h3 :id="`${uid}-nodes`" class="text-sm font-semibold">Nodes</h3>
      <p v-if="!nodes.length" class="text-sm text-ink-3">No nodes yet.</p>
      <ul v-else class="divide-y divide-line rounded-lg border border-line">
        <li v-for="v in verdicts" :key="v.nodeId" class="px-3 py-2.5">
          <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
            <span class="font-medium">{{ v.nodeName }}</span>
            <span
              class="rounded-full border px-2 py-0.5 text-2xs"
              :class="v.mode === 'full' ? 'border-warn/50 bg-warn/10 text-warn' : 'border-line text-ink-2'"
              :title="v.mode === 'full' ? 'The portal\'s admins can run root-equivalent containers on this node.' : undefined"
            >
              <TriangleAlert v-if="v.mode === 'full'" class="mr-0.5 inline size-3 align-[-2px]" aria-hidden="true" />{{ v.mode ? MODE_LABEL[v.mode] : "Not reported" }}
            </span>
            <span v-if="!v.online" class="text-2xs text-ink-3">offline</span>
            <span class="min-w-0 text-xs text-ink-3">{{ policySummary(nodeByName.get(v.nodeId)?.policy ?? null) }}</span>
          </div>
          <p v-if="!isEmptyHost(h) || needsEnv" class="mt-1 flex items-start gap-1.5 text-xs" :class="v.reasons.length ? 'text-warn' : 'text-ok'">
            <component :is="v.reasons.length ? X : Check" class="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
            <span v-if="!v.reasons.length">Would accept these options.</span>
            <span v-else class="min-w-0 break-words">Would refuse: this node {{ v.reasons.join("; ") }}.</span>
          </p>
        </li>
      </ul>
      <p v-if="nodes.length && !anyAllows" class="text-xs text-ink-3">
        No node allows host options. A node's owner turns them on with <span class="font-mono">CHA_HOST_OPTIONS</span> in the node's
        <span class="font-mono">.env</span>; the portal has no switch for it.
      </p>
      <p v-if="nobody" class="flex items-start gap-2 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-sm text-warn" role="status">
        <TriangleAlert class="mt-0.5 size-4 shrink-0" aria-hidden="true" />
        <span>No node accepts these options as they are, so this environment can't start anywhere. You can still save it.</span>
      </p>
    </section>

    <!-- Mounts -->
    <section class="space-y-3" :aria-labelledby="`${uid}-mounts`">
      <div class="flex items-center justify-between gap-3">
        <h3 :id="`${uid}-mounts`" class="text-sm font-semibold">Folders</h3>
        <button type="button" class="btn-ghost px-3 py-1 text-xs" @click="addMount"><Plus class="size-3.5" aria-hidden="true" />Add a folder</button>
      </div>
      <datalist :id="`${uid}-names`">
        <option v-for="c in choices" :key="c.name" :value="c.name">{{ c.readOnlyOn.length ? `${c.name} (read-only on ${c.readOnlyOn.join(", ")})` : c.name }}</option>
      </datalist>
      <p v-if="!mounts.length" class="text-xs text-ink-3">
        Mounts a folder the node's owner named<template v-if="full">, a path on the host or a network share</template> into the app.
      </p>
      <ul class="space-y-3">
        <li v-for="(m, i) in mounts" :key="i" class="space-y-2 rounded-lg border border-line p-3">
          <div class="grid gap-2 sm:grid-cols-[10rem_minmax(0,1fr)]">
            <div>
              <label :for="`${uid}-k${i}`" class="label">Kind</label>
              <select :id="`${uid}-k${i}`" :value="m.source.kind" class="field" @change="setKind(i, value($event) as Kind)">
                <option v-for="k in kindsFor(m)" :key="k.value" :value="k.value">{{ k.label }}</option>
              </select>
            </div>
            <div v-if="m.source.kind === 'named'">
              <label :for="`${uid}-s${i}`" class="label">Mount name</label>
              <input
                :id="`${uid}-s${i}`"
                :value="m.source.name"
                :list="`${uid}-names`"
                class="field font-mono"
                placeholder="media"
                autocomplete="off"
                spellcheck="false"
                @input="setSource(i, { name: value($event) })"
              />
            </div>
            <div v-else-if="m.source.kind === 'path'">
              <label :for="`${uid}-s${i}`" class="label">Path on the host</label>
              <input :id="`${uid}-s${i}`" :value="m.source.path" class="field font-mono" placeholder="/mnt/media" autocomplete="off" spellcheck="false" @input="setSource(i, { path: value($event) })" />
            </div>
            <div v-else class="grid gap-2 sm:grid-cols-[7rem_minmax(0,1fr)]">
              <div>
                <label :for="`${uid}-f${i}`" class="label">Type</label>
                <select :id="`${uid}-f${i}`" :value="m.source.fsType" class="field" @change="setSource(i, { fsType: value($event) as NetworkFs })">
                  <option value="nfs">NFS</option>
                  <option value="nfs4">NFS4</option>
                  <option value="cifs">CIFS</option>
                </select>
              </div>
              <div>
                <label :for="`${uid}-d${i}`" class="label">Device</label>
                <input
                  :id="`${uid}-d${i}`"
                  :value="m.source.device"
                  class="field font-mono"
                  :placeholder="m.source.fsType === 'cifs' ? '//server/share' : ':/export/media'"
                  autocomplete="off"
                  spellcheck="false"
                  @input="setSource(i, { device: value($event) })"
                />
              </div>
              <div class="sm:col-span-2">
                <label :for="`${uid}-o${i}`" class="label">Options</label>
                <input
                  :id="`${uid}-o${i}`"
                  :value="m.source.options ?? ''"
                  class="field font-mono"
                  :placeholder="m.source.fsType === 'cifs' ? 'username=guest' : 'addr=10.0.0.5,nfsvers=4'"
                  autocomplete="off"
                  spellcheck="false"
                  @input="setSource(i, { options: value($event) })"
                />
                <p class="mt-1 text-xs text-ink-3">Docker's local volume options. They are stored in the portal, so don't put a password you need to keep secret.</p>
              </div>
            </div>
          </div>
          <div class="flex flex-wrap items-end gap-x-4 gap-y-2">
            <div class="min-w-0 flex-1 basis-56">
              <label :for="`${uid}-t${i}`" class="label">Where the app sees it</label>
              <input :id="`${uid}-t${i}`" :value="m.target" class="field font-mono" placeholder="/mnt/media" autocomplete="off" spellcheck="false" @input="setTarget(i, value($event))" />
            </div>
            <label class="flex items-center gap-2 text-sm text-ink-2" :class="CTRL">
              <input type="checkbox" :checked="!!m.readOnly" class="size-4 accent-[var(--cha-accent)]" @change="setRo(i, checked($event))" />
              Read-only
            </label>
            <button type="button" class="btn-ghost px-3 hover:border-danger/60 hover:text-danger" :aria-label="`Remove folder ${i + 1}`" @click="removeMount(i)">
              <Trash2 class="size-4" aria-hidden="true" />
            </button>
          </div>
          <p v-if="readOnlyNote(m)" class="text-xs text-ink-3">{{ readOnlyNote(m) }} Read-write is mounted read-only there.</p>
        </li>
      </ul>
    </section>

    <!-- Ports -->
    <section class="space-y-3" :aria-labelledby="`${uid}-ports`">
      <div class="flex items-center justify-between gap-3">
        <h3 :id="`${uid}-ports`" class="text-sm font-semibold">Ports</h3>
        <button type="button" class="btn-ghost px-3 py-1 text-xs" :disabled="!!h.networkHost" @click="addPort"><Plus class="size-3.5" aria-hidden="true" />Add a port</button>
      </div>
      <p class="text-xs text-ink-3">
        Opens a path from the network straight into the app, past the portal.
        <template v-if="ranges">Nodes allow: <span class="font-mono">{{ ranges }}</span>.</template>
        <template v-if="full"> A node in full mode allows any free port.</template>
        Leave the node's port empty to let the node pick one.
      </p>
      <p v-if="h.networkHost" class="text-xs text-warn">Ports mean nothing on the host's network.</p>
      <ul class="space-y-2">
        <li v-for="(p, i) in ports" :key="i" class="flex flex-wrap items-end gap-x-3 gap-y-2">
          <div class="w-28">
            <label :for="`${uid}-pc${i}`" class="label">In the app</label>
            <input :id="`${uid}-pc${i}`" type="number" min="1" max="65535" :value="p.container || ''" class="field font-mono" @input="setPort(i, { container: portNumber($event) ?? 0 })" />
          </div>
          <div class="w-24">
            <label :for="`${uid}-pp${i}`" class="label">Protocol</label>
            <select :id="`${uid}-pp${i}`" :value="p.protocol" class="field" @change="setPort(i, { protocol: value($event) as PortProtocol })">
              <option value="tcp">TCP</option>
              <option value="udp">UDP</option>
            </select>
          </div>
          <div class="w-36">
            <label :for="`${uid}-ph${i}`" class="label">On the node <span class="font-normal text-ink-3">(optional)</span></label>
            <input :id="`${uid}-ph${i}`" type="number" min="1" max="65535" :value="p.host ?? ''" class="field font-mono" placeholder="any" @input="setPort(i, { host: portNumber($event) })" />
          </div>
          <button type="button" class="btn-ghost px-3 hover:border-danger/60 hover:text-danger" :aria-label="`Remove port ${i + 1}`" @click="removePort(i)">
            <Trash2 class="size-4" aria-hidden="true" />
          </button>
        </li>
      </ul>
    </section>

    <!-- Capabilities and devices -->
    <section class="space-y-2" :aria-labelledby="`${uid}-caps`">
      <h3 :id="`${uid}-caps`" class="text-sm font-semibold">Capabilities</h3>
      <ChipPicker
        label="capability"
        :selected="h.capAdd ?? []"
        :choices="capChoices"
        :free="full"
        placeholder="SYS_NICE"
        :problem="capProblem"
        @update:selected="setList('capAdd', $event)"
      />
    </section>
    <section class="space-y-2" :aria-labelledby="`${uid}-devs`">
      <h3 :id="`${uid}-devs`" class="text-sm font-semibold">Devices</h3>
      <ChipPicker
        label="device"
        :selected="h.devices ?? []"
        :choices="devChoices"
        :free="full"
        placeholder="/dev/dri/card1"
        :problem="devProblem"
        @update:selected="setList('devices', $event)"
      />
    </section>

    <!-- Full only -->
    <section class="space-y-3" :aria-labelledby="`${uid}-full`">
      <h3 :id="`${uid}-full`" class="flex items-center gap-2 text-sm font-semibold">
        <TriangleAlert class="size-4 text-warn" aria-hidden="true" />Only nodes in full mode
      </h3>
      <p class="text-xs text-ink-3">Each of these gives the app, and so whoever runs it, more of the machine. Other nodes refuse them.</p>
      <label class="flex items-start gap-2 text-sm">
        <input type="checkbox" :checked="!!h.privileged" class="mt-0.5 size-4 accent-[var(--cha-accent)]" @change="update((n) => (n.privileged = checked($event)))" />
        <span>Privileged <span class="block text-xs text-ink-3">Root on the machine, in effect.</span></span>
      </label>
      <label class="flex items-start gap-2 text-sm">
        <input
          type="checkbox"
          :checked="!!h.networkHost"
          class="mt-0.5 size-4 accent-[var(--cha-accent)]"
          @change="update((n) => (n.networkHost = checked($event)))"
        />
        <span>The host's network <span class="block text-xs text-ink-3">Instead of Docker's bridge. Not with ports.</span></span>
      </label>
      <div>
        <p class="label">Docker security options</p>
        <ChipPicker label="security option" :selected="h.securityOpt ?? []" :choices="[]" free placeholder="seccomp=unconfined" @update:selected="setList('securityOpt', $event)" />
      </div>
    </section>

    <FormError :message="shapeProblem" />
  </div>
</template>

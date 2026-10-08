<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { ArrowLeft, Package, Plus, Trash2, Upload } from "lucide-vue-next";
import { computed, ref, useId, watch } from "vue";
import { useRoute, useRouter } from "vue-router";

import {
  api,
  catalogIconUrl,
  type CatalogSecurity,
  type CustomOverrides,
  type CustomTemplate,
  type Fps,
  type HostOptions,
  type PadKind,
  type SharedAccess,
  type Template,
} from "../api";
import { errorText } from "../catalogs";
import FormError from "../components/FormError.vue";
import HostOptionsEditor from "../components/HostOptionsEditor.vue";
import OverrideRow from "../components/OverrideRow.vue";
import {
  CUSTOM_KEY,
  HOST_OPTIONS_KEY,
  SHARE_DATA_EXPLAIN,
  checkEnv,
  cleanOverrides,
  customId,
  envRowProblems,
  envToRows,
  isOverridden,
  parseShareData,
  resetOverride,
  rowsToEnv,
  securityNote,
  setOverride,
  slugProblem,
  type EnvRow,
  type OverrideKey,
} from "../customEnv";
import { checkShape, isEmptyHost, normalizeHost } from "../hostOptions";
import { KINDS, kindLabel } from "../controllerKinds";
import { megabytes } from "../format";

// The editor for one custom environment (ADR 0021), new (`?base=…`) or existing (`:id`): each
// field shows the base's value or its own, with a reset; then variables, a logo and host options.
const route = useRoute();
const router = useRouter();
const queryClient = useQueryClient();
const uid = useId();

const editingId = computed(() => (route.name === "custom-edit" ? String(route.params.id) : null));
const isNew = computed(() => editingId.value === null);

const catalog = useQuery({ queryKey: ["catalog"], queryFn: api.catalog, staleTime: 60_000 });
const customs = useQuery({ queryKey: CUSTOM_KEY, queryFn: api.customTemplates });
const hostOptions = useQuery({ queryKey: HOST_OPTIONS_KEY, queryFn: api.hostOptions, refetchInterval: 15_000, refetchIntervalInBackground: false });
// What the base's pickers default to; any of these may be missing on an older server.
const fpsApps = useQuery({ queryKey: ["app-settings"], queryFn: api.appSettings, staleTime: 30_000, retry: false });
const padApps = useQuery({ queryKey: ["controller-apps"], queryFn: api.controllerApps, staleTime: 30_000, retry: false });
const storage = useQuery({ queryKey: ["admin-storage"], queryFn: api.adminStorage, staleTime: 30_000, retry: false });

const existing = computed<CustomTemplate | null>(() => customs.data.value?.find((c) => c.id === editingId.value) ?? null);
const baseId = computed(() => (isNew.value ? String(route.query.base ?? "") : (existing.value?.base ?? "")));
const base = computed<Template | undefined>(() => catalog.data.value?.find((t) => t.id === baseId.value));
const baseName = computed(() => base.value?.name ?? existing.value?.baseName ?? baseId.value);

// ---- The form ----

const slug = ref(isNew.value ? String(route.query.slug ?? "") : "");
const shareData = ref(isNew.value ? parseShareData(route.query.shareData) : false);
const overrides = ref<CustomOverrides>({});
const envRows = ref<EnvRow[]>([]);
const host = ref<HostOptions>({});
const initialized = ref(isNew.value);

watch(
  existing,
  (c) => {
    if (!c || initialized.value) return;
    initialized.value = true;
    shareData.value = c.shareData;
    const { env, ...rest } = c.overrides;
    overrides.value = rest;
    envRows.value = envToRows(env);
    host.value = c.host ? JSON.parse(JSON.stringify(c.host)) : {};
  },
  { immediate: true },
);

// ---- The base's values ----

const storageApp = computed(() => storage.data.value?.apps.find((a) => a.template === baseId.value));
const inherited = computed(() => {
  const t = base.value;
  return {
    name: t?.name,
    description: t?.description,
    image: t?.image,
    class: t?.class,
    shmMb: t?.shmMb,
    fps: (fpsApps.data.value?.apps.find((a) => a.template === baseId.value)?.defaultFps ?? 60) as Fps,
    gamepad: (padApps.data.value?.apps.find((a) => a.template === baseId.value)?.default ?? "xbox360") as PadKind,
    fixedSize: t?.fixedSize ?? false,
    needsGpu: t?.needsGpu ?? false,
    persistent: storageApp.value?.defaultPersistent ?? true,
    sharedAccess: (t?.shared?.access ?? storageApp.value?.sharedAccess ?? "none") as SharedAccess,
    security: (t?.security ?? "standard") as CatalogSecurity,
  };
});

type FieldKey = Exclude<OverrideKey, "env">;
interface Field {
  key: FieldKey;
  label: string;
  kind: "text" | "textarea" | "number" | "select" | "bool";
  options?: { value: string | number; label: string }[];
  hint?: string;
}
const FIELDS: Field[] = [
  { key: "name", label: "Name", kind: "text" },
  { key: "description", label: "Description", kind: "textarea" },
  { key: "image", label: "Image", kind: "text", hint: "A container image reference, like registry/name:tag. Replaces the base's image, and any image kept on the node." },
  { key: "class", label: "Class", kind: "text", hint: "Groups the card: browser, desktop, gaming…" },
  { key: "shmMb", label: "Shared memory", kind: "number", hint: "Megabytes of /dev/shm." },
  { key: "fps", label: "Frame rate", kind: "select", options: [60, 90, 120].map((v) => ({ value: v, label: `${v} fps` })) },
  { key: "gamepad", label: "Controller", kind: "select", options: KINDS.map((k) => ({ value: k.kind, label: k.label })) },
  { key: "fixedSize", label: "Fixed display size", kind: "bool", hint: "The page never asks the app to resize." },
  { key: "needsGpu", label: "Needs a GPU", kind: "bool", hint: "It never runs on the CPU." },
  { key: "persistent", label: "Keeps data", kind: "bool", hint: "The default for users; each can change it." },
  {
    key: "sharedAccess",
    label: "Shared folder",
    kind: "select",
    options: [
      { value: "none", label: "None" },
      { value: "read", label: "Read" },
      { value: "write", label: "Read and write" },
    ],
  },
  {
    key: "security",
    label: "Security profile",
    kind: "select",
    options: [
      { value: "standard", label: "standard" },
      { value: "browser", label: "browser" },
      { value: "steam", label: "steam" },
      { value: "vm", label: "vm" },
    ],
  },
];

function inheritedText(f: Field): string {
  const v = inherited.value[f.key];
  if (v === undefined || v === "") return base.value ? "(empty)" : "Not known: the base isn't available";
  if (f.kind === "bool") return v ? "Yes" : "No";
  if (f.key === "shmMb") return megabytes(Number(v));
  if (f.key === "gamepad") return kindLabel(v as PadKind);
  return f.options?.find((o) => o.value === v)?.label ?? String(v);
}

const valueOf = (k: FieldKey) => overrides.value[k] as string | number | boolean | undefined;
function start(k: FieldKey) {
  overrides.value = setOverride(overrides.value, k, (inherited.value[k] ?? (k === "shmMb" ? 1024 : "")) as never);
}
const reset = (k: OverrideKey) => (overrides.value = resetOverride(overrides.value, k));
function input(f: Field, e: Event) {
  const el = e.target as HTMLInputElement | HTMLSelectElement;
  let v: string | number | boolean = el.value;
  if (f.kind === "bool") v = (el as HTMLInputElement).checked;
  else if (f.kind === "number" || f.key === "fps") v = Number(el.value);
  overrides.value = setOverride(overrides.value, f.key, v as never);
}

const wideNote = computed(() => {
  const chosen = overrides.value.security;
  return chosen && base.value ? securityNote(base.value.security, chosen) : null;
});

// ---- Variables ----

const rowProblems = computed(() => envRowProblems(envRows.value));
const envProblem = computed(() => checkEnv(rowsToEnv(envRows.value)));
const addRow = () => envRows.value.push({ key: "", value: "" });

// ---- Logo ----

const pendingSvg = ref<string | null>(null);
const iconError = ref<string | null>(null);
const iconVersion = ref(0);
const iconRemoved = ref(false);
const pendingSrc = computed(() => (pendingSvg.value ? `data:image/svg+xml;charset=utf-8,${encodeURIComponent(pendingSvg.value)}` : null));
const iconSrc = computed(() => {
  if (pendingSrc.value) return pendingSrc.value;
  if (existing.value?.hasIcon && !iconRemoved.value) return `${catalogIconUrl(existing.value.id)}?v=${existing.value.updatedAt}.${iconVersion.value}`;
  return null;
});

async function chooseIcon(e: Event) {
  const file = (e.target as HTMLInputElement).files?.[0];
  (e.target as HTMLInputElement).value = "";
  iconError.value = null;
  if (!file) return;
  if (file.size > 256 * 1024) return void (iconError.value = "That file is over 256 KB; a logo is a small SVG.");
  const text = await file.text();
  if (!/<svg[\s>]/i.test(text)) return void (iconError.value = "That isn't an SVG.");
  if (isNew.value) {
    pendingSvg.value = text;
  } else {
    try {
      await api.setCustomIcon(editingId.value!, text);
      iconRemoved.value = false;
      iconVersion.value++;
      await queryClient.invalidateQueries({ queryKey: CUSTOM_KEY });
      void queryClient.invalidateQueries({ queryKey: ["catalog"] });
    } catch (err) {
      iconError.value = errorText(err, "Couldn't upload the logo.");
    }
  }
}

async function resetIcon() {
  iconError.value = null;
  if (pendingSvg.value) {
    pendingSvg.value = null;
    return;
  }
  if (isNew.value) return;
  try {
    await api.deleteCustomIcon(editingId.value!);
    iconRemoved.value = true;
    await queryClient.invalidateQueries({ queryKey: CUSTOM_KEY });
    void queryClient.invalidateQueries({ queryKey: ["catalog"] });
  } catch (err) {
    iconError.value = errorText(err, "Couldn't remove the logo.");
  }
}

// ---- Save ----

const payloadOverrides = computed<CustomOverrides>(() => {
  const env = rowsToEnv(envRows.value);
  return cleanOverrides({ ...overrides.value, ...(Object.keys(env).length ? { env } : {}) });
});
const shmProblem = computed(() => {
  const v = overrides.value.shmMb;
  return v !== undefined && (!Number.isInteger(v) || v < 1) ? "Shared memory is a whole number of megabytes." : null;
});
const problem = computed(
  () =>
    (isNew.value ? slugProblem(slug.value) : null) ??
    shmProblem.value ??
    (Object.keys(rowProblems.value).length ? "Fix the variables first." : null) ??
    envProblem.value ??
    checkShape(host.value) ??
    (!baseId.value ? "No base was chosen. Start from Duplicate on an environment." : null),
);

const saveError = ref<string | null>(null);
const save = useMutation({
  mutationFn: async () => {
    const hostBody = normalizeHost(host.value);
    if (isNew.value) {
      const created = await api.createCustomTemplate({
        slug: slug.value.trim(),
        base: baseId.value,
        shareData: shareData.value,
        overrides: payloadOverrides.value,
        host: hostBody,
      });
      if (pendingSvg.value) {
        try {
          await api.setCustomIcon(created.id, pendingSvg.value);
        } catch {
          // Saved without its logo; it can be added from the editor.
        }
      }
      return created;
    }
    return api.updateCustomTemplate(editingId.value!, { overrides: payloadOverrides.value, host: hostBody, shareData: shareData.value });
  },
  onMutate: () => (saveError.value = null),
  onSuccess: () => {
    void queryClient.invalidateQueries({ queryKey: CUSTOM_KEY });
    void queryClient.invalidateQueries({ queryKey: ["catalog"] });
    void router.push({ name: "custom" });
  },
  onError: (err) => (saveError.value = errorText(err, "Couldn't save it.")),
});

const needsEnv = computed(() => Object.keys(rowsToEnv(envRows.value)).length > 0);
const hostSet = computed(() => !isEmptyHost(host.value));
const CARD = "card space-y-4 p-4 sm:p-5";
</script>

<template>
  <div class="max-w-4xl space-y-6">
    <RouterLink :to="{ name: 'custom' }" class="inline-flex items-center gap-1.5 text-sm text-ink-2 hover:text-ink">
      <ArrowLeft class="size-4" aria-hidden="true" />Custom environments
    </RouterLink>

    <p v-if="!isNew && (customs.isPending.value || (!existing && !customs.isError.value && customs.isFetching.value))" class="text-sm text-ink-3">Loading…</p>
    <FormError v-else-if="customs.isError.value" :message="customs.error.value?.message ?? 'Failed to load'" />
    <div v-else-if="!isNew && !existing" class="card px-6 py-10 text-center">
      <p class="text-base font-medium">No such custom environment</p>
      <RouterLink :to="{ name: 'custom' }" class="btn-ghost mt-4">Back to the list</RouterLink>
    </div>

    <form v-else class="space-y-6" @submit.prevent="problem ? (saveError = problem) : save.mutate()">
      <p v-if="existing?.unavailable" class="rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-sm text-warn" role="status">
        Not available: {{ existing.unavailable }}
      </p>

      <!-- Identity and data -->
      <section :class="CARD" aria-labelledby="ce-basics">
        <div class="flex items-center gap-3">
          <div class="flex size-12 shrink-0 items-center justify-center rounded-lg bg-canvas">
            <img v-if="iconSrc" :src="iconSrc" alt="" class="size-8 object-contain" />
            <Package v-else class="size-6 text-ink-3" aria-hidden="true" />
          </div>
          <div class="min-w-0">
            <h2 id="ce-basics" class="truncate text-base font-semibold">
              {{ overrides.name?.trim() || inherited.name || (isNew ? "New custom environment" : existing?.slug) }}
            </h2>
            <p class="text-sm text-ink-2">
              Based on <span class="font-medium">{{ baseName }}</span><template v-if="!base && baseId"> (not available now)</template>
            </p>
          </div>
        </div>

        <div v-if="isNew" class="sm:max-w-sm">
          <label :for="`${uid}-slug`" class="label">Id</label>
          <div class="flex items-center gap-1 font-mono text-sm">
            <span class="text-ink-3">custom.</span>
            <input :id="`${uid}-slug`" v-model="slug" class="field flex-1" autocomplete="off" spellcheck="false" maxlength="40" required />
          </div>
          <p v-if="slug && slugProblem(slug)" class="mt-1 text-xs text-danger">{{ slugProblem(slug) }}</p>
          <p v-else class="mt-1 text-xs text-ink-3">It can't be changed later. Lowercase letters, digits and hyphens.</p>
        </div>
        <p v-else class="font-mono text-sm text-ink-3">{{ existing ? existing.id : customId(slug) }}</p>

        <fieldset class="space-y-2">
          <legend class="label">Saved data</legend>
          <label class="flex items-start gap-2 text-sm">
            <input v-model="shareData" type="radio" :value="false" :name="`${uid}-share`" class="mt-0.5 accent-[var(--cha-accent)]" />
            <span>Its own saved data</span>
          </label>
          <label class="flex items-start gap-2 text-sm">
            <input v-model="shareData" type="radio" :value="true" :name="`${uid}-share`" class="mt-0.5 accent-[var(--cha-accent)]" />
            <span>Share {{ baseName }}'s saved data (home, library, shared folder)</span>
          </label>
          <p class="text-xs text-ink-3">{{ SHARE_DATA_EXPLAIN }}</p>
          <p v-if="!isNew" class="text-xs text-ink-3">This can change only while neither environment is running.</p>
        </fieldset>
      </section>

      <!-- Fields -->
      <section :class="CARD" aria-labelledby="ce-fields">
        <div>
          <h2 id="ce-fields" class="text-base font-semibold">Settings</h2>
          <p class="mt-0.5 text-sm text-ink-2">
            What you leave alone follows {{ baseName }}, including when its catalog or a new release changes it.
          </p>
        </div>
        <div class="divide-y divide-line">
          <OverrideRow
            v-for="f in FIELDS"
            :key="f.key"
            :label="f.label"
            :inherited="inheritedText(f)"
            :overridden="isOverridden(overrides, f.key)"
            :hint="f.hint"
            @override="start(f.key)"
            @reset="reset(f.key)"
          >
            <template #default="{ id }">
              <textarea v-if="f.kind === 'textarea'" :id="id" :value="valueOf(f.key) as string" rows="3" class="field" @input="input(f, $event)" />
              <input
                v-else-if="f.kind === 'text'"
                :id="id"
                :value="valueOf(f.key) as string"
                class="field"
                :class="f.key === 'image' ? 'font-mono' : ''"
                autocomplete="off"
                spellcheck="false"
                @input="input(f, $event)"
              />
              <input
                v-else-if="f.kind === 'number'"
                :id="id"
                type="number"
                min="1"
                step="1"
                :value="valueOf(f.key) as number"
                class="field w-40"
                @input="input(f, $event)"
              />
              <label v-else-if="f.kind === 'bool'" class="flex min-h-9 items-center gap-2 text-sm pointer-coarse:min-h-11">
                <input :id="id" type="checkbox" :checked="valueOf(f.key) as boolean" class="size-4 accent-[var(--cha-accent)]" @change="input(f, $event)" />
                Yes
              </label>
              <select v-else :id="id" :value="valueOf(f.key)" class="field w-auto min-w-40" @change="input(f, $event)">
                <option v-for="o in f.options" :key="o.value" :value="o.value">{{ o.label }}</option>
              </select>
              <p v-if="f.key === 'security' && wideNote" class="mt-2 rounded-lg border border-warn/30 bg-warn/10 px-3 py-2 text-xs text-warn" role="status">{{ wideNote }}</p>
            </template>
          </OverrideRow>
        </div>
        <FormError :message="shmProblem" />
      </section>

      <!-- Variables -->
      <section :class="CARD" aria-labelledby="ce-env">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h2 id="ce-env" class="text-base font-semibold">Variables</h2>
            <p class="mt-0.5 text-sm text-ink-2">Extra environment variables for the app. Names set by the node (<span class="font-mono">CHA_*</span>, <span class="font-mono">HOME</span>, <span class="font-mono">DISPLAY</span>…) are refused.</p>
          </div>
          <button type="button" class="btn-ghost px-3 py-1 text-xs" @click="addRow"><Plus class="size-3.5" aria-hidden="true" />Add a variable</button>
        </div>
        <ul v-if="envRows.length" class="space-y-2">
          <li v-for="(r, i) in envRows" :key="i">
            <div class="flex flex-wrap items-center gap-2">
              <label :for="`${uid}-ek${i}`" class="sr-only">Name</label>
              <input :id="`${uid}-ek${i}`" v-model="r.key" class="field w-48 font-mono" placeholder="NAME" autocomplete="off" spellcheck="false" :aria-invalid="!!rowProblems[i]" />
              <span class="text-ink-3" aria-hidden="true">=</span>
              <label :for="`${uid}-ev${i}`" class="sr-only">Value</label>
              <input :id="`${uid}-ev${i}`" v-model="r.value" class="field min-w-0 flex-1 basis-48 font-mono" placeholder="value" autocomplete="off" spellcheck="false" />
              <button type="button" class="btn-ghost px-3 hover:border-danger/60 hover:text-danger" :aria-label="`Remove variable ${r.key || i + 1}`" @click="envRows.splice(i, 1)">
                <Trash2 class="size-4" aria-hidden="true" />
              </button>
            </div>
            <p v-if="rowProblems[i]" class="mt-1 text-xs text-danger">{{ rowProblems[i] }}</p>
          </li>
        </ul>
        <p v-else class="text-sm text-ink-3">None.</p>
        <FormError :message="Object.keys(rowProblems).length ? null : envProblem" />
      </section>

      <!-- Logo -->
      <section :class="CARD" aria-labelledby="ce-logo">
        <h2 id="ce-logo" class="text-base font-semibold">Logo</h2>
        <div class="flex flex-wrap items-center gap-3">
          <label class="btn-ghost cursor-pointer has-focus-visible:outline-2 has-focus-visible:outline-offset-2 has-focus-visible:outline-focus">
            <Upload class="size-4" aria-hidden="true" />Upload an SVG
            <input type="file" accept=".svg,image/svg+xml" class="sr-only" @change="chooseIcon" />
          </label>
          <button type="button" class="btn-ghost" :disabled="!iconSrc" @click="resetIcon">Use the base's logo</button>
        </div>
        <p class="text-xs text-ink-3">
          <template v-if="isNew">The logo is uploaded when you save.</template>
          <template v-else>An upload or reset applies at once.</template>
          Without one, the card shows the base's logo.
        </p>
        <FormError :message="iconError" />
      </section>

      <!-- Host options -->
      <section :class="CARD" aria-labelledby="ce-host">
        <div>
          <h2 id="ce-host" class="text-base font-semibold">Host options</h2>
          <p class="mt-0.5 text-sm text-ink-2">
            Folders, ports, capabilities and devices from the node. Each node's owner decides what it allows; the environment starts only on a node that allows all of it.
          </p>
        </div>
        <p v-if="hostOptions.isPending.value" class="text-sm text-ink-3">Loading nodes…</p>
        <FormError v-else-if="hostOptions.isError.value" :message="hostOptions.error.value?.message ?? 'Couldn\'t load the nodes\' host options'" />
        <HostOptionsEditor v-model="host" :nodes="hostOptions.data.value ?? []" :needs-env="needsEnv" />
      </section>

      <FormError :message="saveError" />
      <div class="flex flex-wrap items-center gap-2">
        <button type="submit" class="btn-primary" :disabled="save.isPending.value || (!isNew && !initialized)">
          {{ save.isPending.value ? "Saving…" : isNew ? "Create" : "Save" }}
        </button>
        <RouterLink :to="{ name: 'custom' }" class="btn-ghost">Cancel</RouterLink>
        <p v-if="hostSet" class="text-xs text-ink-3">It will start only on nodes that allow its host options.</p>
      </div>
    </form>
  </div>
</template>

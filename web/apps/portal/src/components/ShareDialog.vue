<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { Check, Copy, X } from "lucide-vue-next";
import { computed, reactive, ref, useId, useTemplateRef, watch } from "vue";

import { ApiError, api } from "../api";
import { SHARE_SLOTS, playerLabel, shareLink, timeLeft } from "../shares";
import FormError from "./FormError.vue";

// "Invite player 2/3/4" on the native <dialog> (ADR 0014). A link is made per gamepad slot; its
// full URL is shown once, here, and can be copied. The live links list their slot and expiry and
// can be revoked. Making a link for a slot that has one replaces it.
const props = defineProps<{ open: boolean; environmentId: string; name: string }>();
const emit = defineEmits<{ close: [] }>();

const dialog = useTemplateRef<HTMLDialogElement>("dialog");
const closeButton = useTemplateRef<HTMLButtonElement>("closeButton");
const titleId = useId();
const queryClient = useQueryClient();
let opener: HTMLElement | null = null;

const queryKey = computed(() => ["shares", props.environmentId]);
const shares = useQuery({
  queryKey,
  queryFn: () => api.shares(props.environmentId),
  enabled: computed(() => props.open),
  refetchInterval: 30_000,
});
const bySlot = computed(() => new Map((shares.data.value ?? []).map((s) => [s.slot, s])));

/** The full URL of the link made in this dialog, by share id; gone when it closes. */
const fresh = reactive<Record<string, string>>({});
const copied = ref<string | null>(null);
const copyFailed = ref<string | null>(null);
const error = ref<string | null>(null);

const text = (err: unknown, fallback: string) =>
  err instanceof ApiError && err.code === "not_running"
    ? "The environment isn't running, so it can't be shared."
    : err instanceof Error && err.message
      ? err.message
      : fallback;

const create = useMutation({
  mutationFn: (slot: number) => api.createShare(props.environmentId, slot),
  onMutate: () => (error.value = null),
  onSuccess: (made) => {
    for (const id of Object.keys(fresh)) if (bySlot.value.get(made.slot)?.id === id) delete fresh[id];
    fresh[made.id] = shareLink(window.location.origin, made.url);
    void queryClient.invalidateQueries({ queryKey: queryKey.value });
  },
  onError: (err) => (error.value = text(err, "Couldn't make the link.")),
});
const revoke = useMutation({
  mutationFn: (shareId: string) => api.revokeShare(props.environmentId, shareId),
  onMutate: () => (error.value = null),
  onSuccess: (_, shareId) => {
    delete fresh[shareId];
    void queryClient.invalidateQueries({ queryKey: queryKey.value });
  },
  onError: (err) => (error.value = text(err, "Couldn't revoke the link.")),
});

async function copy(shareId: string) {
  copied.value = null;
  copyFailed.value = null;
  try {
    await navigator.clipboard.writeText(fresh[shareId] ?? "");
    copied.value = shareId;
  } catch {
    // No clipboard permission, or an insecure page: the field is selectable.
    copyFailed.value = shareId;
  }
}

watch(
  () => props.open,
  (open) => {
    const el = dialog.value;
    if (!el) return;
    if (open && !el.open) {
      opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      error.value = null;
      copied.value = null;
      copyFailed.value = null;
      el.showModal();
      closeButton.value?.focus();
    } else if (!open && el.open) {
      el.close();
    }
  },
  { flush: "post" },
);

function onClose() {
  for (const id of Object.keys(fresh)) delete fresh[id];
  emit("close");
  const to = opener;
  opener = null;
  if (to?.isConnected && (document.activeElement === document.body || document.activeElement === null)) to.focus();
}

const left = (expiresAt: number) => timeLeft(expiresAt) ?? "expired";
</script>

<template>
  <dialog
    ref="dialog"
    :aria-labelledby="titleId"
    class="m-auto w-[min(34rem,calc(100%-2rem))] rounded-xl border border-line bg-panel p-0 text-ink backdrop:bg-scrim"
    @click.self="emit('close')"
    @cancel.prevent="emit('close')"
    @close="onClose"
  >
    <div class="space-y-4 p-5">
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0">
          <h2 :id="titleId" class="text-lg font-semibold tracking-tight">Share {{ name }}</h2>
          <p class="mt-1 text-sm text-ink-2">
            Invite a friend to play on a second gamepad. They need no account and get no keyboard, mouse or screen
            controls.
          </p>
        </div>
        <button ref="closeButton" type="button" class="btn-ghost size-9 shrink-0 border-0 p-0" aria-label="Close" @click="emit('close')">
          <X class="size-4" aria-hidden="true" />
        </button>
      </div>

      <ul class="divide-y divide-line rounded-lg border border-line">
        <li v-for="slot in SHARE_SLOTS" :key="slot" class="space-y-2 p-3">
          <div class="flex flex-wrap items-center gap-x-3 gap-y-2">
            <div class="min-w-0 flex-1">
              <p class="text-sm font-medium">Invite {{ playerLabel(slot) }}</p>
              <p v-if="bySlot.get(slot)" class="text-xs text-ink-3">Link active, ends in {{ left(bySlot.get(slot)!.expiresAt) }}</p>
              <p v-else class="text-xs text-ink-3">No link yet</p>
            </div>
            <button
              type="button"
              class="btn-ghost min-h-9 px-3"
              :disabled="create.isPending.value"
              @click="create.mutate(slot)"
            >
              {{ bySlot.get(slot) ? "New link" : "Create link" }}
            </button>
            <button
              v-if="bySlot.get(slot)"
              type="button"
              class="btn-ghost min-h-9 px-3 hover:border-danger/60 hover:text-danger"
              :disabled="revoke.isPending.value"
              @click="revoke.mutate(bySlot.get(slot)!.id)"
            >
              Revoke
            </button>
          </div>
          <div v-if="bySlot.get(slot) && fresh[bySlot.get(slot)!.id]" class="space-y-1">
            <div class="flex gap-2">
              <input
                class="field font-mono text-xs"
                readonly
                :value="fresh[bySlot.get(slot)!.id]"
                :aria-label="`Link for ${playerLabel(slot)}`"
                @focus="($event.target as HTMLInputElement).select()"
              />
              <button type="button" class="btn-primary min-h-9 shrink-0 px-3" @click="copy(bySlot.get(slot)!.id)">
                <Check v-if="copied === bySlot.get(slot)!.id" class="size-4" aria-hidden="true" />
                <Copy v-else class="size-4" aria-hidden="true" />
                {{ copied === bySlot.get(slot)!.id ? "Copied" : "Copy" }}
              </button>
            </div>
            <p class="text-xs text-ink-3">This is the only time the link is shown.</p>
            <p v-if="copyFailed === bySlot.get(slot)!.id" role="status" class="text-xs text-warn">
              Couldn't copy; select the link and copy it.
            </p>
          </div>
        </li>
      </ul>

      <p class="text-xs text-ink-3">
        Anyone with a link can play as that player until the environment stops (at most 24 hours). A new link for a
        slot replaces the old one.
      </p>
      <FormError v-if="error" polite :message="error" />
      <FormError v-else-if="shares.isError.value" :message="shares.error.value?.message ?? 'Couldn\'t load the links.'" />
    </div>
  </dialog>
</template>

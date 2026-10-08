<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from "@tanstack/vue-query";
import { X } from "lucide-vue-next";
import { computed, reactive, ref, useId, useTemplateRef, watch } from "vue";

import { ApiError, api } from "../api";
import { SHARE_SLOTS, playerLabel, shareLink, shareWarning, timeLeft, type ShareSpec } from "../shares";
import FormError from "./FormError.vue";
import ShareLinkField from "./ShareLinkField.vue";

// The Share dialog on the native <dialog> (ADRs 0014 and 0015). Links come in three kinds: "Invite
// player 2/3/4" (one per gamepad slot), "Invite to watch" (any number) and "Invite to control" (one
// at a time). A link's full URL is shown once, here, and can be copied. The live links list their
// kind and expiry and can be revoked. Making a link where one is limited replaces the old one.
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
const live = computed(() => shares.data.value ?? []);
const bySlot = computed(() => new Map(live.value.filter((s) => s.role === "player").map((s) => [s.slot, s])));
const controller = computed(() => live.value.find((s) => s.role === "controller"));
const viewers = computed(() => live.value.filter((s) => s.role === "viewer"));

/** The full URL of each link made in this dialog, by share id; gone when it closes. */
const fresh = reactive<Record<string, string>>({});
const error = ref<string | null>(null);

const text = (err: unknown, fallback: string) =>
  err instanceof ApiError && err.code === "not_running"
    ? "The environment isn't running, so it can't be shared."
    : err instanceof Error && err.message
      ? err.message
      : fallback;

const create = useMutation({
  mutationFn: (spec: ShareSpec) => api.createShare(props.environmentId, spec),
  onMutate: () => (error.value = null),
  onSuccess: (made) => {
    // A player or controller link replaces the old one: its URL is no use now.
    if (made.role !== "viewer") {
      for (const s of live.value) if (s.role === made.role && s.slot === made.slot) delete fresh[s.id];
    }
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

watch(
  () => props.open,
  (open) => {
    const el = dialog.value;
    if (!el) return;
    if (open && !el.open) {
      opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      error.value = null;
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
    <div class="max-h-[calc(100dvh-2rem)] space-y-4 overflow-y-auto p-5">
      <div class="flex items-start justify-between gap-3">
        <div class="min-w-0">
          <h2 :id="titleId" class="text-lg font-semibold tracking-tight">Share {{ name }}</h2>
          <p class="mt-1 text-sm text-ink-2">
            Invite a friend to watch, play on a second gamepad, or use the controls. They need no account.
          </p>
        </div>
        <button ref="closeButton" type="button" class="btn-ghost size-9 shrink-0 border-0 p-0" aria-label="Close" @click="emit('close')">
          <X class="size-4" aria-hidden="true" />
        </button>
      </div>

      <section class="space-y-2" aria-label="Players">
        <h3 class="text-xs font-medium tracking-wide text-ink-3 uppercase">Play on a gamepad</h3>
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
                @click="create.mutate({ role: 'player', slot })"
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
            <ShareLinkField
              v-if="bySlot.get(slot) && fresh[bySlot.get(slot)!.id]"
              :url="fresh[bySlot.get(slot)!.id]!"
              :label="`Link for ${playerLabel(slot)}`"
            />
          </li>
        </ul>
        <p class="text-xs text-ink-3">{{ shareWarning("player", name) }}</p>
      </section>

      <section class="space-y-2" aria-label="Watchers">
        <h3 class="text-xs font-medium tracking-wide text-ink-3 uppercase">Watch</h3>
        <div class="space-y-2 rounded-lg border border-line p-3">
          <div class="flex flex-wrap items-center gap-x-3 gap-y-2">
            <div class="min-w-0 flex-1">
              <p class="text-sm font-medium">Invite to watch</p>
              <p class="text-xs text-ink-3">
                {{ viewers.length ? `${viewers.length} active ${viewers.length === 1 ? "link" : "links"}` : "No link yet" }}
              </p>
            </div>
            <button
              type="button"
              class="btn-ghost min-h-9 px-3"
              :disabled="create.isPending.value"
              @click="create.mutate({ role: 'viewer' })"
            >
              New link
            </button>
          </div>
          <ul v-if="viewers.length" class="divide-y divide-line">
            <li v-for="(v, n) in viewers" :key="v.id" class="space-y-2 py-2">
              <div class="flex items-center gap-3">
                <p class="min-w-0 flex-1 text-xs text-ink-2">Watch link {{ n + 1 }}, ends in {{ left(v.expiresAt) }}</p>
                <button
                  type="button"
                  class="btn-ghost min-h-8 px-3 hover:border-danger/60 hover:text-danger"
                  :disabled="revoke.isPending.value"
                  @click="revoke.mutate(v.id)"
                >
                  Revoke
                </button>
              </div>
              <ShareLinkField v-if="fresh[v.id]" :url="fresh[v.id]!" :label="`Watch link ${n + 1}`" />
            </li>
          </ul>
        </div>
        <p class="text-xs text-ink-3">{{ shareWarning("viewer", name) }}</p>
      </section>

      <section class="space-y-2" aria-label="Controllers">
        <h3 class="text-xs font-medium tracking-wide text-ink-3 uppercase">Control</h3>
        <div class="space-y-2 rounded-lg border border-line p-3">
          <div class="flex flex-wrap items-center gap-x-3 gap-y-2">
            <div class="min-w-0 flex-1">
              <p class="text-sm font-medium">Invite to control</p>
              <p v-if="controller" class="text-xs text-ink-3">Link active, ends in {{ left(controller.expiresAt) }}</p>
              <p v-else class="text-xs text-ink-3">No link yet</p>
            </div>
            <button
              type="button"
              class="btn-ghost min-h-9 px-3"
              :disabled="create.isPending.value"
              @click="create.mutate({ role: 'controller' })"
            >
              {{ controller ? "New link" : "Create link" }}
            </button>
            <button
              v-if="controller"
              type="button"
              class="btn-ghost min-h-9 px-3 hover:border-danger/60 hover:text-danger"
              :disabled="revoke.isPending.value"
              @click="revoke.mutate(controller.id)"
            >
              Revoke
            </button>
          </div>
          <ShareLinkField v-if="controller && fresh[controller.id]" :url="fresh[controller.id]!" label="Link to control" />
        </div>
        <p class="text-xs text-warn">{{ shareWarning("controller", name) }}</p>
      </section>

      <FormError v-if="error" polite :message="error" />
      <FormError v-else-if="shares.isError.value" :message="shares.error.value?.message ?? 'Couldn\'t load the links.'" />
    </div>
  </dialog>
</template>

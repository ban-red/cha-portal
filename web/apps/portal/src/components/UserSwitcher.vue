<script setup lang="ts">
// "View as": an admin (or an admin already viewing as someone) picks any enabled user and the
// portal shows what that user sees. A combobox pattern: the search box keeps focus, Up/Down
// move through the list, Enter switches, Esc closes and gives focus back to the button.
import { useQuery } from "@tanstack/vue-query";
import { Check, ChevronDown, Search, Users } from "lucide-vue-next";
import { computed, nextTick, onBeforeUnmount, ref, useId, watch } from "vue";

import { api } from "../api";
import { filterSwitchable, identityLine, moveActive } from "../switcher";
import { useSession } from "../stores/session";
import FormError from "./FormError.vue";

const emit = defineEmits<{ switched: [] }>();

const session = useSession();
const open = ref(false);
const query = ref("");
const active = ref(-1);
const error = ref<string | null>(null);
const pending = ref(false);

const root = ref<HTMLElement | null>(null);
const button = ref<HTMLButtonElement | null>(null);
const input = ref<HTMLInputElement | null>(null);
const id = useId();
const listId = `${id}-list`;
const optionId = (userId: string) => `${id}-opt-${userId}`;

const users = useQuery({
  queryKey: ["switchable"],
  queryFn: api.switchable,
  enabled: computed(() => open.value && session.canSwitch),
  staleTime: 0,
});
const shown = computed(() => filterSwitchable(users.data.value ?? [], query.value));
const activeId = computed(() => (shown.value[active.value] ? optionId(shown.value[active.value]!.id) : undefined));

watch(shown, (list) => {
  if (active.value >= list.length) active.value = list.length ? 0 : -1;
});
watch(query, () => (active.value = shown.value.length ? 0 : -1));

async function show() {
  open.value = true;
  query.value = "";
  error.value = null;
  active.value = -1;
  document.addEventListener("pointerdown", onPointerDown);
  await nextTick();
  input.value?.focus();
}
function hide(returnFocus = false) {
  if (!open.value) return;
  open.value = false;
  document.removeEventListener("pointerdown", onPointerDown);
  if (returnFocus) void nextTick(() => button.value?.focus());
}
function onPointerDown(e: PointerEvent) {
  if (root.value && !root.value.contains(e.target as Node)) hide();
}
function onFocusOut(e: FocusEvent) {
  const to = e.relatedTarget as Node | null;
  if (open.value && to && !root.value?.contains(to)) hide();
}
onBeforeUnmount(() => document.removeEventListener("pointerdown", onPointerDown));

// Once the list arrives, start on the current user.
watch(
  () => users.data.value,
  () => {
    if (open.value && active.value < 0 && shown.value.length) {
      const here = shown.value.findIndex((u) => u.id === session.user?.id);
      active.value = here >= 0 ? here : 0;
    }
  },
);

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    hide(true);
  } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    active.value = moveActive(active.value, e.key === "ArrowDown" ? 1 : -1, shown.value.length);
    void nextTick(() => document.getElementById(activeId.value ?? "")?.scrollIntoView({ block: "nearest" }));
  } else if (e.key === "Enter") {
    e.preventDefault();
    const u = shown.value[active.value];
    if (u) void choose(u.id);
  }
}

async function choose(userId: string) {
  if (pending.value) return;
  if (userId === session.user?.id) return hide(true);
  pending.value = true;
  error.value = null;
  try {
    // Back to yourself is the same as the banner's button; anything else is a switch.
    if (userId === session.impersonator?.id) await session.switchBack();
    else await session.switchTo(userId);
    hide();
    emit("switched");
  } catch (err) {
    error.value = err instanceof Error ? err.message : "Couldn't switch.";
  } finally {
    pending.value = false;
  }
}

const ROLE = {
  admin: "border-accent/40 bg-accent-soft text-accent",
  user: "border-line-strong text-ink-2",
  guest: "border-line-strong text-ink-2",
} as const;
</script>

<template>
  <div ref="root" class="relative" @focusout="onFocusOut">
    <button
      ref="button"
      type="button"
      class="inline-flex size-9 shrink-0 items-center justify-center gap-2 rounded-lg border border-line-strong text-ink-2 transition hover:border-ink-3 hover:text-ink pointer-coarse:size-11 @min-[52rem]:w-auto @min-[52rem]:px-3"
      aria-haspopup="listbox"
      :aria-expanded="open"
      :aria-controls="open ? `${id}-panel` : undefined"
      aria-label="View as another user"
      title="View as another user"
      @click="open ? hide() : show()"
    >
      <Users class="size-5" aria-hidden="true" />
      <span class="hidden text-sm @min-[52rem]:inline">View as</span>
      <ChevronDown class="hidden size-4 @min-[52rem]:block" aria-hidden="true" />
    </button>

    <div
      v-if="open"
      :id="`${id}-panel`"
      class="absolute top-full right-0 z-40 mt-2 w-80 max-w-[calc(100vw-2rem)] rounded-xl border border-line bg-panel p-2 shadow-lg"
      @keydown="onKey"
    >
      <div class="relative">
        <label :for="`${id}-search`" class="sr-only">Find a user</label>
        <Search class="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-ink-3" aria-hidden="true" />
        <input
          :id="`${id}-search`"
          ref="input"
          v-model="query"
          type="search"
          role="combobox"
          aria-expanded="true"
          :aria-controls="listId"
          :aria-activedescendant="activeId"
          aria-autocomplete="list"
          autocomplete="off"
          spellcheck="false"
          placeholder="Find a user…"
          class="field w-full pl-9"
        />
      </div>

      <p class="sr-only" role="status">{{ users.isPending.value ? "Loading" : `${shown.length} ${shown.length === 1 ? "user" : "users"}` }}</p>
      <p v-if="users.isPending.value" class="px-2 py-3 text-sm text-ink-3">Loading…</p>
      <FormError v-else-if="users.isError.value" class="mt-2" :message="users.error.value?.message ?? 'Failed to load'" />
      <p v-else-if="!shown.length" class="px-2 py-3 text-sm text-ink-3">No one matches.</p>
      <ul v-else :id="listId" role="listbox" aria-label="Users" class="mt-2 max-h-72 overflow-y-auto">
        <li
          v-for="(u, i) in shown"
          :id="optionId(u.id)"
          :key="u.id"
          role="option"
          :aria-selected="u.id === session.user?.id"
          class="flex min-h-9 cursor-pointer items-center gap-2 rounded-lg px-2 py-1.5 pointer-coarse:min-h-11"
          :class="i === active ? 'bg-panel-2' : ''"
          @click="choose(u.id)"
          @mousemove="active = i"
        >
          <div class="min-w-0 flex-1">
            <p class="truncate text-sm font-medium">{{ u.displayName }}</p>
            <p class="truncate text-xs text-ink-3">{{ identityLine(u) }}</p>
          </div>
          <span class="shrink-0 rounded-full border px-2 py-0.5 text-2xs" :class="ROLE[u.role]">{{ u.role }}</span>
          <Check v-if="u.id === session.user?.id" class="size-4 shrink-0 text-accent" aria-label="Current user" />
        </li>
      </ul>
      <FormError v-if="error" class="mt-2" :message="error" />
    </div>
  </div>
</template>

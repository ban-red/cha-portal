<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";

import { useSession } from "../stores/session";
import { useTheme } from "../themes/runtime";
import BrandMark from "./BrandMark.vue";

const session = useSession();
const router = useRouter();
const route = useRoute();
const menuOpen = ref(false);

const nav = computed(() => [
  { to: "/", label: "Environments", show: true },
  { to: "/controllers", label: "Controllers", show: true, section: "Settings" },
  { to: "/settings/appearance", label: "Appearance", show: true },
  { to: "/settings/storage", label: "Storage", show: true },
  { to: "/admin/nodes", label: "Nodes", show: session.isAdmin, section: "Admin" },
  { to: "/admin/users", label: "Users", show: session.isAdmin },
  { to: "/admin/storage", label: "App data", show: session.isAdmin },
  { to: "/admin/audit", label: "Audit log", show: session.isAdmin },
]);

const { prefs, resolved, setPrefs } = useTheme();
const NEXT = { system: "light", light: "dark", dark: "system" } as const;
const NAME = { system: "System", light: "Light", dark: "Dark" } as const;
/** "System (Dark)" for System, otherwise just the name. */
const nameOf = (a: keyof typeof NAME) => (a === "system" ? `System (${NAME[resolved.value.appearance]})` : NAME[a]);
const appearanceLabel = computed(
  () => `Appearance: ${nameOf(prefs.value.appearance)}. Switch to ${nameOf(NEXT[prefs.value.appearance])}`,
);
function cycleAppearance() {
  setPrefs({ appearance: NEXT[prefs.value.appearance] });
}

async function signOut() {
  await session.logout();
  await router.push({ name: "login" });
}
</script>

<template>
  <div class="flex min-h-screen">
    <aside
      class="fixed inset-y-0 left-0 z-20 flex w-64 flex-col border-r border-line bg-panel transition-transform md:sticky md:top-0 md:h-screen md:translate-x-0"
      :class="menuOpen ? 'translate-x-0' : '-translate-x-full'"
    >
      <div class="flex h-16 items-center gap-3 px-5">
        <BrandMark class="size-8" />
        <span class="text-[0.9375rem] font-semibold tracking-tight">Cha Portal</span>
      </div>
      <nav class="flex-1 space-y-0.5 px-3 py-2" @click="menuOpen = false">
        <template v-for="item in nav" :key="item.to">
          <p v-if="item.show && item.section" class="px-3 pt-5 pb-1.5 text-2xs font-medium tracking-wider text-ink-3 uppercase">
            {{ item.section }}
          </p>
          <RouterLink
            v-if="item.show"
            :to="item.to"
            class="block rounded-lg px-3 py-2 text-sm text-ink-2 transition hover:bg-panel-2 hover:text-ink"
            exact-active-class="!bg-panel-2 !text-ink font-medium"
          >
            {{ item.label }}
          </RouterLink>
        </template>
      </nav>
      <div class="border-t border-line p-4">
        <div class="flex items-center justify-between gap-2">
          <div class="min-w-0">
            <p class="truncate text-sm font-medium">{{ session.user?.displayName }}</p>
            <p class="truncate text-xs text-ink-3">{{ session.user?.username }} · {{ session.user?.role }}</p>
          </div>
          <button class="btn-ghost px-3 py-1.5 text-xs" @click="signOut">Sign out</button>
        </div>
      </div>
    </aside>
    <div v-if="menuOpen" class="fixed inset-0 z-10 bg-scrim md:hidden" @click="menuOpen = false" />

    <div class="flex min-w-0 flex-1 flex-col">
      <header class="flex h-16 items-center gap-3 border-b border-line px-4 md:px-8">
        <button class="btn-ghost px-2.5 py-1.5 md:hidden" aria-label="Menu" @click="menuOpen = !menuOpen">☰</button>
        <h1 class="text-lg font-semibold tracking-tight">{{ route.meta.title }}</h1>
        <button
          type="button"
          class="ml-auto inline-flex size-9 items-center justify-center rounded-lg border border-line-strong text-ink-2 transition hover:border-ink-3 hover:text-ink pointer-coarse:size-11"
          :aria-label="appearanceLabel"
          :title="appearanceLabel"
          @click="cycleAppearance"
        >
          <svg class="size-5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <template v-if="prefs.appearance === 'system'">
              <rect x="3" y="4" width="18" height="12" rx="2" />
              <path d="M8 20h8M12 16v4" />
            </template>
            <template v-else-if="prefs.appearance === 'light'">
              <circle cx="12" cy="12" r="4" />
              <path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
            </template>
            <path v-else d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
          </svg>
        </button>
      </header>
      <main class="flex-1 px-4 py-6 md:px-8 md:py-8">
        <RouterView />
      </main>
    </div>
  </div>
</template>

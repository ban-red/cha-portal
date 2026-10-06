<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";

import { useSession } from "../stores/session";
import BrandMark from "./BrandMark.vue";

const session = useSession();
const router = useRouter();
const route = useRoute();
const menuOpen = ref(false);

const nav = computed(() => [
  { to: "/", label: "Environments", show: true },
  { to: "/controllers", label: "Controllers", show: true, section: "Settings" },
  { to: "/settings/storage", label: "Storage", show: true },
  { to: "/admin/nodes", label: "Nodes", show: session.isAdmin, section: "Admin" },
  { to: "/admin/users", label: "Users", show: session.isAdmin },
  { to: "/admin/storage", label: "App data", show: session.isAdmin },
  { to: "/admin/audit", label: "Audit log", show: session.isAdmin },
]);

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
      </header>
      <main class="flex-1 px-4 py-6 md:px-8 md:py-8">
        <RouterView />
      </main>
    </div>
  </div>
</template>

<script setup lang="ts">
// The portal's frame. Three layouts from one <aside>:
// - 1024px and up: the full sidebar (16rem), sticky.
// - 768 to 1023px: an icon rail (4.5rem). Labels stay as sr-only text and `title`; the
//   account card shrinks to its avatar, which opens a small menu with Sign out.
// - Under 768px: an off-canvas drawer opened from the header's menu button. While it is open
//   focus is kept inside it, Esc and the scrim close it, and the page behind is `inert`.
// A page can add controls to the header through the #page-actions element (Teleport).
import { ChevronsLeft, Database, FileText, Folder, Gamepad2, LayoutGrid, LogOut, Menu, Monitor, Moon, Palette, Server, SlidersHorizontal, Sun, User } from "lucide-vue-next";
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch, type Component } from "vue";
import { useRoute, useRouter } from "vue-router";

import { useSession } from "../stores/session";
import { useTheme } from "../themes/runtime";
import BrandMark from "./BrandMark.vue";
import HealthBar from "./HealthBar.vue";

const session = useSession();
const router = useRouter();
const route = useRoute();

interface NavItem {
  to: string;
  label: string;
  icon: Component;
  show: boolean;
  /** A header above this item. */
  section?: string;
}
const nav = computed<NavItem[]>(() => [
  { to: "/", label: "Environments", icon: LayoutGrid, show: true },
  { to: "/controllers", label: "Controllers", icon: Gamepad2, show: true, section: "Settings" },
  { to: "/settings/appearance", label: "Appearance", icon: Palette, show: true },
  { to: "/settings/storage", label: "Storage", icon: Database, show: true },
  { to: "/admin/nodes", label: "Nodes", icon: Server, show: session.isAdmin, section: "Admin" },
  { to: "/admin/users", label: "Users", icon: User, show: session.isAdmin },
  { to: "/admin/storage", label: "App data", icon: Folder, show: session.isAdmin },
  { to: "/admin/settings", label: "Settings", icon: SlidersHorizontal, show: session.isAdmin },
  { to: "/admin/audit", label: "Audit log", icon: FileText, show: session.isAdmin },
]);

// ---- layout mode, from the viewport ------------------------------------------------------

const mql = typeof matchMedia === "undefined" ? null : { md: matchMedia("(min-width: 768px)"), lg: matchMedia("(min-width: 1024px)") };
const isDrawer = ref(mql ? !mql.md.matches : false);
const isRail = ref(mql ? mql.md.matches && !mql.lg.matches : false);
function measure() {
  if (!mql) return;
  isDrawer.value = !mql.md.matches;
  isRail.value = mql.md.matches && !mql.lg.matches;
}
onMounted(() => {
  mql?.md.addEventListener("change", measure);
  mql?.lg.addEventListener("change", measure);
});
onBeforeUnmount(() => {
  mql?.md.removeEventListener("change", measure);
  mql?.lg.removeEventListener("change", measure);
  document.removeEventListener("keydown", onDocumentKey);
});

// ---- the drawer --------------------------------------------------------------------------

const drawerOpen = ref(false);
const sidebar = ref<HTMLElement | null>(null);
const menuButton = ref<HTMLButtonElement | null>(null);
const pageTitle = ref<HTMLElement | null>(null);

const FOCUSABLE = 'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';
const focusables = () =>
  [...(sidebar.value?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])].filter((el) => el.offsetParent !== null || el === document.activeElement);

// Opening shows the drawer at once (only closing animates `visibility`), so its links can
// take focus straight away.
async function openDrawer() {
  drawerOpen.value = true;
  document.addEventListener("keydown", onDocumentKey);
  await nextTick();
  // Into the drawer: the current page's link, else the first thing.
  const list = focusables();
  (list.find((el) => el.getAttribute("aria-current") === "page") ?? list[0])?.focus();
}

function closeDrawer(focus: "menu" | "title" | "none" = "menu") {
  if (!drawerOpen.value) return;
  drawerOpen.value = false;
  document.removeEventListener("keydown", onDocumentKey);
  // After the render that lifts `inert` from the page, or the focus call is ignored.
  if (focus === "menu") void nextTick(() => menuButton.value?.focus());
  else if (focus === "title") void nextTick(() => pageTitle.value?.focus());
}

function onDocumentKey(e: KeyboardEvent) {
  if (!drawerOpen.value) return;
  if (e.key === "Escape") {
    e.preventDefault();
    closeDrawer("menu");
  } else if (e.key === "Tab") {
    // Keep Tab inside the drawer.
    const list = focusables();
    if (!list.length) return;
    const first = list[0]!;
    const last = list[list.length - 1]!;
    const active = document.activeElement;
    if (!sidebar.value?.contains(active)) {
      e.preventDefault();
      first.focus();
    } else if (e.shiftKey && active === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }
}

// Going to a page closes it (focus to the new page's title); growing the window does too.
watch(() => route.fullPath, () => closeDrawer("title"));
watch(isDrawer, (drawer) => {
  if (!drawer) closeDrawer("none");
});

// ---- the rail's account menu -------------------------------------------------------------

const accountOpen = ref(false);
const accountRoot = ref<HTMLElement | null>(null);
const avatarButton = ref<HTMLButtonElement | null>(null);

function closeAccount(returnFocus = false) {
  accountOpen.value = false;
  if (returnFocus) avatarButton.value?.focus();
}
function onAccountKey(e: KeyboardEvent) {
  if (e.key === "Escape" && accountOpen.value) {
    e.preventDefault();
    e.stopPropagation();
    closeAccount(true);
  }
}
function onPointerDown(e: PointerEvent) {
  if (accountOpen.value && accountRoot.value && !accountRoot.value.contains(e.target as Node)) closeAccount();
}
watch(accountOpen, (open) => {
  if (open) document.addEventListener("pointerdown", onPointerDown);
  else document.removeEventListener("pointerdown", onPointerDown);
});
onBeforeUnmount(() => document.removeEventListener("pointerdown", onPointerDown));
// Leaving the rail layout takes the menu with it.
watch(isRail, (rail) => {
  if (!rail) accountOpen.value = false;
});
// Tab out of the menu closes it.
function onAccountFocusOut(e: FocusEvent) {
  const to = e.relatedTarget as Node | null;
  if (accountOpen.value && to && !accountRoot.value?.contains(to)) closeAccount();
}

const initial = computed(() => (session.user?.displayName || session.user?.username || "?").trim().charAt(0).toUpperCase());

async function signOut() {
  await session.logout();
  await router.push({ name: "login" });
}

// ---- appearance toggle -------------------------------------------------------------------

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

/** Shared by the header's buttons: 36px on a fine pointer, 44px on touch. */
const ICON_BUTTON =
  "inline-flex size-9 shrink-0 items-center justify-center rounded-lg border border-line-strong text-ink-2 transition hover:border-ink-3 hover:text-ink pointer-coarse:size-11";
</script>

<template>
  <div class="flex min-h-dvh">
    <aside
      id="sidebar"
      ref="sidebar"
      class="fixed inset-y-0 left-0 z-30 flex w-72 max-w-[85vw] flex-col border-r border-line bg-panel/90 pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)] pl-[env(safe-area-inset-left)] backdrop-blur-xl duration-200 ease-out transparency-reduced:bg-panel transparency-reduced:backdrop-blur-none max-md:shadow-xl md:visible md:sticky md:top-0 md:h-dvh md:w-[4.5rem] md:max-w-none md:translate-x-0 md:pt-0 md:pb-0 md:pl-0 md:shadow-none lg:w-64"
      :class="drawerOpen ? 'visible translate-x-0 transition-transform' : 'invisible -translate-x-full transition-[transform,visibility]'"
      :aria-label="isDrawer ? 'Navigation' : undefined"
      :role="isDrawer ? 'dialog' : undefined"
      :aria-modal="isDrawer && drawerOpen ? 'true' : undefined"
    >
      <div class="flex h-16 shrink-0 items-center gap-3 px-5 md:max-lg:justify-center md:max-lg:px-0">
        <BrandMark class="size-8 shrink-0" />
        <span class="min-w-0 flex-1 truncate text-[0.9375rem] font-semibold tracking-tight md:max-lg:sr-only">Cha Portal</span>
        <button
          type="button"
          class="-mr-2 inline-flex size-9 items-center justify-center rounded-lg text-ink-2 transition hover:bg-panel-2 hover:text-ink pointer-coarse:size-11 md:hidden"
          aria-label="Close navigation"
          @click="closeDrawer('menu')"
        >
          <ChevronsLeft class="size-5" aria-hidden="true" />
        </button>
      </div>

      <nav aria-label="Main" class="min-h-0 flex-1 overflow-y-auto px-3 py-2">
        <ul class="space-y-0.5">
          <template v-for="item in nav" :key="item.to">
            <li
              v-if="item.show && item.section"
              role="presentation"
              class="px-3 pt-5 pb-1.5 text-2xs font-medium tracking-wider text-ink-3 uppercase md:max-lg:mx-2 md:max-lg:mt-3 md:max-lg:mb-2 md:max-lg:border-t md:max-lg:border-line md:max-lg:p-0"
            >
              <span class="md:max-lg:sr-only">{{ item.section }}</span>
            </li>
            <li v-if="item.show">
              <RouterLink
                :to="item.to"
                :title="isRail ? item.label : undefined"
                class="relative flex min-h-9 items-center gap-3 rounded-lg px-3 py-1.5 text-sm text-ink-2 transition before:absolute before:inset-y-1.5 before:left-0 before:w-[3px] before:rounded-full before:bg-accent before:opacity-0 before:transition-opacity hover:bg-panel-2 hover:text-ink aria-[current=page]:bg-accent-soft aria-[current=page]:font-medium aria-[current=page]:text-ink aria-[current=page]:before:opacity-100 pointer-coarse:min-h-11 md:max-lg:justify-center md:max-lg:px-0"
              >
                <component :is="item.icon" class="size-5 shrink-0" aria-hidden="true" />
                <span class="min-w-0 truncate md:max-lg:sr-only">{{ item.label }}</span>
              </RouterLink>
            </li>
          </template>
        </ul>
      </nav>

      <!-- The account: a card and Sign out, or in the rail just the avatar with a menu. -->
      <div class="shrink-0 border-t border-line p-3">
        <div class="md:max-lg:hidden">
          <div class="flex items-center gap-3 rounded-xl border border-line bg-panel-2 p-2.5">
            <span class="grid size-9 shrink-0 place-items-center rounded-full bg-accent-soft text-sm font-semibold text-ink" aria-hidden="true">{{ initial }}</span>
            <div class="min-w-0">
              <p class="truncate text-sm font-medium">{{ session.user?.displayName }}</p>
              <p class="truncate text-xs text-ink-3">{{ session.user?.username }} · {{ session.user?.role }}</p>
            </div>
          </div>
          <button type="button" class="btn-ghost mt-2 min-h-9 w-full pointer-coarse:min-h-11" @click="signOut">
            <LogOut class="size-4" aria-hidden="true" />
            Sign out
          </button>
        </div>
        <div v-if="isRail" ref="accountRoot" class="relative flex justify-center" @keydown="onAccountKey" @focusout="onAccountFocusOut">
          <button
            ref="avatarButton"
            type="button"
            class="grid size-9 place-items-center rounded-full bg-accent-soft text-sm font-semibold text-ink transition hover:ring-2 hover:ring-line-strong pointer-coarse:size-11"
            aria-haspopup="true"
            :aria-expanded="accountOpen"
            aria-controls="account-menu"
            :aria-label="`Account: ${session.user?.displayName}`"
            :title="session.user?.displayName"
            @click="accountOpen = !accountOpen"
          >
            {{ initial }}
          </button>
          <div
            v-if="accountOpen"
            id="account-menu"
            role="group"
            aria-label="Account"
            class="absolute bottom-0 left-full z-40 ml-3 w-56 rounded-xl border border-line bg-panel p-3 shadow-lg"
          >
            <p class="truncate text-sm font-medium">{{ session.user?.displayName }}</p>
            <p class="truncate text-xs text-ink-3">{{ session.user?.username }} · {{ session.user?.role }}</p>
            <button type="button" class="btn-ghost mt-3 min-h-9 w-full pointer-coarse:min-h-11" @click="signOut">
              <LogOut class="size-4" aria-hidden="true" />
              Sign out
            </button>
          </div>
        </div>
      </div>
    </aside>

    <div
      class="fixed inset-0 z-20 bg-scrim transition-opacity duration-200 md:hidden"
      :class="drawerOpen ? 'opacity-100' : 'pointer-events-none opacity-0'"
      aria-hidden="true"
      @click="closeDrawer('menu')"
    />

    <div class="flex min-w-0 flex-1 flex-col" :inert="drawerOpen || undefined">
      <header
        class="@container sticky top-0 z-10 flex min-h-16 items-center gap-3 border-b border-line bg-canvas/80 pt-[env(safe-area-inset-top)] pr-[max(1rem,env(safe-area-inset-right))] pl-[max(1rem,env(safe-area-inset-left))] backdrop-blur transparency-reduced:bg-canvas transparency-reduced:backdrop-blur-none md:px-8"
      >
        <button
          ref="menuButton"
          type="button"
          :class="ICON_BUTTON + ' md:hidden'"
          aria-label="Open navigation"
          aria-controls="sidebar"
          :aria-expanded="drawerOpen"
          @click="openDrawer"
        >
          <Menu class="size-5" aria-hidden="true" />
        </button>
        <div class="min-w-0 flex-1 py-2.5">
          <h1 ref="pageTitle" tabindex="-1" class="truncate text-[1.375rem] leading-7 font-semibold tracking-tight focus:outline-none sm:text-[1.75rem] sm:leading-9">
            {{ route.meta.title }}
          </h1>
          <p v-if="route.meta.subtitle" class="hidden truncate text-sm text-ink-2 @min-[64rem]:block">{{ route.meta.subtitle }}</p>
        </div>
        <!-- Pages put their own controls here (search, say) with <Teleport to="#page-actions" defer>. -->
        <div id="page-actions" class="flex items-center gap-2" />
        <HealthBar v-if="session.isAdmin" />
        <button
          type="button"
          :class="ICON_BUTTON"
          :aria-label="appearanceLabel"
          :title="appearanceLabel"
          @click="cycleAppearance"
        >
          <Monitor v-if="prefs.appearance === 'system'" class="size-5" aria-hidden="true" />
          <Sun v-else-if="prefs.appearance === 'light'" class="size-5" aria-hidden="true" />
          <Moon v-else class="size-5" aria-hidden="true" />
        </button>
      </header>
      <main class="flex-1 py-6 pr-[max(1rem,env(safe-area-inset-right))] pb-[max(1.5rem,env(safe-area-inset-bottom))] pl-[max(1rem,env(safe-area-inset-left))] md:px-8 md:py-8">
        <RouterView />
      </main>
    </div>
  </div>
</template>

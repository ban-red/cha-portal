import { createRouter, createWebHistory } from "vue-router";

import { useSession } from "./stores/session";

declare module "vue-router" {
  interface RouteMeta {
    /** Reachable signed out (sign-in, setup). */
    public?: boolean;
    /** A share link's page: open to anyone, signed in or not, and never redirected. */
    guest?: boolean;
    admin?: boolean;
    title?: string;
    /** One plain line under the page title in the shell header. */
    subtitle?: string;
  }
}

export const router = createRouter({
  history: createWebHistory(),
  routes: [
    { path: "/setup", name: "setup", component: () => import("./views/SetupView.vue"), meta: { public: true, title: "Set up" } },
    { path: "/login", name: "login", component: () => import("./views/LoginView.vue"), meta: { public: true, title: "Sign in" } },
    {
      path: "/",
      component: () => import("./components/AppShell.vue"),
      children: [
        { path: "", name: "dashboard", component: () => import("./views/DashboardView.vue"), meta: { title: "Environments", subtitle: "Launch and manage your applications and desktops on remote GPUs." } },
        { path: "controllers", name: "controllers", component: () => import("./views/ControllersView.vue"), meta: { title: "Controllers" } },
        { path: "settings/appearance", name: "appearance", component: () => import("./views/AppearanceView.vue"), meta: { title: "Appearance" } },
        { path: "settings/storage", name: "storage", component: () => import("./views/StorageView.vue"), meta: { title: "Storage" } },
        { path: "settings/moonlight", name: "moonlight-devices", component: () => import("./views/MoonlightDevicesView.vue"), meta: { title: "Moonlight", subtitle: "Pair Moonlight apps to play your running environments." } },
        { path: "settings/devices", name: "devices", component: () => import("./views/DevicesView.vue"), meta: { title: "Devices", subtitle: "Cha Player installs signed in as you." } },
        { path: "link", name: "link", component: () => import("./views/LinkView.vue"), meta: { title: "Link a device", subtitle: "Approve a sign-in from Cha Player." } },
        { path: "admin/nodes", name: "nodes", component: () => import("./views/NodesView.vue"), meta: { admin: true, title: "Nodes" } },
        { path: "admin/settings", name: "admin-settings", component: () => import("./views/SettingsView.vue"), meta: { admin: true, title: "Settings", subtitle: "Portal-wide settings." } },
        { path: "admin/catalogs", name: "catalogs", component: () => import("./views/CatalogsView.vue"), meta: { admin: true, title: "Catalogs" } },
        { path: "admin/custom", name: "custom", component: () => import("./views/CustomListView.vue"), meta: { admin: true, title: "Custom environments" } },
        { path: "admin/custom/new", name: "custom-new", component: () => import("./views/CustomEditView.vue"), meta: { admin: true, title: "New custom environment" } },
        { path: "admin/custom/:id", name: "custom-edit", component: () => import("./views/CustomEditView.vue"), meta: { admin: true, title: "Edit custom environment" } },
        { path: "admin/users", name: "users", component: () => import("./views/UsersView.vue"), meta: { admin: true, title: "Users" } },
        { path: "admin/storage", name: "admin-storage", component: () => import("./views/AdminStorageView.vue"), meta: { admin: true, title: "App data" } },
        { path: "admin/audit", name: "audit", component: () => import("./views/AuditView.vue"), meta: { admin: true, title: "Audit log" } },
      ],
    },
    // Full screen, outside the shell.
    {
      path: "/environments/:id/session",
      name: "session",
      component: () => import("./views/SessionView.vue"),
      meta: { title: "Session" },
    },
    // A guest with a share link plays with no account (ADR 0014).
    { path: "/s/:token", name: "join", component: () => import("./views/JoinView.vue"), meta: { guest: true, title: "Join" } },
    { path: "/:rest(.*)*", redirect: "/" },
  ],
});

router.beforeEach(async (to) => {
  if (to.meta.guest) return true;
  const session = useSession();
  await session.load();
  if (session.setupNeeded) return to.name === "setup" ? true : { name: "setup" };
  if (to.name === "setup") return { name: "dashboard" };
  if (to.meta.public) return session.user ? { name: "dashboard" } : true;
  if (!session.user) return { name: "login", query: to.fullPath === "/" ? {} : { next: to.fullPath } };
  if (to.meta.admin && !session.isAdmin) return { name: "dashboard" };
  return true;
});

router.afterEach((to) => {
  document.title = to.meta.title ? `${to.meta.title} · Cha Portal` : "Cha Portal";
});

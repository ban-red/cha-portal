import { createRouter, createWebHistory } from "vue-router";

import { useSession } from "./stores/session";

declare module "vue-router" {
  interface RouteMeta {
    /** Reachable signed out (sign-in, setup). */
    public?: boolean;
    admin?: boolean;
    title?: string;
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
        { path: "", name: "dashboard", component: () => import("./views/DashboardView.vue"), meta: { title: "Environments" } },
        { path: "controllers", name: "controllers", component: () => import("./views/ControllersView.vue"), meta: { title: "Controllers" } },
        { path: "settings/storage", name: "storage", component: () => import("./views/StorageView.vue"), meta: { title: "Storage" } },
        { path: "admin/nodes", name: "nodes", component: () => import("./views/NodesView.vue"), meta: { admin: true, title: "Nodes" } },
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
    { path: "/:rest(.*)*", redirect: "/" },
  ],
});

router.beforeEach(async (to) => {
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

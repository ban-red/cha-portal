import "./style.css";

import { QueryClient, VueQueryPlugin } from "@tanstack/vue-query";
import { createPinia } from "pinia";
import { createApp } from "vue";

import App from "./App.vue";
import { ApiError } from "./api";
import { router } from "./router";
import { initTheme } from "./themes/runtime";
import { initPrefsSync } from "./themes/sync";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Don't retry what the server refused on purpose.
      retry: (count, err) => !(err instanceof ApiError && err.status < 500) && count < 2,
      refetchOnWindowFocus: false,
    },
  },
});

initTheme();

const pinia = createPinia();
const app = createApp(App).use(pinia).use(router).use(VueQueryPlugin, { queryClient });
initPrefsSync();
app.mount("#app");

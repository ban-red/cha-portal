import "./style.css";

import { VueQueryPlugin } from "@tanstack/vue-query";
import { createPinia } from "pinia";
import { createApp } from "vue";

import App from "./App.vue";
import { queryClient } from "./queryClient";
import { router } from "./router";
import { initTheme } from "./themes/runtime";
import { initPrefsSync } from "./themes/sync";

initTheme();

const pinia = createPinia();
const app = createApp(App).use(pinia).use(router).use(VueQueryPlugin, { queryClient });
initPrefsSync();
app.mount("#app");

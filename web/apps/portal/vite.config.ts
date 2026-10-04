import tailwindcss from "@tailwindcss/vite";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vite";

// In development the SPA runs on Vite and talks to a local cha-control
// (the `cha-control` launch config, port 8090) through this proxy, so cookies
// stay same-origin. `ws` lets a dev node agent connect through it too.
export default defineConfig({
  plugins: [vue(), tailwindcss()],
  server: {
    port: Number(process.env.PORT) || 5190,
    proxy: {
      "/api": { target: process.env.CHA_CONTROL ?? "http://localhost:8090", changeOrigin: false, ws: true },
    },
  },
  build: { target: "es2023" },
});

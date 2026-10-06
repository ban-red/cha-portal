import tailwindcss from "@tailwindcss/vite";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vite";

// In development the SPA runs on Vite and talks to a local cha-control
// (the `cha-control` launch config, port 7677) through this proxy, so cookies
// stay same-origin. `ws` lets a dev node agent connect through it too.
export default defineConfig({
  plugins: [vue(), tailwindcss()],
  server: {
    port: Number(process.env.PORT) || 7678,
    proxy: {
      "/api": { target: process.env.CHA_CONTROL ?? "http://localhost:7677", changeOrigin: false, ws: true },
    },
  },
  build: { target: "es2023" },
});

import tailwindcss from "@tailwindcss/vite";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vite";

import { devMcp } from "./dev-mcp/plugin";

// In development the SPA runs on Vite and talks to a local cha-control
// (the `cha-control` launch config, port 7677) through this proxy, so cookies
// stay same-origin. `ws` lets a dev node agent connect through it too.
export default defineConfig({
  // CHA_DEV_MCP=1 serves the harness MCP endpoint per session (dev-mcp/): off by default.
  // CHA_DEV_MCP_LAN=1 also lets other machines on the LAN reach it (it has no login).
  plugins: [vue(), tailwindcss(), devMcp(process.env.CHA_DEV_MCP === "1", process.env.CHA_DEV_MCP_LAN === "1")],
  server: {
    port: Number(process.env.PORT) || 7678,
    // The dev server is reached by LAN names (`bun run dev` listens on 0.0.0.0).
    allowedHosts: true,
    proxy: {
      "/api": { target: process.env.CHA_CONTROL ?? "http://localhost:7677", changeOrigin: false, ws: true },
    },
  },
  build: { target: "es2023" },
});

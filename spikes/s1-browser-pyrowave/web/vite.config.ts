import { defineConfig } from "vite";

export default defineConfig({
  // PORT lets the launcher pick a free port; 5180 otherwise.
  server: { port: Number(process.env.PORT) || 5180, strictPort: true },
  worker: { format: "es" },
});

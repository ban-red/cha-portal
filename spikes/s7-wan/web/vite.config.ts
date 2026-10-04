import { defineConfig } from "vite";

export default defineConfig({
  // PORT lets the launcher pick a free port; 5191 otherwise.
  server: { port: Number(process.env.PORT) || 5191, strictPort: true },
  worker: { format: "es" },
});

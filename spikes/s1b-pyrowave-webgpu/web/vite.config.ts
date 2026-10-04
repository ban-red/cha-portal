import { defineConfig } from "vite";

export default defineConfig({
  // PORT lets the launcher pick a free port; 5181 otherwise.
  server: { port: Number(process.env.PORT) || 5181, strictPort: true },
  // public/clips is a local symlink to encoded test clips (see ../README.md).
  publicDir: "public",
  // The clips are for the dev server only; never copy them into dist/.
  build: { copyPublicDir: false },
});

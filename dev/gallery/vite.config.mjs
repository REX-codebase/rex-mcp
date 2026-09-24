// Dev-only gallery: renders run-view states from fixture snapshots so the
// UI can be reviewed without a backend or model. Not part of the app build.
// OUT=/tmp/gallery npx vite build -c dev/gallery/vite.config.mjs
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwind from "@tailwindcss/vite";
export default defineConfig({ root: fileURLToPath(new URL(".", import.meta.url)), base: "./", plugins: [react(), tailwind()], build: { outDir: process.env.OUT ?? "dist-gallery", emptyOutDir: true } });

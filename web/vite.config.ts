import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Сборка кладётся в web/dist, откуда её вшивает в бинарник rust-embed (crates/pult-server/src/static_files.rs).
// Файлы с хешем в имени — в assets/: сервер отдаёт их с долгим кешем, index.html — без кеша.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  build: {
    outDir: "dist",
    assetsDir: "assets",
    emptyOutDir: true,
    sourcemap: false,
  },
  server: {
    port: 5173,
    // В разработке API отвечает `pult serve` (по умолчанию 127.0.0.1:8080).
    proxy: {
      "/api": "http://127.0.0.1:8080",
      "/health": "http://127.0.0.1:8080",
    },
  },
});

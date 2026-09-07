import { defineConfig } from "vite";

export default defineConfig({
  base: "/",
  server: {
    proxy: {
      "/api": "http://127.0.0.1:18791",
    },
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    cssCodeSplit: false,
    rollupOptions: {
      output: {
        entryFileNames: "assets/app.js",
        chunkFileNames: "assets/[name].js",
        assetFileNames: "assets/app[extname]",
      },
    },
  },
});

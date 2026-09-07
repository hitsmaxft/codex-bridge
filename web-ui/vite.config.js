import { createHash } from "node:crypto";
import { defineConfig } from "vite";

function fixedAssetCacheBuster() {
  return {
    name: "fixed-asset-cache-buster",
    enforce: "post",
    transformIndexHtml: {
      order: "post",
      handler(html, context) {
        if (!context.bundle) return html;
        for (const fileName of ["assets/app.js", "assets/app.css"]) {
          const asset = context.bundle[fileName];
          if (!asset) continue;
          const content = asset.type === "asset" ? asset.source : asset.code;
          const version = createHash("sha256").update(content).digest("hex").slice(0, 12);
          html = html.replace(`/${fileName}`, `/${fileName}?v=${version}`);
        }
        return html;
      },
    },
  };
}

export default defineConfig({
  plugins: [fixedAssetCacheBuster()],
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

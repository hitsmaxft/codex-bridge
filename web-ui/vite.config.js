import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { defineConfig } from "vite";

const demoBuild = process.env.VITE_CODEX_BRIDGE_DEMO === "1";
const demoWasmPath = new URL(
  "../target/wasm32-unknown-unknown/release/codex_bridge_demo.wasm",
  import.meta.url,
);

function demoWasmBuildAsset() {
  return {
    name: "demo-wasm-build-asset",
    apply: "build",
    async buildStart() {
      if (!demoBuild) return;
      this.emitFile({
        type: "asset",
        fileName: "codex_bridge_demo.wasm",
        source: await readFile(demoWasmPath),
      });
    },
  };
}

function demoWasmDevAsset() {
  return {
    name: "demo-wasm-dev-asset",
    apply: "serve",
    configureServer(server) {
      if (!demoBuild) return;
      server.middlewares.use(async (request, response, next) => {
        if (request.url?.split("?", 1)[0] !== "/codex_bridge_demo.wasm") return next();
        try {
          response.statusCode = 200;
          response.setHeader("Content-Type", "application/wasm");
          response.setHeader("Cache-Control", "no-store");
          response.end(await readFile(demoWasmPath));
        } catch (error) {
          next(error);
        }
      });
    },
  };
}

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
  plugins: [demoWasmBuildAsset(), demoWasmDevAsset(), fixedAssetCacheBuster()],
  base: "/",
  publicDir: false,
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
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: "assets/app[extname]",
      },
    },
  },
});

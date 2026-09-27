import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

const demoDir = path.resolve(process.argv[2] || "_site/demo");
const siteDir = path.dirname(demoDir);
const imageDir = path.join(demoDir, "images");
const chrome =
  process.env.CHROME_BIN ||
  (process.platform === "darwin"
    ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    : "google-chrome");

const sceneScript = `<script>
addEventListener("load", () => {
  const scene = new URLSearchParams(location.search).get("scene");
  if (!scene || scene === "overview") return;
  const timer = setInterval(() => {
    if (!document.querySelector(".turn-prompt-stack")) return;
    clearInterval(timer);
    document.getElementById("goalHideBtn")?.click();
    if (scene === "steer-stack") {
      document.querySelector(".turn-prompt-stack:last-of-type")?.scrollIntoView({ block: "center" });
    } else if (scene === "tools") {
      document.querySelector(".tool-toggle")?.click();
    } else if (scene === "commands") {
      document.getElementById("slashBtn")?.click();
    }
  }, 50);
});
</script>`;

const scenes = [
  ["overview", "Conversation and goal"],
  ["steer-stack", "Steer cards and token usage"],
  ["tools", "Tools and session statistics"],
  ["commands", "Slash commands and skills"],
];

const gallery = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Web UI demo views</title>
<style>body{margin:0;background:#111713;color:#ecf1ed;font:16px system-ui,sans-serif}main{max-width:1100px;margin:auto;padding:40px 24px 80px}h1{margin:0 0 12px;font-size:clamp(2rem,5vw,3.5rem)}p{color:#aebbb1;line-height:1.5}a{color:#53a9ff}nav{margin:24px 0 40px}.grid{display:grid;gap:28px;grid-template-columns:repeat(auto-fit,minmax(min(100%,460px),1fr))}figure{margin:0;padding:14px;border:1px solid #334037;border-radius:20px;background:#1a211d}img{display:block;width:100%;border-radius:12px}figcaption{padding:14px 4px 2px;font-weight:600}</style></head><body><main><h1>Web UI demo views</h1><p>These screenshots come from the production Web UI and the browser-based demo during the Pages build.</p><nav><a href="./">Try the interactive demo</a> · <a href="../">Project home</a></nav><div class="grid">${scenes.map(([file, title]) => `<figure><a href="images/${file}.png"><img src="images/${file}.png" alt="${title}" loading="lazy"></a><figcaption>${title}</figcaption></figure>`).join("")}</div></main></body></html>`;

const types = {
  ".html": "text/html",
  ".css": "text/css",
  ".js": "text/javascript",
  ".wasm": "application/wasm",
  ".png": "image/png",
};
const server = createServer(async (request, response) => {
  try {
    const url = new URL(request.url, "http://localhost");
    const pathname = decodeURIComponent(url.pathname).replace(/^\/codex-bridge(?=\/demo\/)/, "");
    const file = path.resolve(siteDir, `.${pathname === "/" ? "/index.html" : pathname}`);
    if (!file.startsWith(`${siteDir}${path.sep}`)) throw new Error("Invalid path");
    const actual = (await stat(file)).isDirectory() ? path.join(file, "index.html") : file;
    response.setHeader("Content-Type", types[path.extname(actual)] || "application/octet-stream");
    const data = await readFile(actual);
    response.end(
      actual === path.join(demoDir, "index.html") && url.searchParams.has("scene")
        ? data.toString().replace("</body>", `${sceneScript}</body>`)
        : data,
    );
  } catch {
    response.writeHead(404).end();
  }
});

async function screenshot(url, filename, width, height) {
  const profile = await mkdtemp(path.join(tmpdir(), "codex-demo-chrome-"));
  const output = path.join(imageDir, filename);
  await rm(output, { force: true });
  const child = spawn(
    chrome,
    [
      "--headless=new",
      "--disable-gpu",
      "--no-first-run",
      "--disable-extensions",
      "--disable-background-networking",
      "--hide-scrollbars",
      `--user-data-dir=${profile}`,
      `--window-size=${width},${height}`,
      "--force-device-scale-factor=2",
      "--virtual-time-budget=3500",
      `--screenshot=${output}`,
      url,
    ],
    { stdio: "ignore" },
  );
  try {
    let ready = false;
    for (let attempt = 0; attempt < 200; attempt++) {
      await new Promise((resolve) => setTimeout(resolve, 100));
      if ((await stat(output).catch(() => null))?.size > 1000) {
        ready = true;
        break;
      }
      if (child.exitCode !== null) break;
    }
    if (!ready) throw new Error(`Chrome did not render ${filename}`);
    // Chrome normally writes only core PNG chunks. Reject metadata-bearing images.
    const data = await readFile(output);
    let offset = 8;
    while (offset + 12 <= data.length) {
      const length = data.readUInt32BE(offset);
      const type = data.toString("ascii", offset + 4, offset + 8);
      if (!["IHDR", "PLTE", "tRNS", "IDAT", "IEND"].includes(type))
        throw new Error(`${filename} contains PNG metadata chunk ${type}`);
      offset += length + 12;
    }
    if (offset !== data.length) throw new Error(`${filename} has invalid PNG data`);
    process.stdout.write(`${output}\n`);
  } finally {
    child.kill("SIGTERM");
    await new Promise((resolve) => setTimeout(resolve, 300));
    if (child.exitCode === null) child.kill("SIGKILL");
    await rm(profile, { recursive: true, force: true });
  }
}

await mkdir(imageDir, { recursive: true });
try {
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  for (const [scene] of scenes)
    await screenshot(`${base}/codex-bridge/demo/?scene=${scene}`, `${scene}.png`, 960, 640);
  await writeFile(path.join(demoDir, "gallery.html"), gallery);
} finally {
  server.close();
}

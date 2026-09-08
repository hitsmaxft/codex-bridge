import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { shouldOfferStop } from "../src/composer-state.js";
import { createAuthenticationGate } from "../src/auth-gate.js";
import { demoCommandWithInstance } from "../src/demo-client.js";
import { localFilePath } from "../src/markdown.js";
import { SessionMessageCache } from "../src/message-cache.js";
import {
  LAST_SESSION_STORAGE_KEY,
  rememberSessionId,
  sessionHash,
  sessionIdFromHash,
  storedSessionId,
} from "../src/session-route.js";

const wasmPath = new URL(
  "../../target/wasm32-unknown-unknown/release/codex_bridge_demo.wasm",
  import.meta.url,
);
const stylesheetPath = new URL("../src/styles.css", import.meta.url);
const mainScriptPath = new URL("../src/main.js", import.meta.url);

async function demoClient() {
  const { instance } = await WebAssembly.instantiate(await readFile(wasmPath), {});
  return (request) => demoCommandWithInstance(instance, request);
}

function result(response) {
  assert.equal(response.ok, true, JSON.stringify(response));
  return response.result;
}

test("compiled demo WASM supports refresh and active-run interruption", async () => {
  const command = await demoClient();
  assert.equal(result(command({ command: "status" })).demo, true);

  const before = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  );
  assert.equal(
    result(
      command({
        command: "send",
        thread_id: "demo-thread-web-ui",
        text: "WASM regression",
      }),
    ).status,
    "queued",
  );
  for (let poll = 0; poll < 3; poll += 1) result(command({ command: "pending_messages" }));

  const active = result(command({ command: "thread_activity", thread_id: "demo-thread-web-ui" }));
  assert.equal(active.activity.active_turn_id, "demo-turn-processing");
  assert.equal(
    shouldOfferStop({
      activeTurnId: active.activity.active_turn_id,
      inputFocused: false,
      submitting: false,
      interrupting: false,
    }),
    true,
  );
  assert.equal(
    shouldOfferStop({
      activeTurnId: active.activity.active_turn_id,
      inputFocused: true,
      submitting: false,
      interrupting: false,
    }),
    false,
  );
  const refreshed = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  );
  assert.equal(refreshed.page.total, before.page.total + 1);
  assert.equal(refreshed.messages.at(-1).content[0].text, "WASM regression");

  assert.equal(
    result(command({ command: "interrupt", thread_id: "demo-thread-web-ui" })).status,
    "interrupted",
  );
  const inactive = result(command({ command: "thread_activity", thread_id: "demo-thread-web-ui" }));
  assert.equal(inactive.activity.active_turn_id, null);
  assert.equal(
    shouldOfferStop({
      activeTurnId: inactive.activity.active_turn_id,
      inputFocused: false,
      submitting: false,
      interrupting: false,
    }),
    false,
  );
});

test("session hash routes to the requested demo session", async () => {
  const command = await demoClient();
  const hash = sessionHash("demo-thread-protocol");
  assert.equal(hash, "#session=demo-thread-protocol");
  const threadId = sessionIdFromHash(hash);
  assert.equal(threadId, "demo-thread-protocol");
  const page = result(command({ command: "messages", thread_id: threadId, limit: 1 }));
  assert.equal(page.thread.id, threadId);
  assert.equal(sessionIdFromHash("#demo-thread-protocol"), threadId);
  assert.equal(sessionIdFromHash("#unrelated=value"), null);
});

test("an empty hash restores the last opened session", () => {
  const values = new Map();
  const storage = {
    getItem: (key) => values.get(key) || null,
    setItem: (key, value) => values.set(key, value),
  };
  assert.equal(sessionIdFromHash(""), null);
  assert.equal(storedSessionId(storage), null);
  rememberSessionId(storage, "demo-thread-protocol");
  assert.equal(values.get(LAST_SESSION_STORAGE_KEY), "demo-thread-protocol");
  assert.equal(storedSessionId(storage), "demo-thread-protocol");
});

test("file diffs define distinct light and dark theme palettes", async () => {
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(stylesheet, /:root\s*\{[^}]*--diff-surface:\s*#191c19/s);
  assert.match(stylesheet, /html\[data-theme="light"\]\s*\{[^}]*--diff-surface:\s*#fff/s);
  assert.match(stylesheet, /\.diff-line\.add\s*\{[^}]*var\(--diff-add-bg\)/s);
  assert.match(stylesheet, /\.diff-line\.delete\s*\{[^}]*var\(--diff-delete-bg\)/s);
  assert.match(
    stylesheet,
    /\.composer-shell:focus-within #submitBtn,[\s\S]*?justify-self:\s*end;[\s\S]*?width:\s*44px;/,
  );
});

test("mobile composer stays out of the message grid sizing flow", async () => {
  const stylesheet = await readFile(stylesheetPath, "utf8");
  const source = await readFile(mainScriptPath, "utf8");
  const mobile = stylesheet.slice(stylesheet.indexOf("@media (max-width: 800px)"));
  assert.match(mobile, /\.composer\s*\{[^}]*position:\s*fixed;/s);
  assert.doesNotMatch(mobile, /#messages\s*\{[^}]*grid-row:\s*2;/s);
  assert.doesNotMatch(mobile, /\.composer\s*\{[^}]*grid-row:\s*2;/s);
  assert.doesNotMatch(source, /textarea\.blur\(\)/);
  assert.match(source, /syncOutboxCompactLabel/);
  assert.match(stylesheet, /\.outbox-tray\.compact \.outbox-item:not\(:last-child\)/);
  assert.match(stylesheet, /\.outbox-tray\.compact > \.outbox-item:last-child/);
  assert.match(stylesheet, /\.outbox-tray\.compact \.outbox-actions/);
});

test("structured command actions stay separate and preserve multiline commands", async () => {
  const command = await demoClient();
  const page = result(command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }));
  const summary = page.messages[1].tools[0];
  assert.equal(summary.command_action_count, 2);
  assert.equal(summary.command_actions_parallel, false);
  const detail = result(
    command({
      command: "tool_content",
      thread_id: "demo-thread-web-ui",
      message_index: 1,
      tool_index: 0,
    }),
  );
  assert.equal(detail.display_input.commandActions.length, 2);
  assert.match(detail.display_input.commandActions[0].command, /\n  -p codex-bridge-demo$/);
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /function appendCommandActions/);
  assert.match(source, /action\?\.command \|\| toolValueText\(action\)/);
});

test("local task file links resolve to workspace paths", () => {
  assert.equal(
    localFilePath("/Users/bhe/project/firmware image.elf"),
    "/Users/bhe/project/firmware image.elf",
  );
  assert.equal(
    localFilePath("file:///Users/bhe/project/firmware.elf"),
    "/Users/bhe/project/firmware.elf",
  );
  assert.equal(localFilePath("https://example.com/firmware.elf"), null);
});

test("authentication gate serializes concurrent startup requests", async () => {
  let probes = 0;
  let release;
  const gate = createAuthenticationGate(
    () =>
      new Promise((resolve) => {
        probes += 1;
        release = resolve;
      }),
  );
  const first = gate.wait();
  const second = gate.wait();
  await Promise.resolve();
  assert.equal(probes, 1);
  release();
  await Promise.all([first, second]);
  await gate.wait();
  assert.equal(probes, 1);
});

test("demo session rename updates subsequent thread reads", async () => {
  const command = await demoClient();
  assert.equal(
    result(
      command({
        command: "thread_rename",
        thread_id: "demo-thread-web-ui",
        name: "Renamed demo session",
      }),
    ).status,
    "renamed",
  );
  const page = result(command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 1 }));
  assert.equal(page.thread.title, "Renamed demo session");
});

test("message cache keeps three recently used sessions", () => {
  const cache = new SessionMessageCache(3);
  cache.set("one", { page: 1 });
  cache.set("two", { page: 2 });
  cache.set("three", { page: 3 });
  assert.equal(cache.get("one").page, 1);
  cache.set("four", { page: 4 });
  assert.equal(cache.get("two"), null);
  assert.equal(cache.get("one").page, 1);
  assert.equal(cache.get("three").page, 3);
  assert.equal(cache.get("four").page, 4);
});

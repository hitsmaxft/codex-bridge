import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import { shouldOfferStop } from "../src/composer-state.js";
import { demoCommandWithInstance } from "../src/demo-client.js";
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
});

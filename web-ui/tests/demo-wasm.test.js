import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  effectiveActiveTurnId,
  restoreComposerDraft,
  shouldOfferStop,
} from "../src/composer-state.js";
import {
  BROWSER_NOTIFICATIONS_STORAGE_KEY,
  browserNotificationState,
  disableBrowserNotifications,
  enableBrowserNotifications,
  shouldShowBrowserNotification,
  showBrowserNotification,
} from "../src/browser-notifications.js";
import { createAuthenticationGate } from "../src/auth-gate.js";
import { demoCommandWithInstance } from "../src/demo-client.js";
import { localFilePath } from "../src/markdown.js";
import { SessionMessageCache } from "../src/message-cache.js";
import { runtimeArchitectureModel } from "../src/runtime-architecture.js";
import {
  EXPANDED_PROJECTS_STORAGE_KEY,
  persistExpandedProjects,
  storedExpandedProjects,
} from "../src/project-state.js";
import { taskOverview } from "../src/task-overview.js";
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
const indexPath = new URL("../index.html", import.meta.url);

async function demoClient() {
  const { instance } = await WebAssembly.instantiate(await readFile(wasmPath), {});
  return (request) => demoCommandWithInstance(instance, request);
}

function result(response) {
  assert.equal(response.ok, true, JSON.stringify(response));
  return response.result;
}

test("runtime architecture follows managed and selected voice backends", () => {
  const native = runtimeArchitectureModel({
    managedServices: {
      app_server: { enabled: true, status: { running: true } },
      desktop_interposition: { enabled: false, status: { running: false } },
      whisper: { enabled: true, fallback: true, needed: false, status: { running: false } },
    },
    directAppServer: true,
    audioTranscription: { enabled: true, backend: "app_server_realtime" },
  });
  assert.equal(native.appServer.phase, "running");
  assert.equal(native.whisper.phase, "standby");
  assert.equal(native.voice.nameKey, "appServerRealtime");

  const fallback = runtimeArchitectureModel({
    managedServices: {
      app_server: { enabled: false, status: { running: false } },
      whisper: { enabled: true, fallback: true, needed: true, status: { running: true } },
    },
    directAppServer: true,
    audioTranscription: { enabled: true, backend: "whisper_cpp" },
  });
  assert.equal(fallback.appServer.phase, "running");
  assert.equal(fallback.appServer.labelKey, "componentExternal");
  assert.equal(fallback.voice.phase, "running");
  assert.equal(fallback.voice.nameKey, "whisperBackend");
});

test("authoritative idle suppresses a stale rollout turn", () => {
  assert.equal(effectiveActiveTurnId("stale-rollout-turn", false), null);
  assert.equal(effectiveActiveTurnId("live-turn", true), "live-turn");
  assert.equal(effectiveActiveTurnId("compatibility-turn", undefined), "compatibility-turn");
});

test("same-thread refresh does not rewrite the composer or move its caret", () => {
  let value = "alpha beta",
    writes = 0;
  const textarea = {
    selectionStart: 5,
    get value() {
      return value;
    },
    set value(next) {
      writes += 1;
      value = next;
      this.selectionStart = next.length;
    },
  };
  assert.equal(restoreComposerDraft(textarea, "alpha beta", false), false);
  assert.equal(writes, 0);
  assert.equal(textarea.selectionStart, 5);
  assert.equal(restoreComposerDraft(textarea, "next thread", true), true);
  assert.equal(writes, 1);
  assert.equal(value, "next thread");
});

test("browser notifications require permission, preference, and a background page", async () => {
  const values = new Map(),
    storage = {
      getItem: (key) => values.get(key) || null,
      setItem: (key, value) => values.set(key, value),
      removeItem: (key) => values.delete(key),
    },
    notifications = [];
  class FakeNotification {
    static permission = "default";
    static async requestPermission() {
      FakeNotification.permission = "granted";
      return "granted";
    }
    constructor(title, options) {
      notifications.push({ title, options, instance: this });
    }
    close() {}
  }
  assert.equal(browserNotificationState(FakeNotification, storage).enabled, false);
  assert.equal((await enableBrowserNotifications(FakeNotification, storage)).enabled, true);
  assert.equal(values.get(BROWSER_NOTIFICATIONS_STORAGE_KEY), "enabled");
  assert.equal(
    shouldShowBrowserNotification({
      NotificationApi: FakeNotification,
      storage,
      documentHidden: false,
      windowFocused: true,
    }),
    false,
  );
  assert.equal(
    shouldShowBrowserNotification({
      NotificationApi: FakeNotification,
      storage,
      documentHidden: true,
      windowFocused: false,
    }),
    true,
  );
  let clicked = false;
  assert.equal(
    showBrowserNotification(FakeNotification, {
      title: "Session title",
      body: "Run completed",
      tag: "codex-bridge:thread-1",
      onClick: () => (clicked = true),
    }),
    true,
  );
  notifications[0].instance.onclick();
  assert.equal(clicked, true);
  assert.equal(disableBrowserNotifications(FakeNotification, storage).enabled, false);
});

test("compiled demo WASM supports refresh and active-run interruption", async () => {
  const command = await demoClient();
  const status = result(command({ command: "status" }));
  assert.equal(status.demo, true);
  assert.equal(status.managed_services.app_server.status.running, true);
  assert.equal(status.managed_services.whisper.fallback, true);
  assert.equal(status.capabilities.audio_transcription.enabled, true);
  assert.equal(status.capabilities.audio_transcription.backend, "demo_wasm");

  const before = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  );
  const completedTurn = before.messages.find((message) => message.id === "demo-assistant-4");
  assert.deepEqual(
    completedTurn.content.slice(-2).map((item) => item.kind),
    ["memory_citation", "turn_usage"],
  );
  assert.equal(completedTurn.content.at(-1).total_tokens, 12480);
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

test("live demo supports queued work, steer handoff, dynamic tools, and voice transcription", async () => {
  const command = await demoClient();
  result(
    command({
      command: "send",
      thread_id: "demo-thread-web-ui",
      text: "Fix the live demo code",
    }),
  );
  for (let poll = 0; poll < 3; poll += 1) result(command({ command: "pending_messages" }));

  const queued = result(
    command({
      command: "send",
      thread_id: "demo-thread-web-ui",
      text: "Summarize this next",
    }),
  );
  result(
    command({
      command: "steer",
      thread_id: "demo-thread-web-ui",
      text: "Keep the current answer concise",
    }),
  );
  result(command({ command: "pending_messages" }));
  const pending = result(command({ command: "pending_messages" }));
  assert.equal(pending.messages.length, 1);
  assert.equal(pending.messages[0].action, "queue");

  result(command({ command: "thread_activity", thread_id: "demo-thread-web-ui" }));
  result(command({ command: "thread_activity", thread_id: "demo-thread-web-ui" }));
  result(command({ command: "thread_activity", thread_id: "demo-thread-web-ui" }));
  const progress = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  );
  const toolMessage = progress.messages.findLast((message) => message.tools?.length);
  assert.equal(toolMessage.tools[0].name, "apply_patch");
  const detail = result(
    command({
      command: "tool_content",
      thread_id: "demo-thread-web-ui",
      message_index: toolMessage.message_index,
      tool_index: 0,
    }),
  );
  assert.equal(detail.tool.name, "apply_patch");
  assert.match(detail.display_input.title, /live demo state machine/i);

  const withdrawn = result(
    command({
      command: "pending_message_delete",
      thread_id: "demo-thread-web-ui",
      id: queued.pending_id,
    }),
  );
  assert.equal(withdrawn.queue_deleted, true);
  result(command({ command: "interrupt", thread_id: "demo-thread-web-ui" }));

  const beforeTranscription = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  ).messages.length;
  const transcription = result(
    command({
      command: "audio_transcribe",
      thread_id: "demo-thread-web-ui",
      audio: { data: "AAAAAA==", sample_rate: 24000, num_channels: 1, samples_per_channel: 2 },
    }),
  );
  assert.match(transcription.text, /summarize/i);
  assert.equal(
    result(command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 })).messages
      .length,
    beforeTranscription,
  );
  result(
    command({
      command: "send",
      thread_id: "demo-thread-web-ui",
      text: transcription.text,
    }),
  );
  for (let poll = 0; poll < 3; poll += 1) result(command({ command: "pending_messages" }));
  for (let poll = 0; poll < 48; poll += 1) {
    const activity = result(
      command({ command: "thread_activity", thread_id: "demo-thread-web-ui" }),
    );
    if (!activity.activity.active_turn_id) break;
  }
  const completed = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  );
  const transcribedMessage = completed.messages.findLast((message) => message.role === "user");
  assert.match(transcribedMessage.content[0].text, /summarize/i);
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

test("expanded project folders persist as a bounded browser preference", () => {
  const values = new Map(),
    storage = {
      getItem: (key) => values.get(key) ?? null,
      setItem: (key, value) => values.set(key, value),
    };
  assert.equal(storedExpandedProjects(storage), null);
  persistExpandedProjects(storage, new Set(["/workspace/one", "/workspace/two"]));
  assert.deepEqual([...storedExpandedProjects(storage)], ["/workspace/one", "/workspace/two"]);
  assert.equal(values.has(EXPANDED_PROJECTS_STORAGE_KEY), true);
});

test("file diffs define distinct light and dark theme palettes", async () => {
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(stylesheet, /:root\s*\{[^}]*--diff-surface:\s*#191c19/s);
  assert.match(stylesheet, /html\[data-theme="light"\]\s*\{[^}]*--diff-surface:\s*#fff/s);
  assert.match(stylesheet, /\.diff-line\.add\s*\{[^}]*var\(--diff-add-bg\)/s);
  assert.match(stylesheet, /\.diff-line\.delete\s*\{[^}]*var\(--diff-delete-bg\)/s);
  assert.match(stylesheet, /\.composer-shell\s*\{[^}]*"input input input input"/s);
});

test("session run snapshots preserve active, completed, and failed indicators", async () => {
  const stylesheet = await readFile(stylesheetPath, "utf8");
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /event\.thread_states/);
  assert.match(source, /completedTurnRunState\(params\.turn\)/);
  assert.match(source, /thread-run-state \$\{runState\}/);
  assert.match(stylesheet, /\.thread-run-state\.active\s*\{/);
  assert.match(stylesheet, /\.thread-run-state\.completed::before\s*\{[^}]*content:\s*"✓"/s);
  assert.match(stylesheet, /\.thread-run-state\.cancelled,[\s\S]*?background:\s*var\(--danger\)/);
});

test("task overview uses the demo server's latest input, tool event, and final response", async () => {
  const command = await demoClient();
  const messages = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 20 }),
  );
  const activity = result(command({ command: "thread_activity", thread_id: "demo-thread-web-ui" }));
  const overview = taskOverview(messages, activity);
  assert.match(overview.userText, /Can visitors try submitting/);
  assert.match(overview.assistantText, /WASM state machine/);
  assert.equal(overview.latestTool.name, "web_search");
  assert.equal(overview.thread.id, "demo-thread-web-ui");

  const index = await readFile(indexPath, "utf8");
  const source = await readFile(mainScriptPath, "utf8");
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(index, /id="tasksBtn"/);
  assert.match(index, /id="tasksDialog"[\s\S]*?id="tasksList"/);
  assert.match(source, /limit: 20/);
  assert.match(source, /setInterval\(\(\) => refreshTaskOverviews\(\)/);
  assert.match(source, /message \$\{className\} task-message/);
  assert.doesNotMatch(source, /task-message-label/);
  assert.match(source, /text\.length > 360 \|\| text\.split\("\\n"\)\.length > 8/);
  assert.match(stylesheet, /\.task-message\.collapsible:not\(\.expanded\) \.message-body/);
  const taskEntryStyle = stylesheet.match(/\.task-entry\s*\{[^}]*\}/s)?.[0] || "";
  assert.doesNotMatch(taskEntryStyle, /border|background/);
});

test("mobile composer stays out of the message grid sizing flow", async () => {
  const index = await readFile(indexPath, "utf8");
  const stylesheet = await readFile(stylesheetPath, "utf8");
  const source = await readFile(mainScriptPath, "utf8");
  const mobile = stylesheet.slice(stylesheet.indexOf("@media (max-width: 800px)"));
  assert.match(index, /interactive-widget=resizes-content/);
  assert.match(mobile, /\.composer\s*\{[^}]*position:\s*fixed;/s);
  assert.doesNotMatch(mobile, /#messages\s*\{[^}]*grid-row:\s*2;/s);
  assert.doesNotMatch(mobile, /\.composer\s*\{[^}]*grid-row:\s*2;/s);
  assert.doesNotMatch(source, /textarea\.blur\(\)/);
  assert.match(source, /syncOutboxCompactLabel/);
  assert.doesNotMatch(mobile, /\.composer-shell[^\{]*\{[^}]*grid-template-areas:/s);
  assert.match(
    mobile,
    /\.composer-shell\s*\{[^}]*grid-template-columns:\s*64px 72px minmax\(0, 1fr\) 64px;/s,
  );
  assert.match(mobile, /\.composer-shell #submitBtn\s*\{[^}]*width:\s*64px;/s);
  assert.match(mobile, /\.composer\s*\{[^}]*pointer-events:\s*none;/s);
  assert.match(mobile, /\.composer-shell\s*\{[^}]*pointer-events:\s*none;/s);
  assert.match(mobile, /\.composer-shell textarea,[\s\S]*?touch-action:\s*manipulation;/s);
  assert.match(
    mobile,
    /\.composer-shell textarea\s*\{[^}]*-webkit-appearance:\s*none;[^}]*padding:\s*12px 5px;[^}]*font-family:\s*-apple-system,[^}]*font-size:\s*16px;[^}]*line-height:\s*20px;[^}]*zoom:\s*1;/s,
  );
  assert.match(
    mobile,
    /\.composer,[\s\S]*?\.composer-shell textarea\s*\{[^}]*transform:\s*none;[^}]*filter:\s*none;[^}]*perspective:\s*none;[^}]*will-change:\s*auto;/s,
  );
  assert.match(source, /function resizeComposerAfterViewportChange\(\)/);
  assert.match(
    source,
    /document\.activeElement !== \$\("messageText"\)\) resizeComposerTextarea\(\)/,
  );
  assert.doesNotMatch(
    source,
    /visualViewport\?\.addEventListener\("resize", resizeComposerTextarea/,
  );
  assert.doesNotMatch(stylesheet, /\.outbox-tray\.compact \.outbox-item:not\(:last-child\)/);
  assert.match(stylesheet, /\.outbox-tray\.compact > \.outbox-item\s*\{/);
  assert.match(stylesheet, /\.outbox-tray\.compact \.outbox-actions/);
});

test("steer messages use a distinct bean-green outbox palette", async () => {
  const stylesheet = await readFile(stylesheetPath, "utf8");
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /entry\.action === "steer" \? " steer" : ""/);
  assert.match(stylesheet, /--steer-bg:\s*#263d2d/);
  assert.match(stylesheet, /html\[data-theme="light"\][\s\S]*--steer-bg:\s*#deeddd/);
  assert.match(stylesheet, /\.outbox-item\.steer \.message-body\s*\{[^}]*var\(--steer-bg\)/s);
});

test("mobile right swipe opens Tasks without opening the session drawer", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const gesture = source.match(
    /bindSwipe\(document\.querySelector\("main"\), 1, \(\) => \{[\s\S]*?\n\}\);/,
  )?.[0];
  assert.ok(gesture);
  assert.match(gesture, /openTasksDialog\(\)/);
  assert.doesNotMatch(gesture, /sidebar"\)\.classList\.add\("open"\)/);
});

test("composer keeps images as attachments and transcribes voice into editable text", async () => {
  const index = await readFile(indexPath, "utf8");
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(
    index,
    /id="imageInput"[\s\S]*?accept="image\/jpeg,image\/png,image\/webp,image\/gif"/,
  );
  assert.match(index, /id="audioInput"[^>]*accept="audio\/\*"[^>]*capture/);
  assert.match(source, /attachments = state\.composerAttachments\.map/);
  assert.match(source, /command\(\{ command: name, thread_id: threadId, text, attachments \}\)/);
  assert.match(source, /new MediaRecorder/);
  assert.match(source, /async function pcmAudio\(blob\)/);
  assert.match(source, /command: "audio_transcribe"/);
  assert.match(source, /insertTranscription\(result\.text \|\| ""\)/);
  assert.match(source, /state\.serverCapabilities = event\.capabilities/);
  assert.match(source, /method === "account\/updated"/);
  assert.match(source, /button\.classList\.toggle\("unavailable", unavailable\)/);
  assert.doesNotMatch(source, /type: "audio",\s*url/);
  assert.match(source, /if \(item\.content\) \{[\s\S]*?appendContextValue/);
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(
    stylesheet,
    /\.composer-actions button\.unavailable::after\s*\{[^}]*content:\s*"×"/s,
  );
});

test("expanded folders preload summaries and mobile buttons keep native click delivery", async () => {
  const command = await demoClient();
  const projects = result(command({ command: "projects", include_archived: false })).projects;
  const chats = projects.find((project) => project.kind === "chats");
  assert.ok(chats);
  assert.equal(chats.path, "codex-bridge://chats");
  assert.equal(
    result(
      command({
        command: "project_threads",
        project_path: chats.path,
        include_archived: false,
      }),
    ).threads[0].id,
    "demo-thread-protocol",
  );
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /storedExpandedProjects\(window\.localStorage\)/);
  assert.match(source, /preloadExpandedProjectThreads\(\)/);
  assert.match(source, /Promise\.allSettled\(Array\.from\(\{ length: Math\.min\(3,/);
  assert.match(source, /\$\("sendModeToggle"\)\.onclick = toggleSendModeAndKeepFocus/);
  const pointerHandler = source.match(
    /\$\("submitBtn"\)\.onpointerdown = \(event\) => \{[\s\S]*?\n\};/,
  )?.[0];
  assert.ok(pointerHandler);
  assert.doesNotMatch(pointerHandler, /preventDefault/);
  assert.doesNotMatch(source, /composerShell\.addEventListener\(\s*"pointerdown"/);
  assert.match(source, /p\.kind === "chats" \? tr\("chats"\) : p\.name/);
  assert.match(source, /if \(p\.kind !== "chats"\)/);
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
    localFilePath("/Users/example/project/firmware image.elf"),
    "/Users/example/project/firmware image.elf",
  );
  assert.equal(
    localFilePath("file:///Users/example/project/firmware.elf"),
    "/Users/example/project/firmware.elf",
  );
  assert.equal(
    localFilePath("</Users/example/project/release.zip>"),
    "/Users/example/project/release.zip",
  );
  assert.equal(
    localFilePath("%3C%2FUsers%2Fexample%2Fproject%2Frelease.zip%3E"),
    "/Users/example/project/release.zip",
  );
  assert.equal(
    localFilePath("<file:///Users/example/project/release%20build.zip>"),
    "/Users/example/project/release build.zip",
  );
  assert.equal(
    localFilePath("https://bridge.example/%3C%2FUsers%2Fexample%2Fproject%2Frelease.zip%3E"),
    "/Users/example/project/release.zip",
  );
  assert.equal(
    localFilePath("https://bridge.example/%3C/Users/example/project/release.zip%3E"),
    "/Users/example/project/release.zip",
  );
  assert.equal(localFilePath("/Users/example/project/100%.zip"), "/Users/example/project/100%.zip");
  assert.equal(localFilePath("https://example.com/firmware.elf"), null);
});

test("interrupt requests carry the app-server turn observed by the UI", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /targetRequest\("interrupt", \{ turn_id: state\.activeTurnId \}\)/);
});

test("message refresh preserves stable nodes and viewport anchors", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /root\.insertBefore\(node, cursor\)/);
  assert.match(source, /anchorMessageIndex/);
  assert.match(source, /smoothBottom/);
  const reconcile = source.slice(
    source.indexOf("function reconcileMessageNodes"),
    source.indexOf("function pendingNode"),
  );
  assert.doesNotMatch(reconcile, /replaceChildren/);
});

test("completed turns collapse by server turn id and preserve the full expansion", async () => {
  const command = await demoClient();
  const page = result(command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }));
  assert.equal(page.messages[0].turn_id, "demo-turn-seed-1");
  assert.equal(page.messages[1].turn_id, "demo-turn-seed-1");
  assert.equal(page.messages.at(-1).turn_id, "demo-turn-seed-2");

  const source = await readFile(mainScriptPath, "utf8");
  const layout = source.slice(
    source.indexOf("function groupedTurns"),
    source.indexOf("function messageNode"),
  );
  assert.match(layout, /message\.turn_id/);
  assert.match(layout, /state\.expandedTurnIds/);
  assert.match(layout, /\.\.\.leading,[\s\S]*fold,[\s\S]*divider,[\s\S]*finalNode/);
  assert.match(layout, /turnSummaryText\(group\)/);
});

test("queued messages expose withdraw and convert-to-steer actions", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /function convertPendingToSteer/);
  assert.match(source, /command: "pending_message_delete"/);
  assert.match(source, /command: "steer"/);
  assert.match(source, /className = "outbox-menu"/);
  assert.match(source, /openMenus/);
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

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

import {
  completionMatchesActiveTurn,
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
import { goalToggleState } from "../src/goal-state.js";
import { localFilePath, localFileReference } from "../src/markdown.js";
import { memoryCitationModel } from "../src/memory-citations.js";
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
import { scrollTopForViewportAnchor } from "../src/viewport-state.js";

const wasmPath = new URL(
  "../../target/wasm32-unknown-unknown/release/codex_bridge_demo.wasm",
  import.meta.url,
);
const stylesheetPath = new URL("../src/styles.css", import.meta.url);
const mainScriptPath = new URL("../src/main.js", import.meta.url);
const apiScriptPath = new URL("../src/api.js", import.meta.url);
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

  const browserLimited = runtimeArchitectureModel({
    managedServices: {
      whisper: { enabled: true, fallback: true, needed: true, status: { running: true } },
    },
    audioTranscription: { enabled: true, backend: "whisper_cpp" },
    browserMicrophoneAvailable: false,
  });
  assert.equal(browserLimited.microphone.phase, "failed");
  assert.equal(browserLimited.microphone.labelKey, "browserAudioLimited");
  assert.equal(browserLimited.voice.phase, "running");

  const starting = runtimeArchitectureModel({
    managedServices: {
      whisper: { enabled: true, fallback: true, needed: true, status: { running: true } },
    },
    audioTranscription: { enabled: false, backend: "whisper_cpp", reason: "whisper_starting" },
  });
  assert.equal(starting.whisper.phase, "running");
  assert.equal(starting.voice.phase, "standby");

  const desktopLimited = runtimeArchitectureModel({
    managedServices: {
      desktop_interposition: {
        enabled: true,
        capability: "resume_compatibility_only",
        status: { running: true },
      },
    },
  });
  assert.equal(desktopLimited.wsBridge.phase, "standby");
  assert.equal(desktopLimited.wsBridge.labelKey, "componentLimited");
});

test("managed app-server details expose restart and live transition notifications", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const translations = await readFile(new URL("../src/i18n.js", import.meta.url), "utf8");
  assert.match(source, /key === "app-server" && enabled/);
  assert.match(source, /command\(\{ command: "managed_app_server_restart" \}, false\)/);
  assert.match(source, /observeAppServerService\(event\.managed_services\?\.app_server\)/);
  assert.match(source, /showBrowserNotification\(globalThis\.Notification/);
  assert.match(translations, /restartAppServerConfirm/);
  assert.match(translations, /appServerRecovered/);
});

test("authoritative idle suppresses a stale rollout turn", () => {
  assert.equal(effectiveActiveTurnId("stale-rollout-turn", false), null);
  assert.equal(effectiveActiveTurnId("live-turn", true), "live-turn");
  assert.equal(effectiveActiveTurnId("compatibility-turn", undefined), "compatibility-turn");
  assert.equal(completionMatchesActiveTurn("old-turn", "old-turn"), true);
  assert.equal(completionMatchesActiveTurn("new-turn", "old-turn"), false);
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

test("worktree creation uses a background handle and mobile completion has a toast fallback", async () => {
  const command = await demoClient();
  const started = result(
    command({ command: "thread_create_start", project_path: "/demo/codex-app-server-webui" }),
  );
  assert.equal(started.status, "running");
  assert.equal(
    result(command({ command: "thread_create_status", job_id: started.job_id })).status,
    "running",
  );
  assert.equal(
    result(command({ command: "thread_create_status", job_id: started.job_id })).location,
    "worktree",
  );
  const [source, api, stylesheet] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(apiScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
  ]);
  assert.match(source, /command: "thread_create_start"/);
  assert.match(source, /command: "thread_create_status"/);
  assert.match(source, /state\.threadCreationJobs\.set/);
  assert.match(source, /deferredCompletionToast/);
  assert.match(api, /kind === "completion" \? 5200 : 2600/);
  assert.match(stylesheet, /\.composer-status:not\(\[hidden\]\)::before[\s\S]*radial-gradient/);
  assert.match(stylesheet, /\.toast\[data-kind="completion"\]/);
});

test("new session starts above pinned sessions and chooses from project history", async () => {
  const command = await demoClient();
  const projects = result(command({ command: "projects", include_archived: false })).projects;
  const historical = projects.find((project) => project.path.endsWith("/previous-project"));
  assert.ok(historical);
  assert.equal(historical.thread_count, 0);
  const emptyPage = result(
    command({ command: "project_threads", project_path: historical.path, limit: 50 }),
  );
  assert.equal(emptyPage.available, 0);

  const [html, source, stylesheet] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
  ]);
  assert.match(html, /id="createSessionBtn"[\s\S]*id="projects"/);
  assert.match(html, /id="createProjectStep"[\s\S]*id="createProjectSelect"/);
  assert.match(html, /id="createModeStep"[\s\S]*id="createCurrentBtn"/);
  assert.match(source, /function populateCreateProjectSelect/);
  assert.match(source, /function chooseCreateProject/);
  assert.doesNotMatch(source, /className = "project-add"/);
  assert.match(stylesheet, /\.session-create-button\s*\{[^}]*width:\s*100%;/s);
});

test("compiled demo WASM supports refresh and active-run interruption", async () => {
  const command = await demoClient();
  const status = result(command({ command: "status" }));
  assert.equal(status.demo, true);
  assert.equal(status.managed_services.app_server.status.running, true);
  assert.equal(status.managed_services.whisper.fallback, true);
  assert.equal(status.managed_services.whisper.simplify_chinese, false);
  assert.equal(status.managed_services.desktop_interposition.max_frame_bytes, 67_108_864);
  assert.equal(status.capabilities.audio_transcription.enabled, true);
  assert.equal(status.capabilities.audio_transcription.backend, "demo_wasm");
  assert.equal(status.runtime_resources.session_cache.message_capacity, 10);
  assert.ok(status.runtime_resources.memory.peak_rss_bytes > 0);

  const before = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  );
  const completedTurn = before.messages.find((message) => message.id === "demo-assistant-4");
  assert.deepEqual(
    completedTurn.content.slice(-2).map((item) => item.kind),
    ["memory_citation", "turn_usage"],
  );
  assert.equal(completedTurn.content.at(-1).total_tokens, 12480);
  const deferred = before.messages.find((message) => message.id === "demo-assistant-1");
  assert.equal(deferred.deferred, true);
  assert.deepEqual(deferred.content, []);
  assert.deepEqual(deferred.tools, []);
  const hydrated = result(
    command({
      command: "turn_messages",
      thread_id: "demo-thread-web-ui",
      turn_id: "demo-turn-seed-1",
    }),
  );
  assert.equal(hydrated.messages[1].content[0].kind, "text");
  assert.equal(hydrated.messages[1].tools[0].name, "exec_command");
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
    true,
  );
  const refreshed = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  );
  assert.equal(refreshed.page.total, before.page.total + 1);
  assert.equal(refreshed.messages.at(-1).content[0].text, "WASM regression");

  const queuedAfterStop = result(
    command({
      command: "send",
      thread_id: "demo-thread-web-ui",
      text: "Keep me queued after stopping",
    }),
  );

  assert.equal(
    result(command({ command: "interrupt", thread_id: "demo-thread-web-ui" })).status,
    "interrupted",
  );
  const preservedAfterStop = result(command({ command: "pending_messages" }));
  assert.equal(preservedAfterStop.messages.length, 1);
  assert.equal(preservedAfterStop.messages[0].id, queuedAfterStop.pending_id);
  assert.equal(preservedAfterStop.messages[0].status, "queued");
  const continuedAfterStop = result(
    command({
      command: "pending_message_start",
      thread_id: "demo-thread-web-ui",
      id: queuedAfterStop.pending_id,
    }),
  );
  assert.equal(continuedAfterStop.status, "accepted");
  const continuedMessages = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  );
  assert.equal(continuedMessages.messages.at(-1).content[0].text, "Keep me queued after stopping");
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

test("submission ids remain idempotent across queue negotiation and rollout handoff", async () => {
  const command = await demoClient(),
    request = {
      command: "send",
      thread_id: "demo-thread-web-ui",
      text: "Run this exactly once",
      submission_id: "web-demo-idempotent-1",
    };
  const first = result(command(request));
  const negotiating = result(command(request));
  assert.equal(first.pending_id, request.submission_id);
  assert.equal(negotiating.replayed, true);
  assert.equal(result(command({ command: "pending_messages" })).messages[0].status, "queued");
  result(command({ command: "pending_messages" }));
  result(command({ command: "pending_messages" }));
  const applied = result(command(request));
  assert.equal(applied.replayed, true);
  assert.equal(applied.status, "applied");
  const messages = result(
    command({ command: "messages", thread_id: request.thread_id, limit: 30 }),
  ).messages.filter((message) => message.id === request.submission_id);
  assert.equal(messages.length, 1);
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
  assert.match(
    stylesheet,
    /\.thread-run-state\.active\s*\{[^}]*animation:\s*thread-running-breathe 1\.8s ease-in-out infinite;/s,
  );
  assert.match(stylesheet, /@keyframes thread-running-breathe/);
  assert.match(
    stylesheet,
    /@media \(prefers-reduced-motion: reduce\)[\s\S]*?\.thread-run-state\.active\s*\{[^}]*animation:\s*none;/s,
  );
  assert.match(stylesheet, /\.thread-run-state\.completed::before\s*\{[^}]*content:\s*"✓"/s);
  assert.match(stylesheet, /\.thread-run-state\.cancelled,[\s\S]*?background:\s*var\(--danger\)/);
});

test("task overview uses the demo server's latest input and final response", async () => {
  const command = await demoClient();
  const messages = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 20 }),
  );
  const activity = result(command({ command: "thread_activity", thread_id: "demo-thread-web-ui" }));
  const overview = taskOverview(messages, activity);
  assert.match(overview.userText, /Can visitors try submitting/);
  assert.match(overview.assistantText, /WASM state machine/);
  assert.equal(overview.latestTool, null);
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

test("mobile composer stays out of the message grid and catches its own pointer input", async () => {
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
  assert.match(mobile, /\.composer\s*\{[^}]*pointer-events:\s*auto;/s);
  assert.match(mobile, /\.composer-shell\s*\{[^}]*pointer-events:\s*auto;/s);
  assert.match(mobile, /\.composer-shell textarea,[\s\S]*?touch-action:\s*manipulation;/s);
  assert.match(
    mobile,
    /\.composer-shell textarea\s*\{[^}]*-webkit-appearance:\s*none;[^}]*min-height:\s*56px;[^}]*padding:\s*18px 7px;[^}]*font-family:\s*-apple-system,[^}]*font-size:\s*16px;[^}]*line-height:\s*20px;[^}]*zoom:\s*1;/s,
  );
  assert.match(
    mobile,
    /\.composer,[\s\S]*?\.composer-shell textarea\s*\{[^}]*transform:\s*none;[^}]*filter:\s*none;[^}]*perspective:\s*none;[^}]*will-change:\s*auto;/s,
  );
  assert.match(source, /function resizeComposerAfterViewportChange\(\)/);
  assert.match(
    source,
    /document\.activeElement === \$\("messageText"\)\) syncFocusedComposerViewport\(\)/,
  );
  assert.match(mobile, /\.composer\.viewport-anchored\s*\{[^}]*position:\s*absolute;/s);
  assert.match(source, /viewport\?\.pageTop \?\? window\.scrollY/);
  assert.doesNotMatch(
    source,
    /visualViewport\?\.addEventListener\("resize", resizeComposerTextarea/,
  );
  assert.doesNotMatch(stylesheet, /\.outbox-tray\.compact \.outbox-item:not\(:last-child\)/);
  assert.match(stylesheet, /\.outbox-tray\.compact > \.outbox-item\s*\{/);
  assert.match(stylesheet, /\.outbox-tray\.compact \.outbox-actions/);
  assert.match(source, /recordPerformance\("messages_receive"/);
  assert.match(source, /recordPerformance\("messages_render"/);
  assert.match(source, /recordPerformance\("messages_visible"/);
});

test("chat history header toggles a composer-free full-screen reading mode", async () => {
  const [index, stylesheet, source] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
  ]);
  assert.match(index, /id="historyFullscreenBtn"/);
  assert.match(index, /data-i18n-aria-label="enterHistoryFullscreen"/);
  assert.match(index, /aria-pressed="false"/);
  assert.match(stylesheet, /main\.history-fullscreen \.composer\s*\{[^}]*display:\s*none;/s);
  assert.match(stylesheet, /main\.history-fullscreen #messages\s*\{[^}]*padding-bottom:\s*24px;/s);
  assert.match(source, /function setHistoryFullscreen\(fullscreen\)/);
  assert.match(source, /classList\.toggle\("history-fullscreen", fullscreen\)/);
  assert.match(source, /setAttribute\("aria-pressed", String\(fullscreen\)\)/);
  assert.match(source, /else if \(isHistoryFullscreen\(\)\) setHistoryFullscreen\(false\)/);
});

test("browser performance samples use the bounded demo protocol", async () => {
  const command = await demoClient();
  const response = result(
    command({
      command: "client_performance",
      samples: [
        {
          metric: "messages_visible",
          count: 2,
          total_ms: 42,
          max_ms: 30,
          total_bytes: 2048,
        },
      ],
    }),
  );
  assert.equal(response.recorded, true);
  const api = await readFile(new URL("../src/api.js", import.meta.url), "utf8");
  assert.match(api, /window\.setInterval\(\(\) => flushPerformance\(\), 15_000\)/);
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
  assert.match(source, /submission_id: submissionId/);
  assert.match(source, /source: "web_optimistic"/);
  assert.match(source, /new MediaRecorder/);
  assert.match(source, /async function pcmAudio\(blob\)/);
  assert.match(source, /command: "audio_transcribe"/);
  assert.match(source, /insertTranscription\(result\.text \|\| ""\)/);
  assert.match(source, /state\.serverCapabilities = event\.capabilities/);
  assert.match(source, /method === "account\/updated"/);
  assert.match(source, /button\.classList\.toggle\("unavailable", unavailable\)/);
  assert.match(source, /window\.isSecureContext/);
  assert.match(source, /browserMicrophoneAvailable: browserAudioAvailable\(\)/);
  assert.match(source, /state\.runtimeResources = event\.runtime_resources/);
  assert.match(source, /tr\("cacheSummary"/);
  assert.match(source, /session\.message_entries/);
  assert.match(source, /session\.rollout_bytes/);
  assert.match(index, /<details class="runtime-architecture-disclosure">/);
  assert.match(index, /id="resourceStatus"/);
  assert.doesNotMatch(source, /type: "audio",\s*url/);
  assert.match(source, /if \(item\.content\) \{[\s\S]*?appendContextValue/);
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(
    stylesheet,
    /\.composer-actions button\.unavailable::after\s*\{[^}]*content:\s*"×"/s,
  );
});

test("tool output images open in a zoomable viewer", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(source, /function makeInspectableImage\(img\)/);
  assert.match(source, /parent\.appendChild\(makeInspectableImage\(img\)\)/);
  assert.match(source, /function openImageViewer\(source, alt = "", trigger = null\)/);
  assert.match(source, /stage\.onwheel/);
  assert.match(source, /gesture\?\.kind === "pinch"/);
  assert.match(source, /stage\.ondblclick/);
  assert.match(source, /function lockPageZoomForImageViewer\(\)/);
  assert.match(source, /function restorePageZoomAfterImageViewer\(\)/);
  assert.match(source, /content\.push\("maximum-scale=1", "user-scalable=no"\)/);
  assert.match(source, /\["gesturestart", "gesturechange", "gestureend"\]/);
  assert.match(source, /if \(event\.ctrlKey\) event\.preventDefault\(\)/);
  assert.match(source, /gesture\.pointerType !== "mouse"/);
  assert.match(source, /imageViewer\.suppressDoubleClickUntil = now \+ 500/);
  assert.match(source, /stage\.onlostpointercapture = endPointer/);
  assert.match(source, /restorePageZoomAfterImageViewer\(\)/);
  assert.match(stylesheet, /\.image-viewer\s*\{[^}]*touch-action:\s*none;/s);
  assert.match(stylesheet, /\.image-viewer-stage\s*\{[^}]*touch-action:\s*none;/s);
  assert.match(stylesheet, /\.inspectable-image\s*\{[^}]*cursor:\s*zoom-in;/s);
  assert.match(source, /group\.open = hasImage/);
  assert.match(source, /detail\.open = Boolean\(tool\.has_image\)/);
  assert.match(source, /node\.dataset\.hasToolImage === "1"/);
  assert.match(source, /if \(hasImage\) summary\.appendChild\(toolImageIndicator\(\)\)/);
  assert.match(source, /if \(tool\.has_image\) head\.appendChild\(toolImageIndicator\(\)\)/);
  assert.match(stylesheet, /\.tool-image-indicator svg\s*\{/);
});

test("expanded folders preload summaries and composer focus stays inside its input shell", async () => {
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
  assert.match(source, /\$\("submitBtn"\)\.onclick = \(\) => run/);
  assert.match(source, /\$\("stopBtn"\)\.onclick = \(\) => run/);
  assert.doesNotMatch(source, /pointerAction/);
  assert.doesNotMatch(source, /composerShell\.addEventListener\(\s*"pointerdown"/);
  assert.match(source, /\.querySelectorAll\("\.composer-shell"\)/);
  assert.doesNotMatch(source, /\.querySelectorAll\("\.composer"\)/);
  assert.match(source, /addEventListener\("pointerdown", keepComposerTextFocus\)/);
  assert.match(source, /target\.closest\([\s\S]*?#submitBtn, #temporarySendBtn/);
  assert.match(source, /p\.kind === "chats" \? tr\("chats"\) : p\.name/);
  assert.match(source, /option\.textContent = tr\("chatsWithoutProject"\)/);
  assert.match(source, /project_path: project\.kind === "chats" \? null : project\.path/);
  assert.match(source, /\$\("createWorktreeBtn"\)\.hidden = isChat/);
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(
    stylesheet,
    /@media \(max-width: 800px\)[\s\S]*?\.composer \{[\s\S]*?pointer-events: auto;/,
  );
  assert.match(
    stylesheet,
    /@media \(max-width: 800px\)[\s\S]*?\.composer-shell \{[\s\S]*?pointer-events: auto;/,
  );
});

test("structured command actions stay separate and preserve multiline commands", async () => {
  const command = await demoClient();
  const page = result(command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }));
  assert.equal(page.messages[1].deferred_tool_count, 1);
  const turn = result(
      command({
        command: "turn_messages",
        thread_id: "demo-thread-web-ui",
        turn_id: "demo-turn-seed-1",
      }),
    ),
    summary = turn.messages[1].tools[0];
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
  assert.deepEqual(localFileReference("/Users/example/project/app.js:123:7"), {
    path: "/Users/example/project/app.js",
    line: 123,
    column: 7,
  });
  assert.deepEqual(localFileReference("/Users/example/project/app.js#L45C2"), {
    path: "/Users/example/project/app.js",
    line: 45,
    column: 2,
  });
  assert.equal(localFilePath("https://example.com/firmware.elf"), null);
});

test("local workspace files open in a typed preview before download", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const api = await readFile(new URL("../src/api.js", import.meta.url), "utf8");
  const markdown = await readFile(new URL("../src/markdown.js", import.meta.url), "utf8");
  const preview = await readFile(new URL("../src/file-preview.js", import.meta.url), "utf8");
  const html = await readFile(new URL("../index.html", import.meta.url), "utf8");
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(api, /fetch\("\/api\/file-preview"/);
  assert.match(markdown, /options\.requestLocalFilePreview/);
  assert.doesNotMatch(markdown, /link\.href = "#"/);
  assert.match(markdown, /link\.role = "button"/);
  assert.match(source, /filePreview\.open\(preview, position\)/);
  assert.match(preview, /new EditorView/);
  assert.match(preview, /EditorState\.readOnly\.of\(true\)/);
  assert.match(preview, /EditorView\.scrollIntoView\(anchor/);
  assert.match(preview, /current\.kind === "markdown"/);
  assert.match(preview, /current\.kind === "image"/);
  assert.match(html, /id="filePreviewDialog"/);
  assert.match(stylesheet, /\.file-preview-card\s*\{/);
});

test("interrupt requests carry the app-server turn observed by the UI", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /const interruptedTurnId = state\.activeTurnId/);
  assert.match(source, /targetRequest\("interrupt", \{ turn_id: interruptedTurnId \}\)/);
  assert.match(source, /await Promise\.allSettled\(\[refreshPending\(\), refreshActivity\(\)\]\)/);
  assert.match(source, /submit\.disabled = state\.composerSubmitting \|\| state\.interrupting/);
  assert.match(source, /if \(!reference\) return continuePendingQueue\(\)/);
  assert.match(source, /command: "pending_message_start"/);
  assert.match(source, /\$\("submitBtn"\)\.onclick = \(\) => run\(\(\) => write/);
  assert.match(source, /\$\("stopBtn"\)\.onclick = \(\) =>/);
  const css = await readFile(new URL("../src/styles.css", import.meta.url), "utf8");
  assert.match(css, /\.composer-shell\.stop-visible #stopBtn/);
  assert.match(css, /translateX\(calc\(var\(--submit-button-width\) \* -1\.5\)\)/);
});

test("message refresh preserves stable nodes and viewport anchors", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /root\.insertBefore\(node, cursor\)/);
  assert.match(source, /anchorMessageIndex/);
  assert.match(source, /anchorTurnKey/);
  assert.match(source, /!message\.hidden/);
  assert.match(source, /message\.getClientRects\(\)\.length > 0/);
  assert.match(source, /smoothBottom/);
  const reconcile = source.slice(
    source.indexOf("function reconcileMessageNodes"),
    source.indexOf("function pendingNode"),
  );
  assert.doesNotMatch(reconcile, /replaceChildren/);
});

test("a detected rollout sequence issue appears beside the session title", async () => {
  const [html, source, stylesheet] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
  ]);
  assert.match(html, /id="threadTitle"[\s\S]*id="repairHintBtn"[\s\S]*id="repairHintBubble"/);
  assert.match(source, /renderRepairHint\(r\.repair_required\)/);
  assert.match(source, /\$\("repairHintBtn"\)\.onclick/);
  assert.match(stylesheet, /\.repair-hint-bubble\s*\{[\s\S]*position:\s*absolute/);
  const command = await demoClient();
  const page = result(command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 1 }));
  assert.equal(page.repair_required, false);
});

test("session statistics are generated by the backend and rendered in Tools", async () => {
  const command = await demoClient();
  const page = result(command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 1 }));
  assert.deepEqual(
    {
      turns: page.statistics.turns,
      tools: page.statistics.tool_calls,
      tokens: page.statistics.total_tokens,
      duration: page.statistics.total_duration_ms,
    },
    { turns: 2, tools: 2, tokens: 12840, duration: 42000 },
  );
  const [html, source] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
  ]);
  assert.match(html, /id="threadStatistics"[\s\S]*id="threadStatGrid"/);
  assert.match(source, /renderThreadStatistics\(r\.statistics\)/);
});

test("large-session deferred turns hydrate only while visible and at a bounded rate", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /new IntersectionObserver/);
  assert.match(source, /entry\.isIntersecting/);
  assert.match(source, /rootMargin: "48px 0px"/);
  assert.match(source, /VISIBLE_TURN_HYDRATION_INTERVAL_MS = 600/);
  assert.doesNotMatch(source, /scheduleDeferredTurnPrefetch/);
  assert.doesNotMatch(source, /requestIdleCallback\(\(\) => prefetch/);
});

test("goal is a collapsible right-side panel with typed pause resume and edit controls", async () => {
  assert.deepEqual(goalToggleState("blocked"), {
    canPause: false,
    canResume: true,
    nextStatus: "active",
  });
  assert.equal(goalToggleState("budgetLimited").nextStatus, null);
  const command = await demoClient();
  const initial = result(command({ command: "thread_goal_get", thread_id: "demo-thread-web-ui" }));
  assert.equal(initial.goal.status, "active");
  const paused = result(
    command({ command: "thread_goal_set", thread_id: "demo-thread-web-ui", status: "paused" }),
  );
  assert.equal(paused.goal.status, "paused");
  const edited = result(
    command({
      command: "thread_goal_set",
      thread_id: "demo-thread-web-ui",
      objective: "Updated from the floating panel",
    }),
  );
  assert.equal(edited.goal.objective, "Updated from the floating panel");

  const [html, source, stylesheet] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
  ]);
  assert.match(html, /id="goalPanel"[\s\S]*id="goalToggleBtn"/);
  assert.match(html, /id="goalHideBtn"[\s\S]*d="m9 6 6 6-6 6"/);
  assert.match(html, /id="goalRestoreBtn"[\s\S]*d="m15 6-6 6 6 6"/);
  assert.match(html, /id="goalEditDialog"[\s\S]*id="goalObjectiveInput"/);
  assert.match(source, /command: "thread_goal_get"/);
  assert.match(source, /command: "thread_goal_set"/);
  assert.match(source, /method === "thread\/goal\/updated"/);
  assert.match(stylesheet, /\.goal-float\s*\{[^}]*position:\s*fixed;[^}]*right:/s);
  assert.match(stylesheet, /\.goal-restore\s*\{[^}]*position:\s*fixed;[^}]*right:\s*0;/s);
});

test("pending handoff is cleared only by an authoritative rendered user message", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const merge = source.slice(
    source.indexOf("function pendingLandedInMessages"),
    source.indexOf("async function deletePending"),
  );
  assert.match(merge, /message\.id === entry\.id/);
  assert.match(merge, /message\.message_index <= entry\.after_message_index/);
  assert.match(merge, /pendingLandedInMessages\(entry, authoritativeMessages/);
  assert.match(source, /after_message_index: state\.lastMessageIndex \?\? -1/);
  const writeFlow = source.slice(
    source.indexOf("async function write"),
    source.indexOf("function approval"),
  );
  const acceptedFlow = writeFlow.slice(writeFlow.indexOf("acknowledged = true"));
  assert.doesNotMatch(acceptedFlow, /preserveOptimistic: false/);
  assert.match(acceptedFlow, /await openThread/);
});

test("completed turns collapse by server turn id and preserve the full expansion", async () => {
  assert.equal(scrollTopForViewportAnchor(900, 700, 200), 400);
  assert.equal(scrollTopForViewportAnchor(400, 200, 700), 900);
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
  assert.match(layout, /preserveMessageElementPosition\(finalNode/);
  assert.match(layout, /foldBlock\.replaceChildren\(fold, divider, tokenUsage\)/);
  assert.match(layout, /\.\.\.leading,[\s\S]*foldBlock,[\s\S]*finalNode/);
  assert.match(layout, /turnSummaryText\(group\)/);
  assert.match(layout, /usage = turnUsageItem\(group\)/);
  assert.match(layout, /turnTokenUsageText\(usage\)/);
  assert.match(layout, /querySelectorAll\("\.turn-token-usage"\)/);
  assert.doesNotMatch(layout, /latestTool/);
  assert.doesNotMatch(layout, /toolIconClass/);
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(stylesheet, /\.turn-fold::before\s*\{/);
  assert.doesNotMatch(stylesheet, /\.turn-fold::after\s*\{/);
  assert.match(stylesheet, /\.turn-fold\s*\{[^}]*width:\s*fit-content;[^}]*justify-self:\s*start/s);
  assert.match(
    stylesheet,
    /\.turn-fold-text \.tool-summary-label\s*\{[^}]*text-overflow:\s*ellipsis;[^}]*white-space:\s*nowrap/s,
  );
  assert.match(
    stylesheet,
    /\.turn-fold-block\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\) max-content;/s,
  );
  assert.match(
    stylesheet,
    /\.turn-fold-usage\s*\{[^}]*grid-column:\s*2;[^}]*max-width:\s*none;[^}]*overflow:\s*visible;[^}]*text-align:\s*right;[^}]*text-overflow:\s*clip/s,
  );
  assert.match(
    stylesheet,
    /\.turn-fold:hover \+ \.turn-divider\s*\{[^}]*border-top-color:[^}]*box-shadow:/s,
  );
  assert.match(
    stylesheet,
    /\.turn-group\.collapsed > \.turn-fold-block > \.turn-fold\s*\{[^}]*font-weight:\s*700;/s,
  );
  assert.match(
    stylesheet,
    /\.turn-group\.collapsed > \.turn-fold-block > \.turn-divider\s*\{[^}]*border-top-width:\s*2px;/s,
  );
  assert.match(
    stylesheet,
    /@media \(max-width: 800px\)[\s\S]*\.turn-fold-usage\s*\{[^}]*grid-row:\s*2;[^}]*justify-self:\s*start;[^}]*margin:\s*0 2px 6px 18px[\s\S]*\.turn-fold-block > \.turn-divider\s*\{[^}]*grid-row:\s*3/s,
  );
  assert.match(source, /command: "turn_messages"/);
  assert.match(source, /observeVisibleDeferredTurns/);
});

test("Tools can collapse every expanded message in the current session", async () => {
  const [html, source] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
  ]);
  assert.match(html, /id="collapseMessagesBtn"[^>]*data-i18n="collapseMessages"/);
  const collapseFlow = source.slice(
    source.indexOf("function collapseExpandedMessages"),
    source.indexOf("function layoutTurnGroup"),
  );
  assert.match(collapseFlow, /state\.current\?\.id/);
  assert.match(collapseFlow, /state\.expandedTurnIds\.delete\(key\)/);
  assert.match(collapseFlow, /renderVisibleMessages\(\)/);
  assert.match(source, /\$\("collapseMessagesBtn"\)\.onclick = collapseExpandedMessages/);
});

test("token usage joins the turn fold while memory stays on the final response", async () => {
  assert.deepEqual(
    memoryCitationModel([
      { source: "MEMORY.md:12-18", note: "first" },
      { source: "MEMORY.md:12-18", note: "first" },
      { source: "skills/demo/SKILL.md:1-4", note: "rules" },
      { source: "archive/MEMORY.md:30-32", note: "second" },
    ]),
    {
      entries: [
        { source: "MEMORY.md:12-18", note: "first" },
        { source: "skills/demo/SKILL.md:1-4", note: "rules" },
        { source: "archive/MEMORY.md:30-32", note: "second" },
      ],
      files: ["MEMORY.md", "SKILL.md"],
    },
  );
  const source = await readFile(mainScriptPath, "utf8"),
    render = source.slice(
      source.indexOf("function messageNode"),
      source.indexOf("function pendingNode"),
    );
  assert.match(render, /for \(const item of usageItems\)[\s\S]*memoryCitationNode\(memoryItems\)/);
  assert.match(source, /node\.classList\.add\("turn-token-usage"\)/);
  assert.match(source, /text\.append\(label\)/);
  assert.match(source, /foldBlock\.replaceChildren\(fold, divider, tokenUsage\)/);
});

test("message copy follows text before tools and completion metadata", async () => {
  const source = await readFile(mainScriptPath, "utf8"),
    stylesheet = await readFile(stylesheetPath, "utf8"),
    render = source.slice(
      source.indexOf("function messageNode"),
      source.indexOf("function pendingNode"),
    );
  assert.match(
    render,
    /for \(const item of ordinaryItems\)[\s\S]*copy = copyText \? messageCopyButton\(copyText\) : null,[\s\S]*tools = toolGroupNode\(m,[\s\S]*toolRow\.appendChild\(tools\)[\s\S]*toolRow\.appendChild\(copy\)[\s\S]*usageItems/,
  );
  const copyStyle = stylesheet.match(/\.message-copy\s*\{[^}]*\}/s)?.[0] || "";
  assert.match(copyStyle, /position:\s*absolute/);
  assert.match(copyStyle, /right:\s*0/);
  assert.match(stylesheet, /\.message\.user \.message-body\s*\{[^}]*padding:\s*0 28px 0 0/s);
  assert.match(
    stylesheet,
    /\.message-tool-row > \.tool-message-copy\s*\{[^}]*top:\s*13px;[^}]*right:\s*0;[^}]*bottom:\s*auto;/s,
  );
  assert.match(stylesheet, /\.message-tool-row\s*\{[^}]*display:\s*flow-root;/s);
  assert.match(
    stylesheet,
    /\.message\.user \.message-copy\s*\{[^}]*right:\s*-8px;[^}]*bottom:\s*0/s,
  );
});

test("queued messages expose withdraw and convert-to-steer actions", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /function convertPendingToSteer/);
  assert.match(source, /command: "pending_message_delete"/);
  assert.match(source, /command: "steer"/);
  assert.match(source, /className = "outbox-menu"/);
  assert.match(source, /existingById/);
  assert.match(source, /entry\.status !== "failed"/);
  const convertFlow = source.slice(
    source.indexOf("async function convertPendingToSteer"),
    source.indexOf("function olderButton"),
  );
  assert.match(convertFlow, /submission_id: submissionId/);
  const acceptedFlow = convertFlow.slice(0, convertFlow.indexOf("} catch (error)"));
  assert.doesNotMatch(acceptedFlow, /preserveOptimistic: false/);
});

test("queued text messages expose an ordered merge action", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /command: "pending_messages_merge"/);
  assert.match(source, /mergeableQueueCount > 1/);
  assert.match(source, /refreshPending\(\{ preserveOptimistic: false \}\)/);
});

test("selected text creates an annotated full-screen temporary conversation", async () => {
  const [source, stylesheet, html] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
    readFile(indexPath, "utf8"),
  ]);
  assert.match(
    html,
    /id="temporaryPanelBtn"[\s\S]*class="tool-toggle"/,
    "the temporary entry belongs immediately before Tools",
  );
  assert.match(html, /id="temporaryPanelBtn"[\s\S]*?hidden/);
  assert.match(
    stylesheet,
    /\.head-actions \.temporary-toggle:not\(\[hidden\]\)/,
    "the mobile header must not override an unavailable temporary entry",
  );
  assert.doesNotMatch(html, /temporaryDraftBtn/, "Temp reuses the Queue/Steer mode control");
  assert.match(
    html,
    /id="composerStatus"[\s\S]*?id="composerReference"[\s\S]*?class="composer-shell"/,
    "the reference annotation sits between composer status and input",
  );
  const temporaryPanel = html.slice(
    html.indexOf('class="temporary-chat"'),
    html.indexOf('<section class="tools"'),
  );
  assert.match(temporaryPanel, /class="thread-head temporary-head"/);
  assert.match(temporaryPanel, /class="composer-shell temporary-composer-shell"/);
  assert.doesNotMatch(temporaryPanel, /tool-toggle/, "temporary chat has no toolbar action");
  assert.match(
    html,
    /id="selectionCopyBtn"[\s\S]*id="selectionInsertBtn"[\s\S]*id="selectionTemporaryBtn"/,
  );
  assert.match(source, /command: "temporary_thread_create"/);
  assert.match(source, /command: "temporary_turn_start"/);
  assert.match(source, /command: "temporary_turn_interrupt"/);
  assert.match(source, /last_turn_id: selection\.turnId \|\| null/);
  assert.match(source, /state\.temporaryThreads\.set\(sourceThreadId, temporary\)/);
  assert.match(source, /mode === "send" && state\.temporarySelection\?\.turnId/);
  assert.match(source, /if \(name === "temp"\)[\s\S]*?sendTemporaryMessage\(\)/);
  const insertHandler = source.slice(
    source.indexOf('$("selectionInsertBtn").onclick'),
    source.indexOf('$("selectionTemporaryBtn").onclick'),
  );
  assert.match(insertHandler, /setComposerReference\(selection\)/);
  assert.doesNotMatch(insertHandler, /insertComposerText/);
  assert.match(stylesheet, /\.temporary-chat\.open\s*\{[\s\S]*?transform:\s*none/);
  assert.match(stylesheet, /\.temporary-chat\s*\{[\s\S]*?inset:\s*0;[\s\S]*?width:\s*100%/);
  assert.match(stylesheet, /\.selection-actions\s*\{[^}]*position:\s*fixed/s);

  const command = await demoClient();
  const temporary = result(
    command({
      command: "temporary_thread_create",
      thread_id: "demo-thread-web-ui",
      last_turn_id: "demo-turn-1",
    }),
  );
  assert.equal(temporary.thread.ephemeral, true);
  assert.equal(temporary.thread.forked_from_id, "demo-thread-web-ui");
  const turn = result(
    command({
      command: "temporary_turn_start",
      thread_id: temporary.thread.id,
      text: "Explain this selection",
      submission_id: "temporary-test-1",
      attachments: [],
    }),
  );
  assert.equal(turn.turn_id, "demo-temporary-turn");
  assert.match(turn.demo_reply, /in-memory temporary branch/);
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

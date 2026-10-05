import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { runInNewContext } from "node:vm";

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
import { createEventSequenceTracker } from "../src/event-sequence.js";
import { demoCommandWithInstance } from "../src/demo-client.js";
import { goalToggleState } from "../src/goal-state.js";
import { asyncQuestionReplyMode } from "../src/async-question-state.js";
import { commandPickerTrigger } from "../src/command-picker.js";
import { appendLiveCommandText, commandOutputModel, isCommandTool } from "../src/command-output.js";
import { LARGE_PASTE_CHARS, shouldAttachPastedText } from "../src/large-paste.js";
import { orderedTurnChildren, turnUsageHost } from "../src/turn-stack.js";

test("command output keeps terminal text, exit status, and parsed JSON distinct", () => {
  const native = commandOutputModel({
    aggregatedOutput: '{"ok":true,"items":[1,2]}',
    exitCode: 0,
    durationMs: 1234,
    result: "success",
  });
  assert.equal(native.text, '{"ok":true,"items":[1,2]}');
  assert.equal(native.exitCode, 0);
  assert.equal(native.durationMs, 1234);
  assert.deepEqual(native.parsed, { ok: true, items: [1, 2] });
  assert.deepEqual(native.extra, { result: "success" });
  const wrapped = commandOutputModel([
    {
      type: "text",
      text: "Chunk ID: abc\nWall time: 0.25 seconds\nProcess exited with code 7\nFinal output:\nfailed\n",
    },
  ]);
  assert.equal(wrapped.text, "failed\n");
  assert.equal(wrapped.exitCode, 7);
  assert.equal(wrapped.durationMs, 250);
  const objectResult = commandOutputModel([
    { type: "text", text: '{"output":"done\\n","exit_code":0,"wall_time_seconds":0.05}' },
  ]);
  assert.equal(objectResult.text, "done\n");
  assert.equal(objectResult.exitCode, 0);
  assert.equal(objectResult.durationMs, 50);
  assert.equal(commandOutputModel("exit code 7\n", "failed").text, "exit code 7\n");
  assert.equal(isCommandTool("exec_command"), true);
  assert.equal(isCommandTool("web_search"), false);
  assert.equal(appendLiveCommandText("first", " second"), "first second");
});

test("collapsed steer messages form one ordered prompt stack", () => {
  const [prompt, commentary, steerOne, tool, steerTwo, final, fold, stack] = Array.from(
    { length: 8 },
    () => ({}),
  );
  const messages = [prompt, commentary, steerOne, tool, steerTwo, final];
  const users = [prompt, steerOne, steerTwo];
  assert.deepEqual(orderedTurnChildren(messages, users, fold, stack, false), [
    stack,
    fold,
    commentary,
    tool,
    final,
  ]);
  assert.deepEqual(orderedTurnChildren(messages, users, fold, null, true), [
    prompt,
    fold,
    commentary,
    steerOne,
    tool,
    steerTwo,
    final,
  ]);
  assert.equal(turnUsageHost(users, false), steerTwo);
  assert.equal(turnUsageHost(users, true), prompt);
});

test("skill slash command opens skills for exact and partial queries", () => {
  assert.deepEqual(commandPickerTrigger("/skills"), {
    mode: "skills",
    query: "",
    start: 0,
    end: 7,
  });
  assert.deepEqual(commandPickerTrigger("/skills de"), {
    mode: "skills",
    query: "de",
    start: 0,
    end: 10,
  });
  assert.equal(commandPickerTrigger("/ski").mode, "commands");
  assert.equal(commandPickerTrigger("/model").mode, "commands");
});

test("large clipboard text becomes a pasted-text attachment", async () => {
  assert.equal(shouldAttachPastedText("x".repeat(LARGE_PASTE_CHARS - 1)), false);
  assert.equal(shouldAttachPastedText("x".repeat(LARGE_PASTE_CHARS)), true);
  assert.equal(
    pendingInputSummary("Review this", [{ type: "pasted_text" }]),
    "Review this\n[Pasted text attachment]",
  );
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /messageText"\)\.addEventListener\("paste"/);
  assert.match(source, /state\.composerAttachments\.push\(\{ type: "pasted_text"/);
});

test("submitted pasted text appears only as an attachment in demo history", async () => {
  const command = await demoClient();
  const pasted = "x".repeat(LARGE_PASTE_CHARS);
  result(
    command({
      command: "send",
      thread_id: "demo-thread-web-ui",
      text: "Review this",
      attachments: [
        { type: "pasted_text", id: "d28b8e6c-2ab3-4df6-8d3d-40cb925665d9", text: pasted },
      ],
    }),
  );
  for (let poll = 0; poll < 4; poll++) result(command({ command: "pending_messages" }));
  const messages = result(
    command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }),
  ).messages;
  const user = messages.findLast((message) => message.role === "user");
  assert.deepEqual(
    user.content.map((item) => item.kind),
    ["text", "pasted_text"],
  );
  assert.equal(user.content[0].text, "Review this");
  assert.equal(user.content[1].text, pasted);
});

test("async clarification answers steer only into their originating turn", () => {
  assert.equal(asyncQuestionReplyMode("turn-a", "turn-a"), "steer");
  assert.equal(asyncQuestionReplyMode("turn-a", "turn-b"), "changed");
  assert.equal(asyncQuestionReplyMode("turn-a", null), "new_turn");
});
import { isLocalImagePath, localFilePath, localFileReference } from "../src/markdown.js";
import { memoryCitationModel } from "../src/memory-citations.js";
import {
  compactToolFilePath,
  shouldCollapseAssistantOutput,
  shouldCollapseUserMessage,
  toolFileList,
} from "../src/message-presentation.js";
import {
  mergePendingSnapshot,
  pendingInputSummary,
  reconcilePendingMessages,
} from "../src/pending-state.js";
import { SessionMessageCache } from "../src/message-cache.js";
import { runtimeArchitectureModel } from "../src/runtime-architecture.js";
import {
  EXPANDED_PROJECTS_STORAGE_KEY,
  persistExpandedProjects,
  sidebarProjectGroups,
  sidebarProjectOrder,
  storedExpandedProjects,
} from "../src/project-state.js";

test("custom sections stay below Pinned and before project folders", () => {
  const projects = [
    { path: "/workspace", kind: "project" },
    { path: "codex-bridge://section/design", kind: "section" },
    { path: "codex-bridge://chats", kind: "chats" },
  ];
  assert.deepEqual(
    sidebarProjectOrder(projects).map((project) => project.kind),
    ["section", "project", "chats"],
  );
  assert.equal(projects[0].kind, "project");
});

test("a project assigned to a section renders beneath that section once", () => {
  const section = { path: "codex-bridge://section/design", kind: "section" };
  const project = { path: "/workspace", kind: "project", section_path: section.path };
  const groups = sidebarProjectGroups([project, section, { path: "/other", kind: "project" }]);
  assert.deepEqual(
    groups.map(({ project, children }) => [project.path, children.map((child) => child.path)]),
    [
      [section.path, [project.path]],
      ["/other", []],
    ],
  );
});
import { taskOverview } from "../src/task-overview.js";
import { toolOutputImageUrl, toolOutputImageUrls } from "../src/tool-image.js";
import {
  activeActivityOverlay,
  activityOverlayConfirmed,
  completedActivityOverlay,
  mergeActivityProjection,
} from "../src/thread-activity-state.js";
import {
  LAST_SESSION_STORAGE_KEY,
  rememberSessionId,
  sessionHash,
  sessionIdFromHash,
  storedSessionId,
} from "../src/session-route.js";
import {
  activeToolGroupTarget,
  activityAgents,
  activityToolTitle,
  activityThreadIds,
  adjacentTurnIndex,
  documentOwnsMessageScroll,
  latestActivityMessages,
  messageIdentity,
  messageBottomDistance,
  messageBottomScrollTop,
  messagePersistsWhenTurnCollapsed,
  scrollTopForViewportAnchor,
  shouldFollowMessageTail,
  shouldResumeMessageTail,
  toolGroupIdentity,
} from "../src/viewport-state.js";

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

test("event sequence tracking rejects duplicates and requests scoped recovery after gaps", () => {
  const tracker = createEventSequenceTracker();
  assert.deepEqual(
    tracker.observe({ type: "bridge_event_stream", status: "ready", sequence: 10 }),
    { accept: true, gap: null },
  );
  assert.deepEqual(tracker.observe({ bridge_sequence: 11 }), { accept: true, gap: null });
  assert.equal(tracker.observe({ bridge_sequence: 11 }).accept, false);
  assert.deepEqual(tracker.observe({ bridge_sequence: 14 }).gap, {
    type: "bridge_event_gap",
    reason: "sequence_gap",
    skipped: 2,
    previous_sequence: 11,
    sequence: 14,
  });
  assert.deepEqual(
    tracker.observe({ type: "bridge_event_stream", status: "ready", sequence: 3 }).gap,
    {
      type: "bridge_event_gap",
      reason: "sequence_reset",
      skipped: 0,
      previous_sequence: 14,
      sequence: 3,
    },
  );
  assert.deepEqual(tracker.observe({ bridge_sequence: 4 }), { accept: true, gap: null });
});

test("live activity overlays prevent delayed rollout snapshots from restoring stale tools", () => {
  const running = activeActivityOverlay("turn-new", "tool", {
    id: "call-new",
    name: "apply_patch",
  });
  const oldSnapshot = {
    active_turn_id: "turn-old",
    phase: "tool",
    active_tool: "exec_command",
    active_tool_call_id: "call-old",
  };
  assert.equal(activityOverlayConfirmed(running, oldSnapshot), false);
  assert.deepEqual(mergeActivityProjection(oldSnapshot, running), running);

  const waitingForModel = activeActivityOverlay("turn-new", "model");
  assert.equal(
    activityOverlayConfirmed(waitingForModel, {
      active_turn_id: "turn-new",
      phase: "tool",
      active_tool_call_id: "call-new",
    }),
    false,
  );
  assert.equal(
    activityOverlayConfirmed(waitingForModel, {
      active_turn_id: "turn-new",
      phase: "model",
      active_tool_call_id: null,
    }),
    true,
  );
  assert.equal(activityOverlayConfirmed(completedActivityOverlay(), oldSnapshot), false);
  assert.equal(activityOverlayConfirmed(completedActivityOverlay(), {}), true);
});

test("authoritative active state retains the live turn until rollout catches up", () => {
  assert.equal(effectiveActiveTurnId(null, true, "turn-live"), "turn-live");
  assert.equal(effectiveActiveTurnId("turn-rollout", true, "turn-live"), "turn-rollout");
  assert.equal(effectiveActiveTurnId("turn-rollout", false, "turn-live"), null);
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

test("Codex GUI component diagnoses missing WS setup and Desktop stdio fallback", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const translations = await readFile(new URL("../src/i18n.js", import.meta.url), "utf8");
  assert.match(source, /renderCodexGuiService\(root, expanded, services\.codex_gui\)/);
  assert.match(source, /service\.ws_environment_configured/);
  assert.match(source, /service\.ws_environment_matches_expected/);
  assert.match(source, /service\.stdio_app_server_count/);
  assert.match(source, /expected && !configured && stdioCount > 0/);
  assert.match(translations, /codexGuiMissingWsAndStdio/);
  assert.match(translations, /codexGuiStdioWarning/);
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
  assert.match(source, /function openProjectlessDraft\(\)/);
  assert.match(source, /projectless_draft: true/);
  assert.match(source, /async function materializeProjectlessDraft\(prompt\)/);
  assert.match(
    source,
    /command: "thread_create",[\s\S]*project_path: null,[\s\S]*prompt: prompt \|\| null/,
  );
  assert.match(
    source,
    /if \(state\.current\.projectless_draft\)[\s\S]*materializeProjectlessDraft/,
  );
  assert.doesNotMatch(source, /className = "project-add"/);
  assert.match(stylesheet, /\.session-create-button\s*\{[^}]*width:\s*100%;/s);
});

test("compiled demo WASM supports refresh and active-run interruption", async () => {
  const command = await demoClient();
  const status = result(command({ command: "status" }));
  assert.equal(status.demo, true);
  assert.equal(status.managed_services.codex_gui.stdio_app_server_count, 0);
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

test("startup loads a routed session independently from project and pin discovery", async () => {
  const source = await readFile(mainScriptPath, "utf8"),
    initialFlow = source.slice(
      source.indexOf("async function loadInitialView"),
      source.indexOf("async function loadPins"),
    ),
    projectsFlow = source.slice(
      source.indexOf("async function loadProjects"),
      source.indexOf("async function loadInitialView"),
    );
  assert.match(initialFlow, /const sessionRequest = openSessionById/);
  assert.match(initialFlow, /projectsRequest = loadProjects\(\{ restoreSession: false \}\)/);
  assert.ok(initialFlow.indexOf("sessionRequest") < initialFlow.indexOf("await projectsRequest"));
  assert.doesNotMatch(projectsFlow, /Promise\.all\(\[[\s\S]*loadPins\(\)/);
  assert.match(projectsFlow, /void loadPins\(\)\.then/);
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
  assert.match(overview.userText, /latest steer card/);
  assert.match(overview.assistantText, /steer cards stack/);
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
    /\.composer-shell\s*\{[^}]*grid-template-columns:\s*106px 72px minmax\(0, 1fr\) 64px;/s,
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

test("mobile session rows reveal a direct archive action with a right swipe", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const stylesheet = await readFile(stylesheetPath, "utf8");
  const translations = await readFile(new URL("../src/i18n.js", import.meta.url), "utf8");
  assert.match(source, /const THREAD_ARCHIVE_SWIPE_WIDTH = 76/);
  assert.match(source, /gesture\.startOffset \+ dx/);
  assert.match(source, /gesture\.offset >= THREAD_ARCHIVE_SWIPE_WIDTH \* 0\.45/);
  assert.match(
    source,
    /if \(performance\.now\(\) < suppressOpenUntil\) \{[\s\S]*?event\.stopPropagation\(\);[\s\S]*?return;[\s\S]*?if \(!row\.classList\.contains\("swipe-open"\)\) return;/,
  );
  assert.match(source, /archive\.className = "thread-archive-action"/);
  assert.match(source, /run\(\(\) => archiveThread\(thread\)\)/);
  assert.match(source, /async function archiveThread\(thread, \{ confirm = false \} = \{\}\)/);
  assert.match(source, /command\(\{ command: "thread_archive", thread_id: threadId \}, false\)/);
  assert.match(
    stylesheet,
    /@media \(max-width: 800px\)[\s\S]*?\.thread-archive-action\s*\{[\s\S]*?width: 76px;/,
  );
  assert.match(stylesheet, /translate3d\(var\(--thread-swipe-offset, 0px\), 0, 0\)/);
  assert.match(translations, /archiveSession: "归档"/);
  const gesture = source.slice(
    source.indexOf("function bindThreadArchiveSwipe"),
    source.indexOf("function threadRow"),
  );
  const touchStart = gesture.slice(gesture.indexOf('"touchstart"'), gesture.indexOf('"touchmove"'));
  assert.doesNotMatch(touchStart, /closeThreadArchiveSwipe\(/);
  assert.match(
    gesture,
    /Math\.abs\(dy\) > 10[\s\S]*?closeThreadArchiveSwipe\(null, \{ immediate: true \}\)/,
  );
  assert.match(
    source,
    /\$\("projects"\)\.addEventListener\([\s\S]*?"scroll",[\s\S]*?closeThreadArchiveSwipe\(null, \{ immediate: true \}\)/,
  );
  assert.match(
    stylesheet,
    /\.thread-archive-action\s*\{[\s\S]*?visibility: hidden;[\s\S]*?pointer-events: none;/,
  );
  assert.match(
    stylesheet,
    /\.thread-row\.swipe-dragging \.thread-archive-action,[\s\S]*?\.thread-row\.swipe-open \.thread-archive-action\s*\{\s*visibility: visible;/,
  );
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
  assert.match(source, /requestFilePreview\(threadId, tool\.image_path, \{/);
  assert.match(source, /message_index: tool\.message_index/);
  assert.match(source, /frame\.replaceChildren\(makeInspectableImage\(img\)\)/);
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
  assert.match(
    source,
    /document\.addEventListener\(eventName, preventPageGestureWhileViewingImage/,
  );
  assert.match(source, /event\.touches\?\.length > 1/);
  assert.match(source, /restorePageZoomAfterImageViewer\(\)/);
  assert.match(stylesheet, /body\.image-viewer-open\s*\{[^}]*touch-action:\s*none;/s);
  assert.match(stylesheet, /\.image-viewer\s*\{[^}]*touch-action:\s*none;/s);
  assert.match(stylesheet, /\.image-viewer-stage\s*\{[^}]*touch-action:\s*none;/s);
  assert.match(stylesheet, /\.inspectable-image\s*\{[^}]*cursor:\s*zoom-in;/s);
  assert.match(stylesheet, /\.tool-image-preview\s*\{[^}]*height:\s*clamp\(/s);
  assert.doesNotMatch(source, /group\.open = hasImage/);
  assert.doesNotMatch(source, /detail\.open = Boolean\(tool\.has_image\)/);
  assert.doesNotMatch(source, /hasToolImage|imageToolNodes/);
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

test("final answer image tags render one inspectable image control", async () => {
  assert.equal(isLocalImagePath("/Users/example/project/preview.png"), true);
  assert.equal(isLocalImagePath("/Users/example/project/Preview.JPEG#L2"), true);
  assert.equal(isLocalImagePath("/Users/example/project/notes.md"), false);
  const source = await readFile(mainScriptPath, "utf8");
  const markdown = await readFile(new URL("../src/markdown.js", import.meta.url), "utf8");
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(markdown, /options\.localImagePreviewNode\?\./);
  assert.match(markdown, /token\.startsWith\("!\["\)/);
  const linkRenderer = markdown.match(/function appendLink[\s\S]*?\n}\n\nfunction appendImage/);
  assert.ok(linkRenderer);
  assert.doesNotMatch(linkRenderer[0], /appendLocalImagePreview\(parent/);
  assert.match(source, /function markdownImagePreviewNode\(threadId, path, label\)/);
  assert.match(source, /preview\.kind !== "image" \|\| !preview\.preview_url/);
  assert.match(source, /frame\.replaceChildren\(makeInspectableImage\(img\)\)/);
  assert.match(stylesheet, /\.markdown-image-preview\s*\{[^}]*height:\s*clamp\(/s);
});

test("local workspace files open in a typed preview before download", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const api = await readFile(new URL("../src/api.js", import.meta.url), "utf8");
  const markdown = await readFile(new URL("../src/markdown.js", import.meta.url), "utf8");
  const preview = await readFile(new URL("../src/file-preview.js", import.meta.url), "utf8");
  const html = await readFile(new URL("../index.html", import.meta.url), "utf8");
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(api, /fetchWithTimeout\(\s*"\/api\/file-preview"/);
  assert.match(markdown, /options\.requestLocalFilePreview/);
  assert.doesNotMatch(markdown, /link\.href = "#"/);
  assert.match(markdown, /link\.role = "button"/);
  assert.match(source, /import\("\.\/file-preview\.js"\)/);
  assert.match(source, /controller\.open\(preview, position\)/);
  assert.match(preview, /new EditorView/);
  assert.match(preview, /EditorState\.readOnly\.of\(true\)/);
  assert.match(preview, /EditorView\.scrollIntoView\(anchor/);
  assert.match(preview, /current\.kind === "markdown"/);
  assert.match(preview, /current\.kind === "html"/);
  assert.match(preview, /current\.kind === "image"/);
  assert.match(preview, /frame\.setAttribute\("sandbox", ""\)/);
  assert.match(preview, /frame\.srcdoc = secureHtmlPreviewDocument/);
  assert.match(preview, /"script-src 'none'"/);
  assert.match(preview, /"connect-src 'none'"/);
  assert.match(html, /id="filePreviewDialog"/);
  assert.match(stylesheet, /\.file-preview-card\s*\{/);
  assert.match(stylesheet, /\.file-preview-html\s*\{/);
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
  assert.match(source, /viewport\?\.pageTop \?\? window\.scrollY/);
  assert.match(source, /viewport\?\.height \?\? window\.innerHeight/);
  assert.match(source, /shouldFollowMessageTail\(state\.followMessageTail\)/);
  assert.doesNotMatch(
    source,
    /state\.expandedTurnIds\.add\(`\$\{thread\.id\}:\$\{messageView\.anchorTurnKey\}`\)/,
  );
  const restoreView = source.slice(
    source.indexOf("function restoreMessageView"),
    source.indexOf("async function openThread"),
  );
  assert.doesNotMatch(restoreView, /scrollMessagesToBottom\("smooth"\)/);
  assert.match(restoreView, /view\.intentVersion !== messageScrollIntentVersion/);
  assert.match(restoreView, /if \(view\.atBottom\)[\s\S]*?scrollMessagesToBottom\("auto"\)/);
  assert.doesNotMatch(restoreView, /else setMessageScrollTop\(view\.top\)/);
  assert.match(restoreView, /setMessageScrollTop\(view\.top\)/);
  const reconcile = source.slice(
    source.indexOf("function reconcileMessageNodes"),
    source.indexOf("function pendingNode"),
  );
  assert.doesNotMatch(reconcile, /replaceChildren/);
});

test("live tool progress follows the exact newest call instead of animating stale tools", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /mergeActivityProjection\(effectiveActivity, state\.liveActivityOverlay\)/);
  assert.match(source, /tool\.call_id === state\.activeToolCallId/);
  assert.match(source, /runningTool = matchedRunningTool \|\| liveTool/);
  assert.match(source, /priorToolCount = tools\.length - \(matchedRunningTool \? 1 : 0\)/);
  assert.match(source, /activeTarget = activeToolGroupTarget\(/);
  assert.match(source, /messages = sourceMessages/);
  assert.doesNotMatch(source, /activeTools[\s\S]*?\.flatMap/);
  assert.match(
    source,
    /messageTurnRunning = message\.message_index === activeTarget\?\.message_index/,
  );
  assert.match(
    source,
    /detail\.dataset\.toolKey = `\$\{sourceMessageIndex\}:\$\{sourceToolIndex\}`/,
  );
  assert.match(source, /activeTurnSection\.insertBefore\([\s\S]*?turn-live-usage/);
  assert.doesNotMatch(source, /nodes\.push\(liveToolActivityNode\(root\)\)/);
  assert.match(source, /method === "item\/started"/);
  assert.match(source, /state\.liveAppServerTool = liveTool/);
  assert.match(source, /state\.activeToolCallId = liveTool\.id/);
  assert.match(source, /method === "item\/completed"/);
  const toolGroup = source.slice(
    source.indexOf("function toolGroupNode"),
    source.indexOf("function toolImageIndicator"),
  );
  assert.doesNotMatch(toolGroup, /findLast\(\(tool\) => !toolFinished\(tool\)\)/);
});

test("only the last tool group in a running turn receives live state", () => {
  const firstGroup = {
      message_index: 10,
      turn_id: "turn-a",
      role: "assistant",
      tools: [{ call_id: "call-1", name: "exec_command" }],
    },
    commentary = {
      message_index: 11,
      turn_id: "turn-a",
      role: "assistant",
      tools: [],
    },
    lastGroup = {
      message_index: 12,
      turn_id: "turn-a",
      role: "assistant",
      tools: [{ call_id: "call-2", name: "apply_patch" }],
    },
    newestCommentary = {
      message_index: 13,
      turn_id: "turn-a",
      role: "assistant",
      tools: [],
    },
    messages = [firstGroup, commentary, lastGroup, newestCommentary];
  assert.equal(activeToolGroupTarget(messages, "turn-a", "call-2"), lastGroup);
  assert.equal(activeToolGroupTarget(messages, "turn-a", null), lastGroup);
  assert.deepEqual(
    firstGroup.tools.map((tool) => tool.call_id),
    ["call-1"],
  );
  assert.deepEqual(
    lastGroup.tools.map((tool) => tool.call_id),
    ["call-2"],
  );
  assert.equal(
    activeToolGroupTarget([commentary, newestCommentary], "turn-a", "call-3"),
    newestCommentary,
  );
});

test("streaming tool disclosure survives snapshot host changes", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const stateSource = await readFile(new URL("../src/state.js", import.meta.url), "utf8");
  const specification = await readFile(
    new URL("../../docs/web-ui-streaming.md", import.meta.url),
    "utf8",
  );
  assert.match(stateSource, /expandedToolGroupIds: new Set\(\)/);
  assert.match(stateSource, /expandedToolCallIds: new Set\(\)/);
  assert.match(source, /groupKey = toolGroupIdentity\(threadId, message, liveTool\)/);
  assert.match(source, /box\.dataset\.messageKey = messageIdentity\(m\)/);
  assert.match(source, /group\.open = state\.expandedToolGroupIds\.has\(groupKey\)/);
  assert.match(source, /if \(group\.open\) state\.expandedToolGroupIds\.delete\(groupKey\)/);
  assert.match(source, /detail\.open = state\.expandedToolCallIds\.has\(disclosureKey\)/);
  assert.match(source, /if \(detail\.open\) state\.expandedToolCallIds\.delete\(disclosureKey\)/);
  assert.doesNotMatch(source, /ontoggle = \([^)]*\) => \{[\s\S]{0,160}expandedTool/);
  assert.match(source, /previousToolGroups = openToolGroupKeys\(root\)/);
  assert.match(source, /previousToolDetails = loadedToolDetails\(root\)/);
  assert.match(source, /preserveToolDisclosure\(previousToolGroups, previousToolDetails, next\)/);
  assert.match(specification, /User-controlled disclosure belongs to the logical entity/);
  assert.match(specification, /preserve a visible message\/turn anchor/);
});

test("tail following requires explicit reader intent and resumes only after reaching bottom", () => {
  assert.equal(messageBottomDistance({ top: 900, height: 1500, client: 600 }), 0);
  assert.equal(messageBottomDistance({ top: 650, height: 1500, client: 600 }), 250);
  assert.equal(messageBottomScrollTop({ height: 1500, client: 600 }), 900);
  assert.equal(messageBottomScrollTop({ height: 300, client: 600 }), 0);
  assert.equal(shouldFollowMessageTail(false), false);
  assert.equal(shouldFollowMessageTail(true), true);
  assert.equal(shouldResumeMessageTail(900, { top: 899, height: 1500, client: 600 }), false);
  assert.equal(shouldResumeMessageTail(850, { top: 900, height: 1500, client: 600 }), true);
  assert.equal(shouldResumeMessageTail(850, { top: 890, height: 1500, client: 600 }), false);
  assert.equal(shouldResumeMessageTail(850, { top: 897, height: 1500, client: 600 }), false);
});

test("mobile tail lock targets the message area, not the composer-expanded document", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const metrics = source.slice(
    source.indexOf("function messageScrollMetrics"),
    source.indexOf("function setMessageScrollTop"),
  );
  assert.match(metrics, /height: root\.getBoundingClientRect\(\)\.bottom \+ window\.scrollY/);
  assert.doesNotMatch(metrics, /document\.documentElement\.scrollHeight/);
  assert.match(source, /setMessageScrollTop\(messageBottomScrollTop\(messageScrollMetrics\(\)\)/);
});

test("user scroll intent supersedes tail lock before live content arrives", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const scrollHandler = source.slice(
    source.indexOf("const handleMessageScroll ="),
    source.indexOf('$("messages").onscroll = handleMessageScroll'),
  );
  assert.match(
    scrollHandler,
    /shouldResumeMessageTail\(messageInteractionStartTop, messageScrollMetrics\(\)\)/,
  );
  assert.match(
    scrollHandler,
    /state\.followMessageTail = true;[\s\S]*settleMessageInteraction\(\)/,
  );
  assert.doesNotMatch(source, /messageBottomDistance\(messageScrollMetrics\(\)\) < 100/);
});

test("the real scroll owner changes for mobile history fullscreen", () => {
  assert.equal(documentOwnsMessageScroll({ mobile: false, fullscreen: false }), false);
  assert.equal(documentOwnsMessageScroll({ mobile: true, fullscreen: false }), true);
  assert.equal(documentOwnsMessageScroll({ mobile: true, fullscreen: true }), false);
});

test("timeline and tool group identities survive pagination and live host migration", () => {
  assert.equal(messageIdentity({ id: "message-a", message_index: 40 }), "id:message-a");
  assert.equal(messageIdentity({ message_index: 40 }), "index:40");
  assert.equal(
    toolGroupIdentity("thread-a", { message_index: 40, tools: [] }, { id: "call-a" }),
    "thread-a:call:call-a",
  );
  assert.equal(
    toolGroupIdentity("thread-a", {
      id: "message-new-host",
      message_index: 44,
      tools: [{ call_id: "call-a" }],
    }),
    "thread-a:call:call-a",
  );
  assert.equal(
    toolGroupIdentity("thread-a", {
      id: "message-new-host",
      message_index: 44,
      tools: [{ call_id: "call-a" }, { call_id: "call-b" }, { call_id: "call-c" }],
    }),
    "thread-a:call:call-a",
  );
  assert.equal(
    toolGroupIdentity(
      "thread-a",
      {
        id: "message-new-host",
        message_index: 44,
        tools: [{ call_id: "call-a" }],
      },
      { call_id: "call-b", name: "exec_command" },
    ),
    "thread-a:call:call-a",
  );
  assert.equal(
    toolGroupIdentity("thread-a", {
      message_index: 55,
      tools: [{ call_id: "wait-9", activity_key: "wait:agent-a" }],
    }),
    "thread-a:activity:wait:agent-a",
  );
});

test("completed turns retain image messages while other history remains folded", () => {
  assert.equal(
    messagePersistsWhenTurnCollapsed({ tools: [{ name: "view_image", has_image: true }] }),
    true,
  );
  assert.equal(messagePersistsWhenTurnCollapsed({ category: "compaction", tools: [] }), true);
  assert.equal(
    messagePersistsWhenTurnCollapsed({ tools: [{ name: "exec_command", has_image: false }] }),
    false,
  );
});

test("turn navigation selects the nearest turn start in either direction", () => {
  const tops = [-800, -12, 420, 1100];
  assert.equal(adjacentTurnIndex(tops, 50, "up"), 1);
  assert.equal(adjacentTurnIndex(tops, 50, "down"), 2);
  assert.equal(adjacentTurnIndex([50, 420], 50, "up"), -1);
  assert.equal(adjacentTurnIndex([50, 420], 50, "down"), 1);
  assert.equal(adjacentTurnIndex(tops, 50, "sideways"), -1);
});

test("async tool activities keep only their latest snapshot across turns", () => {
  const messages = [
      {
        message_index: 1,
        turn_id: "turn-1",
        tools: [
          { name: "wait", activity_key: "wait:agent-a" },
          { name: "write_stdin", activity_key: "write_stdin:7" },
        ],
      },
      {
        message_index: 8,
        turn_id: "turn-2",
        tools: [{ name: "wait", activity_key: "wait:agent-a" }],
      },
      {
        message_index: 12,
        turn_id: "turn-3",
        tools: [
          { name: "write_stdin", activity_key: "write_stdin:7" },
          { name: "write_stdin", activity_key: "write_stdin:9" },
        ],
      },
    ],
    visible = latestActivityMessages(messages);
  assert.deepEqual(visible[0].tools, []);
  assert.deepEqual(
    visible[1].tools.map((tool) => tool.name),
    ["wait"],
  );
  assert.deepEqual(
    visible[2].tools.map((tool) => tool.activity_key),
    ["write_stdin:7", "write_stdin:9"],
  );
  assert.equal(
    activityToolTitle(
      { name: "wait", activity_sender_id: "agent-a" },
      { id: "agent-a", title: "layout-review" },
    ),
    "subagent",
  );
  assert.equal(activityToolTitle({ name: "wait", activity_label: "Atlas" }, null), "Atlas");
  assert.equal(activityToolTitle({ name: "wait" }, null), "subagent");
  assert.deepEqual(
    activityThreadIds({ name: "wait", activity_thread_ids: ["agent-a", "agent-a"] }),
    ["agent-a"],
  );
  assert.deepEqual(activityThreadIds({ name: "wait", activity_key: "wait:agent-a,agent-b" }), [
    "agent-a",
    "agent-b",
  ]);
  assert.deepEqual(
    activityAgents(
      {
        name: "wait",
        activity_thread_ids: ["agent-a"],
        activity_agents: [{ thread_id: "agent-a", name: "Atlas" }],
      },
      null,
    ),
    [{ id: "agent-a", name: "Atlas" }],
  );
});

test("subagent activity opens its original conversation in the temporary panel", async () => {
  const [source, stylesheet, html] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
    readFile(indexPath, "utf8"),
  ]);
  assert.match(source, /function openSubagentConversation\(tool\)/);
  assert.match(source, /activityAgents\(tool, state\.current\)/);
  assert.match(
    source,
    /command: "subagent_messages", thread_id: threadId, before: null, limit: 30/,
  );
  assert.match(source, /messageNode\(message, false, temporary\.id\)/);
  assert.match(source, /function prependSubagentMessages\(\)/);
  assert.match(source, /scheduleSubagentConversationRefresh/);
  assert.match(html, /id="temporaryComposer"/);
  assert.match(stylesheet, /\.temporary-chat\.read-only \.temporary-composer/);
});

test("conversation exposes floating previous and next turn controls", async () => {
  const [html, source, stylesheet] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
  ]);
  assert.match(html, /id="turnNavigation"/);
  assert.match(html, /id="previousTurnBtn"/);
  assert.match(html, /id="nextTurnBtn"/);
  assert.match(source, /scrollToAdjacentTurn\("up"\)/);
  assert.match(source, /scrollToAdjacentTurn\("down"\)/);
  assert.match(source, /messageBottomDistance\(messageScrollMetrics\(\)\) < 2/);
  assert.match(source, /state\.followMessageTail = false/);
  assert.match(stylesheet, /\.turn-navigation\s*\{[^}]*position:\s*fixed/s);
});

test("mobile tail lock covers asynchronous layout changes", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(
    source,
    /new MutationObserver\(\(\) => \{[\s\S]*?scheduleMessageTailLock\(\);[\s\S]*?updateTurnNavigation\(\);/,
  );
  assert.match(source, /event\.target instanceof HTMLImageElement/);
  assert.match(
    source,
    /document\.fonts\?\.ready\.then\(\(\) => \{[\s\S]*scheduleMessageTailLock\(\)/,
  );
  assert.match(source, /view\.intentVersion !== messageScrollIntentVersion/);
  assert.match(source, /fullscreen: isHistoryFullscreen\(\)/);
});

test("the hidden tools drawer cannot retain focus after it closes", async () => {
  const [html, source] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
  ]);
  assert.match(html, /<section class="tools" id="tools" inert>/);
  assert.match(
    source,
    /if \(\$\("tools"\)\.contains\(document\.activeElement\)\) document\.activeElement\.blur\(\)/,
  );
  assert.match(source, /\$\("tools"\)\.inert = true/);
  assert.match(source, /\$\("tools"\)\.inert = !opening/);
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
    { turns: 3, tools: 3, tokens: 14472, duration: 45000 },
  );
  const [html, source] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
  ]);
  assert.match(html, /id="threadStatistics"[\s\S]*id="threadStatGrid"/);
  assert.match(source, /renderThreadStatistics\(mergeThreadStatistics\(r\.statistics\)\)/);
  assert.match(source, /thread\/tokenUsage\/updated/);
  assert.match(source, /sessionCacheHitRate/);
  assert.match(source, /cached_input_tokens/);
  assert.match(source, /cache_write_input_tokens/);
});

test("large-session deferred turns hydrate only after explicit expansion", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /fold\.onclick = \(\) => \{/);
  assert.match(source, /await hydrateTurn\(group\.turnId\)/);
  assert.doesNotMatch(source, /new IntersectionObserver/);
  assert.doesNotMatch(source, /observeVisibleDeferredTurns/);
  assert.doesNotMatch(source, /scheduleVisibleTurnHydration/);
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

test("pending handoff matches multimodal user messages one-to-one", async () => {
  assert.equal(
    pendingInputSummary("look here", [{ type: "image" }]),
    "look here\n[Image attachment]",
  );
  const pending = [
    {
      id: "optimistic-1",
      text: "look here\n[Image attachment]",
      after_message_index: 10,
    },
    {
      id: "optimistic-2",
      text: "look here\n[Image attachment]",
      after_message_index: 10,
    },
  ];
  const authoritative = [
    {
      id: "server-message",
      role: "user",
      message_index: 11,
      content: [
        { kind: "context", label: "Image attachment" },
        { kind: "text", text: "look here" },
      ],
    },
  ];
  assert.deepEqual(reconcilePendingMessages(pending, authoritative), [pending[1]]);
  assert.deepEqual(
    reconcilePendingMessages(
      [{ id: "image-only", text: "[Image attachment]", after_message_index: 11 }],
      [
        ...authoritative,
        {
          id: "server-image-only",
          role: "user",
          message_index: 12,
          content: [{ kind: "context", label: "Image attachment" }],
        },
      ],
    ),
    [],
  );
  assert.equal(
    reconcilePendingMessages(
      [{ id: "future", text: "look here\n[Image attachment]", after_message_index: 11 }],
      authoritative,
    ).length,
    1,
  );

  const source = await readFile(mainScriptPath, "utf8");
  const merge = source.slice(
    source.indexOf("function mergePendingResponse"),
    source.indexOf("async function deletePending"),
  );
  assert.match(merge, /reconcilePendingMessages/);
  assert.match(source, /after_message_index: state\.lastMessageIndex \?\? -1/);
  const writeFlow = source.slice(
    source.indexOf("async function write"),
    source.indexOf("function approval"),
  );
  const acceptedFlow = writeFlow.slice(writeFlow.indexOf("acknowledged = true"));
  assert.doesNotMatch(acceptedFlow, /preserveOptimistic: false/);
  assert.match(acceptedFlow, /await openThread/);
});

test("authoritative pending refresh removes consumed steer without losing an in-flight send", () => {
  const consumed = { id: "old-steer", source: "bridge", status: "accepted" },
    inFlight = { id: "new-steer", source: "web_optimistic", status: "steering" },
    local = [consumed, inFlight];
  assert.deepEqual(mergePendingSnapshot([], local, new Set([inFlight.id])), [inFlight]);
  assert.deepEqual(mergePendingSnapshot([], local, new Set()), []);
  const confirmed = { id: inFlight.id, source: "bridge", status: "accepted" };
  assert.deepEqual(mergePendingSnapshot([confirmed], local, new Set([inFlight.id])), [confirmed]);
});

test("completed turns collapse by server turn id and preserve the full expansion", async () => {
  assert.equal(scrollTopForViewportAnchor(900, 700, 200), 400);
  assert.equal(scrollTopForViewportAnchor(400, 200, 700), 900);
  const command = await demoClient();
  const page = result(command({ command: "messages", thread_id: "demo-thread-web-ui", limit: 30 }));
  assert.equal(page.messages[0].turn_id, "demo-turn-seed-1");
  assert.equal(page.messages[1].turn_id, "demo-turn-seed-1");
  assert.equal(page.messages.at(-1).turn_id, "demo-turn-seed-3");
  assert.equal(
    page.messages.filter(
      (message) => message.turn_id === "demo-turn-seed-3" && message.role === "user",
    ).length,
    3,
  );

  const source = await readFile(mainScriptPath, "utf8");
  const layout = source.slice(
    source.indexOf("function groupedTurns"),
    source.indexOf("function messageNode"),
  );
  assert.match(layout, /message\.turn_id/);
  assert.match(layout, /state\.expandedTurnIds/);
  assert.match(layout, /captureMessageElementPosition\(fold\)/);
  assert.match(layout, /restorePosition\(revealed \|\| fold\)/);
  assert.match(layout, /foldBlock\.replaceChildren\(fold, divider\)/);
  assert.match(layout, /const usageHost = turnUsageHost\(userNodes, expanded\)/);
  assert.match(layout, /usageHost\.append\(tokenUsage\)/);
  assert.match(layout, /!expanded && userNodes\.length > 1/);
  assert.match(layout, /reconcileChildren\(stack, userNodes\)/);
  assert.match(
    layout,
    /orderedTurnChildren\(messageNodes, userNodes, foldBlock, stack, expanded\)/,
  );
  assert.match(layout, /node\.style\.setProperty\("--turn-stack-depth", String\(index \+ 1\)\)/);
  assert.match(layout, /turnSummaryText\(group\)/);
  assert.match(layout, /usage = turnUsageItem\(group\)/);
  assert.match(layout, /turnTokenUsageText\(usage\)/);
  assert.match(layout, /const usageText = usage \? turnTokenUsageText\(usage\) : ""/);
  assert.match(layout, /tokenUsage\.textContent = usageText/);
  assert.match(layout, /tokenUsage\.title = usageText/);
  assert.match(layout, /querySelectorAll\("\.turn-token-usage"\)/);
  assert.doesNotMatch(layout, /latestTool/);
  assert.doesNotMatch(layout, /toolIconClass/);
  const stylesheet = await readFile(stylesheetPath, "utf8");
  assert.match(stylesheet, /\.turn-fold::after\s*\{[^}]*transform:\s*rotate\(-45deg\)/s);
  assert.match(
    stylesheet,
    /\.turn-group\.expanded \.turn-fold::after\s*\{[^}]*transform:\s*rotate\(45deg\)/s,
  );
  assert.doesNotMatch(stylesheet, /\.turn-fold::before\s*\{/);
  assert.match(stylesheet, /\.turn-fold\s*\{[^}]*width:\s*fit-content;[^}]*justify-self:\s*start/s);
  assert.match(
    stylesheet,
    /\.turn-fold-text \.tool-summary-label\s*\{[^}]*text-overflow:\s*ellipsis;[^}]*white-space:\s*nowrap/s,
  );
  assert.match(
    stylesheet,
    /\.turn-fold-block\s*\{[^}]*grid-template-columns:\s*minmax\(0, 1fr\);/s,
  );
  assert.match(
    stylesheet,
    /\.turn-fold-usage\s*\{[^}]*grid-row:\s*1;[^}]*justify-self:\s*end;[^}]*max-width:\s*100%;[^}]*overflow:\s*hidden;[^}]*text-align:\s*right;[^}]*text-overflow:\s*ellipsis/s,
  );
  assert.match(
    stylesheet,
    /\.turn-fold\s*\{[^}]*grid-row:\s*2;[^}]*color:\s*color-mix\(in srgb, var\(--muted\) 82%, var\(--panel\)\);[^}]*font-size:\s*var\(--message-disclosure-font-size\);[^}]*font-weight:\s*400;/s,
  );
  assert.match(
    stylesheet,
    /\.turn-fold-usage\s*\{[^}]*border-radius:\s*0 0 18px 18px;[^}]*background:\s*var\(--queue-bg\);[^}]*color:\s*var\(--queue-text\);/s,
  );
  assert.match(stylesheet, /\.turn-fold-usage\[hidden\]\s*\{[^}]*display:\s*none;/s);
  assert.match(
    stylesheet,
    /\.turn-prompt-stack > \.message\.user\.has-turn-usage,\s*\.turn-group:is\(\.collapsed, \.expanded\) > \.message\.user\.has-turn-usage\s*\{[^}]*padding:\s*0;[^}]*background:\s*var\(--queue-bg\);/s,
  );
  assert.match(
    stylesheet,
    /\.message\.user\.has-turn-usage > \.message-body\s*\{[^}]*border-radius:\s*21px;[^}]*background:\s*var\(--user-bg\);/s,
  );
  assert.match(
    stylesheet,
    /\.message\.user\.has-turn-usage > \.turn-fold-usage\s*\{[^}]*width:\s*100%;[^}]*margin:\s*-8px 0 0;[^}]*white-space:\s*normal;/s,
  );
  assert.match(
    stylesheet,
    /\.turn-prompt-stack\s*\{[^}]*width:\s*max-content;[^}]*max-width:\s*82%;[^}]*margin:\s*0 0 8px auto;/s,
  );
  assert.match(
    stylesheet,
    /\.turn-prompt-stack > \.message\.user\s*\{[^}]*width:\s*100%;[^}]*box-shadow:\s*0 3px 8px/s,
  );
  assert.match(
    stylesheet,
    /\.turn-prompt-stack > \.message\.user\s*\{[^}]*z-index:\s*var\(--turn-stack-depth, 0\);/s,
  );
  assert.match(
    stylesheet,
    /\.turn-prompt-stack > \.message\.user:not\(:last-child\)\s*\{[^}]*border-radius:\s*21px 21px 0 0;/s,
  );
  assert.match(
    stylesheet,
    /\.turn-prompt-stack > \.message\.user:not\(:last-child\)::after\s*\{[^}]*top:\s*calc\(100% - 8px\);[^}]*height:\s*24px;[^}]*background:\s*var\(--user-bg\);/s,
  );
  assert.match(
    stylesheet,
    /\.turn-prompt-stack > \.message\.user \+ \.message\.user\s*\{[^}]*margin-top:\s*-8px;[^}]*box-shadow:\s*0 -3px 8px/s,
  );
  assert.match(
    stylesheet,
    /\.turn-prompt-stack > \.message\.user \+ \.message\.user\.has-turn-usage > \.message-body\s*\{[^}]*box-shadow:\s*0 -3px 8px[^}]*0 3px 8px/s,
  );
  assert.match(
    stylesheet,
    /\.message\.user\.has-turn-usage > \.message-body\s*\{[^}]*box-shadow:\s*0 3px 8px/s,
  );
  assert.match(
    stylesheet,
    /\.turn-group:is\(\.collapsed, \.expanded\) > article\[class~="user"\]\s*\{[^}]*z-index:\s*1;[^}]*margin-bottom:\s*8px;/s,
  );
  assert.match(
    stylesheet,
    /\.turn-divider\s*\{[^}]*width:\s*66\.6667%;[^}]*height:\s*1px;[^}]*margin:\s*0 auto 10px;[^}]*background:\s*linear-gradient\(90deg, var\(--panel\), var\(--line\) 22%, var\(--line\) 78%, var\(--panel\)\);/s,
  );
  assert.match(stylesheet, /\.turn-divider\s*\{[^}]*grid-row:\s*3;/s);
  assert.match(source, /command: "turn_messages"/);
  assert.match(source, /await hydrateTurn\(group\.turnId\)/);
});

test("message disclosure labels share the running tool type scale", async () => {
  const stylesheet = await readFile(stylesheetPath, "utf8");
  for (const selector of [
    ".turn-fold",
    ".message-detail-toggle",
    ".memory-citations summary",
    ".context-block summary",
    ".tool-group > summary",
    ".tool-call > summary",
  ]) {
    const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    assert.match(
      stylesheet,
      new RegExp(`${escaped}\\s*\\{[^}]*font-size:\\s*var\\(--message-disclosure-font-size\\);`),
    );
    assert.match(
      stylesheet,
      new RegExp(
        `${escaped}\\s*\\{[^}]*line-height:\\s*var\\(--message-disclosure-line-height\\);`,
      ),
    );
  }
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

test("Tools refresh clears current message caches and follows the latest tail", async () => {
  const [html, source] = await Promise.all([
    readFile(indexPath, "utf8"),
    readFile(mainScriptPath, "utf8"),
  ]);
  assert.match(html, /class="brand-row-actions"[\s\S]*id="sessionRefreshBtn"/);
  const refreshFlow = source.slice(
    source.indexOf("function clearCurrentSessionMessageCaches"),
    source.indexOf("async function loadOlder"),
  );
  assert.match(refreshFlow, /state\.messageCache\.delete\(threadId\)/);
  assert.match(refreshFlow, /state\.hydratedTurns\.delete\(key\)/);
  assert.match(refreshFlow, /state\.visibleMessages = \[\]/);
  assert.match(
    refreshFlow,
    /await openThread\(thread, \{ quiet: true, writeHash: false, reconnect: true \}\)/,
  );
  assert.match(refreshFlow, /scrollMessagesToBottom\("auto"\)/);
});

test("session return renders the latest batch before any older history", async () => {
  const [source, stateSource, stylesheet, translations] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(new URL("../src/state.js", import.meta.url), "utf8"),
    readFile(stylesheetPath, "utf8"),
    readFile(new URL("../src/i18n.js", import.meta.url), "utf8"),
  ]);
  const openFlow = source.slice(
    source.indexOf("async function openThread"),
    source.indexOf("async function refreshThread"),
  );
  assert.equal(openFlow.match(/fetchMessages\(/g)?.length, 1);
  assert.match(openFlow, /fetchMessages\(null, state\.initialPageSize\)/);
  assert.match(openFlow, /await yieldToBrowser\(\)/);
  assert.match(openFlow, /const messagesRequest = fetchMessages/);
  assert.match(openFlow, /command\(\{ command: "thread_watch"/);
  assert.match(openFlow, /command\(\{ command: "pending_messages"/);
  assert.match(openFlow, /r = await messagesRequest/);
  assert.ok(
    openFlow.indexOf('command({ command: "thread_watch"') <
      openFlow.indexOf("r = await messagesRequest"),
  );
  assert.match(stateSource, /initialPageSize: 8,[\s\S]*pageSize: 30/);
  assert.match(source, /state\.messageSyncPhase === "loading"/);
  assert.match(source, /validCached \|\| !changedThread \? "reconnecting" : "loading"/);
  assert.match(source, /setMessageSyncPhase\("reconnecting"\)/);
  assert.match(source, /state\.lastMessageRefresh = Date\.now\(\);\s+setMessageSyncPhase\(null\)/);
  assert.match(source, /knownTailEnd < r\.page\.start/);
  assert.match(source, /if \(cachedTailDoesNotOverlap\) \{\s+state\.visibleMessages = \[\]/);
  assert.doesNotMatch(source, /forwardMessagePageRequests/);
  assert.doesNotMatch(source, /messageSyncPhase === "catching_up"/);
  assert.match(source, /nodes\.push\(messageTailStatusNode\(root\)\)/);
  assert.match(stylesheet, /\.message-tail-spinner[\s\S]*animation: message-tail-spin/);
  assert.match(translations, /reconnectingMessages:/);
  assert.doesNotMatch(translations, /catchingUpMessages:|waitingForModelOutput:/);
});

test("initial session loading does not wait for an animation frame", async () => {
  const source = await readFile(new URL("../src/main.js", import.meta.url), "utf8"),
    yieldBody = source.match(/function yieldToBrowser\(\) \{([\s\S]*?)\n\}/)?.[1] || "";
  assert.match(yieldBody, /setTimeout\(resolve, 0\)/);
  assert.doesNotMatch(yieldBody, /requestAnimationFrame/);
});

test("token usage stacks beneath the user prompt while memory stays on the final response", async () => {
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
  assert.match(source, /foldBlock\.replaceChildren\(fold, divider\)/);
  assert.match(source, /usageHost\.append\(tokenUsage\)/);
  assert.match(source, /activity\.turn_token_usage/);
  assert.match(source, /turn-live-usage/);
  assert.match(source, /liveTurnTokenUsageText\(usage\)/);
  assert.match(source, /hitRate: cacheHitPercent\(item\)\.toFixed\(0\)/);
  const [stylesheet, translations] = await Promise.all([
    readFile(stylesheetPath, "utf8"),
    readFile(new URL("../src/i18n.js", import.meta.url), "utf8"),
  ]);
  assert.match(
    stylesheet,
    /\.tool-group-summary\.running \.tool-summary-label,\s*\.turn-live-usage\s*\{[^}]*animation:\s*tool-summary-shimmer/s,
  );
  assert.match(
    stylesheet,
    /@media \(prefers-reduced-motion: reduce\)\s*\{\s*\.tool-group-summary\.running \.tool-summary-label,\s*\.turn-live-usage\s*\{[^}]*animation:\s*none/s,
  );
  assert.match(translations, /liveTurnTokenUsage: "\{total\} tokens · \{hitRate\}% cached"/);
  assert.match(translations, /liveTurnTokenUsage: "\{total\} Token · 缓存 \{hitRate\}%"/);
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
  const copyStyle = stylesheet.match(/(?:^|\n)\.message-copy\s*\{[^}]*\}/s)?.[0] || "";
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
  assert.match(
    stylesheet,
    /\.message\.user\.has-turn-usage > \.message-body > \.message-copy\s*\{[^}]*right:\s*8px;/s,
  );
});

test("long assistant output collapses by viewport with controls at the message bottom", async () => {
  const [source, stylesheet, translations] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
    readFile(new URL("../src/i18n.js", import.meta.url), "utf8"),
  ]);
  assert.equal(shouldCollapseAssistantOutput(700, 600), true);
  assert.equal(shouldCollapseAssistantOutput(600, 600), false);
  assert.equal(shouldCollapseAssistantOutput(319, 200), false);
  assert.equal(shouldCollapseUserMessage(226, 22.5), false);
  assert.equal(shouldCollapseUserMessage(240, 22.5), true);
  assert.match(source, /shouldCollapseAssistantOutput\(output\.scrollHeight/);
  assert.match(source, /shouldCollapseUserMessage\(\s*output\.scrollHeight/);
  assert.match(stylesheet, /max-height:\s*16em/);
  assert.match(stylesheet, /\.user-output\.collapsible:not\(\.expanded\)/);
  assert.match(stylesheet, /max-height:\s*15em/);
  assert.match(source, /body\.appendChild\(toggle\)/);
  assert.ok(
    source.indexOf("body.appendChild(toggle)") < source.indexOf("const copyText = content"),
    "the disclosure belongs directly below the response, before tool activity",
  );
  assert.match(source, /preserveMessageElementPosition\(anchor/);
  assert.match(source, /if \(toggle\.textContent !== label\) toggle\.textContent = label/);
  assert.match(stylesheet, /\.assistant-output\.collapsible:not\(\.expanded\)/);
  assert.match(stylesheet, /\.message-detail-toggle\s*\{[^}]*border-radius:\s*999px/s);
  assert.match(translations, /expandMessageDetails:\s*"展开详情"/);
  assert.match(translations, /collapseMessageDetails:\s*"收起"/);
  assert.match(translations, /expandUserMessage:\s*"展开全文"/);
});

test("view image uses an image tool icon", async () => {
  const [source, stylesheet] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
  ]);
  assert.match(source, /"web_search", "view_image"/);
  assert.match(stylesheet, /\.tool-icon\.view_image::before/);
});

test("multi-file tool summaries show compact relative file paths", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  assert.equal(compactToolFilePath("src/components/main.js"), "src/…/main.js");
  assert.equal(compactToolFilePath("/repo/web-ui/src/main.js", "/repo"), "web-ui/…/main.js");
  assert.equal(
    toolFileList(
      [
        { file_paths: ["src/a/main.js", "src/b/state.js"] },
        { file_paths: ["tests/demo.test.js", "docs/guide.md"] },
      ],
      "",
      3,
    ),
    "src/…/main.js, src/…/state.js, tests/demo.test.js +1",
  );
  assert.match(source, /editedFileList \|\| \(editedFiles/);
  assert.match(source, /toolFileList\(tools, state\.current\?\.cwd\)/);
});

test("context compaction renders as a persistent special message", async () => {
  const [source, stylesheet, translations] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
    readFile(new URL("../src/i18n.js", import.meta.url), "utf8"),
  ]);
  assert.match(source, /item\.kind === "context_compaction"/);
  assert.match(source, /messagePersistsWhenTurnCollapsed\(group\.messages\[index\]\)/);
  assert.equal(messagePersistsWhenTurnCollapsed({ category: "compaction" }), true);
  assert.match(source, /visibleLeadingNodes/);
  assert.match(stylesheet, /\.context-compaction-notice/);
  assert.match(translations, /contextCompactionComplete:\s*"上下文已压缩"/);
});

test("composer command picker receives skills and starts compaction in the demo", async () => {
  const command = await demoClient();
  const skills = result(command({ command: "skills_list", thread_id: "demo-thread-web-ui" }));
  assert.equal(skills.skills[0].name, "demo");
  const compact = result(command({ command: "thread_compact", thread_id: "demo-thread-web-ui" }));
  assert.equal(compact.status, "started");
});

test("computer-use tool output exposes its latest inline screenshot", () => {
  const first = "data:image/png;base64,YQ==";
  const latest = "data:image/jpeg;base64,Yg==";
  const output = {
    result: {
      contentItems: [
        { type: "image", mimeType: "image/png", data: "YQ==" },
        { output: JSON.stringify({ image_url: latest }) },
      ],
    },
  };
  assert.deepEqual(toolOutputImageUrls(output), [first, latest]);
  assert.equal(toolOutputImageUrl(output), latest);
  assert.equal(
    toolOutputImageUrl({
      content: [{ type: "image", mimeType: "image/png", data: "YQ==" }, { image_url: latest }],
    }),
    latest,
  );
  assert.equal(toolOutputImageUrl(JSON.stringify({ output: { image_url: first } })), first);
  assert.equal(toolOutputImageUrl({ image_url: "https://example.com/tracker.png" }), null);
});

test("live demo exposes a computer-use screenshot while its turn is active", async () => {
  const command = await demoClient(),
    threadId = "demo-thread-web-ui";
  result(command({ command: "send", thread_id: threadId, text: "Take a screenshot" }));
  for (let poll = 0; poll < 4; poll++) result(command({ command: "pending_messages" }));
  for (let poll = 0; poll < 18; poll++)
    result(command({ command: "thread_activity", thread_id: threadId }));
  const page = result(command({ command: "messages", thread_id: threadId, limit: 30 }));
  const message = page.messages.findLast((entry) =>
    entry.tools?.some((tool) => tool.name === "mcp__cua_repl__js" && tool.has_image),
  );
  assert.ok(message);
  const detail = result(
    command({
      command: "tool_content",
      thread_id: threadId,
      message_index: message.message_index,
      tool_index: 0,
    }),
  );
  assert.equal(toolOutputImageUrls(detail.tool.output).length, 2);
  assert.match(toolOutputImageUrl(detail.tool.output), /^data:image\/png;base64,/);
  const source = await readFile(mainScriptPath, "utf8");
  assert.match(source, /list\.appendChild\(toolCallImagesNode\(imageTool, threadId\)\)/);
  assert.match(source, /toolImageGalleryNode\(/);
});

test("session writer ownership drives read-only UI and explicit release", async () => {
  const client = await demoClient();
  const watch = result(await client({ command: "thread_watch", thread_id: "demo-active" }));
  assert.equal(watch.writer_lock.state, "owned");
  assert.equal(watch.writer_lock.read_only, false);
  const released = result(
    await client({ command: "thread_writer_release", thread_id: "demo-active" }),
  );
  assert.equal(released.writer_lock.state, "released");
  assert.equal(released.writer_lock.read_only, true);

  const [source, html, stylesheet, translations] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(indexPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
    readFile(new URL("../src/i18n.js", import.meta.url), "utf8"),
  ]);
  assert.match(html, /id="writerLockNotice"/);
  assert.match(html, /id="writerLockBtn"/);
  assert.match(source, /command: releasing \? "thread_writer_release" : "thread_writer_acquire"/);
  assert.match(source, /stateName === "external"\s*\? "sessionInUse"/);
  assert.match(source, /readOnly = !currentThreadWritable\(\)/);
  assert.match(source, /function currentThreadQueueable\(\)/);
  assert.match(source, /submit\.disabled =[^;]*!queueable/);
  assert.doesNotMatch(
    source,
    /if \(name === "steer" && !currentThreadWritable\(\)\) name = "send"/,
  );
  assert.match(source, /queueOnlyPlaceholder/);
  assert.match(stylesheet, /\.composer-shell\.read-only/);
  assert.match(translations, /sessionInUse:\s*"会话已被其他 app-server 使用 · 只读"/);
  assert.match(translations, /queueWithoutLockAria:\s*"加入 queue，不获取会话锁"/);
});

test("queued messages expose withdraw and convert-to-steer actions", async () => {
  const [source, stylesheet, translations] = await Promise.all([
    readFile(mainScriptPath, "utf8"),
    readFile(stylesheetPath, "utf8"),
    readFile(new URL("../src/i18n.js", import.meta.url), "utf8"),
  ]);
  assert.match(source, /function convertPendingToSteer/);
  assert.match(source, /command: "pending_message_delete"/);
  assert.match(source, /command: "steer"/);
  assert.match(source, /className = "outbox-menu"/);
  assert.match(source, /existingById/);
  assert.match(source, /entry\.status !== "failed"/);
  assert.match(source, /entry\.status === "failed" \|\|/);
  assert.match(source, /\["app_server_queue", "demo_wasm"\]\.includes\(entry\.source\)/);
  assert.match(source, /if \(!withdrawable\)/);
  assert.match(source, /sentCannotWithdraw/);
  assert.match(stylesheet, /\.outbox-delete\s*\{[^}]*color:\s*var\(--text\)/s);
  assert.match(translations, /sentCannotWithdraw:\s*"已交接 · 无法撤回"/);
  const withdrawFlow = source.slice(
    source.indexOf("async function deletePending"),
    source.indexOf("async function convertPendingToSteer"),
  );
  assert.match(withdrawFlow, /refreshPending\(\{ preserveOptimistic: false \}\)/);
  assert.match(
    source,
    /state\.eventStreamConnected &&\s*state\.pending\.some\(\(entry\) => entry\.thread_id === state\.current\?\.id\)/,
  );
  const convertFlow = source.slice(
    source.indexOf("async function convertPendingToSteer"),
    source.indexOf("function olderButton"),
  );
  assert.match(convertFlow, /submission_id: submissionId/);
  const acceptedFlow = convertFlow.slice(0, convertFlow.indexOf("} catch (error)"));
  assert.doesNotMatch(acceptedFlow, /preserveOptimistic: false/);
});

test("a queued card renders its Convert to Steer control without aborting session refresh", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const pendingSource = source.slice(
    source.indexOf("function pendingNode(entry) {"),
    source.indexOf("function renderPending() {"),
  );
  const created = [];
  const document = {
    createElement(tag) {
      const node = {
        tag,
        children: [],
        dataset: {},
        append(...children) {
          this.children.push(...children);
        },
        appendChild(child) {
          this.children.push(child);
        },
        prepend(child) {
          this.children.unshift(child);
        },
        setAttribute() {},
      };
      created.push(node);
      return node;
    },
  };
  const entry = {
    id: "queued-1",
    thread_id: "manta",
    text: "queued prompt",
    action: "queue",
    status: "queued",
    source: "app_server_queue",
  };
  const context = {
    document,
    state: { current: { id: "manta" }, pending: [entry] },
    markdownNode: () => document.createElement("span"),
    markdownOptions: () => ({}),
    currentThreadWritable: () => false,
    tr: (key) => key,
    run: () => {},
  };
  runInNewContext(`${pendingSource}\npendingNode(${JSON.stringify(entry)})`, context);
  const convert = created.find(
    (node) => node.tag === "button" && node.textContent === "convertToSteer",
  );
  assert.ok(convert);
  assert.notEqual(convert.disabled, true);
});

test("Queue to Steer rechecks the server writer before withdrawing the queued message", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const convertSource = source.slice(
    source.indexOf("async function convertPendingToSteer(entry, button) {"),
    source.indexOf("async function mergePendingMessages(entry, button) {"),
  );
  const entry = {
    id: "queued-1",
    thread_id: "manta",
    text: "queued prompt",
    action: "queue",
    status: "queued",
  };
  const state = {
    current: { id: "manta" },
    threadWriterLock: { state: "checking" },
    pending: [entry],
    pendingInFlight: new Set(),
    lastMessageIndex: 8,
  };
  const button = { disabled: false, isConnected: true };
  const calls = [];
  const notices = [];
  const context = {
    state,
    command: async (request) => {
      calls.push(request.command);
      if (request.command === "thread_watch") return { writer_lock: { state: "owned" } };
      if (request.command === "pending_message_delete") return { text: entry.text };
      if (request.command === "steer") return { status: "steered" };
      throw new Error(`unexpected command: ${request.command}`);
    },
    applyThreadWriterLock: (lock) => (state.threadWriterLock = lock),
    requireCurrentThreadWriter: () => {
      if (state.threadWriterLock.state !== "owned") throw new Error("writer unavailable");
    },
    refreshActivity: async () => ({ activity: { active_turn_id: "active-turn" } }),
    newSubmissionId: () => "converted-steer-1",
    renderPending: () => {},
    notify: (message) => notices.push(message),
    openThread: async () => {},
    tr: (key) => key,
  };
  await runInNewContext(
    `${convertSource}\nconvertPendingToSteer(${JSON.stringify(entry)}, button)`,
    {
      ...context,
      button,
    },
  );
  assert.deepEqual(calls, ["thread_watch", "pending_message_delete", "steer"]);
  assert.equal(
    state.pending.some((item) => item.id === entry.id),
    false,
  );
  assert.equal(state.pending[0].action, "steer");
  assert.equal(button.disabled, false);
  assert.deepEqual(notices, ["queueConverted"]);

  state.threadWriterLock = { state: "checking" };
  state.pending = [entry];
  calls.length = 0;
  const blocked = {
    ...context,
    command: async (request) => {
      calls.push(request.command);
      return { writer_lock: { state: "external" } };
    },
  };
  await assert.rejects(
    runInNewContext(`${convertSource}\nconvertPendingToSteer(${JSON.stringify(entry)}, button)`, {
      ...blocked,
      button,
    }),
    /writer unavailable/,
  );
  assert.deepEqual(calls, ["thread_watch"]);
  assert.equal(button.disabled, false);
  assert.equal(state.pending[0].id, entry.id);
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

test("failed Temp creation returns the main composer to a usable mode", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const writeSource = source.slice(
    source.indexOf("async function write(name) {"),
    source.indexOf("function approval(name) {"),
  );
  const message = { value: "Retry in the main conversation" };
  const sendMode = { value: "temp" };
  const modes = [];
  const submitting = [];
  const context = {
    state: {
      current: { id: "manta" },
      activeTurnId: "active-turn",
      composerAttachments: [],
      composerReference: null,
    },
    $: (id) => ({ messageText: message, sendMode })[id],
    voiceRecorder: null,
    setComposerSubmitting: (value) => submitting.push(value),
    createTemporaryThread: async () => {
      throw new Error("fork failed");
    },
    setSendMode: (mode) => {
      modes.push(mode);
      sendMode.value = mode;
    },
  };
  await assert.rejects(runInNewContext(`${writeSource}\nwrite("temp")`, context), /fork failed/);
  assert.equal(message.value, "Retry in the main conversation");
  assert.deepEqual(modes, ["steer"]);
  assert.deepEqual(submitting, [true, false]);
});

test("failed Temp turn keeps its text available for an explicit retry", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const sendSource = source.slice(
    source.indexOf("async function sendTemporaryMessage() {"),
    source.indexOf("async function transcribeAudio(audio) {"),
  );
  const textarea = { value: "Inspect this selection" };
  const temporary = {
    id: "temporary-thread",
    sourceThreadId: "manta",
    selection: { text: "selected text" },
    messages: [],
    activeTurnId: null,
  };
  const context = {
    state: { current: { id: "manta" }, temporaryThread: temporary },
    $: () => textarea,
    newSubmissionId: () => "temp-submission",
    renderTemporaryMessages: () => {},
    selectedContextPrompt: (_selection, text) => text,
    command: async () => {
      throw new Error("turn start failed");
    },
  };
  await assert.rejects(
    runInNewContext(`${sendSource}\nsendTemporaryMessage()`, context),
    /turn start failed/,
  );
  assert.equal(textarea.value, "Inspect this selection");
  assert.equal(temporary.activeTurnId, null);
  assert.equal(temporary.messages.at(-1).running, false);
});

test("a lost Steer response reconciles server acceptance without trapping the composer", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const writeSource = source.slice(
    source.indexOf("async function write(name) {"),
    source.indexOf("function approval(name) {"),
  );
  const message = { value: "Check the mounting slope" };
  const state = {
    current: { id: "manta" },
    composerAttachments: [],
    composerReference: null,
    pending: [],
    pendingInFlight: new Set(),
    lastMessageIndex: 10,
    drafts: new Map(),
    attachmentDrafts: new Map(),
    referenceDrafts: new Map(),
  };
  const submitting = [];
  const calls = [];
  const context = {
    state,
    $: () => message,
    voiceRecorder: null,
    newSubmissionId: () => "steer-attempt",
    currentThreadWritable: () => true,
    pendingInputSummary: (text) => text,
    renderPending: () => {},
    setComposerSubmitting: (value) => submitting.push(value),
    command: async (request) => {
      calls.push(request.command);
      throw new Error("request timed out after 30000 ms");
    },
    closeCommandPicker: () => {},
    resizeComposerTextarea: () => {},
    saveDraft: (id, text) => state.drafts.set(id, text),
    refreshPending: async () => {
      state.pending = [{ ...state.pending[0], status: "accepted", source: "app_server_queue" }];
    },
    renderComposerAttachments: () => {},
    setComposerReference: () => {},
    openThread: async () => {},
    notify: () => {},
    tr: (key) => key,
  };
  await runInNewContext(`${writeSource}\nwrite("steer")`, context);
  assert.deepEqual(calls, ["steer"]);
  assert.equal(state.pending[0].status, "accepted");
  assert.equal(state.pendingInFlight.size, 0);
  assert.equal(message.value, "");
  assert.equal(submitting.at(-1), false);
});

test("a stale writer lock cannot silently turn an explicit Steer into Queue", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const writeSource = source.slice(
    source.indexOf("async function write(name) {"),
    source.indexOf("function approval(name) {"),
  );
  const message = { value: "Check the mounting slope" };
  const state = {
    current: { id: "manta" },
    composerAttachments: [],
    composerReference: null,
    pending: [],
    pendingInFlight: new Set(),
    lastMessageIndex: 10,
    drafts: new Map(),
    attachmentDrafts: new Map(),
    referenceDrafts: new Map(),
  };
  const calls = [];
  const notifications = [];
  let finishHistoryRefresh;
  const context = {
    state,
    $: () => message,
    voiceRecorder: null,
    newSubmissionId: () => "steer-with-stale-lock",
    currentThreadWritable: () => false,
    pendingInputSummary: (text) => text,
    renderPending: () => {},
    setComposerSubmitting: () => {},
    command: async (request) => {
      calls.push(request.command);
      return { status: "steered" };
    },
    closeCommandPicker: () => {},
    resizeComposerTextarea: () => {},
    saveDraft: (id, text) => state.drafts.set(id, text),
    renderComposerAttachments: () => {},
    setComposerReference: () => {},
    openThread: () => new Promise((resolve) => (finishHistoryRefresh = resolve)),
    notify: (message) => notifications.push(message),
    tr: (key) => key,
  };
  const writing = runInNewContext(`${writeSource}\nwrite("steer")`, context);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(calls, ["steer"]);
  assert.equal(state.pending[0].action, "steer");
  assert.deepEqual(notifications, ["guidanceSteered"]);
  finishHistoryRefresh();
  await writing;
});

test("stale writer status keeps Steer selection and submit controls available", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const lockSource = source.slice(
    source.indexOf("function applyThreadWriterLock(lock) {"),
    source.indexOf("function setComposerSubmitting(active) {"),
  );
  const submitSource = source.slice(
    source.indexOf("function syncSubmitAction() {"),
    source.indexOf("async function interruptCurrentRun("),
  );
  const controls = new Map();
  const element = (id) => {
    if (!controls.has(id))
      controls.set(id, {
        value: id === "sendMode" ? "steer" : "",
        dataset: {},
        classList: { toggle: () => {} },
        setAttribute: () => {},
      });
    return controls.get(id);
  };
  const state = {
    current: { id: "manta" },
    threadWriterLock: null,
    activeTurnId: "active-turn",
    composerSubmitting: false,
    interrupting: false,
  };
  const context = {
    state,
    $: element,
    document: { querySelector: () => element("shell") },
    tr: (key) => key,
    currentThreadWritable: () => state.threadWriterLock?.state === "owned",
    currentThreadQueueable: () => true,
    shouldOfferStop: () => true,
    syncVoiceCapability: () => {},
    syncComposerPlaceholder: () => {},
    renderAsyncQuestion: () => {},
  };
  runInNewContext(
    `${lockSource}\n${submitSource}\napplyThreadWriterLock({state: "checking", read_only: true})`,
    context,
  );
  assert.equal(element("sendMode").value, "steer");
  assert.equal(element("sendModeToggle").disabled, false);
  assert.equal(element("submitBtn").disabled, false);
});

test("a command timeout also covers a stalled response body", async () => {
  const source = await readFile(new URL("../src/api.js", import.meta.url), "utf8");
  const sendSource = source.slice(
    source.indexOf("async function sendCommand(request) {"),
    source.indexOf("export function recordPerformance"),
  );
  const context = {
    authenticate: async () => {},
    demoMode: false,
    AbortController,
    window: {
      setTimeout: (callback) => setTimeout(callback, 0),
      clearTimeout,
    },
    fetch: async (_url, options) => ({
      status: 200,
      json: () =>
        new Promise((_resolve, reject) => {
          options.signal.addEventListener("abort", () => reject(new Error("body aborted")));
        }),
    }),
  };
  await assert.rejects(
    runInNewContext(`${sendSource}\nsendCommand({command: "steer"})`, context),
    /request timed out after 30000 ms/,
  );
});

test("a slow history page does not delay writer or queue state", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const openSource = source.slice(
    source.indexOf("async function openThread("),
    source.indexOf("async function refreshThread() {"),
  );
  const locks = [];
  const state = {
    openToken: 0,
    current: null,
    pending: [],
    messageCache: new Map(),
    initialPageSize: 30,
  };
  const context = {
    state,
    $: (id) => ({ textContent: "", id }),
    renderAsyncQuestion: () => {},
    applyThreadWriterLock: (lock) => locks.push(lock.state),
    syncTemporaryForCurrent: () => {},
    resetThreadViewState: () => {},
    refreshComposerStatus: async () => {},
    renderPending: () => {},
    renderProjects: () => {},
    renderRepairHint: () => {},
    renderThreadStatistics: () => {},
    tr: (key) => key,
    setMessageSyncPhase: () => {},
    yieldToBrowser: async () => {},
    refreshActivity: async () => {},
    refreshAsyncQuestions: async () => {},
    fetchMessages: () => new Promise(() => {}),
    command: async (request) => {
      if (request.command === "thread_watch")
        return { writer_lock: { state: "owned" }, thread: { status: { type: "active" } } };
      if (request.command === "pending_messages")
        return { messages: [{ id: "queued-1", thread_id: "manta", status: "queued" }] };
      return { goal: null };
    },
    updateThreadLiveFromStatus: () => {},
    mergePendingResponse: (messages) => messages,
    renderGoalPanel: () => {},
  };
  runInNewContext(
    `let pendingSnapshotGeneration = 0, pendingAppliedGeneration = 0;\n${openSource}\nopenThread({id: "manta", title: "Manta58", cwd: "/tmp"}, {quiet: true})`,
    context,
  );
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(locks, ["checking", "owned"]);
  assert.equal(state.pending[0].status, "queued");
});

test("Queue remains actionable while writer status is still loading", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const queueableSource = source.slice(
    source.indexOf("function currentThreadQueueable() {"),
    source.indexOf("function requireCurrentThreadWriter() {"),
  );
  const state = { current: { id: "manta" }, threadWriterLock: { state: "checking" } };
  const context = { state };
  assert.equal(runInNewContext(`${queueableSource}\ncurrentThreadQueueable()`, context), true);
  state.threadWriterLock.state = "unavailable";
  assert.equal(runInNewContext(`${queueableSource}\ncurrentThreadQueueable()`, context), true);
  state.current = null;
  assert.equal(runInNewContext(`${queueableSource}\ncurrentThreadQueueable()`, context), false);
});

test("pending snapshots apply promptly and never overwrite a newer applied result", async () => {
  const source = await readFile(mainScriptPath, "utf8");
  const refreshSource = source.slice(
    source.indexOf("let pendingSnapshotGeneration = 0;"),
    source.indexOf("async function deletePending(entry, button) {"),
  );
  const resolvers = [];
  const state = { current: { id: "manta" }, pending: [] };
  const context = {
    state,
    command: () => new Promise((resolve) => resolvers.push(resolve)),
    mergePendingResponse: (messages) => messages,
    renderPending: () => {},
  };
  const { refreshPending } = runInNewContext(`${refreshSource}\n({refreshPending})`, context);
  const first = refreshPending();
  const second = refreshPending();
  resolvers[0]({ messages: [{ id: "older" }] });
  await first;
  assert.equal(state.pending[0].id, "older");
  resolvers[1]({ messages: [{ id: "newer" }] });
  await second;
  assert.equal(state.pending[0].id, "newer");
  const third = refreshPending();
  const fourth = refreshPending();
  resolvers[3]({ messages: [{ id: "latest" }] });
  await fourth;
  resolvers[2]({ messages: [{ id: "stale" }] });
  await third;
  assert.equal(state.pending[0].id, "latest");
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
  assert.equal(cache.delete("three"), true);
  assert.equal(cache.get("three"), null);
});

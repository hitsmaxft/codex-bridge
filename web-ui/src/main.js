import "./styles.css";
import {
  $,
  authenticate,
  command,
  demoMode,
  notify,
  recordPerformance,
  requestFilePreview,
  run,
  subscribeEvents,
  timeText,
} from "./api.js";
import { createFilePreviewController } from "./file-preview.js";
import { goalToggleState } from "./goal-state.js";
import { applyLanguage, getLanguage, LANGUAGE_STORAGE_KEY, t as tr } from "./i18n.js";
import { markdownNode } from "./markdown.js";
import { memoryCitationModel } from "./memory-citations.js";
import {
  browserNotificationState,
  disableBrowserNotifications,
  enableBrowserNotifications,
  shouldShowBrowserNotification,
  showBrowserNotification,
} from "./browser-notifications.js";
import {
  completionMatchesActiveTurn,
  effectiveActiveTurnId,
  restoreComposerDraft,
  shouldOfferStop,
} from "./composer-state.js";
import { persistExpandedProjects, storedExpandedProjects } from "./project-state.js";
import { renderRuntimeArchitecture } from "./runtime-architecture.js";
import { taskOverview } from "./task-overview.js";
import {
  rememberSessionId,
  sessionHash,
  sessionIdFromHash,
  storedSessionId,
} from "./session-route.js";
import {
  DRAFT_STORAGE_KEY,
  applyTheme,
  persistDrafts,
  saveDraft,
  state,
  storedTheme,
  watchSystemTheme,
} from "./state.js";
import {
  messageBottomDistance,
  scrollTopForViewportAnchor,
  shouldFollowMessageTail,
} from "./viewport-state.js";
document.documentElement.toggleAttribute("data-demo", demoMode);
const restoredExpandedProjects = storedExpandedProjects(window.localStorage);
if (restoredExpandedProjects !== null) {
  state.expanded = restoredExpandedProjects;
  state.expandedPreferenceSaved = true;
}

function rememberExpandedProjects() {
  state.expandedPreferenceSaved = true;
  persistExpandedProjects(window.localStorage, state.expanded);
}
function setProjectExpanded(projectPath, expanded, persist = true) {
  if (expanded) state.expanded.add(projectPath);
  else state.expanded.delete(projectPath);
  if (persist) rememberExpandedProjects();
}

function browserAudioAvailable() {
  return Boolean(
    demoMode ||
    (window.isSecureContext &&
      navigator.mediaDevices?.getUserMedia &&
      window.MediaRecorder &&
      (window.AudioContext || window.webkitAudioContext)),
  );
}

function formatResourceBytes(value) {
  const bytes = Number(value);
  if (!Number.isFinite(bytes) || bytes < 0) return tr("unknownValue");
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 ** 2).toFixed(bytes < 10 * 1024 ** 2 ? 1 : 0)} MiB`;
}

function renderRuntimeResources() {
  const root = $("resourceStatus"),
    resources = state.runtimeResources;
  if (!root) return;
  const wasOpen = Boolean(root.querySelector("details")?.open);
  root.textContent = "";
  if (!resources) return;
  const session = resources.session_cache || {},
    tools = resources.tool_cache || {},
    project = resources.project_cache || {},
    performanceSummary = resources.performance?.summary || [],
    serverTiming = performanceSummary.find(
      (item) => item.source === "server" && item.metric === "messages_read",
    ),
    clientTiming = performanceSummary.find(
      (item) => item.source === "client" && item.metric === "messages_visible",
    ),
    entry = document.createElement("details"),
    summary = document.createElement("summary"),
    dot = document.createElement("span"),
    name = document.createElement("span"),
    value = document.createElement("span"),
    detail = document.createElement("div");
  entry.className = "component-entry resource-entry";
  entry.open = wasOpen;
  dot.className = "component-dot";
  name.className = "component-name";
  name.textContent = tr("memoryAndCaches");
  value.className = "component-state";
  value.textContent = tr("cacheSummary", {
    used: session.message_entries ?? 0,
    capacity: session.message_capacity ?? 0,
    size: formatResourceBytes(session.rollout_bytes ?? 0),
  });
  detail.className = "component-detail resource-detail";
  for (const text of [
    tr("peakMemory", { size: formatResourceBytes(resources.memory?.peak_rss_bytes) }),
    tr("sessionMessageCache", {
      used: session.message_entries ?? 0,
      capacity: session.message_capacity ?? 0,
      messages: session.messages ?? 0,
    }),
    tr("cachedRollouts", {
      size: formatResourceBytes(session.rollout_bytes ?? 0),
      records: session.tool_records ?? 0,
    }),
    tr("toolParseCache", { threads: tools.threads ?? 0, calls: tools.tool_calls ?? 0 }),
    tr("projectIndexCache", { state: project.indexed ? tr("cached") : tr("uncached") }),
    serverTiming
      ? tr("serverMessageTiming", {
          average: serverTiming.average_ms ?? 0,
          max: serverTiming.max_ms ?? 0,
          count: serverTiming.count ?? 0,
        })
      : null,
    clientTiming
      ? tr("clientMessageTiming", {
          average: clientTiming.average_ms ?? 0,
          max: clientTiming.max_ms ?? 0,
          count: clientTiming.count ?? 0,
        })
      : null,
  ]) {
    if (!text) continue;
    const row = document.createElement("div");
    row.textContent = text;
    detail.appendChild(row);
  }
  summary.append(dot, name, value);
  entry.append(summary, detail);
  root.appendChild(entry);
}

function renderManagedServices() {
  const root = $("componentStatus"),
    services = state.managedServices;
  if (!root) return;
  renderRuntimeArchitecture(
    $("runtimeArchitecture"),
    {
      managedServices: state.managedServices,
      directAppServer: state.directAppServer,
      serverCapabilities: state.serverCapabilities,
      browserMicrophoneAvailable: browserAudioAvailable(),
    },
    tr,
  );
  renderRuntimeResources();
  const expanded = new Set(
    [...root.querySelectorAll("details[open]")].map((entry) => entry.dataset.component),
  );
  root.textContent = "";
  if (!services) return;
  const heading = document.createElement("div");
  heading.className = "component-status-title";
  heading.textContent = tr("managedComponents");
  root.appendChild(heading);
  for (const [key, label, service] of [
    ["app-server", tr("appServerComponent"), services.app_server],
    ["ws-bridge", tr("wsBridgeComponent"), services.desktop_interposition],
    ["whisper", tr("whisperComponent"), services.whisper],
  ]) {
    const status = service?.status || {},
      enabled = Boolean(service?.enabled),
      running = enabled && Boolean(status.running),
      limited = running && service?.capability === "resume_compatibility_only",
      failed = enabled && !running && Boolean(status.last_error),
      entry = document.createElement("details"),
      summary = document.createElement("summary"),
      dot = document.createElement("span"),
      name = document.createElement("span"),
      value = document.createElement("span"),
      detail = document.createElement("div");
    entry.className = `component-entry${limited ? " limited" : running ? " running" : failed ? " failed" : ""}`;
    entry.dataset.component = key;
    entry.open = expanded.has(key);
    dot.className = "component-dot";
    name.className = "component-name";
    name.textContent = label;
    value.className = "component-state";
    value.textContent = !enabled
      ? tr("componentExternal")
      : service?.fallback && !service?.needed
        ? tr("componentStandby")
        : limited
          ? tr("componentLimited")
          : running
            ? tr("componentRunning")
            : tr("componentStopped");
    detail.className = "component-detail";
    if (enabled) {
      const restart = document.createElement("div");
      restart.textContent = tr("restartCount", { count: status.restart_count || 0 });
      detail.appendChild(restart);
    } else {
      const ownership = document.createElement("div");
      ownership.textContent = tr("componentExternalDetail");
      detail.appendChild(ownership);
    }
    if (service?.listen) {
      const listen = document.createElement("div");
      listen.textContent = tr("listenAddress", { address: service.listen });
      detail.appendChild(listen);
    }
    if (key === "whisper" && enabled) {
      for (const text of [
        service.model ? tr("whisperModel", { model: service.model }) : null,
        service.language ? tr("whisperLanguage", { language: service.language }) : null,
        service.prompt ? tr("whisperPrompt", { prompt: service.prompt }) : null,
        tr(service.simplify_chinese ? "whisperSimplifiedEnabled" : "whisperSimplifiedDisabled"),
        service.threads ? tr("whisperThreads", { count: service.threads }) : null,
      ]) {
        if (!text) continue;
        const parameter = document.createElement("div");
        parameter.textContent = text;
        detail.appendChild(parameter);
      }
    }
    if (key === "ws-bridge" && service?.capability === "resume_compatibility_only") {
      const limitation = document.createElement("div");
      limitation.textContent = tr("desktopMcpCompatibilityOnly");
      detail.appendChild(limitation);
    }
    if (key === "ws-bridge" && service?.max_frame_bytes) {
      const limits = document.createElement("div");
      limits.textContent = tr("webSocketLimits", {
        frame: formatResourceBytes(service.max_frame_bytes),
        message: formatResourceBytes(service.max_message_bytes),
      });
      detail.appendChild(limits);
    }
    if (enabled) {
      const error = document.createElement("div");
      error.textContent = status.last_error
        ? tr("lastStartupError", { error: status.last_error })
        : tr("noStartupError");
      detail.appendChild(error);
    }
    if (key === "app-server" && enabled) {
      const restartButton = document.createElement("button");
      restartButton.className = "component-action";
      restartButton.type = "button";
      restartButton.disabled = appServerRestartPending;
      restartButton.textContent = tr("restartAppServer");
      restartButton.onclick = async (event) => {
        event.stopPropagation();
        if (!window.confirm(tr("restartAppServerConfirm"))) return;
        appServerRestartPending = true;
        appServerRestartBaseline = Number(status.restart_count || 0);
        renderManagedServices();
        try {
          await command({ command: "managed_app_server_restart" }, false);
          notify(tr("restartAppServerRequested"));
        } catch (error) {
          appServerRestartPending = false;
          renderManagedServices();
          notify(error.message, true);
        }
      };
      detail.appendChild(restartButton);
    }
    summary.append(dot, name, value);
    entry.append(summary, detail);
    root.appendChild(entry);
  }
}

async function loadStatus() {
  const r = await command({ command: "status" }, false),
    backend = r.write_backend || {};
  state.directAppServer = Boolean(backend.app_server_available);
  state.appServerMode = backend.app_server_mode || null;
  state.managedServices = r.managed_services || null;
  state.serverCapabilities = r.capabilities || null;
  state.runtimeResources = r.runtime_resources || null;
  renderManagedServices();
  syncVoiceCapability();
  const appServer = state.directAppServer
    ? tr("directOnline")
    : state.appServerMode === "desktop_bundled_only"
      ? tr("bundledPrivate")
      : tr("directOffline");
  const base = r.demo
    ? tr("demoReady", { protocol: r.protocol_version })
    : tr("bridgeReady", {
        protocol: r.protocol_version,
        backend: appServer,
      });
  const schemaVersion = backend.schema_version,
    runtimeVersion = backend.runtime_version,
    runtimeBase = runtimeVersion?.replace(/-(?:bundled|standalone)$/, ""),
    mismatch = Boolean(schemaVersion && runtimeBase && schemaVersion !== runtimeBase);
  $("bridgeState").textContent = [
    base,
    schemaVersion ? `schema ${schemaVersion}` : "",
    runtimeVersion ? `app-server ${runtimeVersion}` : "",
  ]
    .filter(Boolean)
    .join(" · ");
  $("bridgeState").classList.toggle("version-mismatch", mismatch);
  $("bridgeState").title = mismatch
    ? `Schema ${schemaVersion} does not match app-server ${runtimeVersion}`
    : backend.runtime_user_agent || "";
}
function setThreadHeaderExpanded(expanded) {
  if (!matchMedia("(max-width:800px)").matches) expanded = false;
  const header = document.querySelector(".thread-head"),
    title = $("threadTitle");
  header.classList.toggle("expanded", expanded);
  title.setAttribute("aria-expanded", String(expanded));
  title.title = tr(expanded ? "collapseHeader" : "expandHeader");
}
function isHistoryFullscreen() {
  return document.querySelector("main").classList.contains("history-fullscreen");
}
function syncHistoryFullscreenButton() {
  const fullscreen = isHistoryFullscreen(),
    button = $("historyFullscreenBtn"),
    labelKey = fullscreen ? "exitHistoryFullscreen" : "enterHistoryFullscreen";
  button.setAttribute("aria-pressed", String(fullscreen));
  button.dataset.i18nAriaLabel = labelKey;
  button.setAttribute("aria-label", tr(labelKey));
  button.title = tr(labelKey);
}
function setHistoryFullscreen(fullscreen) {
  document.querySelector("main").classList.toggle("history-fullscreen", fullscreen);
  document.documentElement.classList.toggle("history-fullscreen", fullscreen);
  setThreadHeaderExpanded(false);
  syncHistoryFullscreenButton();
}
function renderRepairHint(required = state.repairRequired) {
  state.repairRequired = Boolean(required);
  const button = $("repairHintBtn"),
    bubble = $("repairHintBubble");
  button.hidden = !state.current || !state.repairRequired;
  if (button.hidden) {
    bubble.hidden = true;
    button.setAttribute("aria-expanded", "false");
  }
}
function formatSessionDuration(milliseconds) {
  const seconds = Math.max(0, Math.round(Number(milliseconds || 0) / 1000));
  if (seconds < 60) return tr("turnDurationSeconds", { seconds });
  return tr("turnDurationMinutes", {
    minutes: Math.floor(seconds / 60),
    seconds: seconds % 60,
  });
}
function renderThreadStatistics(statistics = state.threadStatistics) {
  state.threadStatistics = statistics || null;
  const section = $("threadStatistics"),
    grid = $("threadStatGrid");
  section.hidden = !state.current || !statistics;
  grid.textContent = "";
  if (section.hidden) return;
  const active =
    Number(statistics.turns || 0) >
    Number(statistics.completed_turns || 0) + Number(statistics.cancelled_turns || 0);
  const values = [
    [tr("sessionDuration"), formatSessionDuration(statistics.total_duration_ms)],
    [
      tr("sessionTurns"),
      active
        ? tr("activeTurnIncluded", { count: statistics.turns || 0 })
        : String(statistics.turns || 0),
    ],
    [tr("sessionTools"), Number(statistics.tool_calls || 0).toLocaleString()],
    [tr("sessionTokens"), Number(statistics.total_tokens || 0).toLocaleString()],
  ];
  for (const [label, value] of values) {
    const item = document.createElement("div"),
      name = document.createElement("span"),
      amount = document.createElement("strong");
    name.textContent = label;
    amount.textContent = value;
    item.append(name, amount);
    grid.appendChild(item);
  }
}
const GOAL_PANEL_COLLAPSED_KEY = "codex-bridge.goal-panel-collapsed.v1";
let goalPanelCollapsed = window.localStorage.getItem(GOAL_PANEL_COLLAPSED_KEY) === "1";

function formatGoalDuration(value) {
  const seconds = Math.max(0, Math.floor(Number(value) || 0));
  if (seconds < 60) return tr("goalSeconds", { count: seconds });
  if (seconds < 3600) return tr("goalMinutes", { count: Math.floor(seconds / 60) });
  return tr("goalHours", { count: (seconds / 3600).toFixed(seconds < 36_000 ? 1 : 0) });
}
function renderGoalPanel() {
  const goal = state.threadGoal,
    panel = $("goalPanel"),
    restore = $("goalRestoreBtn");
  panel.hidden = !goal || goalPanelCollapsed;
  restore.hidden = !goal || !goalPanelCollapsed;
  if (!goal) return;
  const status = goal.status || "active",
    statusKey =
      {
        active: "goalActive",
        paused: "goalPaused",
        blocked: "goalBlocked",
        usageLimited: "goalUsageLimited",
        budgetLimited: "goalBudgetLimited",
        complete: "goalComplete",
      }[status] || "goalUnknown";
  $("goalStatus").dataset.status = status;
  $("goalStatus").textContent = tr(statusKey);
  $("goalTime").textContent = tr("goalElapsed", {
    time: formatGoalDuration(goal.timeUsedSeconds),
  });
  $("goalObjective").textContent = goal.objective || "";
  const tokenBudget = Number(goal.tokenBudget),
    tokensUsed = Math.max(0, Number(goal.tokensUsed) || 0),
    hasBudget = Number.isFinite(tokenBudget) && tokenBudget > 0;
  $("goalBudget").hidden = !hasBudget;
  if (hasBudget) {
    $("goalBudgetText").textContent =
      `${tokensUsed.toLocaleString()} / ${tokenBudget.toLocaleString()}`;
    const percent = Math.min(100, Math.max(0, (tokensUsed / tokenBudget) * 100));
    $("goalProgress").style.setProperty("--goal-progress", `${percent.toFixed(1)}%`);
    $("goalProgress").setAttribute("aria-valuenow", String(Math.round(percent)));
    $("goalProgress").setAttribute("aria-valuemin", "0");
    $("goalProgress").setAttribute("aria-valuemax", "100");
  }
  const toggle = $("goalToggleBtn"),
    { canPause, canResume } = goalToggleState(status);
  toggle.hidden = !canPause && !canResume;
  toggle.disabled = state.goalBusy;
  toggle.textContent = tr(canPause ? "pauseGoal" : "resumeGoal");
  $("goalEditBtn").disabled = state.goalBusy;
  $("goalActionHelp").textContent = tr(
    canPause ? "pauseGoalHelp" : canResume ? "resumeGoalHelp" : "goalReadOnlyHelp",
  );
}
async function setThreadGoal(change, successKey) {
  if (!state.current || !state.threadGoal || state.goalBusy) return;
  const threadId = state.current.id;
  state.goalBusy = true;
  renderGoalPanel();
  try {
    const result = await command(
      { command: "thread_goal_set", thread_id: threadId, ...change },
      false,
    );
    if (state.current?.id === threadId) {
      state.threadGoal = result.goal || null;
      renderGoalPanel();
    }
    notify(tr(successKey));
  } finally {
    state.goalBusy = false;
    renderGoalPanel();
  }
}
function setGoalPanelCollapsed(collapsed) {
  goalPanelCollapsed = collapsed;
  window.localStorage.setItem(GOAL_PANEL_COLLAPSED_KEY, collapsed ? "1" : "0");
  renderGoalPanel();
}
function openGoalEditDialog() {
  if (!state.threadGoal) return;
  $("goalObjectiveInput").value = state.threadGoal.objective || "";
  $("goalEditDialog").hidden = false;
  requestAnimationFrame(() => $("goalObjectiveInput").focus());
}
function closeGoalEditDialog() {
  $("goalEditDialog").hidden = true;
}
async function toggleLanguage() {
  applyLanguage(getLanguage() === "en" ? "zh" : "en");
  syncHistoryFullscreenButton();
  syncComposerPlaceholder();
  setSendMode($("sendMode").value, state.modeAutomatic);
  renderProjects();
  renderPending();
  renderComposerAttachments();
  renderComposerReference();
  syncVoiceCapability();
  renderManagedServices();
  renderTasksButton();
  renderTaskOverviews();
  renderBrowserNotifications();
  renderGoalPanel();
  showActivity();
  await loadStatus();
  if (state.current) await openThread(state.current, { quiet: true });
}
async function loadProjects() {
  const [r] = await Promise.all([
    command({ command: "projects", include_archived: $("archived").checked }, false),
    loadPins(),
  ]);
  state.projects = r.projects || [];
  state.projectThreads.clear();
  state.projectLoadGeneration += 1;
  const knownPaths = new Set(state.projects.map((project) => project.path));
  let removedStalePath = false;
  for (const path of state.expanded) {
    if (knownPaths.has(path)) continue;
    state.expanded.delete(path);
    removedStalePath = true;
  }
  if (removedStalePath && state.expandedPreferenceSaved) rememberExpandedProjects();
  renderProjects();
  if (!state.current) {
    const hashedId = sessionIdFromHash(window.location.hash),
      requestedId = hashedId || storedSessionId(window.localStorage);
    if (requestedId) {
      try {
        await openSessionById(requestedId, {
          fromHash: Boolean(hashedId),
          replaceHash: !hashedId,
        });
        preloadExpandedProjectThreads();
        return;
      } catch {
        state.current = null;
      }
    }
    if (state.projects.length) {
      const project = state.projects[0];
      if (!state.expandedPreferenceSaved) setProjectExpanded(project.path, true);
      const threads = await loadProjectThreads(project);
      if (threads.length) await openThread(threads[0], { replaceHash: true });
    }
  }
  preloadExpandedProjectThreads();
}
async function loadPins() {
  if (!state.directAppServer) {
    state.pinAvailable = false;
    state.pinnedIds.clear();
    state.pinnedThreads = [];
    return;
  }
  try {
    const result = await command({ command: "thread_pins" }, false),
      ids = Array.isArray(result.thread_ids) ? result.thread_ids : [];
    state.pinAvailable = Boolean(result.available);
    state.pinnedIds = new Set(ids);
    const returned = new Map(
      (Array.isArray(result.threads) ? result.threads : []).map((thread) => [thread.id, thread]),
    );
    for (const data of state.projectThreads.values()) {
      for (const thread of data.threads) {
        if (!returned.has(thread.id)) returned.set(thread.id, thread);
      }
    }
    state.pinnedThreads = ids
      .map((id) => returned.get(id))
      .filter(Boolean)
      .map((thread) => ({ ...thread, pinned: true }));
    const ranks = new Map(ids.map((id, rank) => [id, rank]));
    for (const data of state.projectThreads.values()) {
      for (const thread of data.threads) thread.pinned = state.pinnedIds.has(thread.id);
      data.threads.sort(
        (left, right) =>
          (ranks.get(left.id) ?? Number.MAX_SAFE_INTEGER) -
          (ranks.get(right.id) ?? Number.MAX_SAFE_INTEGER),
      );
    }
  } catch {
    state.pinAvailable = false;
    state.pinnedIds.clear();
    state.pinnedThreads = [];
  }
}
async function loadProjectThreads(project, offset = 0) {
  const generation = state.projectLoadGeneration,
    includeArchived = $("archived").checked,
    key = `${generation}:${includeArchived ? 1 : 0}:${project.path}:${offset}`;
  if (state.projectThreadLoads.has(key)) return state.projectThreadLoads.get(key);
  const request = (async () => {
    const r = await command(
      {
        command: "project_threads",
        project_path: project.path,
        include_archived: includeArchived,
        offset,
        limit: 50,
      },
      false,
    );
    if (generation !== state.projectLoadGeneration || includeArchived !== $("archived").checked)
      return [];
    const prior = offset ? state.projectThreads.get(project.path)?.threads || [] : [];
    state.projectThreads.set(project.path, {
      threads: prior.concat(
        (r.threads || []).map((thread) => ({ ...thread, project_path: project.path })),
      ),
      available: r.available || 0,
    });
    renderProjects();
    return state.projectThreads.get(project.path).threads;
  })();
  state.projectThreadLoads.set(key, request);
  try {
    return await request;
  } finally {
    if (state.projectThreadLoads.get(key) === request) state.projectThreadLoads.delete(key);
  }
}
function preloadExpandedProjectThreads() {
  const projects = state.projects.filter(
    (project) => state.expanded.has(project.path) && !state.projectThreads.has(project.path),
  );
  let next = 0;
  const worker = async () => {
    while (next < projects.length) {
      const project = projects[next++];
      await loadProjectThreads(project).catch(() => {});
    }
  };
  void Promise.allSettled(Array.from({ length: Math.min(3, projects.length) }, worker));
}
async function toggleProject(project, autoOpen = false) {
  if (state.expanded.has(project.path) && !autoOpen) {
    setProjectExpanded(project.path, false);
    renderProjects();
    return;
  }
  setProjectExpanded(project.path, true);
  let threads = state.projectThreads.get(project.path)?.threads;
  if (!threads) threads = await loadProjectThreads(project);
  else renderProjects();
  if (autoOpen && !state.current && threads.length) await openThread(threads[0]);
}
async function toggleThreadPin(thread) {
  if (!state.pinAvailable || state.pinBusy.has(thread.id)) return;
  const pinned = !thread.pinned,
    project = pinnedProject(thread);
  state.pinBusy.add(thread.id);
  renderProjects();
  try {
    await command({ command: "thread_pin", thread_id: thread.id, pinned }, false);
    if (pinned) state.pinnedIds.add(thread.id);
    else state.pinnedIds.delete(thread.id);
    await loadPins();
    if (!pinned && project && state.projectThreads.has(project.path)) {
      await loadProjectThreads(project);
    }
    notify(tr(pinned ? "sessionPinned" : "sessionUnpinned"));
  } finally {
    state.pinBusy.delete(thread.id);
    renderProjects();
  }
}
function threadMatches(thread, query) {
  return `${thread.title || ""} ${thread.id} ${thread.cwd || ""} ${thread.git_branch || ""}`
    .toLowerCase()
    .includes(query);
}
function setThreadRunState(threadId, runState) {
  if (!threadId || !["active", "completed", "cancelled", "failed"].includes(runState)) return;
  state.threadRunStates.set(threadId, runState);
  if (runState === "active") {
    state.updatingThreads.add(threadId);
    trackTask(threadId);
  } else state.updatingThreads.delete(threadId);
  if (state.taskTrackedIds.has(threadId)) markTaskDirty(threadId);
  renderTasksButton();
}
function completedTurnRunState(turn) {
  if (["interrupted", "cancelled", "canceled"].includes(turn?.status)) return "cancelled";
  if (turn?.status === "failed" || turn?.error) return "failed";
  if (["inProgress", "active"].includes(turn?.status)) return "active";
  return "completed";
}
function knownThread(threadId) {
  return (
    state.pinnedThreads.find((thread) => thread.id === threadId) ||
    [...state.projectThreads.values()]
      .flatMap((entry) => entry.threads)
      .find((thread) => thread.id === threadId) ||
    (state.current?.id === threadId ? state.current : null)
  );
}
function renderBrowserNotifications() {
  const status = browserNotificationState(globalThis.Notification, window.localStorage),
    button = $("notificationBtn"),
    help = $("notificationHelp");
  if (!button || !help) return;
  button.disabled = !status.supported || status.permission === "denied";
  button.dataset.enabled = String(status.enabled);
  button.textContent = tr(
    !status.supported
      ? "notificationsUnsupported"
      : status.permission === "denied"
        ? "notificationsBlocked"
        : status.enabled
          ? "disableNotifications"
          : "enableNotifications",
  );
  help.textContent = tr(
    !status.supported
      ? "notificationsUnsupportedHelp"
      : status.permission === "denied"
        ? "notificationsBlockedHelp"
        : status.enabled
          ? "notificationsEnabledHelp"
          : "notificationsDisabledHelp",
  );
}
async function toggleBrowserNotifications() {
  const status = browserNotificationState(globalThis.Notification, window.localStorage),
    next = status.enabled
      ? disableBrowserNotifications(globalThis.Notification, window.localStorage)
      : await enableBrowserNotifications(globalThis.Notification, window.localStorage);
  renderBrowserNotifications();
  if (next.permission === "granted") {
    notify(tr(next.enabled ? "notificationsEnabled" : "notificationsDisabled"));
  }
}
let deferredCompletionToast = null,
  previousAppServerService = null,
  appServerRestartPending = false,
  appServerRestartBaseline = 0;

function notifyServiceEvent(body, bad = false) {
  const useSystemNotification = shouldShowBrowserNotification({
    NotificationApi: globalThis.Notification,
    storage: window.localStorage,
    documentHidden: document.hidden,
    windowFocused: document.hasFocus(),
  });
  if (useSystemNotification) {
    showBrowserNotification(globalThis.Notification, {
      title: tr("appServerComponent"),
      body,
      tag: "codex-bridge:app-server",
      onClick: () => window.focus(),
    });
  } else {
    notify(body, bad);
  }
}

function observeAppServerService(service) {
  if (!service) return;
  const next = {
      running: Boolean(service.enabled && service.status?.running),
      restartCount: Number(service.status?.restart_count || 0),
      error: service.status?.last_error || null,
    },
    previous = previousAppServerService;
  previousAppServerService = next;
  if (!previous) return;
  if (appServerRestartPending && next.running && next.restartCount > appServerRestartBaseline) {
    appServerRestartPending = false;
    notifyServiceEvent(tr("appServerRecovered"));
    return;
  }
  if (previous.running && !next.running) {
    notifyServiceEvent(
      next.error ? tr("appServerStopped", { reason: next.error }) : tr("appServerUnavailable"),
      true,
    );
  } else if (!previous.running && next.running) {
    notifyServiceEvent(tr("appServerRecovered"));
  }
}
function notifyTurnFinished(threadId, runState, turnId) {
  if (turnId) {
    if (state.notifiedTurnIds.has(turnId)) return;
    state.notifiedTurnIds.add(turnId);
    while (state.notifiedTurnIds.size > 100) {
      state.notifiedTurnIds.delete(state.notifiedTurnIds.values().next().value);
    }
  }
  const thread = knownThread(threadId),
    title = thread?.title || tr("session"),
    body = tr(
      runState === "failed"
        ? "notificationRunFailed"
        : runState === "cancelled"
          ? "notificationRunCancelled"
          : "notificationRunCompleted",
    ),
    notificationState = browserNotificationState(globalThis.Notification, window.localStorage),
    useSystemNotification = shouldShowBrowserNotification({
      NotificationApi: globalThis.Notification,
      storage: window.localStorage,
      documentHidden: document.hidden,
      windowFocused: document.hasFocus(),
    });
  if (!useSystemNotification) {
    if (!notificationState.supported || notificationState.permission !== "granted") {
      const text = `${title} · ${body}`;
      if (document.hidden) deferredCompletionToast = text;
      else notify(text, false, "completion");
    }
    return;
  }
  showBrowserNotification(globalThis.Notification, {
    title,
    body,
    tag: `codex-bridge:${threadId}`,
    onClick: () => {
      window.focus();
      window.location.hash = sessionHash(threadId);
    },
  });
}
document.addEventListener("visibilitychange", () => {
  if (document.hidden || !deferredCompletionToast) return;
  notify(deferredCompletionToast, false, "completion");
  deferredCompletionToast = null;
});
function trackTask(threadId) {
  if (!threadId) return;
  state.taskTrackedIds.add(threadId);
  const terminalIds = [...state.taskTrackedIds].filter(
    (id) => state.threadRunStates.get(id) !== "active",
  );
  while (terminalIds.length > 8) {
    const stale = terminalIds.shift();
    state.taskTrackedIds.delete(stale);
    state.taskOverviews.delete(stale);
    state.taskDirtyIds.delete(stale);
  }
}
function markTaskDirty(threadId) {
  if (!state.taskTrackedIds.has(threadId)) return;
  state.taskDirtyIds.add(threadId);
  scheduleTasksRefresh();
}
function taskStateLabel(runState) {
  return tr(
    runState === "active"
      ? "taskActive"
      : runState === "completed"
        ? "taskCompleted"
        : runState === "cancelled"
          ? "taskCancelled"
          : "taskFailed",
  );
}
function renderTasksButton() {
  const count = [...state.threadRunStates.values()].filter((value) => value === "active").length,
    badge = $("tasksCount"),
    button = $("tasksBtn");
  if (!badge || !button) return;
  badge.textContent = String(count);
  badge.hidden = count === 0;
  button.classList.toggle("has-active", count > 0);
  button.setAttribute("aria-label", tr("tasksAriaCount", { count }));
}
function taskMessageNode(
  text,
  className,
  threadId,
  { collapsible = false, expanded = false } = {},
) {
  const message = document.createElement("section"),
    body = document.createElement("div");
  message.className = `message ${className} task-message`;
  body.className = "message-body";
  body.appendChild(markdownNode(text, markdownOptions(threadId)));
  message.appendChild(body);
  if (collapsible && (text.length > 360 || text.split("\n").length > 8)) {
    const toggle = document.createElement("button"),
      sync = () => {
        message.classList.toggle("expanded", expanded);
        toggle.textContent = tr(expanded ? "showLess" : "showMore");
        toggle.setAttribute("aria-expanded", String(expanded));
      };
    message.classList.add("collapsible");
    toggle.className = "task-message-toggle";
    toggle.type = "button";
    toggle.onclick = () => {
      expanded = !expanded;
      sync();
    };
    sync();
    message.appendChild(toggle);
  }
  return message;
}
function taskEventNode(overview, runState, threadId) {
  const event = document.createElement("section"),
    body = document.createElement("div");
  event.className = "message assistant task-message task-event";
  body.className = "message-body";
  if (overview?.activity?.phase === "compacting") {
    body.textContent = tr("taskCompacting");
  } else if (overview?.activity?.active_tool || overview?.latestTool) {
    const activeToolName = overview.activity?.active_tool,
      tool =
        activeToolName && overview.latestTool?.name !== activeToolName
          ? { name: activeToolName, preview: activeToolName, status: "running" }
          : overview.latestTool || {
              name: activeToolName,
              preview: activeToolName,
              status: "running",
            },
      row = document.createElement("div"),
      icon = document.createElement("span"),
      text = document.createElement("span"),
      running = runState === "active" && (activeToolName === tool.name || !toolFinished(tool));
    row.className = "task-event-tool";
    icon.className = `tool-icon ${toolIconClass(tool.name)}`;
    text.textContent = `${toolActionText(tool.name, running)} ${toolSummaryPreview(tool)}`.trim();
    row.append(icon, text);
    body.appendChild(row);
  } else if (runState === "active") {
    body.textContent = tr("taskThinking");
  } else {
    body.textContent = taskStateLabel(runState);
  }
  event.appendChild(body);
  return event;
}
function taskCardNode(threadId, overview, runState, replyExpanded = false) {
  const card = document.createElement("article"),
    head = document.createElement("header"),
    title = document.createElement("h3"),
    open = document.createElement("a"),
    status = document.createElement("span"),
    thread = overview?.thread || knownThread(threadId);
  card.className = `task-entry ${runState}`;
  card.dataset.threadId = threadId;
  head.className = "task-entry-head";
  title.className = "task-entry-title";
  title.textContent = thread?.title || threadId;
  open.className = "task-open";
  open.href = sessionHash(threadId);
  open.textContent = tr("openSession");
  open.onclick = (event) => {
    event.preventDefault();
    closeTasksDialog();
    run(() => openSessionById(threadId));
  };
  status.className = `thread-run-state ${runState} task-status`;
  status.title = taskStateLabel(runState);
  status.setAttribute("aria-label", status.title);
  head.append(title, open, status);
  card.appendChild(head);
  if (overview?.userText) card.appendChild(taskMessageNode(overview.userText, "user", threadId));
  if (overview?.assistantText)
    card.appendChild(
      taskMessageNode(overview.assistantText, "assistant", threadId, {
        collapsible: true,
        expanded: replyExpanded,
      }),
    );
  if (overview?.error) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = tr("taskLoadFailed");
    card.appendChild(empty);
  } else if (overview) {
    if (runState === "active" || !overview.assistantText)
      card.appendChild(taskEventNode(overview, runState, threadId));
  } else {
    const loading = document.createElement("div");
    loading.className = "empty";
    loading.textContent = tr("loadingLatest");
    card.appendChild(loading);
  }
  card.dataset.signature = JSON.stringify({ overview, runState, language: getLanguage() });
  return card;
}
function renderTaskOverviews() {
  const root = $("tasksList");
  if (!root) return;
  const existing = new Map(
      [...root.querySelectorAll(":scope > .task-entry")].map((card) => [
        card.dataset.threadId,
        card,
      ]),
    ),
    ids = [...state.taskTrackedIds].sort((left, right) => {
      const leftActive = state.threadRunStates.get(left) === "active" ? 1 : 0,
        rightActive = state.threadRunStates.get(right) === "active" ? 1 : 0;
      return rightActive - leftActive;
    }),
    fragment = document.createDocumentFragment();
  if (!ids.length) {
    const empty = document.createElement("div");
    empty.className = "empty";
    empty.textContent = tr("noTrackedTasks");
    root.replaceChildren(empty);
    return;
  }
  for (const threadId of ids) {
    const overview = state.taskOverviews.get(threadId),
      runState = state.threadRunStates.get(threadId) || "active",
      signature = JSON.stringify({ overview, runState, language: getLanguage() }),
      previous = existing.get(threadId);
    fragment.appendChild(
      previous?.dataset.signature === signature
        ? previous
        : taskCardNode(
            threadId,
            overview,
            runState,
            Boolean(previous?.querySelector(".task-message.assistant.expanded")),
          ),
    );
  }
  root.replaceChildren(fragment);
}
let tasksRefreshTimer = null;
function scheduleTasksRefresh(delay = 180) {
  if (!state.tasksOpen || tasksRefreshTimer) return;
  tasksRefreshTimer = setTimeout(() => {
    tasksRefreshTimer = null;
    refreshTaskOverviews().catch(() => {});
  }, delay);
}
async function refreshTaskOverviews(force = false) {
  if (!state.tasksOpen || state.tasksRefreshing) return;
  const ids = [...state.taskTrackedIds].filter(
    (threadId) =>
      force ||
      !state.taskOverviews.has(threadId) ||
      state.taskDirtyIds.has(threadId) ||
      state.threadRunStates.get(threadId) === "active",
  );
  if (!ids.length) return renderTaskOverviews();
  state.tasksRefreshing = true;
  $("tasksBtn").classList.add("refreshing");
  for (const threadId of ids) state.taskDirtyIds.delete(threadId);
  try {
    await Promise.all(
      ids.map(async (threadId) => {
        try {
          const [messages, activity] = await Promise.all([
            command({ command: "messages", thread_id: threadId, before: null, limit: 20 }, false),
            command({ command: "thread_activity", thread_id: threadId }, false),
          ]);
          const overview = taskOverview(messages, activity),
            runState = state.threadRunStates.get(threadId),
            previousAttempts = state.taskOverviews.get(threadId)?.terminalRefreshes || 0;
          if (runState !== "active" && !overview.assistantText && previousAttempts < 4) {
            overview.terminalRefreshes = previousAttempts + 1;
            state.taskDirtyIds.add(threadId);
          }
          state.taskOverviews.set(threadId, overview);
        } catch {
          if (!state.taskOverviews.has(threadId))
            state.taskOverviews.set(threadId, { error: true });
          state.taskDirtyIds.add(threadId);
        }
      }),
    );
  } finally {
    state.tasksRefreshing = false;
    $("tasksBtn").classList.remove("refreshing");
    renderTaskOverviews();
  }
}
function openTasksDialog() {
  for (const [threadId, runState] of state.threadRunStates) {
    if (runState === "active") trackTask(threadId);
  }
  state.tasksOpen = true;
  $("tasksDialog").hidden = false;
  renderTaskOverviews();
  refreshTaskOverviews(true).catch(() => {});
}
function closeTasksDialog() {
  state.tasksOpen = false;
  $("tasksDialog").hidden = true;
  clearTimeout(tasksRefreshTimer);
  tasksRefreshTimer = null;
}
function threadRow(thread) {
  const row = document.createElement("div");
  row.className = `thread-row${state.pinAvailable && !thread.archived ? "" : " no-pin"}`;
  const button = document.createElement("button");
  button.className = "thread" + (state.current?.id === thread.id ? " active" : "");
  button.innerHTML = '<div class="thread-name"></div><div class="thread-meta"></div>';
  const name = document.createElement("span");
  name.textContent = thread.title || thread.id;
  button.children[0].appendChild(name);
  const runState =
    state.threadRunStates.get(thread.id) ||
    (state.updatingThreads.has(thread.id) ? "active" : null);
  if (runState) {
    const indicator = document.createElement("span"),
      label = tr(
        runState === "active"
          ? "modelRunning"
          : runState === "completed"
            ? "runCompleted"
            : "runFailedOrCancelled",
      );
    indicator.className = `thread-run-state ${runState}`;
    indicator.setAttribute("aria-label", label);
    indicator.title = label;
    button.children[0].appendChild(indicator);
  }
  button.children[1].textContent = `${thread.git_branch || tr("noBranch")} · ${timeText(thread.updated_at_ms, getLanguage() === "zh" ? "zh-CN" : "en")}${thread.archived ? ` · ${tr("archived")}` : ""}`;
  button.onclick = () => openThread(thread);
  row.appendChild(button);
  if (state.pinAvailable && !thread.archived) {
    const pin = document.createElement("button"),
      label = tr(thread.pinned ? "unpinSession" : "pinSession");
    pin.className = `thread-pin${thread.pinned ? " pinned" : ""}`;
    pin.type = "button";
    pin.innerHTML =
      '<svg aria-hidden="true" viewBox="0 0 24 24"><path d="M9 3h6l-1 6 3 3v2H7v-2l3-3-1-6Zm3 11v7" /></svg>';
    pin.title = label;
    pin.setAttribute("aria-label", label);
    pin.setAttribute("aria-pressed", String(Boolean(thread.pinned)));
    pin.disabled = state.pinBusy.has(thread.id);
    pin.onclick = () => run(() => toggleThreadPin(thread));
    row.appendChild(pin);
  }
  return row;
}
function pinnedProject(thread) {
  if (thread.project_path) {
    const assigned = state.projects.find((project) => project.path === thread.project_path);
    if (assigned) return assigned;
  }
  for (const [projectPath, data] of state.projectThreads) {
    if (data.threads.some((candidate) => candidate.id === thread.id))
      return state.projects.find((project) => project.path === projectPath);
  }
  const cwd = String(thread.cwd || "").replace(/\/+$/, "");
  return state.projects
    .filter((project) => {
      const path = String(project.path).replace(/\/+$/, "");
      return cwd === path || cwd.startsWith(`${path}/`);
    })
    .sort((left, right) => String(right.path).length - String(left.path).length)[0];
}
function updateSessionHash(threadId, replace = false) {
  const hash = sessionHash(threadId);
  if (window.location.hash === hash) return;
  window.history[replace ? "replaceState" : "pushState"](null, "", hash);
}
async function revealSessionProject(thread) {
  if (state.pinnedIds.has(thread.id)) return renderProjects();
  const project = pinnedProject(thread);
  if (!project) return renderProjects();
  setProjectExpanded(project.path, true);
  if (!state.projectThreads.has(project.path)) await loadProjectThreads(project);
  else renderProjects();
}
async function openSessionById(threadId, { fromHash = false, replaceHash = false } = {}) {
  const previous = state.current;
  const known = state.pinnedThreads.find((thread) => thread.id === threadId) ||
    [...state.projectThreads.values()]
      .flatMap((entry) => entry.threads)
      .find((thread) => thread.id === threadId) || {
      id: threadId,
      title: threadId,
      cwd: "",
      git_branch: null,
    };
  try {
    await openThread(known, { writeHash: !fromHash, replaceHash });
    await revealSessionProject(state.current);
  } catch (error) {
    state.current = previous;
    renderProjects();
    if (previous) await openThread(previous, { quiet: true, writeHash: false });
    throw error;
  }
}
function renderProjects() {
  const q = $("search").value.trim().toLowerCase(),
    root = $("projects");
  root.textContent = "";
  const pinned = state.pinnedThreads.filter((thread) => threadMatches(thread, q));
  const projects = state.projects.filter(
    (p) =>
      `${p.kind === "chats" ? tr("chats") : p.name} ${p.kind === "chats" ? "" : p.path}`
        .toLowerCase()
        .includes(q) ||
      state.projectThreads
        .get(p.path)
        ?.threads.some((t) => !state.pinnedIds.has(t.id) && threadMatches(t, q)),
  );
  if (!projects.length && !pinned.length) {
    root.innerHTML = `<div class="empty" style="padding:12px">${tr("noMatchingProjects")}</div>`;
    return;
  }
  if (pinned.length) {
    const section = document.createElement("section"),
      heading = document.createElement("div"),
      list = document.createElement("div");
    section.className = "pinned-section";
    heading.className = "pinned-heading";
    heading.innerHTML = '<span class="pinned-title"></span><span class="pinned-count"></span>';
    heading.children[0].textContent = tr("pinnedSessions");
    heading.children[1].textContent = `${pinned.length}`;
    list.className = "pinned-threads";
    for (const thread of pinned) list.appendChild(threadRow(thread));
    section.append(heading, list);
    root.appendChild(section);
  }
  for (const p of projects) {
    const wrap = document.createElement("section");
    wrap.className = "project";
    const head = document.createElement("div");
    head.className = "project-head";
    const button = document.createElement("button");
    button.className = "project-button";
    button.innerHTML =
      '<span class="chevron"></span><span class="project-name"></span><span class="project-count"></span>';
    button.children[0].textContent = state.expanded.has(p.path) ? "▾" : "▸";
    button.children[1].textContent = p.kind === "chats" ? tr("chats") : p.name;
    const pinnedCount = state.pinnedThreads.filter(
      (thread) => pinnedProject(thread)?.path === p.path,
    ).length;
    button.children[2].textContent = `${Math.max(0, p.thread_count - pinnedCount)}`;
    button.onclick = () => run(() => toggleProject(p));
    head.appendChild(button);
    wrap.appendChild(head);
    const list = document.createElement("div");
    list.className = "project-threads";
    list.hidden = !state.expanded.has(p.path);
    const data = state.projectThreads.get(p.path);
    if (!data) {
      list.innerHTML = `<div class="empty" style="padding:8px">${tr("loadingSessions")}</div>`;
    } else {
      const creation = state.threadCreationJobs.get(p.path);
      if (creation) {
        const placeholder = document.createElement("div");
        placeholder.className = "thread creation-placeholder";
        placeholder.innerHTML = '<span class="creation-spinner"></span><span></span>';
        placeholder.children[1].textContent = tr("creatingWorktreeBackground");
        list.appendChild(placeholder);
      }
      for (const t of data.threads) {
        if (!state.pinnedIds.has(t.id)) list.appendChild(threadRow(t));
      }
      if (data.threads.length < data.available) {
        const more = document.createElement("button");
        more.className = "thread";
        more.textContent = tr("loadMoreSessions", {
          count: data.available - data.threads.length,
        });
        more.onclick = () => run(() => loadProjectThreads(p, data.threads.length));
        list.appendChild(more);
      }
    }
    wrap.appendChild(list);
    root.appendChild(wrap);
  }
}
function populateCreateProjectSelect() {
  const select = $("createProjectSelect"),
    projects = state.projects.filter((project) => project.kind !== "chats"),
    chats = state.projects.find((project) => project.kind === "chats"),
    currentProject = state.current ? pinnedProject(state.current) : null;
  select.textContent = "";
  if (projects.length) {
    const group = document.createElement("optgroup");
    group.label = tr("recentProjects");
    for (const project of projects) {
      const option = document.createElement("option");
      option.value = project.path;
      option.textContent = `${project.name} — ${project.path}`;
      group.appendChild(option);
    }
    select.appendChild(group);
  }
  if (chats) {
    const group = document.createElement("optgroup"),
      option = document.createElement("option");
    group.label = tr("otherLocations");
    option.value = chats.path;
    option.textContent = tr("chatsWithoutProject");
    group.appendChild(option);
    select.appendChild(group);
  }
  if (currentProject && state.projects.some((project) => project.path === currentProject.path)) {
    select.value = currentProject.path;
  }
  const empty = !select.options.length;
  select.disabled = empty;
  $("createProjectNextBtn").disabled = empty;
}
function showCreateDialog(project = null) {
  state.creatingProject = null;
  populateCreateProjectSelect();
  $("createProjectStep").hidden = false;
  $("createModeStep").hidden = true;
  $("createProgress").textContent = "";
  $("createDialog").hidden = false;
  if (project) return showCreateMode(project);
  $("createProjectSelect").focus();
}
function showCreateMode(project) {
  state.creatingProject = project;
  const isChat = project.kind === "chats";
  $("createProject").textContent = isChat ? tr("chats") : project.path;
  $("createWorktreeBtn").hidden = isChat;
  $("createCurrentBtn").querySelector("strong").textContent = tr(
    isChat ? "newChat" : "currentDirectory",
  );
  $("createCurrentBtn").querySelector("span").textContent = tr(
    isChat ? "newChatHelp" : "currentDirectoryHelp",
  );
  $("createProjectStep").hidden = true;
  $("createModeStep").hidden = false;
  $("createCurrentBtn").focus();
}
function chooseCreateProject() {
  const project = state.projects.find((item) => item.path === $("createProjectSelect").value);
  if (!project) throw new Error(tr("chooseProject"));
  showCreateMode(project);
}
function backToCreateProject() {
  if ($("createCurrentBtn").disabled) return;
  state.creatingProject = null;
  $("createModeStep").hidden = true;
  $("createProjectStep").hidden = false;
  $("createProjectSelect").focus();
}
function closeCreateDialog() {
  if ($("createCurrentBtn").disabled) return;
  $("createDialog").hidden = true;
  state.creatingProject = null;
}
async function createThread(worktree) {
  const project = state.creatingProject;
  if (!project) throw new Error(tr("chooseProject"));
  const buttons = [$("createCurrentBtn"), $("createWorktreeBtn"), $("createCancelBtn")];
  buttons.forEach((button) => (button.disabled = true));
  $("createProgress").textContent = worktree ? tr("creatingWorktree") : tr("creatingSession");
  try {
    let r;
    if (worktree) {
      const started = await command(
          { command: "thread_create_start", project_path: project.path, model: null },
          false,
        ),
        deadline = Date.now() + 10 * 60 * 1000;
      $("createDialog").hidden = true;
      state.creatingProject = null;
      state.threadCreationJobs.set(project.path, {
        jobId: started.job_id,
        startedAt: Date.now(),
      });
      renderProjects();
      notify(tr("creatingWorktreeBackground"));
      do {
        await new Promise((resolve) => setTimeout(resolve, 900));
        r = await command({ command: "thread_create_status", job_id: started.job_id }, false);
      } while (r.status === "running" && Date.now() < deadline);
      if (r.status === "running") throw new Error(tr("creatingWorktreeTimeout"));
      state.threadCreationJobs.delete(project.path);
      renderProjects();
    } else {
      r = await command(
        {
          command: "thread_create",
          project_path: project.kind === "chats" ? null : project.path,
          worktree: false,
          model: null,
        },
        false,
      );
    }
    $("createDialog").hidden = true;
    state.creatingProject = null;
    await loadProjects();
    const targetPath = r.project_path,
      target = state.projects.find((item) => item.path === targetPath);
    if (target) {
      setProjectExpanded(target.path, true);
      await loadProjectThreads(target);
    }
    const thread = target
      ? state.projectThreads.get(target.path)?.threads.find((item) => item.id === r.thread.id) ||
        r.thread
      : r.thread;
    await openThread(thread);
    notify(worktree ? tr("createdWorktree") : tr("createdCurrent"));
  } finally {
    state.threadCreationJobs.delete(project.path);
    renderProjects();
    buttons.forEach((button) => (button.disabled = false));
    $("createProgress").textContent = "";
  }
}
const MAX_COMPOSER_ATTACHMENTS = 6;
const MAX_ATTACHMENT_DATA_URL_BYTES = 6 * 1024 * 1024;
const TRANSCRIPTION_SAMPLE_RATE = 24_000;
const MAX_VOICE_SECONDS = 120;
let voiceRecorder = null;
let voiceStream = null;
let voiceChunks = [];
let voiceLimitTimer = null;
let voiceTranscribing = false;
function audioTranscriptionCapability() {
  return state.serverCapabilities?.audio_transcription || null;
}
function voiceCapabilityLabel(capability = audioTranscriptionCapability()) {
  if (!browserAudioAvailable()) return tr("microphoneUnavailable");
  if (!capability) return tr("voiceStatusPending");
  if (capability.enabled) return tr("recordVoice");
  if (capability.reason === "api_key_auth_required") return tr("voiceApiKeyRequired");
  return tr("voiceUnavailable");
}
function syncVoiceCapability() {
  const button = $("voiceBtn"),
    capability = audioTranscriptionCapability(),
    unavailable = !capability?.enabled || !browserAudioAvailable(),
    submitting = document.querySelector(".composer-shell")?.classList.contains("submitting"),
    busy = submitting || voiceTranscribing;
  button.classList.toggle("unavailable", unavailable);
  button.disabled = unavailable || busy;
  $("audioInput").disabled = unavailable || busy;
  if (!button.classList.contains("recording") && !button.classList.contains("transcribing")) {
    button.title = voiceCapabilityLabel(capability);
    button.setAttribute("aria-label", button.title);
  }
}
function blobDataUrl(blob) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(reader.error || new Error(tr("imageReadFailed")));
    reader.readAsDataURL(blob);
  });
}
function bytesBase64(bytes) {
  let binary = "";
  for (let offset = 0; offset < bytes.length; offset += 0x8000)
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
  return btoa(binary);
}
async function pcmAudio(blob) {
  const AudioContextClass = window.AudioContext || window.webkitAudioContext;
  if (!AudioContextClass) throw new Error(tr("audioDecodeUnavailable"));
  const context = new AudioContextClass();
  let decoded;
  try {
    decoded = await context.decodeAudioData(await blob.arrayBuffer());
  } catch {
    throw new Error(tr("audioDecodeFailed"));
  } finally {
    await context.close().catch(() => {});
  }
  if (!decoded.length || decoded.duration > MAX_VOICE_SECONDS + 1)
    throw new Error(tr("voiceTooLarge"));
  const speechSamples = Math.ceil(decoded.duration * TRANSCRIPTION_SAMPLE_RATE),
    silenceSamples = Math.round(TRANSCRIPTION_SAMPLE_RATE * 0.8),
    samples = new Int16Array(speechSamples + silenceSamples),
    channels = Array.from({ length: decoded.numberOfChannels }, (_, index) =>
      decoded.getChannelData(index),
    );
  for (let index = 0; index < speechSamples; index += 1) {
    const sourcePosition = (index * decoded.sampleRate) / TRANSCRIPTION_SAMPLE_RATE,
      left = Math.min(decoded.length - 1, Math.floor(sourcePosition)),
      right = Math.min(decoded.length - 1, left + 1),
      mix = sourcePosition - left;
    let sample = 0;
    for (const channel of channels)
      sample += channel[left] + (channel[right] - channel[left]) * mix;
    sample = Math.max(-1, Math.min(1, sample / channels.length));
    samples[index] = sample < 0 ? Math.round(sample * 32768) : Math.round(sample * 32767);
  }
  return {
    data: bytesBase64(new Uint8Array(samples.buffer)),
    sample_rate: TRANSCRIPTION_SAMPLE_RATE,
    num_channels: 1,
    samples_per_channel: samples.length,
  };
}
function insertComposerText(text) {
  const textarea = $("messageText"),
    start = textarea.selectionStart ?? textarea.value.length,
    end = textarea.selectionEnd ?? start,
    before = textarea.value.slice(0, start),
    prefix = before && !/\s$/.test(before) ? " " : "",
    suffix = textarea.value.slice(end) && !/^\s/.test(textarea.value.slice(end)) ? " " : "",
    insertion = `${prefix}${text.trim()}${suffix}`;
  textarea.setRangeText(insertion, start, end, "end");
  saveDraft(state.current?.id, textarea.value);
  resizeComposerTextarea();
  textarea.focus({ preventScroll: true });
}
function insertTranscription(text) {
  insertComposerText(text);
}

function selectedContextPrompt(selection, question) {
  if (!selection?.text) return question;
  return `Selected text from the source conversation:\n\n${selection.text}\n\nQuestion:\n${question}`;
}

function temporaryItemText(item) {
  if (!item || typeof item !== "object") return "";
  if (typeof item.text === "string") return item.text;
  if (!Array.isArray(item.content)) return "";
  return item.content
    .map((part) =>
      typeof part?.text === "string" ? part.text : typeof part === "string" ? part : "",
    )
    .filter(Boolean)
    .join("");
}

function messageCopyButton(text) {
  const copy = document.createElement("button");
  copy.className = "message-copy";
  copy.type = "button";
  copy.innerHTML =
    '<svg aria-hidden="true" viewBox="0 0 24 24"><rect x="8" y="8" width="11" height="11" rx="2"/><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"/></svg>';
  copy.title = tr("copyMessage");
  copy.setAttribute("aria-label", copy.title);
  copy.onclick = () =>
    run(async () => {
      await navigator.clipboard.writeText(text);
      notify(tr("messageCopied"));
    });
  return copy;
}

function renderTemporaryMessages() {
  const root = $("temporaryMessages"),
    temporary = state.temporaryThread;
  root.replaceChildren();
  if (!temporary) return;
  if (temporary.selection?.text) {
    const context = document.createElement("article"),
      body = document.createElement("div"),
      label = document.createElement("span");
    context.className = "message context temporary-message";
    body.className = "message-body";
    label.className = "temporary-message-label";
    label.textContent = tr("selectedContext");
    body.append(label, markdownNode(temporary.selection.text));
    context.appendChild(body);
    root.appendChild(context);
  }
  for (const message of temporary.messages) {
    const node = document.createElement("article"),
      body = document.createElement("div");
    node.className = `message temporary-message ${message.role}${message.running ? " running" : ""}`;
    body.className = "message-body";
    body.appendChild(markdownNode(message.text || "…"));
    if (message.text) body.appendChild(messageCopyButton(message.text));
    node.appendChild(body);
    root.appendChild(node);
  }
  const starting = temporary.activeTurnId === "starting";
  $("temporaryStopBtn").hidden = !temporary.activeTurnId || starting;
  $("temporarySendBtn").hidden = Boolean(temporary.activeTurnId) && !starting;
  document.querySelector(".temporary-composer-shell").classList.toggle("submitting", starting);
  requestAnimationFrame(() => {
    root.scrollTop = root.scrollHeight;
    resizeTemporaryTextarea();
  });
}

function resizeTemporaryTextarea() {
  const textarea = $("temporaryText");
  if (!textarea) return;
  const minimum = window.matchMedia("(max-width: 800px)").matches ? 56 : 44;
  textarea.style.height = `${minimum}px`;
  const limit = Math.min(320, (window.visualViewport?.height || window.innerHeight) * 0.34),
    height = Math.min(Math.max(minimum, textarea.scrollHeight), limit);
  textarea.style.height = `${Math.ceil(height)}px`;
  textarea.style.overflowY = textarea.scrollHeight > height + 1 ? "auto" : "hidden";
}

function syncTemporaryForCurrent() {
  const temporary = state.current ? state.temporaryThreads.get(state.current.id) || null : null;
  state.temporaryThread = temporary;
  $("temporaryPanelBtn").hidden = !temporary;
  if (!temporary) {
    $("temporaryPanel").classList.remove("open");
    $("temporaryPanel").inert = true;
    document.querySelector("main").inert = false;
    $("sidebar").inert = false;
    $("tools").inert = !$("tools").classList.contains("open");
  }
  renderTemporaryMessages();
  syncScrim();
}

function openTemporaryPanel() {
  if (!state.temporaryThread) return;
  $("tools").classList.remove("open");
  $("temporaryPanel").inert = false;
  $("temporaryPanel").classList.add("open");
  document.querySelector("main").inert = true;
  $("sidebar").inert = true;
  $("tools").inert = true;
  syncScrim();
  requestAnimationFrame(() => $("temporaryText").focus({ preventScroll: true }));
}

function setTemporarySelection(selection = null) {
  state.temporarySelection = selection?.turnId ? { ...selection } : null;
  if (state.temporarySelection) setSendMode("temp", false);
  else if ($("sendMode").value === "temp") setSendMode(state.activeTurnId ? "steer" : "send", true);
}

async function createTemporaryThread(selection, draft = "") {
  if (!state.current || !selection?.text) throw new Error(tr("chooseSessionError"));
  if (state.temporaryCreating) return;
  const sourceThreadId = state.current.id,
    sourceDraft = draft,
    button = $("sendModeToggle"),
    buttonWasDisabled = button.disabled;
  let temporary = state.temporaryThreads.get(sourceThreadId);
  state.temporaryCreating = true;
  button.disabled = true;
  button.setAttribute("aria-busy", "true");
  try {
    if (!temporary) {
      notify(tr("temporaryCreating"));
      const result = await command(
        {
          command: "temporary_thread_create",
          thread_id: sourceThreadId,
          last_turn_id: selection.turnId || null,
        },
        false,
      );
      temporary = {
        id: result.thread?.id,
        sourceThreadId,
        selection: { ...selection },
        messages: [],
        activeTurnId: null,
      };
      if (!temporary.id) throw new Error(tr("temporaryCreateFailed"));
      state.temporaryThreads.set(sourceThreadId, temporary);
    } else temporary.selection = { ...selection };

    if (state.current?.id !== sourceThreadId) return;
    state.temporaryThread = temporary;
    $("temporaryPanelBtn").hidden = false;
    $("temporaryText").value = sourceDraft;
    if (sourceDraft && $("messageText").value === sourceDraft) {
      $("messageText").value = "";
      saveDraft(sourceThreadId, "", true);
      resizeComposerTextarea();
    }
    setTemporarySelection();
    renderTemporaryMessages();
    openTemporaryPanel();
  } finally {
    state.temporaryCreating = false;
    button.disabled = buttonWasDisabled;
    button.removeAttribute("aria-busy");
  }
}

async function sendTemporaryMessage() {
  const temporary = state.temporaryThread,
    textarea = $("temporaryText"),
    text = textarea.value.trim();
  if (!temporary || !text || temporary.activeTurnId) return;
  const submissionId = newSubmissionId();
  temporary.messages.push({ role: "user", text });
  temporary.messages.push({ role: "assistant", text: "", id: null, running: true });
  temporary.activeTurnId = "starting";
  textarea.value = "";
  renderTemporaryMessages();
  try {
    const result = await command(
      {
        command: "temporary_turn_start",
        thread_id: temporary.id,
        text: selectedContextPrompt(temporary.selection, text),
        submission_id: submissionId,
        attachments: [],
      },
      false,
    );
    if (temporary.activeTurnId === "starting") temporary.activeTurnId = result.turn_id;
    temporary.selection = null;
    if (result.demo_reply) {
      const assistant = temporary.messages.at(-1);
      assistant.text = result.demo_reply;
      assistant.running = false;
      temporary.activeTurnId = null;
    }
    renderTemporaryMessages();
  } catch (error) {
    temporary.activeTurnId = null;
    temporary.messages.at(-1).text = error.message;
    temporary.messages.at(-1).running = false;
    renderTemporaryMessages();
    throw error;
  }
}
async function transcribeAudio(audio) {
  if (!state.current) throw new Error(tr("chooseSessionError"));
  if (voiceTranscribing) return;
  voiceTranscribing = true;
  const button = $("voiceBtn");
  button.disabled = true;
  button.classList.add("transcribing");
  button.title = tr("transcribingVoice");
  button.setAttribute("aria-label", button.title);
  notify(tr("transcribingVoice"));
  try {
    const result = await command(
      { command: "audio_transcribe", thread_id: state.current.id, audio },
      false,
    );
    insertTranscription(result.text || "");
    notify(tr("voiceTranscribed"));
  } catch (error) {
    if (error.message.includes("audio_transcription_auth_required"))
      throw new Error(tr("voiceApiKeyRequired"));
    throw error;
  } finally {
    voiceTranscribing = false;
    button.classList.remove("transcribing");
    syncVoiceCapability();
  }
}
async function imageAttachment(file) {
  if (file.type === "image/gif") {
    const url = await blobDataUrl(file);
    if (url.length > MAX_ATTACHMENT_DATA_URL_BYTES) throw new Error(tr("imageTooLarge"));
    return { type: "image", url, name: file.name };
  }
  let bitmap;
  try {
    bitmap = await createImageBitmap(file);
  } catch {
    throw new Error(tr("imageReadFailed"));
  }
  const scale = Math.min(1, 2048 / Math.max(bitmap.width, bitmap.height));
  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(bitmap.width * scale));
  canvas.height = Math.max(1, Math.round(bitmap.height * scale));
  const context = canvas.getContext("2d");
  context.fillStyle = "#fff";
  context.fillRect(0, 0, canvas.width, canvas.height);
  context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
  bitmap.close?.();
  const blob = await new Promise((resolve) => canvas.toBlob(resolve, "image/webp", 0.88));
  if (!blob) throw new Error(tr("imageReadFailed"));
  const url = await blobDataUrl(blob);
  if (url.length > MAX_ATTACHMENT_DATA_URL_BYTES) throw new Error(tr("imageTooLarge"));
  return { type: "image", url, name: file.name };
}
function renderComposerAttachments() {
  const tray = $("composerAttachments");
  tray.replaceChildren();
  state.composerAttachments.forEach((attachment, index) => {
    const item = document.createElement("div");
    item.className = `composer-attachment ${attachment.type}`;
    if (attachment.type === "image") {
      const image = document.createElement("img");
      image.src = attachment.url;
      image.alt = attachment.name || tr("attachment");
      item.appendChild(image);
    } else {
      item.innerHTML =
        '<svg aria-hidden="true" viewBox="0 0 24 24"><path d="M5 9v6M9 6v12M13 4v16M17 7v10M21 10v4"/></svg>';
      item.title = attachment.name || tr("recordVoice");
    }
    const remove = document.createElement("button");
    remove.type = "button";
    remove.className = "attachment-remove";
    remove.textContent = "×";
    remove.title = tr("removeAttachment");
    remove.setAttribute("aria-label", remove.title);
    remove.onclick = () => {
      state.composerAttachments.splice(index, 1);
      renderComposerAttachments();
    };
    item.appendChild(remove);
    tray.appendChild(item);
  });
  tray.hidden = !state.composerAttachments.length;
  resizeComposerTextarea();
}
function renderComposerReference() {
  const root = $("composerReference"),
    reference = state.composerReference;
  root.replaceChildren();
  if (!reference?.text) {
    root.hidden = true;
    return;
  }
  const content = document.createElement("div"),
    label = document.createElement("span"),
    text = document.createElement("span"),
    remove = document.createElement("button");
  content.className = "composer-reference-content";
  label.className = "composer-reference-label";
  label.textContent = tr("selectedContext");
  text.className = "composer-reference-text";
  text.textContent = reference.text;
  remove.type = "button";
  remove.className = "attachment-remove";
  remove.textContent = "×";
  remove.title = tr("removeReference");
  remove.setAttribute("aria-label", remove.title);
  remove.onclick = () => setComposerReference();
  content.append(label, text);
  root.append(content, remove);
  root.hidden = false;
}

function setComposerReference(reference = null, persist = true) {
  state.composerReference = reference?.turnId ? { ...reference } : null;
  if (state.current?.id && persist) {
    if (state.composerReference)
      state.referenceDrafts.set(state.current.id, state.composerReference);
    else state.referenceDrafts.delete(state.current.id);
  }
  setTemporarySelection(state.composerReference);
  renderComposerReference();
  resizeComposerTextarea();
}
async function addImages(files) {
  const available = MAX_COMPOSER_ATTACHMENTS - state.composerAttachments.length;
  if (files.length > available) throw new Error(tr("attachmentLimit"));
  for (const file of files) state.composerAttachments.push(await imageAttachment(file));
  renderComposerAttachments();
}
async function addAudioFile(file) {
  if (file.size > MAX_ATTACHMENT_DATA_URL_BYTES) throw new Error(tr("voiceTooLarge"));
  await transcribeAudio(await pcmAudio(file));
}
function resetVoiceRecorder() {
  clearTimeout(voiceLimitTimer);
  voiceLimitTimer = null;
  voiceStream?.getTracks().forEach((track) => track.stop());
  voiceStream = null;
  voiceRecorder = null;
  voiceChunks = [];
  $("voiceBtn").classList.remove("recording");
  syncVoiceCapability();
}
async function toggleVoiceRecording() {
  if (voiceRecorder?.state === "recording") {
    voiceRecorder.stop();
    return;
  }
  if (voiceTranscribing) return;
  if (demoMode) {
    await transcribeAudio({
      data: "AAAAAA==",
      sample_rate: TRANSCRIPTION_SAMPLE_RATE,
      num_channels: 1,
      samples_per_channel: 2,
    });
    return;
  }
  if (!navigator.mediaDevices?.getUserMedia || !window.MediaRecorder) {
    $("audioInput").click();
    return;
  }
  try {
    voiceStream = await navigator.mediaDevices.getUserMedia({ audio: true });
    const preferred = ["audio/mp4", "audio/webm;codecs=opus", "audio/webm"].find((type) =>
      MediaRecorder.isTypeSupported(type),
    );
    voiceRecorder = preferred
      ? new MediaRecorder(voiceStream, { mimeType: preferred })
      : new MediaRecorder(voiceStream);
    voiceChunks = [];
    voiceRecorder.ondataavailable = (event) => event.data.size && voiceChunks.push(event.data);
    voiceRecorder.onerror = () => {
      voiceRecorder.onstop = null;
      resetVoiceRecorder();
      notify(tr("microphoneUnavailable"), true);
    };
    voiceRecorder.onstop = async () => {
      const mime = voiceRecorder?.mimeType || voiceChunks[0]?.type || "audio/webm";
      const blob = new Blob(voiceChunks, { type: mime });
      try {
        resetVoiceRecorder();
        await transcribeAudio(await pcmAudio(blob));
      } catch (error) {
        resetVoiceRecorder();
        notify(error.message, true);
      }
    };
    voiceRecorder.start(250);
    $("voiceBtn").classList.add("recording");
    $("voiceBtn").title = tr("stopRecording");
    $("voiceBtn").setAttribute("aria-label", $("voiceBtn").title);
    voiceLimitTimer = setTimeout(() => {
      if (voiceRecorder?.state === "recording") voiceRecorder.stop();
    }, 120000);
    notify(tr("voiceRecording"));
  } catch (error) {
    resetVoiceRecorder();
    throw new Error(error?.message || tr("microphoneUnavailable"));
  }
}
function byteText(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1048576).toFixed(1)} MB`;
}
function tokenCountText(value) {
  const count = Number(value);
  if (!Number.isFinite(count)) return "0";
  return new Intl.NumberFormat(getLanguage() === "zh" ? "zh-CN" : "en", {
    notation: "compact",
    maximumFractionDigits: 1,
  }).format(count);
}
function turnMetaNode(text, title = text) {
  const line = document.createElement("div");
  line.className = "turn-meta-line";
  line.textContent = text;
  line.title = title;
  return line;
}
function turnTokenUsageText(item) {
  return tr("turnTokenUsage", {
    total: tokenCountText(item?.total_tokens),
    input: tokenCountText(item?.input_tokens),
    cached: tokenCountText(item?.cached_input_tokens),
    output: tokenCountText(item?.output_tokens),
  });
}
function memoryCitationNode(items) {
  const model = memoryCitationModel(items),
    details = document.createElement("details"),
    summary = document.createElement("summary"),
    list = document.createElement("div");
  details.className = "memory-citations";
  summary.textContent = model.files.join(" · ");
  summary.title = model.entries
    .map(({ source, note }) => [source, note].filter(Boolean).join(" · "))
    .join("\n");
  list.className = "memory-citation-list";
  for (const { source, note } of model.entries) {
    const line = document.createElement("div");
    line.className = "memory-citation-entry";
    line.textContent = tr("memoryCitation", { source, note: note ? ` · ${note}` : "" });
    list.appendChild(line);
  }
  details.append(summary, list);
  return details;
}
const imageViewer = {
  root: null,
  stage: null,
  image: null,
  scaleLabel: null,
  scale: 1,
  x: 0,
  y: 0,
  pointers: new Map(),
  gesture: null,
  lastTap: null,
  suppressDoubleClickUntil: 0,
  returnFocus: null,
  viewportMeta: null,
  viewportContent: null,
};

function clampImageScale(scale) {
  return Math.min(8, Math.max(1, scale));
}

function renderImageTransform() {
  imageViewer.image.style.transform = `translate3d(${imageViewer.x}px, ${imageViewer.y}px, 0) scale(${imageViewer.scale})`;
  imageViewer.scaleLabel.textContent = `${Math.round(imageViewer.scale * 100)}%`;
}

function setImageScale(scale, origin = null) {
  const previous = imageViewer.scale,
    next = clampImageScale(scale);
  if (origin && previous !== next) {
    const rect = imageViewer.stage.getBoundingClientRect(),
      offsetX = origin.x - rect.left - rect.width / 2,
      offsetY = origin.y - rect.top - rect.height / 2,
      ratio = next / previous;
    imageViewer.x = offsetX - (offsetX - imageViewer.x) * ratio;
    imageViewer.y = offsetY - (offsetY - imageViewer.y) * ratio;
  }
  imageViewer.scale = next;
  if (next === 1) imageViewer.x = imageViewer.y = 0;
  renderImageTransform();
}

function resetImageZoom() {
  imageViewer.scale = 1;
  imageViewer.x = imageViewer.y = 0;
  renderImageTransform();
}

function lockPageZoomForImageViewer() {
  const meta = document.querySelector('meta[name="viewport"]');
  if (!meta || imageViewer.viewportMeta) return;
  imageViewer.viewportMeta = meta;
  imageViewer.viewportContent = meta.getAttribute("content");
  const content = (imageViewer.viewportContent || "width=device-width,initial-scale=1")
    .split(",")
    .map((part) => part.trim())
    .filter(Boolean)
    .filter((part) => !/^(maximum-scale|user-scalable)\s*=/i.test(part));
  content.push("maximum-scale=1", "user-scalable=no");
  meta.setAttribute("content", content.join(","));
}

function restorePageZoomAfterImageViewer() {
  const { viewportMeta, viewportContent } = imageViewer;
  if (!viewportMeta) return;
  if (viewportContent === null) viewportMeta.removeAttribute("content");
  else viewportMeta.setAttribute("content", viewportContent);
  imageViewer.viewportMeta = null;
  imageViewer.viewportContent = null;
}

function closeImageViewer() {
  if (!imageViewer.root || imageViewer.root.hidden) return;
  for (const pointerId of imageViewer.pointers.keys()) {
    if (imageViewer.stage.hasPointerCapture(pointerId)) {
      imageViewer.stage.releasePointerCapture(pointerId);
    }
  }
  resetImageZoom();
  imageViewer.root.hidden = true;
  imageViewer.pointers.clear();
  imageViewer.gesture = null;
  imageViewer.lastTap = null;
  imageViewer.suppressDoubleClickUntil = 0;
  document.body.classList.remove("image-viewer-open");
  restorePageZoomAfterImageViewer();
  imageViewer.returnFocus?.focus();
  imageViewer.returnFocus = null;
}

function ensureImageViewer() {
  if (imageViewer.root) return;
  const root = document.createElement("div"),
    toolbar = document.createElement("div"),
    stage = document.createElement("div"),
    image = document.createElement("img"),
    zoomOut = document.createElement("button"),
    scaleLabel = document.createElement("button"),
    zoomIn = document.createElement("button"),
    close = document.createElement("button");
  root.className = "image-viewer";
  root.hidden = true;
  root.setAttribute("role", "dialog");
  root.setAttribute("aria-modal", "true");
  root.setAttribute("aria-label", tr("imageViewer"));
  toolbar.className = "image-viewer-toolbar";
  stage.className = "image-viewer-stage";
  image.className = "image-viewer-image";
  image.alt = "";
  image.draggable = false;
  for (const [button, text, title] of [
    [zoomOut, "−", tr("zoomOut")],
    [scaleLabel, "100%", tr("resetZoom")],
    [zoomIn, "+", tr("zoomIn")],
    [close, "×", tr("closeImageViewer")],
  ]) {
    button.type = "button";
    button.textContent = text;
    button.title = title;
    button.setAttribute("aria-label", title);
  }
  scaleLabel.className = "image-viewer-scale";
  close.className = "image-viewer-close";
  zoomOut.onclick = () => setImageScale(imageViewer.scale / 1.25);
  scaleLabel.onclick = resetImageZoom;
  zoomIn.onclick = () => setImageScale(imageViewer.scale * 1.25);
  close.onclick = closeImageViewer;
  toolbar.append(zoomOut, scaleLabel, zoomIn, close);
  stage.appendChild(image);
  root.append(toolbar, stage);
  root.onclick = (event) => {
    if (event.target === root) closeImageViewer();
  };
  const preventBrowserZoom = (event) => event.preventDefault();
  for (const eventName of ["gesturestart", "gesturechange", "gestureend"]) {
    root.addEventListener(eventName, preventBrowserZoom, { passive: false });
  }
  root.addEventListener(
    "wheel",
    (event) => {
      if (event.ctrlKey) event.preventDefault();
    },
    { passive: false },
  );
  stage.ondblclick = (event) => {
    event.preventDefault();
    if (performance.now() < imageViewer.suppressDoubleClickUntil) return;
    setImageScale(imageViewer.scale > 1 ? 1 : 2, { x: event.clientX, y: event.clientY });
  };
  stage.onwheel = (event) => {
    event.preventDefault();
    setImageScale(imageViewer.scale * (event.deltaY < 0 ? 1.15 : 1 / 1.15), {
      x: event.clientX,
      y: event.clientY,
    });
  };
  stage.onpointerdown = (event) => {
    event.preventDefault();
    stage.setPointerCapture(event.pointerId);
    imageViewer.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    const points = [...imageViewer.pointers.values()];
    if (points.length === 1) {
      imageViewer.gesture = {
        kind: "pan",
        pointerId: event.pointerId,
        pointerType: event.pointerType,
        startedAt: performance.now(),
        x: points[0].x,
        y: points[0].y,
        imageX: imageViewer.x,
        imageY: imageViewer.y,
      };
    } else if (points.length === 2) {
      imageViewer.gesture = {
        kind: "pinch",
        distance: Math.max(1, Math.hypot(points[1].x - points[0].x, points[1].y - points[0].y)),
        centerX: (points[0].x + points[1].x) / 2,
        centerY: (points[0].y + points[1].y) / 2,
        scale: imageViewer.scale,
        imageX: imageViewer.x,
        imageY: imageViewer.y,
      };
    }
  };
  stage.onpointermove = (event) => {
    if (!imageViewer.pointers.has(event.pointerId)) return;
    imageViewer.pointers.set(event.pointerId, { x: event.clientX, y: event.clientY });
    const points = [...imageViewer.pointers.values()],
      gesture = imageViewer.gesture;
    if (points.length === 1 && gesture?.kind === "pan" && imageViewer.scale > 1) {
      imageViewer.x = gesture.imageX + points[0].x - gesture.x;
      imageViewer.y = gesture.imageY + points[0].y - gesture.y;
      renderImageTransform();
    } else if (points.length === 2 && gesture?.kind === "pinch") {
      const distance = Math.hypot(points[1].x - points[0].x, points[1].y - points[0].y),
        centerX = (points[0].x + points[1].x) / 2,
        centerY = (points[0].y + points[1].y) / 2,
        scale = clampImageScale(gesture.scale * (distance / gesture.distance)),
        ratio = scale / gesture.scale,
        rect = stage.getBoundingClientRect(),
        originX = gesture.centerX - rect.left - rect.width / 2,
        originY = gesture.centerY - rect.top - rect.height / 2,
        nextOriginX = centerX - rect.left - rect.width / 2,
        nextOriginY = centerY - rect.top - rect.height / 2;
      imageViewer.scale = scale;
      imageViewer.x = nextOriginX - (originX - gesture.imageX) * ratio;
      imageViewer.y = nextOriginY - (originY - gesture.imageY) * ratio;
      renderImageTransform();
    }
  };
  const endPointer = (event) => {
    const gesture = imageViewer.gesture,
      point = imageViewer.pointers.get(event.pointerId),
      now = performance.now(),
      isTouchTap =
        event.type === "pointerup" &&
        imageViewer.pointers.size === 1 &&
        gesture?.kind === "pan" &&
        gesture.pointerId === event.pointerId &&
        gesture.pointerType !== "mouse" &&
        now - gesture.startedAt < 350 &&
        point &&
        Math.hypot(point.x - gesture.x, point.y - gesture.y) < 12;
    if (isTouchTap) {
      const previous = imageViewer.lastTap;
      if (
        previous &&
        now - previous.time < 350 &&
        Math.hypot(point.x - previous.x, point.y - previous.y) < 28
      ) {
        imageViewer.lastTap = null;
        imageViewer.suppressDoubleClickUntil = now + 500;
        setImageScale(imageViewer.scale > 1 ? 1 : 2, { x: point.x, y: point.y });
      } else {
        imageViewer.lastTap = { x: point.x, y: point.y, time: now };
      }
    } else if (gesture?.kind === "pinch") {
      imageViewer.lastTap = null;
    }
    imageViewer.pointers.delete(event.pointerId);
    imageViewer.gesture = null;
  };
  stage.onpointerup = endPointer;
  stage.onpointercancel = endPointer;
  stage.onlostpointercapture = endPointer;
  document.body.appendChild(root);
  Object.assign(imageViewer, { root, stage, image, scaleLabel });
}

function openImageViewer(source, alt = "", trigger = null) {
  ensureImageViewer();
  lockPageZoomForImageViewer();
  imageViewer.image.src = source;
  imageViewer.image.alt = alt;
  imageViewer.returnFocus = trigger;
  resetImageZoom();
  imageViewer.root.hidden = false;
  document.body.classList.add("image-viewer-open");
  imageViewer.root.querySelector(".image-viewer-close").focus();
}

function makeInspectableImage(img) {
  img.classList.add("inspectable-image");
  img.tabIndex = 0;
  img.setAttribute("role", "button");
  img.title = tr("openImage");
  img.setAttribute("aria-label", `${img.alt || tr("attachment")}. ${tr("openImage")}`);
  img.onclick = () => openImageViewer(img.currentSrc || img.src, img.alt, img);
  img.onkeydown = (event) => {
    if (!["Enter", " "].includes(event.key)) return;
    event.preventDefault();
    openImageViewer(img.currentSrc || img.src, img.alt, img);
  };
  return img;
}

const filePreview = createFilePreviewController({
  root: $("filePreviewDialog"),
  title: $("filePreviewTitle"),
  meta: $("filePreviewMeta"),
  body: $("filePreviewBody"),
  closeButton: $("filePreviewClose"),
  downloadButton: $("filePreviewDownload"),
  modeButton: $("filePreviewMode"),
  translate: tr,
  renderMarkdown: (source) => markdownNode(source, markdownOptions(state.current?.id)),
  inspectImage: openImageViewer,
});

function appendContextValue(details, value, label) {
  const values = Array.isArray(value) ? value : [value];
  let rendered = false;
  for (const part of values) {
    if (part?.type === "input_image" && typeof part.image_url === "string") {
      const img = document.createElement("img");
      img.src = part.image_url;
      img.alt = label || tr("attachment");
      details.appendChild(makeInspectableImage(img));
      rendered = true;
      continue;
    }
    if (part?.type === "input_audio" && typeof part.audio_url === "string") {
      const audio = document.createElement("audio");
      audio.controls = true;
      audio.src = part.audio_url;
      details.appendChild(audio);
      rendered = true;
      continue;
    }
    const raw =
      typeof part?.text === "string"
        ? part.text
        : typeof part?.output_text === "string"
          ? part.output_text
          : null;
    if (raw !== null && !/^<\/?image(?:\s|>)/.test(raw.trim())) {
      const pre = document.createElement("pre");
      pre.textContent = raw;
      details.appendChild(pre);
      rendered = true;
    }
  }
  if (!rendered) {
    const pre = document.createElement("pre");
    pre.textContent = JSON.stringify(value, null, 2);
    details.appendChild(pre);
  }
}
function contentNode(item) {
  if (item.kind === "text") return markdownNode(item.text, markdownOptions(state.current?.id));
  if (item.kind === "turn_usage") {
    const node = turnMetaNode(turnTokenUsageText(item));
    node.classList.add("turn-token-usage");
    return node;
  }
  const details = document.createElement("details");
  details.className = "context-block";
  const summary = document.createElement("summary");
  summary.textContent = item.label || tr("injectedContext");
  const size = document.createElement("span");
  size.className = "context-size";
  size.textContent = byteText(item.bytes || 0);
  summary.appendChild(size);
  details.appendChild(summary);
  details.ontoggle = () => {
    if (!details.open || details.dataset.loaded) return;
    details.dataset.loaded = "1";
    if (item.content) {
      appendContextValue(details, item.content, item.label);
      return;
    }
    if (typeof item.text === "string") {
      appendContextValue(details, { text: item.text }, item.label);
      return;
    }
    const loading = document.createElement("pre");
    loading.textContent = tr("loading");
    details.appendChild(loading);
    run(async () => {
      const r = await command(
        {
          command: "message_content",
          thread_id: state.current.id,
          message_index: item.message_index,
          content_index: item.content_index,
          content_end: item.content_end ?? null,
        },
        false,
      );
      loading.remove();
      appendContextValue(details, r.content, item.label);
    });
  };
  return details;
}
function markdownOptions(threadId) {
  return {
    requestLocalFilePreview: async (path, position) => {
      if (!threadId) throw new Error(tr("chooseSessionError"));
      const preview = await requestFilePreview(threadId, path);
      filePreview.open(preview, position);
    },
    onError: (error) => notify(error.message, true),
  };
}
function toolValueText(value) {
  if (typeof value === "string") {
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }
  return JSON.stringify(value, null, 2);
}
function appendToolValue(parent, value) {
  let parsed = value;
  if (typeof parsed === "string") {
    try {
      parsed = JSON.parse(parsed);
    } catch {}
  }
  const values = Array.isArray(parsed)
    ? parsed
    : Array.isArray(parsed?.content)
      ? parsed.content
      : [parsed];
  for (const part of values) {
    let imageUrl = typeof part?.image_url === "string" ? part.image_url : null;
    if (!imageUrl && part?.type === "image" && typeof part.data === "string")
      imageUrl = `data:${part.mimeType || part.mime_type || "image/png"};base64,${part.data}`;
    if (imageUrl && /^(?:data:image\/|https?:\/\/)/i.test(imageUrl)) {
      const img = document.createElement("img");
      img.src = imageUrl;
      img.alt = tr("toolResultImage");
      img.loading = "lazy";
      parent.appendChild(makeInspectableImage(img));
      continue;
    }
    if (typeof part?.audio_url === "string") {
      const audio = document.createElement("audio");
      audio.controls = true;
      audio.src = part.audio_url;
      parent.appendChild(audio);
      continue;
    }
    const raw =
      typeof part?.text === "string"
        ? part.text
        : typeof part?.output_text === "string"
          ? part.output_text
          : null;
    const pre = document.createElement("pre");
    pre.textContent = raw === null ? toolValueText(part) : raw;
    parent.appendChild(pre);
  }
}
function appendPatchDiff(parent, patch, label = "Diff") {
  const card = document.createElement("div"),
    head = document.createElement("div"),
    title = document.createElement("span"),
    copy = document.createElement("button"),
    content = document.createElement("pre");
  card.className = "diff-card";
  head.className = "diff-head";
  title.textContent = label;
  copy.className = "diff-copy";
  copy.type = "button";
  copy.textContent = "⧉";
  copy.title = tr("copyPatch");
  copy.setAttribute("aria-label", copy.title);
  copy.onclick = () =>
    run(async () => {
      await navigator.clipboard.writeText(patch);
      notify(tr("diffCopied"));
    });
  head.append(title, copy);
  content.className = "diff-content";
  for (const line of patch.split("\n")) {
    if (["*** Begin Patch", "*** End Patch"].includes(line)) continue;
    const row = document.createElement("span");
    row.className = "diff-line";
    if (/^(?:\*\*\* (?:Add|Update|Delete) File: |--- |\+\+\+ )/.test(line))
      row.classList.add("file");
    else if (line.startsWith("@@")) row.classList.add("hunk");
    else if (line.startsWith("+")) row.classList.add("add");
    else if (line.startsWith("-")) row.classList.add("delete");
    row.textContent = line || " ";
    content.appendChild(row);
  }
  card.append(head, content);
  parent.appendChild(card);
}

function appendCommandActions(parent, input) {
  const actions = Array.isArray(input?.commandActions) ? input.commandActions : [];
  if (!actions.length) return false;
  const title = document.createElement("h5");
  const parallel =
    input.parallel === true ||
    ["parallel", "concurrent"].includes(input.executionMode || input.execution_mode);
  title.textContent = parallel
    ? tr("parallelCommands", { count: actions.length })
    : tr("commandActions", { count: actions.length });
  const list = document.createElement("div");
  list.className = `command-actions${parallel ? " parallel" : ""}`;
  for (const [index, action] of actions.entries()) {
    const item = document.createElement("section"),
      head = document.createElement("div"),
      number = document.createElement("span"),
      kind = document.createElement("span"),
      target = document.createElement("span"),
      command = document.createElement("pre");
    item.className = "command-action";
    head.className = "command-action-head";
    number.className = "command-action-number";
    number.textContent = String(index + 1);
    kind.className = "command-action-kind";
    kind.textContent = action?.type || tr("command");
    target.className = "command-action-target";
    target.textContent = action?.name || action?.path || action?.query || "";
    command.textContent = action?.command || toolValueText(action);
    head.append(number, kind);
    if (target.textContent) head.appendChild(target);
    item.append(head, command);
    list.appendChild(item);
  }
  parent.append(title, list);
  return true;
}
const renderedMessageState = new WeakMap();

function toolGroupNode(message, keepRunning = false) {
  const tools = message.tools || [];
  if (!tools.length) return null;
  const group = document.createElement("details");
  group.className = "tool-group";
  const hasImage = tools.some((tool) => tool.has_image);
  const runningTool = tools.findLast((tool) => !toolFinished(tool)),
    latest = runningTool || tools.at(-1),
    summary = document.createElement("summary"),
    icon = document.createElement("span"),
    label = document.createElement("span"),
    running = Boolean(runningTool) || keepRunning;
  summary.className = "tool-group-summary";
  summary.classList.toggle("running", running);
  icon.className = `tool-icon ${toolIconClass(latest.name)}`;
  icon.title = latest.name;
  const editedFiles = tools.reduce((total, tool) => total + (tool.file_count || 0), 0);
  label.className = "tool-summary-label";
  label.textContent = running
    ? [
        toolActionText(latest.name, true),
        toolSummaryPreview(latest),
        tools.length > 1 ? `+${tools.length - 1}` : "",
      ]
        .filter(Boolean)
        .join(" ")
    : [
        tr("ranTools"),
        tr("toolCount", { count: tools.length }),
        editedFiles ? tr("editedFiles", { count: editedFiles }) : "",
      ]
        .filter(Boolean)
        .join(" ");
  label.title = running ? latest.preview || latest.name : label.textContent;
  summary.append(icon, label);
  if (hasImage) summary.appendChild(toolImageIndicator());
  group.appendChild(summary);
  const list = document.createElement("div");
  list.className = "tool-list";
  for (const [toolPosition, tool] of tools.entries()) {
    const detail = document.createElement("details");
    detail.className = "tool-call";
    detail.dataset.toolIndex = String(tool.tool_index ?? toolPosition);
    const head = document.createElement("summary"),
      icon = document.createElement("span"),
      preview = document.createElement("span"),
      status = document.createElement("span");
    head.classList.toggle("has-image", Boolean(tool.has_image));
    icon.className = `tool-icon ${toolIconClass(tool.name)}`;
    icon.title = tool.name;
    preview.className = "tool-preview";
    preview.appendChild(document.createTextNode(localizedToolPreview(tool)));
    if (tool.additions !== null && tool.additions !== undefined) {
      const add = document.createElement("span"),
        del = document.createElement("span");
      add.className = "tool-add";
      add.textContent = `+${tool.additions}`;
      del.className = "tool-del";
      del.textContent = `−${tool.deletions || 0}`;
      preview.append(add, del);
    }
    status.className = "tool-state";
    status.textContent = toolFinished(tool) ? "✓" : "…";
    head.append(icon, preview);
    if (tool.has_image) head.appendChild(toolImageIndicator());
    head.appendChild(status);
    detail.appendChild(head);
    detail.ontoggle = () => {
      if (!detail.open || detail.dataset.loaded) return;
      detail.dataset.loaded = "1";
      const body = document.createElement("div");
      body.className = "tool-detail";
      body.textContent = tr("loading");
      detail.appendChild(body);
      run(async () => {
        const r = await command(
          {
            command: "tool_content",
            thread_id: state.current.id,
            message_index: message.message_index,
            tool_index: tool.tool_index,
          },
          false,
        );
        body.textContent = "";
        const outputTitle = document.createElement("h5");
        outputTitle.textContent = tr("result");
        if (r.display_input?.type === "fileChange" && Array.isArray(r.display_input?.changes)) {
          for (const change of r.display_input.changes) {
            if (typeof change?.diff === "string")
              appendPatchDiff(body, change.diff, change.path || "Diff");
          }
          body.appendChild(outputTitle);
        } else if (r.display_input?.operation === "apply_patch" && r.display_input?.patch) {
          appendPatchDiff(body, r.display_input.patch);
          body.appendChild(outputTitle);
        } else {
          const inputTitle = document.createElement("h5");
          inputTitle.textContent = tr("input");
          const input = document.createElement("pre");
          if (appendCommandActions(body, r.display_input)) {
            body.appendChild(outputTitle);
          } else {
            input.textContent =
              typeof r.display_input?.command === "string"
                ? r.display_input.command
                : toolValueText(r.display_input);
            body.append(inputTitle, input, outputTitle);
          }
        }
        appendToolValue(body, r.tool.output);
      });
    };
    list.appendChild(detail);
  }
  group.appendChild(list);
  return group;
}
function toolImageIndicator() {
  const indicator = document.createElement("span");
  indicator.className = "tool-image-indicator";
  indicator.title = tr("toolResultImage");
  indicator.setAttribute("role", "img");
  indicator.setAttribute("aria-label", indicator.title);
  indicator.innerHTML =
    '<svg aria-hidden="true" viewBox="0 0 24 24"><rect x="3.5" y="4.5" width="17" height="15" rx="2"/><circle cx="9" cy="10" r="1.5"/><path d="m5.5 17 4.2-4.2 3.2 3 2.2-2.1 3.4 3.3"/></svg>';
  return indicator;
}
function toolFinished(tool) {
  return tool.has_output || ["completed", "failed", "declined", "cancelled"].includes(tool.status);
}
function toolIconClass(name) {
  if (["exec", "exec_command"].includes(name)) return "exec_command";
  return ["apply_patch", "write_stdin", "web_search"].includes(name) ? name : "other";
}
function toolActionText(name, running) {
  if (running) {
    if (name === "apply_patch") return tr("editing");
    if (name === "write_stdin") return tr("waitingOutput");
    if (name === "web_search") return tr("searching");
    return tr("running");
  }
  if (name === "apply_patch") return tr("edited");
  if (name === "write_stdin") return tr("continued");
  return ["exec", "exec_command"].includes(name) ? tr("ran") : tr("completed");
}
function toolSummaryPreview(tool) {
  if (tool.name === "write_stdin") return "";
  return localizedToolPreview(tool);
}
function localizedToolPreview(tool) {
  const preview = tool.preview || tool.name;
  if (tool.name === "write_stdin") return tr("waitingOutput");
  if (tool.name === "web_search") {
    const match = preview.replace(/^搜索\s+/, "").match(/^(.*) 等 (\d+) 项$/);
    return match
      ? `${match[1]} ${tr("queryCount", { count: match[2] })}`
      : preview.replace(/^搜索\s+/, "");
  }
  if (tool.name === "apply_patch") {
    if (tool.file_count > 1) return tr("filesShort", { count: tool.file_count });
    return preview.replace(/^已(?:编辑|新建|删除|移动)\s+/, "");
  }
  if (tool.name === "exec_command" && tool.command_action_count > 1) {
    const mode = tool.command_actions_parallel ? "parallelCommandsShort" : "commandActionsShort";
    return `${preview} · ${tr(mode, { count: tool.command_action_count })}`;
  }
  return preview;
}

function groupedTurns(messages) {
  const groups = [];
  let legacyTurn = 0;
  for (const message of messages) {
    let key = message.turn_id ? `turn:${message.turn_id}` : null;
    const previous = groups.at(-1);
    if (!key) {
      if (!previous || (message.role === "user" && previous.hasUser)) legacyTurn += 1;
      key = `legacy:${legacyTurn}`;
    }
    let group = groups.at(-1);
    if (!group || group.key !== key) {
      group = { key, turnId: message.turn_id || null, messages: [], hasUser: false };
      groups.push(group);
    }
    group.messages.push(message);
    if (message.role === "user" && message.category === "user") group.hasUser = true;
  }
  return groups;
}

function turnHasUsage(message) {
  return message.content?.some((item) => item.kind === "turn_usage");
}

function turnUsageItem(group) {
  return group.messages
    .flatMap((message) => message.content || [])
    .findLast((item) => item.kind === "turn_usage");
}

function turnDuration(group) {
  const times = group.messages
    .map((message) => Date.parse(message.timestamp))
    .filter(Number.isFinite);
  const seconds =
    times.length > 1
      ? Math.max(0, Math.round((Math.max(...times) - Math.min(...times)) / 1000))
      : 0;
  return seconds >= 60
    ? tr("turnDurationMinutes", { minutes: Math.floor(seconds / 60), seconds: seconds % 60 })
    : tr("turnDurationSeconds", { seconds });
}

function turnSummaryText(group) {
  const tools = group.messages.flatMap((message) => message.tools || []),
    deferredTools = group.messages.reduce(
      (total, message) => total + (message.deferred_tool_count || 0),
      0,
    ),
    files =
      tools.reduce((total, tool) => total + (tool.file_count || 0), 0) +
      group.messages.reduce((total, message) => total + (message.deferred_file_count || 0), 0);
  return tr("turnSummary", {
    duration: turnDuration(group),
    tools: tools.length + deferredTools,
    files: files ? tr("turnEditedFiles", { count: files }) : "",
  });
}

function reconcileChildren(parent, nodes) {
  let cursor = parent.firstChild;
  for (const node of nodes) {
    if (node === cursor) cursor = cursor.nextSibling;
    else parent.insertBefore(node, cursor);
  }
  while (cursor) {
    const next = cursor.nextSibling;
    cursor.remove();
    cursor = next;
  }
}

function preserveMessageElementPosition(element, change) {
  const beforeTop = element?.getBoundingClientRect().top,
    scrollTop = messageScrollMetrics().top;
  change();
  if (!element?.isConnected || element.hidden || element.getClientRects().length === 0) return;
  setMessageScrollTop(
    scrollTopForViewportAnchor(scrollTop, beforeTop, element.getBoundingClientRect().top),
  );
}

function collapseExpandedMessages() {
  const prefix = `${state.current?.id || ""}:`,
    expanded = [...state.expandedTurnIds].filter((key) => key.startsWith(prefix));
  for (const key of expanded) state.expandedTurnIds.delete(key);
  if (expanded.length) renderVisibleMessages();
  notify(
    tr(expanded.length ? "messagesCollapsed" : "noExpandedMessages", { count: expanded.length }),
  );
  closePanels();
}

function layoutTurnGroup(section, group, messageNodes, completed) {
  const finalPosition = group.messages.findLastIndex(
      (message) => message.role === "assistant" && message.phase === "final_answer",
    ),
    fallbackFinalPosition = group.messages.findLastIndex((message) => message.role === "assistant"),
    finalIndex = finalPosition >= 0 ? finalPosition : fallbackFinalPosition,
    finalNode = finalIndex >= 0 ? messageNodes[finalIndex] : null,
    userNodes = messageNodes.filter(
      (node, index) =>
        group.messages[index].role === "user" && group.messages[index].category === "user",
    ),
    hiddenNodes = messageNodes.filter((node) => node !== finalNode && !userNodes.includes(node)),
    hasFinalTools = Boolean(finalIndex >= 0 && group.messages[finalIndex].tools?.length),
    hasDeferred = group.messages.some((message) => message.deferred),
    usage = turnUsageItem(group),
    foldable =
      completed &&
      Boolean(finalNode) &&
      (hiddenNodes.length > 0 || hasFinalTools || hasDeferred || Boolean(usage));

  section.className = "turn-group";
  section.dataset.turnKey = group.key;
  if (!foldable) {
    for (const node of messageNodes) {
      node.hidden = false;
      for (const token of node.querySelectorAll(".turn-token-usage")) token.hidden = false;
    }
    reconcileChildren(section, messageNodes);
    return;
  }
  for (const node of messageNodes)
    for (const token of node.querySelectorAll(".turn-token-usage")) token.hidden = true;

  const expansionKey = `${state.current?.id || ""}:${group.key}`,
    expanded = state.expandedTurnIds.has(expansionKey),
    foldBlock = section.querySelector(":scope > .turn-fold-block") || document.createElement("div"),
    fold = foldBlock.querySelector(":scope > .turn-fold") || document.createElement("button"),
    divider = foldBlock.querySelector(":scope > .turn-divider") || document.createElement("hr"),
    tokenUsage =
      foldBlock.querySelector(":scope > .turn-fold-usage") || document.createElement("span");
  foldBlock.className = "turn-fold-block";
  fold.type = "button";
  fold.className = "turn-fold tool-group-summary";
  fold.title = tr(expanded ? "collapseTurn" : "expandTurn");
  fold.setAttribute("aria-label", fold.title);
  fold.setAttribute("aria-expanded", String(expanded));
  if (hasDeferred && group.turnId) fold.dataset.deferredTurnId = group.turnId;
  else delete fold.dataset.deferredTurnId;
  fold.replaceChildren();
  const text = document.createElement("span"),
    label = document.createElement("span");
  text.className = "turn-fold-text";
  label.className = "tool-summary-label";
  label.textContent = turnSummaryText(group);
  tokenUsage.className = "turn-fold-usage";
  tokenUsage.textContent = usage ? turnTokenUsageText(usage) : "";
  tokenUsage.hidden = !usage;
  text.append(label);
  fold.append(text);
  fold.onclick = () => {
    if (!expanded && hasDeferred && group.turnId) {
      fold.disabled = true;
      fold.classList.add("loading");
      run(async () => {
        try {
          await hydrateTurn(group.turnId);
          state.expandedTurnIds.add(expansionKey);
          renderVisibleMessages();
        } finally {
          if (fold.isConnected) {
            fold.disabled = false;
            fold.classList.remove("loading");
          }
        }
      });
      return;
    }
    preserveMessageElementPosition(finalNode, () => {
      if (expanded) state.expandedTurnIds.delete(expansionKey);
      else state.expandedTurnIds.add(expansionKey);
      layoutTurnGroup(section, group, messageNodes, completed);
    });
  };
  divider.className = "turn-divider";
  foldBlock.replaceChildren(fold, divider, tokenUsage);
  section.classList.toggle("collapsed", !expanded);
  section.classList.toggle("expanded", expanded);
  for (const node of messageNodes) node.hidden = !expanded && hiddenNodes.includes(node);
  const leading = expanded ? messageNodes.filter((node) => node !== finalNode) : userNodes;
  reconcileChildren(section, [
    ...leading,
    foldBlock,
    finalNode,
    ...hiddenNodes.filter((node) => !leading.includes(node)),
  ]);
}

function messageNode(m, keepToolsRunning = false) {
  const box = document.createElement("article");
  box.className = `message ${m.category || m.role || ""}`;
  box.dataset.messageIndex = String(m.message_index);
  if (m.turn_id) box.dataset.turnId = m.turn_id;
  const head = document.createElement("div");
  head.className = "message-head";
  const a = document.createElement("span"),
    b = document.createElement("span"),
    date = m.timestamp ? new Date(m.timestamp) : null,
    validDate = date && !Number.isNaN(date.valueOf());
  a.textContent = m.category === "context" ? tr("injectedContext") : m.role || "message";
  b.textContent = validDate
    ? date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : m.timestamp || "";
  if (validDate) b.title = date.toLocaleString();
  if (m.role === "assistant") {
    if (b.textContent) head.appendChild(b);
  } else head.append(a, b);
  const body = document.createElement("div");
  body.className = "message-body";
  const content = m.content || [],
    ordinaryItems = content.filter(
      (item) => !["turn_usage", "memory_citation"].includes(item.kind),
    ),
    usageItems = content.filter((item) => item.kind === "turn_usage"),
    memoryItems = content.filter((item) => item.kind === "memory_citation");
  for (const item of ordinaryItems) body.appendChild(contentNode(item));
  const copyText = content
    .filter((item) => item.kind === "text" && typeof item.text === "string")
    .map((item) => item.text)
    .join("\n\n");
  const copy = copyText ? messageCopyButton(copyText) : null,
    tools = toolGroupNode(m, keepToolsRunning);
  if (tools) {
    const toolRow = document.createElement("div");
    toolRow.className = "message-tool-row";
    toolRow.appendChild(tools);
    if (copy) {
      toolRow.classList.add("has-message-copy");
      copy.classList.add("tool-message-copy");
      toolRow.appendChild(copy);
    }
    body.appendChild(toolRow);
  } else if (copy) body.appendChild(copy);
  for (const item of usageItems) body.appendChild(contentNode(item));
  if (memoryItems.length) body.appendChild(memoryCitationNode(memoryItems));
  if (head.childNodes.length) box.appendChild(head);
  box.appendChild(body);
  renderedMessageState.set(box, {
    signature: JSON.stringify(m),
    keepToolsRunning,
  });
  return box;
}

function preserveLoadedToolDetails(previous, next) {
  const previousGroup = previous.querySelector(".tool-group"),
    nextGroup = next.querySelector(".tool-group");
  if (!previousGroup || !nextGroup) return;
  nextGroup.open = previousGroup.open;
  const previousTools = new Map(
    [...previousGroup.querySelectorAll(".tool-call")].map((detail) => [
      detail.dataset.toolIndex,
      detail,
    ]),
  );
  for (const detail of nextGroup.querySelectorAll(".tool-call")) {
    const previousDetail = previousTools.get(detail.dataset.toolIndex);
    if (!previousDetail) continue;
    detail.open = previousDetail.open;
    const loadedBody = previousDetail.querySelector(":scope > .tool-detail");
    if (!loadedBody) continue;
    detail.dataset.loaded = "1";
    detail.appendChild(loadedBody);
  }
}

function reconcileMessageNodes(root, response, activeToolMessage) {
  const existing = new Map(
      [...root.querySelectorAll(".message[data-message-index]")].map((message) => [
        message.dataset.messageIndex,
        message,
      ]),
    ),
    messageNodes = new Map(),
    nodes = [],
    turnSections = new Map(
      [...root.querySelectorAll(":scope > .turn-group")].map((section) => [
        section.dataset.turnKey,
        section,
      ]),
    );
  if (response.page.has_more) nodes.push(root.querySelector(":scope > .older") || olderButton());
  for (const message of response.messages) {
    const key = String(message.message_index),
      previous = existing.get(key),
      keepToolsRunning = message === activeToolMessage,
      previousState = previous ? renderedMessageState.get(previous) : null,
      signature = JSON.stringify(message);
    if (
      previous &&
      previousState?.signature === signature &&
      previousState.keepToolsRunning === keepToolsRunning
    ) {
      messageNodes.set(key, previous);
      continue;
    }
    const next = messageNode(message, keepToolsRunning);
    if (previous) preserveLoadedToolDetails(previous, next);
    messageNodes.set(key, next);
  }
  const turns = groupedTurns(response.messages);
  for (const [turnIndex, turn] of turns.entries()) {
    const section = turnSections.get(turn.key) || document.createElement("section"),
      members = turn.messages.map((message) => messageNodes.get(String(message.message_index))),
      completed =
        turn.turnId !== state.activeTurnId &&
        (turn.messages.some(
          (message) => message.phase === "final_answer" || turnHasUsage(message),
        ) ||
          turnIndex < turns.length - 1);
    layoutTurnGroup(section, turn, members, completed);
    nodes.push(section);
  }
  if (!response.messages.length) {
    const empty = root.querySelector(":scope > .empty") || document.createElement("div");
    empty.className = "empty";
    empty.textContent = tr("noMessages");
    nodes.push(empty);
  }
  // Move only nodes whose order changed. Re-appending every existing message to
  // a fragment briefly detaches the whole timeline and produces a visible jump.
  let cursor = root.firstChild;
  for (const node of nodes) {
    if (node === cursor) cursor = cursor.nextSibling;
    else root.insertBefore(node, cursor);
  }
  while (cursor) {
    const next = cursor.nextSibling;
    cursor.remove();
    cursor = next;
  }
  observeVisibleDeferredTurns();
}
function pendingNode(entry) {
  const box = document.createElement("article");
  // The dashed outline represents an input that has not yet materialized in
  // the authoritative rollout. Keep it through queueing/accepted/dequeued;
  // reconciliation removes the item only when the user message lands.
  const submitting = entry.status !== "failed";
  box.className = `outbox-item${entry.action === "steer" ? " steer" : ""}${entry.handoff ? " handoff" : ""}${submitting ? " submitting" : ""}`;
  box.dataset.pendingId = entry.id;
  box.dataset.pendingSignature = JSON.stringify(entry);
  const body = document.createElement("div");
  body.className = "message-body";
  body.appendChild(markdownNode(entry.text, markdownOptions(entry.thread_id || state.current?.id)));
  const meta = document.createElement("div"),
    busy = !["queued", "failed"].includes(entry.status);
  meta.className = "outbox-meta";
  if (entry.handoff) {
    const status = document.createElement("div");
    status.className = "outbox-status";
    status.textContent = tr(entry.status === "processing" ? "processingShort" : "acceptedShort");
    status.title = tr(entry.status === "processing" ? "handoffProcessing" : "serverAccepted");
    meta.appendChild(status);
  } else {
    const mode = document.createElement("span"),
      menu = document.createElement("details"),
      summary = document.createElement("summary"),
      popover = document.createElement("div");
    mode.className = "outbox-mode";
    mode.textContent = tr(entry.action === "steer" ? "followUp" : "queue");
    menu.className = "outbox-menu";
    summary.textContent = "•••";
    summary.title = tr("pendingMenu");
    summary.setAttribute("aria-label", summary.title);
    popover.className = "outbox-menu-popover";
    const remove = document.createElement("button");
    remove.className = "outbox-delete";
    remove.type = "button";
    remove.textContent = tr("withdraw");
    remove.disabled = busy;
    remove.title = busy ? tr("deleteBusy") : tr("deleteRestore");
    remove.setAttribute("aria-label", remove.title);
    remove.onclick = (event) => {
      event.stopPropagation();
      menu.open = false;
      run(() => deletePending(entry, remove));
    };
    popover.appendChild(remove);
    const hasAttachmentSummary = entry.text
      .split("\n")
      .some((line) => ["[Image attachment]", "[Audio attachment]"].includes(line.trim()));
    const nativeQueue = ["app_server_queue", "demo_wasm"].includes(entry.source);
    const mergeableQueueCount = state.pending.filter(
      (pending) =>
        pending.thread_id === entry.thread_id &&
        pending.action === "queue" &&
        pending.status === "queued" &&
        ["app_server_queue", "demo_wasm"].includes(pending.source) &&
        !pending.text
          .split("\n")
          .some((line) => ["[Image attachment]", "[Audio attachment]"].includes(line.trim())),
    ).length;
    if (
      entry.action === "queue" &&
      entry.status === "queued" &&
      nativeQueue &&
      !hasAttachmentSummary &&
      mergeableQueueCount > 1
    ) {
      const merge = document.createElement("button");
      merge.type = "button";
      merge.textContent = tr("mergeQueuedMessages");
      merge.onclick = (event) => {
        event.stopPropagation();
        menu.open = false;
        run(() => mergePendingMessages(entry, merge));
      };
      popover.prepend(merge);
    }
    if (
      entry.action === "queue" &&
      entry.status === "queued" &&
      nativeQueue &&
      !hasAttachmentSummary
    ) {
      const convert = document.createElement("button");
      convert.type = "button";
      convert.textContent = tr("convertToSteer");
      convert.disabled = busy;
      convert.onclick = (event) => {
        event.stopPropagation();
        menu.open = false;
        run(() => convertPendingToSteer(entry, convert));
      };
      popover.prepend(convert);
    }
    menu.append(summary, popover);
    meta.append(mode, menu);
  }
  box.append(meta, body);
  return box;
}
function renderPending() {
  const tray = $("outboxTray"),
    entries = state.pending.filter((entry) => entry.thread_id === state.current?.id),
    displayEntries = [...entries];
  const existingById = new Map([...tray.children].map((node) => [node.dataset.pendingId, node])),
    nodes = displayEntries.map((entry) => {
      const existing = existingById.get(entry.id),
        signature = JSON.stringify(entry);
      if (existing?.dataset.pendingSignature === signature) return existing;
      const open = existing?.querySelector(".outbox-menu")?.open,
        node = pendingNode(entry);
      if (open && node.querySelector(".outbox-menu"))
        node.querySelector(".outbox-menu").open = true;
      return node;
    });
  let cursor = tray.firstChild;
  for (const node of nodes) {
    if (node === cursor) cursor = cursor.nextSibling;
    else tray.insertBefore(node, cursor);
  }
  while (cursor) {
    const next = cursor.nextSibling;
    cursor.remove();
    cursor = next;
  }
  tray.hidden = !tray.childElementCount;
  syncOutboxCompactLabel();
}
function syncOutboxCompactLabel() {
  const tray = $("outboxTray"),
    label = tr(tray.classList.contains("compact") ? "expandPending" : "collapsePending");
  tray.title = label;
  tray.setAttribute("aria-label", label);
}
function pendingLandedInMessages(entry, messages) {
  return messages.some((message) => {
    if (message.role !== "user") return false;
    if (message.id && message.id === entry.id) return true;
    if (!Number.isSafeInteger(entry.after_message_index)) return false;
    if (message.message_index <= entry.after_message_index) return false;
    const text = (message.content || [])
      .filter((item) => item.kind === "text" && typeof item.text === "string")
      .map((item) => item.text)
      .join("\n");
    return Boolean(text) && text === entry.text;
  });
}
function mergePendingResponse(
  messages,
  preserveLocal = true,
  authoritativeMessages = state.visibleMessages,
) {
  const remote = (Array.isArray(messages) ? messages : []).filter(
    (entry) => !pendingLandedInMessages(entry, authoritativeMessages || []),
  );
  if (!preserveLocal) return remote;
  const ids = new Set(remote.map((entry) => entry.id));
  return [
    ...remote,
    ...state.pending.filter(
      (entry) => !ids.has(entry.id) && !pendingLandedInMessages(entry, authoritativeMessages || []),
    ),
  ];
}
async function refreshPending({ preserveOptimistic = true } = {}) {
  const r = await command(
    { command: "pending_messages", thread_id: state.current?.id || null },
    false,
  );
  state.pending = mergePendingResponse(r.messages, preserveOptimistic);
  renderPending();
}
async function deletePending(entry, button) {
  if (state.current?.id !== entry.thread_id) throw new Error(tr("pendingWrongSession"));
  button.disabled = true;
  try {
    const r = await command(
      { command: "pending_message_delete", id: entry.id, thread_id: entry.thread_id },
      false,
    );
    if (state.current?.id !== entry.thread_id) return;
    const text = typeof r.text === "string" ? r.text : entry.text;
    $("messageText").value = text;
    resizeComposerTextarea();
    saveDraft(entry.thread_id, text, true);
    setSendMode(r.message_action === "queue" ? "send" : "steer", false);
    state.pending = state.pending.filter(
      (item) => item.id !== entry.id || item.thread_id !== entry.thread_id,
    );
    renderPending();
    $("messageText").focus();
    $("messageText").setSelectionRange(text.length, text.length);
    notify(r.queue_deleted ? tr("queueDeletedRestore") : tr("deletedRestore"));
  } finally {
    if (button.isConnected) button.disabled = false;
  }
}
async function convertPendingToSteer(entry, button) {
  if (state.current?.id !== entry.thread_id) throw new Error(tr("pendingWrongSession"));
  button.disabled = true;
  const activity = await refreshActivity();
  if (!activity?.activity.active_turn_id) {
    button.disabled = false;
    throw new Error(tr("convertRequiresActive"));
  }
  const withdrawn = await command(
    { command: "pending_message_delete", id: entry.id, thread_id: entry.thread_id },
    false,
  );
  state.pending = state.pending.filter(
    (item) => item.id !== entry.id || item.thread_id !== entry.thread_id,
  );
  const submissionId = newSubmissionId();
  state.pending.push({
    id: submissionId,
    thread_id: entry.thread_id,
    text: withdrawn.text,
    action: "steer",
    status: "steering",
    source: "web_optimistic",
    after_message_index: state.lastMessageIndex ?? -1,
  });
  renderPending();
  try {
    await command(
      {
        command: "steer",
        thread_id: entry.thread_id,
        text: withdrawn.text,
        attachments: [],
        submission_id: submissionId,
      },
      false,
    );
    if (state.current?.id === entry.thread_id)
      await openThread(state.current, { quiet: true, preserveView: true });
    notify(tr("queueConverted"));
  } catch (error) {
    state.pending = state.pending.filter((item) => item.id !== submissionId);
    try {
      await refreshPending({ preserveOptimistic: false });
    } catch {
      renderPending();
    }
    if (state.current?.id === entry.thread_id) {
      $("messageText").value = withdrawn.text;
      saveDraft(entry.thread_id, withdrawn.text, true);
      setSendMode("steer", false);
      resizeComposerTextarea();
    }
    throw new Error(`${tr("convertFailedRestored")} ${error.message || error}`);
  } finally {
    if (button.isConnected) button.disabled = false;
  }
}
async function mergePendingMessages(entry, button) {
  if (state.current?.id !== entry.thread_id) throw new Error(tr("pendingWrongSession"));
  button.disabled = true;
  try {
    const result = await command(
      { command: "pending_messages_merge", id: entry.id, thread_id: entry.thread_id },
      false,
    );
    await refreshPending({ preserveOptimistic: false });
    notify(tr("queuedMessagesMerged", { count: result.merged_count }));
  } finally {
    if (button.isConnected) button.disabled = false;
  }
}
function olderButton() {
  const button = document.createElement("button"),
    label = tr("loadOlder", { count: state.before });
  button.className = "older";
  button.textContent = "↑";
  button.title = label;
  button.setAttribute("aria-label", label);
  button.onclick = () => run(loadOlder);
  return button;
}
async function fetchMessages(before = null, limit = state.pageSize) {
  const started = performance.now(),
    result = await command(
      { command: "messages", thread_id: state.current.id, before, limit },
      false,
    ),
    received = performance.now(),
    bytes = new TextEncoder().encode(JSON.stringify(result)).byteLength;
  recordPerformance("messages_receive", received - started, {
    bytes,
  });
  messageResponseTimings.set(result, { started, received });
  return result;
}
const messageResponseTimings = new WeakMap();
function reportMessagesRendered(result, renderStarted) {
  const timing = messageResponseTimings.get(result);
  requestAnimationFrame(() => {
    const rendered = performance.now(),
      renderDuration = rendered - renderStarted;
    // Background WebKit/Chromium tabs can throttle animation frames for tens
    // of seconds. Those samples do not describe an interactive first paint.
    if (renderDuration > 5_000) return;
    recordPerformance("messages_render", renderDuration);
    if (timing) recordPerformance("messages_visible", rendered - timing.started);
  });
}
function hydratedTurnKey(threadId, turnId) {
  return `${threadId}:${turnId}`;
}
function mergeHydratedTurnMessages(messages, threadId = state.current?.id) {
  if (!threadId) return messages;
  return messages.map((message) => {
    if (!message.turn_id) return message;
    return (
      state.hydratedTurns
        .get(hydratedTurnKey(threadId, message.turn_id))
        ?.get(message.message_index) || message
    );
  });
}
function renderVisibleMessages() {
  if (!state.current) return;
  const activeToolMessage = state.activeTurnId
    ? state.visibleMessages.findLast((message) => message.tools?.length)
    : null;
  reconcileMessageNodes(
    $("messages"),
    {
      messages: state.visibleMessages,
      page: {
        start: state.historyStart ?? 0,
        end: state.historyEnd ?? state.visibleMessages.length,
        total: state.historyTotal ?? state.visibleMessages.length,
        has_more: state.hasMore,
      },
    },
    activeToolMessage,
  );
}
async function hydrateTurn(turnId, { render = false } = {}) {
  const threadId = state.current?.id;
  if (!threadId || !turnId) return null;
  const key = hydratedTurnKey(threadId, turnId);
  let request = state.hydratingTurns.get(key);
  if (!request && !state.hydratedTurns.has(key)) {
    request = command({ command: "turn_messages", thread_id: threadId, turn_id: turnId }, false)
      .then((result) => {
        if (result.thread_id !== threadId || result.turn_id !== turnId) {
          throw new Error(tr("historyOutOfSync"));
        }
        const messages = new Map(
          (result.messages || [])
            .filter(
              (message) =>
                message.turn_id === turnId && Number.isSafeInteger(message.message_index),
            )
            .map((message) => [message.message_index, message]),
        );
        if (!messages.size) throw new Error(tr("historyOutOfSync"));
        state.hydratedTurns.set(key, messages);
        return messages;
      })
      .finally(() => state.hydratingTurns.delete(key));
    state.hydratingTurns.set(key, request);
  }
  if (request) await request;
  if (state.current?.id !== threadId) return null;
  state.visibleMessages = mergeHydratedTurnMessages(state.visibleMessages, threadId);
  if (render) renderVisibleMessages();
  return state.hydratedTurns.get(key) || null;
}
const VISIBLE_TURN_HYDRATION_INTERVAL_MS = 600;
let deferredTurnObserver = null,
  deferredTurnHydrationTimer = null,
  deferredTurnHydrationActive = false,
  lastDeferredTurnHydration = 0;
const visibleDeferredTurns = new Map();

function resetVisibleTurnHydration() {
  deferredTurnObserver?.disconnect();
  deferredTurnObserver = null;
  clearTimeout(deferredTurnHydrationTimer);
  deferredTurnHydrationTimer = null;
  visibleDeferredTurns.clear();
}

function scheduleVisibleTurnHydration() {
  if (deferredTurnHydrationActive || deferredTurnHydrationTimer || !visibleDeferredTurns.size)
    return;
  const delay = Math.max(
    0,
    VISIBLE_TURN_HYDRATION_INTERVAL_MS - (Date.now() - lastDeferredTurnHydration),
  );
  deferredTurnHydrationTimer = setTimeout(async () => {
    deferredTurnHydrationTimer = null;
    const next = visibleDeferredTurns.entries().next().value;
    if (!next) return;
    const [key, candidate] = next;
    visibleDeferredTurns.delete(key);
    if (
      !candidate.element.isConnected ||
      state.current?.id !== candidate.threadId ||
      state.openToken !== candidate.openToken
    ) {
      scheduleVisibleTurnHydration();
      return;
    }
    deferredTurnHydrationActive = true;
    try {
      await hydrateTurn(candidate.turnId);
    } catch {
      // Expanding the turn retries through the normal user-visible error path.
    } finally {
      lastDeferredTurnHydration = Date.now();
      deferredTurnHydrationActive = false;
      scheduleVisibleTurnHydration();
    }
  }, delay);
}

function observeVisibleDeferredTurns() {
  deferredTurnObserver?.disconnect();
  visibleDeferredTurns.clear();
  if (typeof IntersectionObserver !== "function" || !state.current) return;
  const threadId = state.current.id,
    openToken = state.openToken;
  deferredTurnObserver = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        const turnId = entry.target.dataset.deferredTurnId,
          key = hydratedTurnKey(threadId, turnId);
        if (!turnId || state.hydratedTurns.has(key) || state.hydratingTurns.has(key)) {
          visibleDeferredTurns.delete(key);
          continue;
        }
        if (entry.isIntersecting) {
          visibleDeferredTurns.set(key, { element: entry.target, threadId, turnId, openToken });
        } else visibleDeferredTurns.delete(key);
      }
      scheduleVisibleTurnHydration();
    },
    {
      root: usesDocumentMessageScroll() ? null : $("messages"),
      rootMargin: "48px 0px",
      threshold: 0.1,
    },
  );
  for (const fold of $("messages").querySelectorAll(".turn-fold[data-deferred-turn-id]")) {
    const key = hydratedTurnKey(threadId, fold.dataset.deferredTurnId);
    if (!state.hydratedTurns.has(key) && !state.hydratingTurns.has(key)) {
      deferredTurnObserver.observe(fold);
    }
  }
}
// Message indices come from the current rollout parse, not immutable event IDs. Continue a
// page only while its boundary is exact; otherwise rebuild the visible region atomically.
function isValidMessagePage(result, { latest = false, expectedEnd = null } = {}) {
  const page = result?.page,
    messages = result?.messages;
  if (!page || !Array.isArray(messages)) return false;
  const bounds = [page.start, page.end, page.total];
  if (
    !bounds.every(Number.isSafeInteger) ||
    page.start < 0 ||
    page.start > page.end ||
    page.end > page.total ||
    page.end - page.start !== messages.length ||
    page.before !== page.start ||
    (latest && page.end !== page.total) ||
    (expectedEnd !== null && page.end !== expectedEnd)
  )
    return false;
  return messages.every((message, offset) => message.message_index === page.start + offset);
}
function requireMessagePage(result, options) {
  if (!isValidMessagePage(result, options)) throw new Error(tr("historyOutOfSync"));
  return result;
}
function applyMessagePageState(result) {
  state.lastMessageIndex = result.messages.length ? result.page.end - 1 : null;
  state.before = result.page.before;
  state.hasMore = result.page.has_more;
  state.historyStart = result.page.start;
  state.historyEnd = result.page.end;
  state.historyTotal = result.page.total;
}
function setSendMode(mode, automatic = false) {
  if (mode === "temp" && !state.temporarySelection?.turnId)
    mode = state.activeTurnId ? "steer" : "send";
  $("sendMode").value = mode;
  state.modeAutomatic = automatic;
  const button = $("sendModeToggle"),
    isSteer = mode === "steer",
    isTemporary = mode === "temp",
    nextMode = isSteer
      ? "send"
      : mode === "send" && state.temporarySelection?.turnId
        ? "temp"
        : "steer";
  button.dataset.mode = mode;
  button.textContent = tr(isTemporary ? "temporaryShort" : isSteer ? "followUp" : "queue");
  button.title = tr(
    nextMode === "temp"
      ? "temporaryConversation"
      : nextMode === "send"
        ? "switchToQueue"
        : "switchToSteer",
  );
  button.setAttribute("aria-label", button.title);
}
function usesDocumentMessageScroll() {
  return matchMedia("(max-width:800px)").matches;
}
function messageScrollMetrics() {
  const root = $("messages");
  if (usesDocumentMessageScroll()) {
    const viewport = window.visualViewport;
    return {
      top: viewport?.pageTop ?? window.scrollY,
      height: document.documentElement.scrollHeight,
      client: viewport?.height ?? window.innerHeight,
    };
  }
  return { top: root.scrollTop, height: root.scrollHeight, client: root.clientHeight };
}
function setMessageScrollTop(top, behavior = "auto") {
  if (usesDocumentMessageScroll()) window.scrollTo({ top, behavior });
  else $("messages").scrollTo({ top, behavior });
}
function scrollMessagesToBottom(behavior = "auto") {
  state.followMessageTail = true;
  setMessageScrollTop(messageScrollMetrics().height, behavior);
}
function messageViewportTop() {
  if (!usesDocumentMessageScroll()) return $("messages").getBoundingClientRect().top;
  return Math.max(0, document.querySelector(".thread-head")?.getBoundingClientRect().bottom || 0);
}
function showActivity() {
  const active = Boolean(state.activeTurnId),
    el = $("runState"),
    label =
      state.activityPhase === "compacting"
        ? tr("sessionCompacting")
        : state.activityPhase === "tool"
          ? tr("toolRunning", { tool: state.activeTool ? ` · ${state.activeTool}` : "" })
          : tr("modelRunning");
  el.classList.toggle("active", active);
  document.querySelector(".composer-shell").classList.toggle("agent-active", active);
  syncSubmitAction();
  el.textContent = active
    ? state.pendingChanges
      ? tr("hasUpdates", { label })
      : label
    : state.pendingChanges
      ? tr("idleUpdates")
      : tr("idle");
}
function setUsageUnavailable(error) {
  state.usageUnavailable = true;
  const health = $("usageHealth");
  health.hidden = false;
  health.title = error?.message || error || tr("weeklyUnavailable");
  $("usageState").textContent = "";
  $("composerStatus").hidden = false;
  renderPending();
}
async function refreshComposerStatus() {
  if (!state.current) return;
  const usage = $("usageState"),
    health = $("usageHealth"),
    effort = $("effortState");
  if (!state.directAppServer && state.appServerMode === "desktop_bundled_only") {
    state.usageUnavailable = false;
    state.composerModel = null;
    state.composerEffort = null;
    usage.textContent = "";
    health.hidden = true;
    health.title = "";
    effort.textContent = tr("bundledOnly");
    $("modelPickerBtn").title = tr("bundledOnlyHelp");
    $("modelPickerBtn").disabled = true;
    $("composerStatus").hidden = false;
    renderPending();
    return;
  }
  const threadId = state.current.id;
  let r;
  try {
    r = await command({ command: "composer_status", thread_id: threadId }, false);
  } catch (error) {
    if (state.current?.id === threadId) setUsageUnavailable(error);
    return;
  }
  if (state.current?.id !== threadId) return;
  const weekly = r.weekly_usage;
  state.composerModel = r.model || null;
  state.composerEffort = r.reasoning_effort || null;
  state.usageUnavailable = !weekly;
  usage.textContent = weekly ? tr("weeklyRemaining", { percent: weekly.remaining_percent }) : "";
  usage.title = weekly?.resets_at
    ? tr("resetsAt", {
        time: new Date(weekly.resets_at * 1000).toLocaleString(
          getLanguage() === "zh" ? "zh-CN" : "en",
        ),
      })
    : "";
  health.hidden = Boolean(weekly);
  health.title = r.weekly_usage_error?.message || "";
  effort.textContent = state.composerModel
    ? [state.composerModel, state.composerEffort].filter(Boolean).join(" · ")
    : tr("selectModel");
  $("modelPickerBtn").title = state.composerModel ? effort.textContent : tr("selectModelHelp");
  $("modelPickerBtn").disabled = false;
  $("composerStatus").hidden = !state.current;
  renderPending();
}
function selectedModelOption() {
  return state.modelOptions.find((model) => model.id === $("modelSelect").value);
}
function renderEffortOptions(preferred) {
  const model = selectedModelOption(),
    select = $("effortSelect");
  select.textContent = "";
  for (const effort of model?.efforts || []) {
    const option = document.createElement("option");
    option.value = effort.id;
    option.textContent = effort.id;
    select.appendChild(option);
  }
  const available = (model?.efforts || []).map((effort) => effort.id);
  select.value = available.includes(preferred)
    ? preferred
    : available.includes(model?.default_effort)
      ? model.default_effort
      : available[0] || "";
  renderModelDescription();
}
function renderModelDescription() {
  const model = selectedModelOption(),
    effort = model?.efforts?.find((item) => item.id === $("effortSelect").value),
    parts = [model?.description, effort?.description].filter(Boolean);
  $("modelDescription").textContent = parts.join(" · ");
}
function renderModelOptions() {
  const select = $("modelSelect");
  select.textContent = "";
  for (const model of state.modelOptions) {
    const option = document.createElement("option");
    option.value = model.id;
    option.textContent = model.name;
    select.appendChild(option);
  }
  if (state.modelOptions.some((model) => model.id === state.composerModel))
    select.value = state.composerModel;
  renderEffortOptions(state.composerEffort);
}
function renderWorkspaceDiff(summary) {
  const button = $("workspaceDiff");
  button.hidden = Boolean(summary.clean);
  button.classList.toggle("dirty", !summary.clean);
  if (summary.clean) {
    button.textContent = tr("diffClean");
  } else {
    const untracked = summary.untracked_files
      ? tr("untracked", { count: summary.untracked_files })
      : "";
    button.textContent = tr("diffFiles", {
      count: summary.files_changed,
      additions: summary.additions,
      deletions: summary.deletions,
      untracked,
    });
  }
  const baseline = summary.base_branch || summary.base_sha.slice(0, 8);
  button.title = tr("diffWorkingTree", {
    baseline,
    sha: summary.base_sha.slice(0, 8),
    skipped: summary.untracked_lines_skipped
      ? tr("diffSkipped", { count: summary.untracked_lines_skipped })
      : "",
  });
}
async function refreshWorkspaceDiff(force = false) {
  if (!state.current || state.workspaceDiffPolling) return;
  if (!force && Date.now() - state.lastWorkspaceDiffRefresh < 3000) return;
  const threadId = state.current.id,
    button = $("workspaceDiff");
  state.workspaceDiffPolling = true;
  button.disabled = true;
  try {
    const summary = await command({ command: "workspace_diff", thread_id: threadId }, false);
    if (state.current?.id !== threadId) return;
    state.lastWorkspaceDiffRefresh = Date.now();
    renderWorkspaceDiff(summary);
  } catch (error) {
    if (state.current?.id !== threadId) return;
    button.hidden = false;
    button.classList.remove("dirty");
    button.textContent = error.message.startsWith("not_git_repository")
      ? tr("notGit")
      : tr("diffUnavailable");
    button.title = error.message;
  } finally {
    state.workspaceDiffPolling = false;
    button.disabled = false;
  }
}
async function toggleModelPicker() {
  const picker = $("modelPicker");
  if (!picker.hidden) {
    picker.hidden = true;
    return;
  }
  if (!state.current) throw new Error(tr("chooseSessionError"));
  const r = await command({ command: "composer_options" }, false);
  state.modelOptions = Array.isArray(r.models) ? r.models : [];
  if (!state.modelOptions.length) throw new Error(tr("noModels"));
  renderModelOptions();
  picker.hidden = false;
}
async function applyThreadSettings() {
  if (!state.current) throw new Error(tr("chooseSessionError"));
  const threadId = state.current.id,
    model = $("modelSelect").value,
    effort = $("effortSelect").value;
  await command({ command: "thread_settings_update", thread_id: threadId, model, effort }, false);
  if (state.current?.id !== threadId) return;
  $("modelPicker").hidden = true;
  await refreshComposerStatus();
  notify(tr("switchedModel", { model, effort }));
}
async function refreshActivity() {
  if (!state.current) return null;
  const threadId = state.current.id,
    r = await command({ command: "thread_activity", thread_id: threadId }, false);
  if (state.current?.id !== threadId) return null;
  const activity = r.activity,
    previous = state.activityFileLen;
  state.activityFileLen = activity.file_len;
  state.activeTurnId = effectiveActiveTurnId(
    activity.active_turn_id,
    state.authoritativeThreadActive.get(threadId),
  );
  state.activityPhase = activity.phase || null;
  state.activeTool = activity.active_tool || null;
  if (state.activeTurnId) setThreadRunState(threadId, "active");
  else if (state.threadRunStates.get(threadId) === "active")
    setThreadRunState(threadId, "completed");
  renderPending();
  if (!state.activeTurnId && $("sendMode").value === "steer") setSendMode("send", true);
  else if (state.activeTurnId && state.modeAutomatic) setSendMode("steer", true);
  showActivity();
  return { activity, changed: previous !== null && previous !== activity.file_len };
}
async function pollActivity() {
  if (state.polling || !state.current) return;
  state.polling = true;
  try {
    const result = await refreshActivity();
    if (result?.changed) {
      state.pendingChanges = true;
      showActivity();
      refreshWorkspaceDiff().catch(() => {});
    }
    if (state.pendingChanges) {
      const refreshDelay = 400 - (Date.now() - state.lastMessageRefresh);
      if (refreshDelay > 0) {
        scheduleEventRefresh(state.current.id, false, refreshDelay);
        return;
      }
      await openThread(state.current, { quiet: true, preserveView: true });
      return;
    }
    await refreshPending();
  } finally {
    state.polling = false;
  }
}
let eventRefreshTimer = null;
function scheduleEventRefresh(threadId, immediate = false, delay = null) {
  if (threadId !== state.current?.id) return;
  state.pendingChanges = true;
  clearTimeout(eventRefreshTimer);
  eventRefreshTimer = setTimeout(
    () => {
      eventRefreshTimer = null;
      pollActivity().catch(() => {});
    },
    immediate ? 0 : (delay ?? 280),
  );
}
function handleTemporaryAppServerEvent(method, params, threadId) {
  const temporary = [...state.temporaryThreads.values()].find((entry) => entry.id === threadId);
  if (!temporary) return false;
  const item = params.item || null,
    itemType = item?.type,
    assistant = () => {
      let message = temporary.messages.findLast(
        (entry) => entry.role === "assistant" && entry.running,
      );
      if (!message) {
        message = { role: "assistant", text: "", id: null, running: true };
        temporary.messages.push(message);
      }
      return message;
    };
  if (method === "turn/started") temporary.activeTurnId = params.turn?.id || temporary.activeTurnId;
  else if (method === "item/started" && itemType === "agentMessage") {
    assistant().id = item.id || null;
  } else if (method === "item/agentMessage/delta") {
    const message = assistant();
    message.id ||= params.itemId || null;
    message.text += typeof params.delta === "string" ? params.delta : "";
  } else if (method === "item/completed" && itemType === "agentMessage") {
    const message = assistant(),
      text = temporaryItemText(item);
    message.id = item.id || message.id;
    if (text) message.text = text;
    message.running = false;
  } else if (method === "turn/completed") {
    temporary.activeTurnId = null;
    const message = temporary.messages.findLast((entry) => entry.role === "assistant");
    if (message) message.running = false;
  } else return true;
  if (temporary === state.temporaryThread) renderTemporaryMessages();
  return true;
}

function handleBridgeEvent(event) {
  if (event?.type === "bridge_event_stream") {
    state.eventStreamConnected = event.status === "ready";
    if (!state.eventStreamConnected) pollActivity().catch(() => {});
    return;
  }
  if (event?.type === "bridge_event_gap") {
    state.lastMessageRefresh = 0;
    for (const threadId of state.taskTrackedIds) state.taskDirtyIds.add(threadId);
    scheduleTasksRefresh(0);
    scheduleEventRefresh(state.current?.id, true);
    loadProjects().catch(() => {});
    return;
  }
  if (event?.type === "bridge_app_server_connection") {
    loadStatus().catch(() => {});
    return;
  }
  if (event?.type === "bridge_service_snapshot") {
    observeAppServerService(event.managed_services?.app_server);
    state.managedServices = event.managed_services || null;
    state.serverCapabilities = event.capabilities || state.serverCapabilities;
    state.runtimeResources = event.runtime_resources || state.runtimeResources;
    renderManagedServices();
    syncVoiceCapability();
    return;
  }
  if (event?.type === "bridge_thread_activity_snapshot") {
    if (!Array.isArray(event.active_thread_ids)) return;
    state.updatingThreads.clear();
    state.threadRunStates.clear();
    const snapshotIds = new Set(
      event.thread_states && typeof event.thread_states === "object"
        ? Object.keys(event.thread_states)
        : event.active_thread_ids,
    );
    for (const threadId of state.taskTrackedIds) {
      if (snapshotIds.has(threadId)) continue;
      state.taskTrackedIds.delete(threadId);
      state.taskOverviews.delete(threadId);
      state.taskDirtyIds.delete(threadId);
    }
    if (event.thread_states && typeof event.thread_states === "object") {
      for (const [threadId, runState] of Object.entries(event.thread_states)) {
        state.authoritativeThreadActive.set(threadId, runState === "active");
        setThreadRunState(threadId, runState);
      }
    }
    for (const threadId of event.active_thread_ids) {
      if (typeof threadId === "string" && threadId) setThreadRunState(threadId, "active");
    }
    renderProjects();
    renderTaskOverviews();
    return;
  }
  if (event?.type !== "app_server") return;
  const message = event.message || {},
    method = message.method,
    params = message.params || {},
    threadId = params.threadId || null;
  if (!method) return;
  if (threadId && handleTemporaryAppServerEvent(method, params, threadId)) return;
  if (method === "account/updated") {
    loadStatus().catch(() => {});
    return;
  }
  if (threadId && state.taskTrackedIds.has(threadId)) markTaskDirty(threadId);
  if (method === "thread/status/changed" && threadId) {
    updateThreadLiveFromStatus(threadId, params.status);
  } else if (method === "turn/started" && threadId) {
    state.authoritativeThreadActive.set(threadId, true);
    setThreadRunState(threadId, "active");
    renderProjects();
    if (threadId === state.current?.id) {
      state.activeTurnId = params.turn?.id || state.activeTurnId;
      showActivity();
    }
  } else if (method === "turn/completed" && threadId) {
    const completedTurnId = params.turn?.id || null,
      completionIsCurrent =
        threadId !== state.current?.id ||
        completionMatchesActiveTurn(state.activeTurnId, completedTurnId),
      runState = completedTurnRunState(params.turn);
    // An interrupted turn may complete after the queued input has already
    // started a successor turn. Never let that older completion clear the new
    // active turn or replace its green activity state with a terminal state.
    if (completionIsCurrent) {
      state.authoritativeThreadActive.set(threadId, false);
      setThreadRunState(threadId, runState);
    }
    notifyTurnFinished(threadId, runState, params.turn?.id);
    renderProjects();
    if (threadId === state.current?.id && completionIsCurrent) {
      state.activeTurnId = null;
      showActivity();
    }
  }
  if (method === "thread/queue/changed" && threadId === state.current?.id) {
    refreshPending().catch(() => {});
  }
  if (method === "thread/goal/updated" && threadId === state.current?.id) {
    state.threadGoal = params.goal || null;
    renderGoalPanel();
  } else if (method === "thread/goal/cleared" && threadId === state.current?.id) {
    state.threadGoal = null;
    renderGoalPanel();
  }
  if (
    threadId === state.current?.id &&
    (method.startsWith("turn/") ||
      method.startsWith("item/") ||
      method === "thread/status/changed" ||
      method === "thread/reverted")
  ) {
    scheduleEventRefresh(threadId, method === "turn/completed");
  }
}
function updateThreadLiveFromStatus(threadId, status) {
  const statusType = typeof status === "string" ? status : status?.type;
  if (!statusType) return;
  const active = statusType === "active";
  state.authoritativeThreadActive.set(threadId, active);
  if (active) setThreadRunState(threadId, "active");
  else {
    if (state.threadRunStates.get(threadId) === "active") {
      setThreadRunState(threadId, "completed");
    }
    if (threadId === state.current?.id) {
      state.activeTurnId = null;
      state.activityPhase = null;
      state.activeTool = null;
      if ($("sendMode").value === "steer") setSendMode("send", true);
      showActivity();
    }
  }
  renderProjects();
}
function resetHorizontalPosition() {
  const root = $("messages");
  root.scrollLeft = 0;
  document.documentElement.scrollLeft = 0;
  document.body.scrollLeft = 0;
  window.scrollTo(0, window.scrollY);
}
function resizeComposerTextarea() {
  const textarea = $("messageText"),
    composer = document.querySelector(".composer"),
    shell = document.querySelector(".composer-shell"),
    threadHead = document.querySelector(".thread-head");
  if (!textarea || !composer || !shell || !threadHead) return;
  shell.classList.toggle("has-text", Boolean(textarea.value || state.composerAttachments.length));
  syncComposerPlaceholder();
  const minimum = usesDocumentMessageScroll() ? 56 : 44;
  textarea.style.height = `${minimum}px`;
  const viewportHeight = window.visualViewport?.height || window.innerHeight,
    composerChrome = Math.max(0, composer.getBoundingClientRect().height - minimum),
    available = Math.max(
      minimum,
      viewportHeight - threadHead.getBoundingClientRect().height - composerChrome - 16,
    ),
    desktopLimit = Math.min(320, viewportHeight * 0.34),
    limit = usesDocumentMessageScroll() ? available : desktopLimit,
    height = Math.min(Math.max(minimum, textarea.scrollHeight), limit);
  textarea.style.height = `${Math.ceil(height)}px`;
  textarea.style.overflowY = textarea.scrollHeight > height + 1 ? "auto" : "hidden";
}
function resizeComposerAfterViewportChange() {
  if (document.activeElement === $("messageText")) syncFocusedComposerViewport();
  else resizeComposerTextarea();
}

let focusedComposerFrame = 0;
function syncFocusedComposerViewport() {
  const composer = document.querySelector(".composer"),
    main = document.querySelector("main"),
    viewport = window.visualViewport;
  if (
    !composer ||
    !main ||
    !usesDocumentMessageScroll() ||
    document.activeElement !== $("messageText")
  ) {
    composer?.classList.remove("viewport-anchored");
    composer?.style.removeProperty("--composer-viewport-top");
    return;
  }
  cancelAnimationFrame(focusedComposerFrame);
  focusedComposerFrame = requestAnimationFrame(() => {
    const pageTop = viewport?.pageTop ?? window.scrollY + (viewport?.offsetTop || 0),
      visibleHeight = viewport?.height || window.innerHeight,
      mainPageTop = main.getBoundingClientRect().top + window.scrollY,
      top = Math.max(0, pageTop + visibleHeight - composer.offsetHeight - mainPageTop);
    composer.style.setProperty("--composer-viewport-top", `${Math.round(top)}px`);
    composer.classList.add("viewport-anchored");
  });
}
function syncComposerPlaceholder() {
  const textarea = $("messageText"),
    shell = document.querySelector(".composer-shell"),
    compact =
      usesDocumentMessageScroll() &&
      !textarea.value &&
      document.activeElement !== textarea &&
      !shell.classList.contains("input-focused");
  textarea.placeholder = tr(compact ? "messagePlaceholderCompact" : "messagePlaceholder");
}
function settleHorizontalPosition() {
  requestAnimationFrame(() => {
    resetHorizontalPosition();
    requestAnimationFrame(resetHorizontalPosition);
  });
}
function captureMessageView() {
  const root = $("messages"),
    metrics = messageScrollMetrics(),
    viewportTop = messageViewportTop(),
    anchor = [...root.querySelectorAll(".message[data-message-index]")].find(
      (message) =>
        !message.hidden &&
        message.getClientRects().length > 0 &&
        message.getBoundingClientRect().bottom > viewportTop + 1,
    ),
    anchorTurnKey = anchor?.closest(".turn-group")?.dataset.turnKey || null;
  return {
    atBottom: shouldFollowMessageTail(state.followMessageTail, metrics),
    top: metrics.top,
    anchorMessageIndex: anchor?.dataset.messageIndex || null,
    anchorTurnKey,
    anchorOffset: anchor ? anchor.getBoundingClientRect().top - viewportTop : null,
    openDetails: [...root.querySelectorAll(".message details[open]")].map((detail) => {
      const message = detail.closest(".message");
      return `${message?.dataset.messageIndex || ""}:${[
        ...message.querySelectorAll("details"),
      ].indexOf(detail)}`;
    }),
  };
}
function restoreMessageView(view) {
  if (!view) return scrollMessagesToBottom();
  const openDetails = new Set(view.openDetails);
  for (const message of $("messages").querySelectorAll(".message")) {
    [...message.querySelectorAll("details")].forEach((detail, index) => {
      detail.open = openDetails.has(`${message.dataset.messageIndex || ""}:${index}`);
    });
  }
  if (view.atBottom) {
    // Incremental tool and assistant updates can arrive before the previous layout has settled.
    // Keep tail-following deterministic: an interrupted smooth scroll retains an obsolete target
    // in WebKit and Firefox and can jump back into older history on the next refresh.
    scrollMessagesToBottom("auto");
    requestAnimationFrame(() => scrollMessagesToBottom("auto"));
    return;
  }
  const anchor = view.anchorMessageIndex
    ? [...$("messages").querySelectorAll(".message[data-message-index]")].find(
        (message) => message.dataset.messageIndex === view.anchorMessageIndex,
      )
    : null;
  if (
    anchor &&
    !anchor.hidden &&
    anchor.getClientRects().length > 0 &&
    view.anchorOffset !== null
  ) {
    const delta = anchor.getBoundingClientRect().top - messageViewportTop() - view.anchorOffset;
    if (Number.isFinite(delta)) setMessageScrollTop(messageScrollMetrics().top + delta);
    else setMessageScrollTop(view.top);
  } else setMessageScrollTop(view.top);
}
async function openThread(
  thread,
  { quiet = false, preserveView = false, writeHash = true, replaceHash = false } = {},
) {
  const token = ++state.openToken,
    changedThread = state.current?.id !== thread.id,
    messageView = quiet && preserveView && !changedThread ? captureMessageView() : null;
  if (!quiet) {
    closePanels();
    setThreadHeaderExpanded(false);
  }
  if (changedThread && state.current) {
    saveDraft(state.current.id, $("messageText").value, true);
    state.attachmentDrafts.set(state.current.id, state.composerAttachments);
    if (state.composerReference)
      state.referenceDrafts.set(state.current.id, state.composerReference);
  }
  state.current = thread;
  syncTemporaryForCurrent();
  if (changedThread) {
    state.threadGoal = null;
    renderGoalPanel();
    resetVisibleTurnHydration();
    restoreComposerDraft($("messageText"), state.drafts.get(thread.id), true);
    state.composerAttachments = state.attachmentDrafts.get(thread.id) || [];
    renderComposerAttachments();
    setComposerReference(state.referenceDrafts.get(thread.id) || null, false);
    state.before = null;
    state.hasMore = false;
    state.activityFileLen = null;
    state.activeTurnId = null;
    state.activityPhase = null;
    state.activeTool = null;
    state.pendingChanges = false;
    state.repairRequired = false;
    state.threadStatistics = null;
    state.lastMessageIndex = null;
    state.visibleMessages = [];
    state.hydratedTurns.clear();
    state.hydratingTurns.clear();
    state.historyStart = null;
    state.historyEnd = null;
    state.historyTotal = null;
    state.lastWorkspaceDiffRefresh = 0;
    $("composerStatus").hidden = true;
    $("modelPicker").hidden = true;
  }
  refreshComposerStatus().catch(() => {});
  renderPending();
  if (!quiet) {
    state.userScrolled = false;
    state.followMessageTail = true;
    document.activeElement?.blur();
    resetHorizontalPosition();
  }
  renderProjects();
  $("threadTitle").textContent = thread.title || thread.id;
  renderRepairHint();
  renderThreadStatistics();
  $("threadCompactMeta").textContent = thread.git_branch || tr("noBranch");
  $("threadMeta").textContent =
    `${thread.cwd} · ${thread.git_branch || tr("noBranch")} · ${thread.id}`;
  const root = $("messages");
  const cached = changedThread ? state.messageCache.get(thread.id) : null;
  if (!quiet) {
    if (cached && isValidMessagePage(cached, { latest: true })) {
      root.replaceChildren();
      state.visibleMessages = cached.messages;
      reconcileMessageNodes(root, cached, null);
      applyMessagePageState(cached);
      requestAnimationFrame(scrollMessagesToBottom);
    } else {
      root.innerHTML = `<div class="empty">${tr("loadingLatest")}</div>`;
    }
  }
  const [r, , , pendingResult, goalResult] = await Promise.all([
    fetchMessages(),
    refreshActivity(),
    demoMode
      ? Promise.resolve()
      : command({ command: "thread_watch", thread_id: thread.id }, false)
          .then((watch) => updateThreadLiveFromStatus(thread.id, watch.thread?.status))
          .catch(() => {}),
    command({ command: "pending_messages", thread_id: thread.id }, false).catch(() => null),
    command({ command: "thread_goal_get", thread_id: thread.id }, false).catch(() => undefined),
  ]);
  if (token !== state.openToken) return;
  requireMessagePage(r, { latest: true });
  renderRepairHint(r.repair_required);
  renderThreadStatistics(r.statistics);
  r.messages = mergeHydratedTurnMessages(r.messages, thread.id);
  state.pending = mergePendingResponse(pendingResult?.messages || state.pending, true, r.messages);
  state.messageCache.set(thread.id, r);
  state.current = { ...thread, ...r.thread };
  if (goalResult) state.threadGoal = goalResult.goal || null;
  renderGoalPanel();
  rememberSessionId(window.localStorage, state.current.id);
  if (writeHash) updateSessionHash(state.current.id, replaceHash);
  const activeToolMessage = state.activeTurnId
    ? r.messages.findLast((message) => message.tools?.length)
    : null;
  const preservedHasMore = state.hasMore,
    olderMessages = changedThread
      ? []
      : state.visibleMessages.filter((message) => message.message_index < r.page.start),
    visibleResponse = {
      ...r,
      messages: [...olderMessages, ...r.messages],
      page: { ...r.page, has_more: olderMessages.length ? preservedHasMore : r.page.has_more },
    };
  state.visibleMessages = visibleResponse.messages;
  if (messageView?.anchorTurnKey && !messageView.atBottom) {
    state.expandedTurnIds.add(`${thread.id}:${messageView.anchorTurnKey}`);
  }
  const renderStarted = performance.now();
  reconcileMessageNodes(root, visibleResponse, activeToolMessage);
  applyMessagePageState(r);
  if (olderMessages.length) {
    state.historyStart = olderMessages[0].message_index;
    state.before = state.historyStart;
    state.hasMore = preservedHasMore;
  }
  renderPending();
  restoreMessageView(messageView);
  reportMessagesRendered(r, renderStarted);
  state.pendingChanges = false;
  state.lastMessageRefresh = Date.now();
  showActivity();
  await refreshWorkspaceDiff(true);
  if (!quiet) settleHorizontalPosition();
}
async function refreshThread() {
  if (!state.current) return notify(tr("chooseSessionError"), true);
  return openThread(state.current);
}
function clearCurrentSessionMessageCaches(threadId) {
  const prefix = `${threadId}:`;
  state.messageCache.delete(threadId);
  for (const key of [...state.hydratedTurns.keys()]) {
    if (key.startsWith(prefix)) state.hydratedTurns.delete(key);
  }
  for (const key of [...state.hydratingTurns.keys()]) {
    if (key.startsWith(prefix)) state.hydratingTurns.delete(key);
  }
  resetVisibleTurnHydration();
  state.visibleMessages = [];
  state.before = null;
  state.hasMore = false;
  state.historyStart = null;
  state.historyEnd = null;
  state.historyTotal = null;
  state.lastMessageIndex = null;
}
async function refreshCurrentSessionFromTools() {
  if (!state.current) throw new Error(tr("chooseSessionError"));
  const thread = { ...state.current };
  closePanels();
  clearCurrentSessionMessageCaches(thread.id);
  state.userScrolled = false;
  state.followMessageTail = true;
  $("messages").innerHTML = `<div class="empty">${tr("loadingLatest")}</div>`;
  await openThread(thread, { quiet: true, writeHash: false });
  scrollMessagesToBottom("auto");
  requestAnimationFrame(() => scrollMessagesToBottom("auto"));
  notify(tr("sessionRefreshed"));
}
async function loadOlder() {
  if (!state.current || !state.hasMore || state.loadingHistory) return;
  state.loadingHistory = true;
  state.userScrolled = false;
  state.followMessageTail = false;
  const threadId = state.current.id,
    openToken = state.openToken,
    expectedEnd = state.historyStart ?? state.before,
    root = $("messages"),
    metrics = messageScrollMetrics(),
    oldHeight = metrics.height,
    oldTop = metrics.top;
  let r;
  try {
    r = await fetchMessages(state.before);
  } finally {
    state.loadingHistory = false;
  }
  if (state.current?.id !== threadId || state.openToken !== openToken) return;
  if (
    !isValidMessagePage(r, { expectedEnd }) ||
    (state.historyTotal !== null && r.page.total < state.historyTotal)
  ) {
    await openThread(state.current, { quiet: true });
    notify(tr("historyResynced"));
    return;
  }
  state.before = r.page.before;
  state.hasMore = r.page.has_more;
  state.historyStart = r.page.start;
  state.historyTotal = Math.max(state.historyTotal ?? 0, r.page.total);
  r.messages = mergeHydratedTurnMessages(r.messages, threadId);
  state.visibleMessages = [...r.messages, ...state.visibleMessages];
  const activeToolMessage = state.activeTurnId
    ? state.visibleMessages.findLast((message) => message.tools?.length)
    : null;
  const renderStarted = performance.now();
  reconcileMessageNodes(
    root,
    {
      ...r,
      messages: state.visibleMessages,
      page: { ...r.page, start: state.historyStart, end: state.historyEnd },
    },
    activeToolMessage,
  );
  reportMessagesRendered(r, renderStarted);
  const newHeight = messageScrollMetrics().height;
  if (usesDocumentMessageScroll()) window.scrollTo(0, oldTop + (newHeight - oldHeight));
  else root.scrollTop = oldTop + (newHeight - oldHeight);
}
function targetRequest(name, extra = {}) {
  if (!state.current) throw new Error(tr("chooseSessionError"));
  return { command: name, thread_id: state.current.id, ...extra };
}
function setComposerSubmitting(active) {
  state.composerSubmitting = active;
  const shell = document.querySelector(".composer-shell");
  shell.classList.toggle("submitting", active);
  $("messageText").disabled = active;
  $("sendModeToggle").disabled = active;
  $("attachBtn").disabled = active;
  $("imageInput").disabled = active;
  syncVoiceCapability();
  syncSubmitAction();
}
function syncSubmitAction() {
  const shell = document.querySelector(".composer-shell"),
    stopReady = shouldOfferStop({
      activeTurnId: state.activeTurnId,
    }),
    stop = $("stopBtn"),
    submit = $("submitBtn");
  shell.classList.toggle("stop-visible", stopReady);
  shell.classList.toggle("interrupting", state.interrupting);
  stop.title = tr("stopRunAria");
  stop.setAttribute("aria-label", stop.title);
  stop.setAttribute("aria-hidden", String(!stopReady));
  stop.tabIndex = stopReady ? 0 : -1;
  stop.disabled = state.interrupting;
  submit.disabled = state.composerSubmitting || state.interrupting;
  submit.title = tr("submitAria");
  submit.setAttribute("aria-label", submit.title);
  syncComposerPlaceholder();
}
async function interruptCurrentRun({ confirm = true, requireActive = false } = {}) {
  const interruptedTurnId = state.activeTurnId;
  if ((requireActive && !interruptedTurnId) || state.interrupting) return;
  const pendingCount = state.pending.filter(
    (entry) => entry.thread_id === state.current?.id && entry.status !== "failed",
  ).length;
  if (
    confirm &&
    !window.confirm(
      tr(pendingCount ? "stopRunConfirmPending" : "stopRunConfirm", { count: pendingCount }),
    )
  )
    return;
  state.interrupting = true;
  syncSubmitAction();
  try {
    const threadId = state.current?.id;
    await command(targetRequest("interrupt", { turn_id: interruptedTurnId }));
    notify(tr("turnStopRequested"));
    // Do not delete pending inputs here. A queue/add request may still be in
    // flight, a queued item remains safely withdrawable, and an accepted item
    // must stay visible until rollout reconciliation proves that it landed.
    await Promise.allSettled([refreshPending(), refreshActivity()]);
    if (state.current?.id === threadId && !state.activeTurnId)
      setThreadRunState(threadId, "cancelled");
  } finally {
    state.interrupting = false;
    syncSubmitAction();
  }
}
async function continuePendingQueue() {
  if (!state.current) throw new Error(tr("chooseSessionError"));
  const threadId = state.current.id,
    entry = state.pending.find(
      (pending) =>
        pending.thread_id === threadId &&
        pending.action === "queue" &&
        pending.status === "queued" &&
        ["app_server_queue", "demo_wasm"].includes(pending.source),
    );
  if (!entry) throw new Error(tr("messageRequired"));
  setComposerSubmitting(true);
  try {
    await command({ command: "pending_message_start", thread_id: threadId, id: entry.id }, false);
    notify(tr("queueContinueStarted"));
    await refreshPending({ preserveOptimistic: false });
    if (state.current?.id === threadId)
      await openThread(state.current, { quiet: true, preserveView: true });
  } finally {
    setComposerSubmitting(false);
  }
}
function newSubmissionId() {
  return `web-${
    typeof crypto.randomUUID === "function"
      ? crypto.randomUUID()
      : `${Date.now()}-${Math.random().toString(36).slice(2)}`
  }`;
}
async function write(name) {
  const draft = $("messageText").value,
    text = draft.trim(),
    attachments = state.composerAttachments.map((attachment) => ({ ...attachment })),
    reference = state.composerReference ? { ...state.composerReference } : null,
    requestText = reference ? selectedContextPrompt(reference, text) : text;
  if (!text && !attachments.length) {
    if (!reference) return continuePendingQueue();
    throw new Error(tr("messageRequired"));
  }
  if (voiceRecorder?.state === "recording") throw new Error(tr("stopRecording"));
  if (!state.current) throw new Error(tr("chooseSessionError"));
  if (name === "temp") {
    if (attachments.length) throw new Error(tr("temporaryCreateFailed"));
    setComposerSubmitting(true);
    try {
      await createTemporaryThread(state.temporarySelection, draft);
      await sendTemporaryMessage();
      setComposerReference();
    } finally {
      setComposerSubmitting(false);
    }
    return;
  }
  const threadId = state.current.id,
    submissionId = newSubmissionId();
  let action = name === "steer" ? "steer" : "queue";
  const pendingText =
    text ||
    attachments
      .map((attachment) =>
        attachment.type === "audio" ? "[Audio attachment]" : "[Image attachment]",
      )
      .join("\n");
  let acknowledged = false;
  state.pending.push({
    id: submissionId,
    thread_id: threadId,
    text: pendingText,
    action,
    status: `${action}ing`,
    source: "web_optimistic",
    after_message_index: state.lastMessageIndex ?? -1,
  });
  renderPending();
  setComposerSubmitting(true);
  try {
    if (name === "steer") {
      const activity = await refreshActivity();
      if (!activity?.activity.active_turn_id) {
        name = "send";
        action = "queue";
        const optimistic = state.pending.find((entry) => entry.id === submissionId);
        if (optimistic) {
          optimistic.action = action;
          optimistic.status = "queueing";
          renderPending();
        }
        setSendMode("send", true);
        notify(tr("noActiveTurnQueued"));
      }
    }
    $("messageText").value = "";
    resizeComposerTextarea();
    saveDraft(threadId, "", true);
    const request = command({
      command: name,
      thread_id: threadId,
      text: requestText,
      attachments,
      submission_id: submissionId,
    }).then(
      (value) => ({ value }),
      (error) => ({ error }),
    );
    const outcome = await request;
    if (outcome.error) {
      await refreshPending({ preserveOptimistic: false });
      throw outcome.error;
    }
    setComposerSubmitting(false);
    acknowledged = true;
    state.composerAttachments = [];
    state.attachmentDrafts.delete(threadId);
    renderComposerAttachments();
    state.referenceDrafts.delete(threadId);
    if (state.current?.id === threadId) setComposerReference(null, false);
    try {
      if (state.current?.id === threadId) await openThread(state.current, { quiet: true });
      else await refreshPending();
    } catch {
      notify(tr("acceptedRefreshFailed"), true);
      return;
    }
    notify(name === "send" ? tr("messageQueued") : tr("guidanceSteered"));
  } catch (error) {
    if (!acknowledged) {
      if (!state.drafts.has(threadId)) saveDraft(threadId, draft, true);
      if (state.current?.id === threadId && !$("messageText").value)
        $("messageText").value = state.drafts.get(threadId) || "";
      resizeComposerTextarea();
    }
    throw error;
  } finally {
    setComposerSubmitting(false);
  }
}
function approval(name) {
  const id = Number($("approvalId").value);
  if (!Number.isSafeInteger(id) || id < 0) throw new Error(tr("validRequestId"));
  return command({ command: name, id });
}
$("search").oninput = renderProjects;
$("archived").onchange = () => run(loadProjects);
$("reloadBtn").onclick = () => run(loadProjects);
$("tasksBtn").onclick = openTasksDialog;
$("tasksCloseBtn").onclick = closeTasksDialog;
$("tasksDialog").onclick = (event) => {
  if (event.target === $("tasksDialog")) closeTasksDialog();
};
$("statusBtn").onclick = () => run(loadStatus);
$("notificationBtn").onclick = () => run(toggleBrowserNotifications);
$("languageBtn").onclick = () => run(toggleLanguage);
$("refreshBtn").onclick = () => run(refreshThread);
$("historyFullscreenBtn").onclick = () => setHistoryFullscreen(!isHistoryFullscreen());
$("collapseMessagesBtn").onclick = collapseExpandedMessages;
$("sessionRefreshBtn").onclick = () => run(refreshCurrentSessionFromTools);
$("createSessionBtn").onclick = () => showCreateDialog();
$("createProjectNextBtn").onclick = () => run(chooseCreateProject);
$("createProjectBackBtn").onclick = backToCreateProject;
$("createCurrentBtn").onclick = () => run(() => createThread(false));
$("createWorktreeBtn").onclick = () => run(() => createThread(true));
$("attachBtn").onclick = () => $("imageInput").click();
$("imageInput").onchange = (event) => {
  const files = [...event.target.files];
  event.target.value = "";
  run(() => addImages(files));
};
$("audioInput").onchange = (event) => {
  const [file] = event.target.files;
  event.target.value = "";
  if (file) run(() => addAudioFile(file));
};
$("voiceBtn").onclick = () => run(toggleVoiceRecording);
$("createCancelBtn").onclick = closeCreateDialog;
$("createDialog").onclick = (event) => {
  if (event.target === $("createDialog")) closeCreateDialog();
};
$("goalHideBtn").onclick = () => setGoalPanelCollapsed(true);
$("goalRestoreBtn").onclick = () => setGoalPanelCollapsed(false);
$("goalEditBtn").onclick = openGoalEditDialog;
$("goalEditCancelBtn").onclick = closeGoalEditDialog;
$("goalEditDialog").onclick = (event) => {
  if (event.target === $("goalEditDialog")) closeGoalEditDialog();
};
$("goalToggleBtn").onclick = () =>
  run(() => {
    const { nextStatus } = goalToggleState(state.threadGoal?.status);
    if (!nextStatus) return;
    return setThreadGoal(
      { status: nextStatus },
      nextStatus === "active" ? "goalResumed" : "goalPausedNotice",
    );
  });
$("goalEditSaveBtn").onclick = () =>
  run(async () => {
    const objective = $("goalObjectiveInput").value.trim();
    if (!objective) throw new Error(tr("goalObjectiveRequired"));
    await setThreadGoal({ objective }, "goalSaved");
    closeGoalEditDialog();
  });
function toggleSendModeAndKeepFocus() {
  const mode = $("sendMode").value,
    nextMode =
      mode === "steer"
        ? "send"
        : mode === "send" && state.temporarySelection?.turnId
          ? "temp"
          : "steer";
  setSendMode(nextMode, false);
  $("messageText").focus({ preventScroll: true });
}
$("sendModeToggle").onclick = toggleSendModeAndKeepFocus;
$("submitBtn").onclick = () => run(() => write($("sendMode").value));
$("stopBtn").onclick = () => run(() => interruptCurrentRun({ requireActive: true }));
$("interruptBtn").onclick = () => run(() => interruptCurrentRun());
$("mobileInterruptBtn").onclick = () => $("interruptBtn").click();
$("renameThreadBtn").onclick = () =>
  run(async () => {
    if (!state.current) throw new Error(tr("chooseSessionError"));
    const currentName = state.current.title || "";
    const entered = window.prompt(tr("renamePrompt"), currentName);
    if (entered === null) return;
    const name = entered.trim();
    if (!name || [...name].length > 200) throw new Error(tr("renameInvalid"));
    const threadId = state.current.id;
    await command({ command: "thread_rename", thread_id: threadId, name }, false);
    if (state.current?.id === threadId) {
      state.current.title = name;
      $("threadTitle").textContent = name;
    }
    await loadProjects();
    notify(tr("sessionRenamed"));
  });
$("archiveThreadBtn").onclick = () =>
  run(async () => {
    if (!state.current) throw new Error(tr("chooseSessionError"));
    const threadId = state.current.id,
      title = state.current.title || threadId;
    if (!window.confirm(tr("archiveConfirm", { title }))) return;
    await command({ command: "thread_archive", thread_id: threadId }, false);
    state.current = null;
    state.threadGoal = null;
    renderGoalPanel();
    state.activeTurnId = null;
    state.activeTool = null;
    state.activityPhase = null;
    $("threadTitle").textContent = tr("chooseSession");
    $("threadCompactMeta").textContent = "";
    $("threadMeta").textContent = tr("archivedRemoved");
    $("messages").innerHTML = `<div class="empty">${tr("loadingAnother")}</div>`;
    $("messageText").value = "";
    state.composerAttachments = [];
    state.attachmentDrafts.delete(threadId);
    state.composerReference = null;
    state.referenceDrafts.delete(threadId);
    renderComposerAttachments();
    renderComposerReference();
    resizeComposerTextarea();
    renderPending();
    closePanels();
    await loadProjects();
    notify(tr("sessionArchived"));
  });
$("repairOrdinalsBtn").onclick = () =>
  run(async () => {
    if (!state.current) throw new Error(tr("chooseSessionError"));
    if (!window.confirm(tr("repairOrdinalsConfirm"))) return;
    const button = $("repairOrdinalsBtn"),
      threadId = state.current.id;
    button.disabled = true;
    try {
      const result = await command(
        { command: "thread_repair_ordinals", thread_id: threadId },
        false,
      );
      notify(
        result.status === "repaired"
          ? tr("repairOrdinalsDone", { count: result.renumbered_records })
          : tr("repairOrdinalsClean"),
      );
      if (state.current?.id === threadId)
        await openThread(state.current, { quiet: true, preserveView: true });
    } finally {
      button.disabled = false;
    }
  });
$("usageHealth").onclick = () => notify($("usageHealth").title || tr("weeklyUnavailable"), true);
$("usageHealth").onkeydown = (event) => {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    $("usageHealth").click();
  }
};
$("workspaceDiff").onclick = () => refreshWorkspaceDiff(true);
$("themeSelect").onchange = (event) => applyTheme(event.target.value);
$("modelPickerBtn").onclick = () => run(toggleModelPicker);
$("modelPickerClose").onclick = () => ($("modelPicker").hidden = true);
$("modelSelect").onchange = () => renderEffortOptions(state.composerEffort);
$("effortSelect").onchange = renderModelDescription;
$("modelApply").onclick = () => run(applyThreadSettings);
$("messageText").oninput = () => {
  saveDraft(state.current?.id, $("messageText").value);
  resizeComposerTextarea();
};
$("messageText").onpointerdown = () => (
  document.querySelector(".composer-shell").classList.add("focused", "input-focused"),
  syncSubmitAction()
);
$("messageText").onfocus = () => {
  document.querySelector(".composer-shell").classList.add("focused", "input-focused");
  syncSubmitAction();
  requestAnimationFrame(() => {
    resizeComposerTextarea();
    syncFocusedComposerViewport();
  });
};
$("messageText").onblur = () =>
  requestAnimationFrame(() => {
    const shell = document.querySelector(".composer-shell");
    if (document.activeElement !== $("messageText")) shell.classList.remove("input-focused");
    syncFocusedComposerViewport();
    syncSubmitAction();
  });
function keepComposerTextFocus(event) {
  const composer = event.currentTarget,
    textarea = composer.querySelector("textarea"),
    target = event.target;
  if (
    !textarea ||
    textarea.disabled ||
    target.closest("textarea, select, input:not([type='hidden']), #submitBtn, #temporarySendBtn")
  )
    return;
  // Composer chrome and auxiliary buttons must not dismiss the mobile
  // keyboard. Cancelling pointer focus still permits the button's click
  // action, while keeping the editing selection anchored in the textarea.
  event.preventDefault();
  textarea.focus({ preventScroll: true });
}
document
  .querySelectorAll(".composer-shell")
  .forEach((composer) => composer.addEventListener("pointerdown", keepComposerTextFocus));
document.querySelector(".composer-shell").addEventListener("focusout", () =>
  requestAnimationFrame(() => {
    const shell = document.querySelector(".composer-shell");
    if (!shell.contains(document.activeElement)) shell.classList.remove("focused");
    syncSubmitAction();
  }),
);
$("messageText").onkeydown = (e) => {
  if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
    e.preventDefault();
    $("submitBtn").click();
  }
};
$("threadTitle").onclick = () =>
  setThreadHeaderExpanded(!document.querySelector(".thread-head").classList.contains("expanded"));
$("threadTitle").onkeydown = (event) => {
  if (event.key === "Enter" || event.key === " ") {
    event.preventDefault();
    $("threadTitle").click();
  }
};
$("repairHintBtn").onclick = (event) => {
  event.stopPropagation();
  const bubble = $("repairHintBubble"),
    open = bubble.hidden;
  bubble.hidden = !open;
  $("repairHintBtn").setAttribute("aria-expanded", String(open));
};
document.addEventListener("click", (event) => {
  if (event.target.closest("#repairHintBtn, #repairHintBubble")) return;
  $("repairHintBubble").hidden = true;
  $("repairHintBtn").setAttribute("aria-expanded", "false");
});
$("currentBtn").onclick = () =>
  run(async () => {
    const r = await command({ command: "current" });
    await openThread(r.thread);
  });
$("tailBtn").onclick = () => run(() => command({ command: "tail" }));
$("pendingBtn").onclick = () => run(() => command({ command: "pending" }));
$("applyLastBtn").onclick = () => {
  state.pageSize = Number($("lastCount").value);
  run(refreshThread);
};
$("scrollBtn").onclick = () =>
  run(() => {
    const mode = $("scrollMode").value,
      v = $("scrollValue").value,
      req = { command: "scroll", direction: null, pixels: null, target: null, message_id: null };
    req[mode] = mode === "pixels" ? Number(v) : v;
    return command(req);
  });
$("approveBtn").onclick = () => run(() => approval("approve"));
$("declineBtn").onclick = () => run(() => approval("decline"));
$("execBtn").onclick = () =>
  run(async () => {
    let argv;
    try {
      argv = JSON.parse($("argv").value);
    } catch {
      throw new Error(tr("argvValidJson"));
    }
    if (!Array.isArray(argv) || !argv.every((x) => typeof x === "string"))
      throw new Error(tr("argvStringArray"));
    const timeout = $("timeout").value;
    return command(
      targetRequest("host_exec", { argv, timeout_seconds: timeout ? Number(timeout) : null }),
    );
  });
$("rpcBtn").onclick = () =>
  run(() => {
    const method = $("rpcMethod").value.trim();
    if (!method) throw new Error(tr("methodRequired"));
    let params;
    try {
      params = JSON.parse($("rpcParams").value);
    } catch {
      throw new Error(tr("paramsValidJson"));
    }
    return command({ command: "app_server_rpc", method, params });
  });
$("rawBtn").onclick = () =>
  run(() => {
    let req;
    try {
      req = JSON.parse($("rawRequest").value);
    } catch {
      throw new Error(tr("requestValidJson"));
    }
    return command(req);
  });
$("temporaryPanelBtn").onclick = () => {
  if ($("temporaryPanel").classList.contains("open")) closePanels();
  else openTemporaryPanel();
};
$("temporaryCloseBtn").onclick = closePanels;
$("temporarySendBtn").onclick = () => run(sendTemporaryMessage);
$("temporaryText").onkeydown = (event) => {
  if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
    event.preventDefault();
    $("temporarySendBtn").click();
  }
};
$("temporaryText").oninput = resizeTemporaryTextarea;
$("temporaryStopBtn").onclick = () =>
  run(async () => {
    const temporary = state.temporaryThread;
    if (!temporary?.activeTurnId || !window.confirm(tr("stopRunConfirm"))) return;
    await command({
      command: "temporary_turn_interrupt",
      thread_id: temporary.id,
      turn_id: temporary.activeTurnId,
    });
  });
function hideSelectionActions(clear = false) {
  $("selectionActions").hidden = true;
  if (clear) {
    window.getSelection()?.removeAllRanges();
    state.selectedMessageText = null;
  }
}
function updateSelectionActions() {
  const selection = window.getSelection();
  if (!selection || selection.isCollapsed) return hideSelectionActions();
  const text = selection.toString().trim();
  if (!text) return hideSelectionActions();
  const range = selection.getRangeAt(0),
    ancestor =
      range.commonAncestorContainer.nodeType === Node.ELEMENT_NODE
        ? range.commonAncestorContainer
        : range.commonAncestorContainer.parentElement,
    message = ancestor?.closest?.("#messages .message[data-message-index]");
  if (!message || !message.contains(selection.anchorNode) || !message.contains(selection.focusNode))
    return hideSelectionActions();
  const rect = range.getBoundingClientRect(),
    menu = $("selectionActions");
  state.selectedMessageText = {
    text,
    turnId: message.dataset.turnId || null,
    messageIndex: Number(message.dataset.messageIndex),
  };
  $("selectionTemporaryBtn").hidden = !state.selectedMessageText.turnId;
  menu.hidden = false;
  const width = menu.offsetWidth,
    height = menu.offsetHeight;
  menu.style.left = `${Math.max(8, Math.min(innerWidth - width - 8, rect.left + rect.width / 2 - width / 2))}px`;
  menu.style.top = `${Math.max(8, rect.top - height - 8)}px`;
}
let selectionUpdateTimer = null;
document.addEventListener("selectionchange", () => {
  clearTimeout(selectionUpdateTimer);
  selectionUpdateTimer = setTimeout(updateSelectionActions, 40);
});
$("selectionActions").addEventListener("pointerdown", (event) => event.preventDefault());
$("selectionCopyBtn").onclick = () =>
  run(async () => {
    const selection = state.selectedMessageText;
    if (!selection) return;
    await navigator.clipboard.writeText(selection.text);
    notify(tr("messageCopied"));
    hideSelectionActions(true);
  });
$("selectionInsertBtn").onclick = () => {
  const selection = state.selectedMessageText;
  if (!selection) return;
  setComposerReference(selection);
  $("messageText").focus({ preventScroll: true });
  hideSelectionActions(true);
};
$("selectionTemporaryBtn").onclick = () => {
  const selection = state.selectedMessageText;
  if (!selection) return;
  hideSelectionActions(true);
  run(() => createTemporaryThread(selection));
};
function syncScrim() {
  $("scrim").classList.toggle(
    "show",
    $("tools").classList.contains("open") ||
      $("temporaryPanel").classList.contains("open") ||
      $("sidebar").classList.contains("open"),
  );
}
function closePanels() {
  if ($("tools").contains(document.activeElement)) document.activeElement.blur();
  $("tools").classList.remove("open");
  $("tools").inert = true;
  $("temporaryPanel").classList.remove("open");
  $("temporaryPanel").inert = true;
  document.querySelector("main").inert = false;
  $("sidebar").inert = false;
  $("sidebar").classList.remove("open");
  syncScrim();
}
function bindSwipe(element, direction, onSwipe, { ignoreInteractive = false } = {}) {
  let gesture = null;
  element.addEventListener(
    "touchstart",
    (event) => {
      if (!matchMedia("(max-width:800px)").matches || event.touches.length !== 1) return;
      if (
        !ignoreInteractive &&
        event.target.closest(
          '.composer,button,input,textarea,select,a,pre,[contenteditable="true"]',
        )
      )
        return;
      const touch = event.touches[0];
      gesture = { x: touch.clientX, y: touch.clientY, horizontal: false };
    },
    { passive: true },
  );
  element.addEventListener(
    "touchmove",
    (event) => {
      if (!gesture || event.touches.length !== 1) return;
      const touch = event.touches[0],
        dx = touch.clientX - gesture.x,
        dy = touch.clientY - gesture.y;
      if (!gesture.horizontal && Math.abs(dx) > 12 && Math.abs(dx) > Math.abs(dy) * 1.35)
        gesture.horizontal = true;
      if (gesture.horizontal) event.preventDefault();
    },
    { passive: false },
  );
  element.addEventListener(
    "touchend",
    (event) => {
      if (!gesture) return;
      const touch = event.changedTouches[0],
        dx = touch.clientX - gesture.x,
        dy = touch.clientY - gesture.y;
      if (
        gesture.horizontal &&
        Math.abs(dx) > 64 &&
        Math.abs(dx) > Math.abs(dy) * 1.35 &&
        Math.sign(dx) === direction
      )
        onSwipe();
      gesture = null;
    },
    { passive: true },
  );
  element.addEventListener(
    "touchcancel",
    () => {
      gesture = null;
    },
    { passive: true },
  );
}
bindSwipe(document.querySelector("main"), 1, () => {
  if (
    $("tasksDialog").hidden &&
    !$("tools").classList.contains("open") &&
    !$("sidebar").classList.contains("open")
  )
    openTasksDialog();
});
bindSwipe(
  $("sidebar"),
  -1,
  () => {
    $("sidebar").classList.remove("open");
    syncScrim();
  },
  { ignoreInteractive: true },
);
window.addEventListener("pagehide", () => {
  persistDrafts();
  voiceStream?.getTracks().forEach((track) => track.stop());
});
const refreshAfterResume = () => {
  if (!state.current || document.visibilityState === "hidden") return;
  state.pendingChanges = true;
  state.lastMessageRefresh = 0;
  pollActivity().catch(() => {});
};
document.addEventListener("visibilitychange", refreshAfterResume);
window.addEventListener("focus", refreshAfterResume);
window.addEventListener("online", refreshAfterResume);
window.addEventListener("hashchange", () => {
  const hashedId = sessionIdFromHash(window.location.hash),
    threadId = hashedId || storedSessionId(window.localStorage);
  if (!threadId) return;
  if (threadId === state.current?.id) {
    if (!hashedId) updateSessionHash(threadId, true);
    return;
  }
  run(() => openSessionById(threadId, { fromHash: Boolean(hashedId), replaceHash: !hashedId }));
});
const composer = document.querySelector(".composer"),
  temporaryComposer = document.querySelector(".temporary-composer"),
  mainPanel = document.querySelector("main"),
  threadHead = document.querySelector(".thread-head"),
  syncFrameInsets = () => {
    mainPanel.style.setProperty(
      "--composer-height",
      `${Math.ceil(composer.getBoundingClientRect().height)}px`,
    );
    mainPanel.style.setProperty(
      "--thread-head-height",
      `${Math.ceil(threadHead.getBoundingClientRect().height)}px`,
    );
    $("temporaryPanel").style.setProperty(
      "--temporary-composer-height",
      `${Math.ceil(temporaryComposer.getBoundingClientRect().height)}px`,
    );
  };
const frameResizeObserver = new ResizeObserver(syncFrameInsets);
frameResizeObserver.observe(composer);
frameResizeObserver.observe(threadHead);
frameResizeObserver.observe(temporaryComposer);
syncFrameInsets();
resizeComposerTextarea();
window.addEventListener("resize", resizeComposerAfterViewportChange, { passive: true });
window.visualViewport?.addEventListener("resize", resizeComposerAfterViewportChange, {
  passive: true,
});
window.visualViewport?.addEventListener("scroll", syncFocusedComposerViewport, { passive: true });
window.addEventListener("scroll", syncFocusedComposerViewport, { passive: true });
document.addEventListener("click", (event) => {
  if (
    !$("modelPicker").hidden &&
    !$("modelPicker").contains(event.target) &&
    !$("modelPickerBtn").contains(event.target)
  )
    $("modelPicker").hidden = true;
  for (const menu of document.querySelectorAll(".outbox-menu[open]")) {
    if (!menu.contains(event.target)) menu.open = false;
  }
});
document.addEventListener("keydown", (event) => {
  if (event.key !== "Escape") return;
  if (imageViewer.root && !imageViewer.root.hidden) closeImageViewer();
  else if (filePreview.isOpen()) filePreview.close();
  else if (!$("tasksDialog").hidden) closeTasksDialog();
  else if (!$("createDialog").hidden) closeCreateDialog();
  else if (isHistoryFullscreen()) setHistoryFullscreen(false);
  else closePanels();
});
$("outboxTray").addEventListener("click", (event) => {
  if (event.target !== event.currentTarget) return;
  $("outboxTray").classList.toggle("compact");
  syncOutboxCompactLabel();
});
syncOutboxCompactLabel();
const markUserMessageScroll = () => {
  state.userScrolled = true;
  state.followMessageTail = false;
};
$("messages").addEventListener("touchmove", markUserMessageScroll, { passive: true });
$("messages").addEventListener("wheel", markUserMessageScroll, { passive: true });
const handleMessageScroll = () => {
  if (state.userScrolled) {
    state.followMessageTail = messageBottomDistance(messageScrollMetrics()) < 100;
  }
  if (
    state.userScrolled &&
    messageScrollMetrics().top < 8 &&
    !state.loadingHistory &&
    state.hasMore
  )
    run(loadOlder);
};
$("messages").onscroll = handleMessageScroll;
window.addEventListener("scroll", handleMessageScroll, { passive: true });
$("messages").addEventListener("pointerdown", () => setThreadHeaderExpanded(false), {
  passive: true,
});
document
  .querySelector(".composer")
  .addEventListener("pointerdown", () => setThreadHeaderExpanded(false));
document
  .querySelector(".composer")
  .addEventListener("focusin", () => setThreadHeaderExpanded(false));
document.querySelectorAll(".tool-toggle").forEach(
  (b) =>
    (b.onclick = () => {
      const opening = !$("tools").classList.contains("open");
      if (!opening && $("tools").contains(document.activeElement)) document.activeElement.blur();
      $("temporaryPanel").classList.remove("open");
      $("temporaryPanel").inert = true;
      document.querySelector("main").inert = false;
      $("sidebar").inert = false;
      $("tools").inert = !opening;
      $("tools").classList.toggle("open", opening);
      syncScrim();
    }),
);
document.querySelectorAll(".nav-toggle").forEach(
  (b) =>
    (b.onclick = () => {
      $("sidebar").classList.toggle("open");
      syncScrim();
    }),
);
$("scrim").onclick = closePanels;
applyLanguage(localStorage.getItem(LANGUAGE_STORAGE_KEY) || "en", false);
renderBrowserNotifications();
document.addEventListener("visibilitychange", renderBrowserNotifications);
renderTasksButton();
setSendMode("steer", false);
applyTheme(storedTheme(), false);
watchSystemTheme();
run(async () => {
  await authenticate();
  subscribeEvents(handleBridgeEvent);
  await loadStatus();
  await loadProjects();
});
setInterval(() => {
  if (!state.eventStreamConnected) pollActivity().catch(() => {});
}, 1500);
setInterval(() => refreshTaskOverviews().catch(() => {}), 1500);
setInterval(() => refreshComposerStatus().catch(() => {}), 60000);

import { SessionMessageCache } from "./message-cache.js";

export const DRAFT_STORAGE_KEY = "codex-bridge.drafts.v1";
export { applyTheme, storedTheme, watchSystemTheme } from "./theme.js";

function loadDrafts() {
  try {
    const entries = JSON.parse(localStorage.getItem(DRAFT_STORAGE_KEY) || "[]");
    return new Map(
      Array.isArray(entries)
        ? entries
            .filter(
              (entry) =>
                Array.isArray(entry) &&
                typeof entry[0] === "string" &&
                typeof entry[1] === "string",
            )
            .slice(-100)
        : [],
    );
  } catch {
    return new Map();
  }
}

export const state = {
  projects: [],
  projectThreads: new Map(),
  projectThreadLoads: new Map(),
  projectLoadGeneration: 0,
  pinAvailable: false,
  pinnedIds: new Set(),
  pinnedThreads: [],
  pinBusy: new Set(),
  expanded: new Set(),
  expandedPreferenceSaved: false,
  current: null,
  creatingProject: null,
  threadCreationJobs: new Map(),
  before: null,
  hasMore: false,
  loadingHistory: false,
  messageCache: new SessionMessageCache(3),
  visibleMessages: [],
  expandedTurnIds: new Set(),
  hydratedTurns: new Map(),
  hydratingTurns: new Map(),
  openToken: 0,
  initialPageSize: 8,
  pageSize: 30,
  userScrolled: false,
  followMessageTail: true,
  messageSyncPhase: null,
  pending: [],
  drafts: loadDrafts(),
  composerAttachments: [],
  attachmentDrafts: new Map(),
  composerReference: null,
  referenceDrafts: new Map(),
  modelOptions: [],
  composerModel: null,
  composerEffort: null,
  usageUnavailable: false,
  directAppServer: false,
  appServerMode: null,
  managedServices: null,
  serverCapabilities: null,
  runtimeResources: null,
  repairRequired: false,
  threadStatistics: null,
  threadGoal: null,
  goalBusy: false,
  activityFileLen: null,
  activeTurnId: null,
  activityPhase: null,
  activeTool: null,
  composerSubmitting: false,
  interrupting: false,
  workspaceDiffPolling: false,
  lastWorkspaceDiffRefresh: 0,
  modeAutomatic: false,
  eventStreamConnected: false,
  polling: false,
  pendingChanges: false,
  updatingThreads: new Set(),
  threadRunStates: new Map(),
  authoritativeThreadActive: new Map(),
  notifiedTurnIds: new Set(),
  taskTrackedIds: new Set(),
  taskOverviews: new Map(),
  tasksOpen: false,
  tasksRefreshing: false,
  taskDirtyIds: new Set(),
  lastMessageIndex: null,
  historyStart: null,
  historyEnd: null,
  historyTotal: null,
  lastMessageRefresh: 0,
  selectedMessageText: null,
  temporarySelection: null,
  temporaryCreating: false,
  temporaryThread: null,
  temporaryThreads: new Map(),
};

let draftPersistTimer = null;

export function persistDrafts() {
  clearTimeout(draftPersistTimer);
  draftPersistTimer = null;
  try {
    localStorage.setItem(DRAFT_STORAGE_KEY, JSON.stringify([...state.drafts].slice(-100)));
  } catch {
    // Draft persistence is best effort when storage is unavailable.
  }
}

export function saveDraft(threadId, text, flush = false) {
  if (!threadId) return;
  if (text) state.drafts.set(threadId, text);
  else state.drafts.delete(threadId);
  clearTimeout(draftPersistTimer);
  if (flush) persistDrafts();
  else draftPersistTimer = setTimeout(persistDrafts, 180);
}

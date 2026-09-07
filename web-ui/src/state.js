import { $ } from "./api.js";

export const DRAFT_STORAGE_KEY = "codex-bridge.drafts.v1";
export const THEME_STORAGE_KEY = "codex-bridge.theme.v2";

export function applyTheme(theme, persist = true) {
  const value = ["dark", "light", "system"].includes(theme) ? theme : "system";
  document.documentElement.dataset.theme = value;
  if (persist) localStorage.setItem(THEME_STORAGE_KEY, value);
  const select = $("themeSelect");
  if (select) select.value = value;
}

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
  pinAvailable: false,
  pinnedIds: new Set(),
  pinBusy: new Set(),
  expanded: new Set(),
  current: null,
  creatingProject: null,
  before: null,
  hasMore: false,
  loadingHistory: false,
  openToken: 0,
  pageSize: 30,
  userScrolled: false,
  pending: [],
  drafts: loadDrafts(),
  modelOptions: [],
  composerModel: null,
  composerEffort: null,
  usageUnavailable: false,
  directAppServer: false,
  appServerMode: null,
  activityFileLen: null,
  activeTurnId: null,
  activityPhase: null,
  activeTool: null,
  workspaceDiffPolling: false,
  lastWorkspaceDiffRefresh: 0,
  modeAutomatic: false,
  polling: false,
  pendingChanges: false,
  delivery: null,
  lastMessageIndex: null,
  historyStart: null,
  historyEnd: null,
  historyTotal: null,
  lastMessageRefresh: 0,
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

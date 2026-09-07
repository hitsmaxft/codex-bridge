export const LAST_SESSION_STORAGE_KEY = "codex-bridge.last-session.v1";

export function sessionIdFromHash(hash) {
  const value = String(hash || "").replace(/^#/, "");
  if (!value) return null;
  try {
    if (!value.includes("=")) return decodeURIComponent(value).trim() || null;
    const sessionId = new URLSearchParams(value).get("session")?.trim();
    return sessionId || null;
  } catch {
    return null;
  }
}

export function sessionHash(sessionId) {
  return `#session=${encodeURIComponent(sessionId)}`;
}

export function storedSessionId(storage) {
  try {
    return storage.getItem(LAST_SESSION_STORAGE_KEY)?.trim() || null;
  } catch {
    return null;
  }
}

export function rememberSessionId(storage, sessionId) {
  try {
    storage.setItem(LAST_SESSION_STORAGE_KEY, sessionId);
  } catch {
    // Session restoration is best effort when browser storage is unavailable.
  }
}

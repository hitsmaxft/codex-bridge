import { createAuthenticationGate } from "./auth-gate.js";

export const $ = (id) => document.getElementById(id);

const demoMode = import.meta.env.VITE_CODEX_BRIDGE_DEMO === "1";
let demoClient;
let eventSocket;
let eventRetryTimer;
let eventRetryDelay = 500;
let eventStreamUnavailable = false;
const eventListeners = new Set();
const performanceBuckets = new Map();
let performanceFlush = null;
const authentication = createAuthenticationGate(async () => {
  if (demoMode) return;
  const response = await fetch("/api/auth", {
    credentials: "same-origin",
    cache: "no-store",
  });
  if (!response.ok) throw new Error(`authentication failed (${response.status})`);
});

export const authenticate = () => authentication.wait();

export async function createFileDownloadTicket(threadId, path) {
  await authenticate();
  const response = await fetch("/api/file-ticket", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ thread_id: threadId, path }),
  });
  if (response.status === 401) {
    const error = new Error("authentication expired; reload the page to sign in again");
    authentication.block(error);
    throw error;
  }
  if (!response.ok)
    throw new Error((await response.text()) || `download failed (${response.status})`);
  return response.json();
}

export async function requestFilePreview(threadId, path) {
  await authenticate();
  const response = await fetch("/api/file-preview", {
    method: "POST",
    credentials: "same-origin",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ thread_id: threadId, path }),
  });
  if (response.status === 401) {
    const error = new Error("authentication expired; reload the page to sign in again");
    authentication.block(error);
    throw error;
  }
  if (!response.ok)
    throw new Error((await response.text()) || `preview failed (${response.status})`);
  return response.json();
}

async function sendCommand(request) {
  if (demoMode) {
    demoClient ||= import("./demo-client.js");
    const client = await demoClient;
    return client.demoCommand(request);
  }
  await authenticate();
  const response = await fetch("/api/command", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
  if (response.status === 401) {
    const error = new Error("authentication expired; reload the page to sign in again");
    authentication.block(error);
    throw error;
  }
  return response.json();
}

export function recordPerformance(metric, durationMs, { count = 1, bytes = 0 } = {}) {
  if (!Number.isFinite(durationMs) || durationMs < 0 || !Number.isSafeInteger(count) || count < 1)
    return;
  const bucket = performanceBuckets.get(metric) || {
    metric,
    count: 0,
    total_ms: 0,
    max_ms: 0,
    total_bytes: 0,
  };
  const rounded = Math.ceil(durationMs);
  bucket.count += count;
  bucket.total_ms += rounded;
  bucket.max_ms = Math.max(bucket.max_ms, rounded);
  bucket.total_bytes += Number.isFinite(bytes) && bytes > 0 ? Math.ceil(bytes) : 0;
  performanceBuckets.set(metric, bucket);
}

export async function flushPerformance() {
  if (performanceFlush || !performanceBuckets.size) return performanceFlush;
  const samples = [...performanceBuckets.values()];
  performanceBuckets.clear();
  performanceFlush = command({ command: "client_performance", samples }, false)
    .catch(() => {
      for (const sample of samples) {
        const current = performanceBuckets.get(sample.metric);
        if (!current) performanceBuckets.set(sample.metric, sample);
        else {
          current.count += sample.count;
          current.total_ms += sample.total_ms;
          current.max_ms = Math.max(current.max_ms, sample.max_ms);
          current.total_bytes += sample.total_bytes;
        }
      }
    })
    .finally(() => (performanceFlush = null));
  return performanceFlush;
}

if (typeof window !== "undefined") {
  window.setInterval(() => flushPerformance(), 15_000);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") flushPerformance();
  });
}

export async function command(request, showResult = true) {
  const data = await sendCommand(request);
  if (showResult) {
    $("result").removeAttribute("data-i18n");
    $("result").textContent = JSON.stringify(data, null, 2);
  }
  if (!data.ok) {
    throw new Error(`${data.error?.code || "error"}: ${data.error?.message || "request failed"}`);
  }
  return data.result;
}

export function notify(text, bad = false, kind = null) {
  const element = $("toast");
  element.textContent = text;
  if (kind) element.dataset.kind = kind;
  else delete element.dataset.kind;
  element.style.borderColor = bad ? "var(--danger)" : "";
  element.classList.add("show");
  clearTimeout(notify.timer);
  notify.timer = setTimeout(
    () => element.classList.remove("show"),
    kind === "completion" ? 5200 : 2600,
  );
}

export async function run(action) {
  try {
    return await action();
  } catch (error) {
    notify(error.message, true);
    throw error;
  }
}

export function timeText(milliseconds, locale) {
  return milliseconds ? new Date(milliseconds).toLocaleString(locale) : "";
}

function emitEvent(event) {
  for (const listener of eventListeners) listener(event);
}

async function connectEventStream() {
  if (
    demoMode ||
    eventSocket?.readyState === WebSocket.OPEN ||
    eventSocket?.readyState === WebSocket.CONNECTING
  )
    return;
  clearTimeout(eventRetryTimer);
  try {
    await authenticate();
  } catch {
    eventStreamUnavailable = true;
    return;
  }
  if (
    eventSocket?.readyState === WebSocket.OPEN ||
    eventSocket?.readyState === WebSocket.CONNECTING
  )
    return;
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  eventSocket = new WebSocket(`${protocol}//${window.location.host}/api/events`);
  eventSocket.onopen = () => {
    eventRetryDelay = 500;
    eventStreamUnavailable = false;
  };
  eventSocket.onmessage = (message) => {
    try {
      const event = JSON.parse(message.data);
      if (event.type === "bridge_app_server_connection" && event.status === "unavailable") {
        eventStreamUnavailable = true;
      }
      emitEvent(event);
    } catch {
      // Ignore malformed event frames; the polling path remains available for recovery.
    }
  };
  eventSocket.onclose = () => {
    eventSocket = null;
    emitEvent({ type: "bridge_event_stream", status: "disconnected" });
    if (!eventStreamUnavailable) {
      eventRetryTimer = setTimeout(connectEventStream, eventRetryDelay);
      eventRetryDelay = Math.min(eventRetryDelay * 2, 10_000);
    }
  };
  eventSocket.onerror = () => eventSocket?.close();
}

export function subscribeEvents(listener) {
  eventListeners.add(listener);
  connectEventStream();
  return () => eventListeners.delete(listener);
}

export { demoMode };

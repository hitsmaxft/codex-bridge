export const $ = (id) => document.getElementById(id);

const demoMode = import.meta.env.VITE_CODEX_BRIDGE_DEMO === "1";
let demoClient;
let eventSocket;
let eventRetryTimer;
let eventRetryDelay = 500;
let eventStreamUnavailable = false;
const eventListeners = new Set();

async function sendCommand(request) {
  if (demoMode) {
    demoClient ||= import("./demo-client.js");
    const client = await demoClient;
    return client.demoCommand(request);
  }
  const response = await fetch("/api/command", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
  return response.json();
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

export function notify(text, bad = false) {
  const element = $("toast");
  element.textContent = text;
  element.style.borderColor = bad ? "var(--danger)" : "";
  element.classList.add("show");
  clearTimeout(notify.timer);
  notify.timer = setTimeout(() => element.classList.remove("show"), 2600);
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

function connectEventStream() {
  if (
    demoMode ||
    eventSocket?.readyState === WebSocket.OPEN ||
    eventSocket?.readyState === WebSocket.CONNECTING
  )
    return;
  clearTimeout(eventRetryTimer);
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

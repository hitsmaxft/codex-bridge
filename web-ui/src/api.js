export const $ = (id) => document.getElementById(id);

export async function command(request, showResult = true) {
  const response = await fetch("/api/command", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(request),
  });
  const data = await response.json();
  if (showResult) $("result").textContent = JSON.stringify(data, null, 2);
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

export function timeText(milliseconds) {
  return milliseconds ? new Date(milliseconds).toLocaleString() : "";
}

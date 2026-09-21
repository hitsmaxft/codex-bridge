export function scrollTopForViewportAnchor(scrollTop, beforeTop, afterTop) {
  const current = Number(scrollTop),
    before = Number(beforeTop),
    after = Number(afterTop);
  if (![current, before, after].every(Number.isFinite)) return current;
  return Math.max(0, current + after - before);
}

export function messageBottomDistance({ top, height, client }) {
  const values = [top, height, client].map(Number);
  if (!values.every(Number.isFinite)) return Number.POSITIVE_INFINITY;
  return Math.max(0, values[1] - values[0] - values[2]);
}

export function shouldFollowMessageTail(following, metrics, threshold = 100) {
  return Boolean(following) || messageBottomDistance(metrics) < threshold;
}

export function documentOwnsMessageScroll({ mobile, fullscreen }) {
  return Boolean(mobile) && !Boolean(fullscreen);
}

export function messageIdentity(message) {
  const id = typeof message?.id === "string" ? message.id.trim() : "";
  return id ? `id:${id}` : `index:${message?.message_index ?? "unknown"}`;
}

export function toolGroupIdentity(threadId, message, liveTool = null) {
  // A message-level tool group grows by appending calls while a turn runs. Its
  // first call is the stable anchor; using the newest call would rename the
  // group on every incremental update and discard the user's open state.
  // A live-only placeholder has no snapshotted calls yet, so use its call ID
  // until that same first call appears in the snapshot.
  const tool = message?.tools?.[0] || liveTool || null,
    activityKey = typeof tool?.activity_key === "string" ? tool.activity_key.trim() : "",
    callId = typeof (tool?.call_id || tool?.id) === "string" ? tool.call_id || tool.id : "",
    identity = activityKey
      ? `activity:${activityKey}`
      : callId
        ? `call:${callId}`
        : messageIdentity(message);
  return `${threadId || ""}:${identity}`;
}

export function messagePersistsWhenTurnCollapsed(message) {
  return (
    message?.category === "compaction" ||
    message?.tools?.some((tool) => Boolean(tool?.has_image)) === true
  );
}

export function adjacentTurnIndex(turnTops, viewportTop, direction, tolerance = 6) {
  const top = Number(viewportTop),
    positions = Array.from(turnTops || [], Number);
  if (!Number.isFinite(top) || !positions.every(Number.isFinite)) return -1;
  if (direction === "up") {
    for (let index = positions.length - 1; index >= 0; index -= 1) {
      if (positions[index] < top - tolerance) return index;
    }
    return -1;
  }
  if (direction === "down") return positions.findIndex((position) => position > top + tolerance);
  return -1;
}

export function latestActivityMessages(messages) {
  const latest = new Map();
  for (const message of messages || []) {
    for (const tool of message.tools || []) {
      if (tool.activity_key) latest.set(tool.activity_key, tool);
    }
  }
  return (messages || []).map((message) => {
    const tools = message.tools || [],
      visible = tools.filter(
        (tool) => !tool.activity_key || latest.get(tool.activity_key) === tool,
      );
    return visible.length === tools.length ? message : { ...message, tools: visible };
  });
}

export function activeToolGroupTarget(messages, activeTurnId, activeToolCallId = null) {
  if (!activeTurnId) return null;
  const assistants = (messages || []).filter(
    (message) => message.turn_id === activeTurnId && message.role === "assistant",
  );
  const matched = activeToolCallId
    ? assistants.findLast((message) =>
        message.tools?.some((tool) => tool.call_id === activeToolCallId),
      )
    : null;
  return (
    matched || assistants.findLast((message) => message.tools?.length) || assistants.at(-1) || null
  );
}

export function activityToolTitle(tool, _currentThread) {
  if (!["wait", "wait_agent"].includes(tool?.name)) return null;
  const agentName = tool.activity_label?.trim() || "subagent";
  return agentName;
}

export function activityThreadIds(tool) {
  if (!["wait", "wait_agent"].includes(tool?.name)) return [];
  if (Array.isArray(tool.activity_thread_ids)) {
    return [...new Set(tool.activity_thread_ids.filter((id) => typeof id === "string" && id))];
  }
  const key = typeof tool.activity_key === "string" ? tool.activity_key : "";
  return key.startsWith("wait:") ? key.slice(5).split(",").filter(Boolean) : [];
}

export function activityAgents(tool, currentThread) {
  const ids = activityThreadIds(tool),
    supplied = Array.isArray(tool?.activity_agents) ? tool.activity_agents : [],
    fallbackName = activityToolTitle(tool, currentThread);
  return ids.map((id) => {
    const suppliedAgent = supplied.find((agent) => agent?.thread_id === id),
      name = suppliedAgent?.name?.trim();
    return { id, name: name || (ids.length === 1 ? fallbackName : "subagent") };
  });
}

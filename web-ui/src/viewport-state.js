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

export function activityToolTitle(tool, currentThread) {
  if (!["wait", "wait_agent"].includes(tool?.name)) return null;
  const currentName =
      tool.activity_sender_id === currentThread?.id ? currentThread?.title?.trim() : "",
    agentName = tool.activity_label?.trim() || currentName || "subagent";
  return `wait (${agentName})`;
}

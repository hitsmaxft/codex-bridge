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

export function messageRangesCoverPage(messages, page) {
  if (!Array.isArray(messages) || !page) return false;
  if (!messages.length) return page.start === page.end;
  const ranges = messages.map((message) =>
    message.turn_stub
      ? [message.turn_stub.start, message.turn_stub.end]
      : [message.message_index, message.message_index + 1],
  );
  return (
    ranges[0][0] === page.start &&
    ranges.at(-1)[1] === page.end &&
    ranges.every(
      ([start, end], index) =>
        Number.isSafeInteger(start) &&
        Number.isSafeInteger(end) &&
        start < end &&
        start >= page.start &&
        end <= page.end &&
        (!index || ranges[index - 1][1] === start),
    )
  );
}

export function mergeHydratedTurnSequence(messages, hydratedForTurn) {
  const output = [],
    emittedTurns = new Set();
  for (const message of messages || []) {
    const hydrated = message.turn_id ? hydratedForTurn(message.turn_id) : null;
    if (!hydrated) {
      output.push(message);
      continue;
    }
    if (emittedTurns.has(message.turn_id)) continue;
    output.push(...hydrated.values());
    emittedTurns.add(message.turn_id);
  }
  return output;
}

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

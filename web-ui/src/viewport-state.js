export function scrollTopForViewportAnchor(scrollTop, beforeTop, afterTop) {
  const current = Number(scrollTop),
    before = Number(beforeTop),
    after = Number(afterTop);
  if (![current, before, after].every(Number.isFinite)) return current;
  return Math.max(0, current + after - before);
}

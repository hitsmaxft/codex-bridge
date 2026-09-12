export function goalToggleState(status) {
  if (status === "active") {
    return { canPause: true, canResume: false, nextStatus: "paused" };
  }
  if (status === "paused" || status === "blocked") {
    return { canPause: false, canResume: true, nextStatus: "active" };
  }
  return { canPause: false, canResume: false, nextStatus: null };
}

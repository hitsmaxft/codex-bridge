function snapshotProjection(activity) {
  const activeTurnId = activity?.active_turn_id || null;
  return {
    activeTurnId,
    activityPhase: activeTurnId ? activity?.phase || null : null,
    activeTool: activeTurnId ? activity?.active_tool || null : null,
    activeToolCallId: activeTurnId ? activity?.active_tool_call_id || null : null,
  };
}

export function activityOverlayConfirmed(overlay, activity) {
  if (!overlay) return true;
  const snapshot = snapshotProjection(activity);
  if (!overlay.activeTurnId) return !snapshot.activeTurnId;
  if (snapshot.activeTurnId !== overlay.activeTurnId) return false;
  if (overlay.activityPhase === "tool") {
    return (
      snapshot.activityPhase === "tool" &&
      Boolean(overlay.activeToolCallId) &&
      snapshot.activeToolCallId === overlay.activeToolCallId
    );
  }
  return snapshot.activityPhase !== "tool" && !snapshot.activeToolCallId;
}

export function mergeActivityProjection(activity, overlay) {
  return overlay || snapshotProjection(activity);
}

export function activeActivityOverlay(turnId, phase = "model", tool = null) {
  return {
    activeTurnId: turnId || null,
    activityPhase: turnId ? phase : null,
    activeTool: turnId && tool ? tool.name || null : null,
    activeToolCallId: turnId && tool ? tool.id || null : null,
  };
}

export function completedActivityOverlay() {
  return activeActivityOverlay(null, null, null);
}

export function shouldOfferStop({ activeTurnId }) {
  return Boolean(activeTurnId);
}

export function effectiveActiveTurnId(rolloutTurnId, authoritativeActive, currentTurnId = null) {
  if (authoritativeActive === false) return null;
  return rolloutTurnId || (authoritativeActive === true ? currentTurnId : null) || null;
}

export function completionMatchesActiveTurn(activeTurnId, completedTurnId) {
  return !activeTurnId || !completedTurnId || activeTurnId === completedTurnId;
}

export function restoreComposerDraft(textarea, draft, changedThread) {
  if (!changedThread) return false;
  textarea.value = draft || "";
  return true;
}

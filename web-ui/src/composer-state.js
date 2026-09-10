export function shouldOfferStop({ activeTurnId }) {
  return Boolean(activeTurnId);
}

export function effectiveActiveTurnId(rolloutTurnId, authoritativeActive) {
  return authoritativeActive === false ? null : rolloutTurnId || null;
}

export function restoreComposerDraft(textarea, draft, changedThread) {
  if (!changedThread) return false;
  textarea.value = draft || "";
  return true;
}

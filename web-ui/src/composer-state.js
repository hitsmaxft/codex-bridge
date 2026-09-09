export function shouldOfferStop({ activeTurnId, inputFocused, submitting, interrupting }) {
  return Boolean(activeTurnId) && !inputFocused && !submitting && !interrupting;
}

export function effectiveActiveTurnId(rolloutTurnId, authoritativeActive) {
  return authoritativeActive === false ? null : rolloutTurnId || null;
}

export function restoreComposerDraft(textarea, draft, changedThread) {
  if (!changedThread) return false;
  textarea.value = draft || "";
  return true;
}

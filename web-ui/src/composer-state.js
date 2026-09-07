export function shouldOfferStop({ activeTurnId, inputFocused, submitting, interrupting }) {
  return Boolean(activeTurnId) && !inputFocused && !submitting && !interrupting;
}

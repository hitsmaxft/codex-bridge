export function asyncQuestionReplyMode(questionTurnId, activeTurnId) {
  if (activeTurnId === questionTurnId) return "steer";
  return activeTurnId ? "changed" : "new_turn";
}

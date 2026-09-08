function messageText(message) {
  return (message?.content || [])
    .filter((item) => item?.kind === "text" && typeof item.text === "string")
    .map((item) => item.text.trim())
    .filter(Boolean)
    .join("\n\n");
}

export function taskOverview(messagesResult, activityResult) {
  const messages = Array.isArray(messagesResult?.messages) ? messagesResult.messages : [];
  const latestUserIndex = messages.findLastIndex(
      (message) => message?.role === "user" && messageText(message),
    ),
    latestUser = latestUserIndex >= 0 ? messages[latestUserIndex] : null,
    currentTurnMessages = messages.slice(Math.max(0, latestUserIndex + 1)),
    latestAssistant = currentTurnMessages.findLast(
      (message) => message?.role === "assistant" && messageText(message),
    ),
    toolMessage = currentTurnMessages.findLast(
      (message) => Array.isArray(message?.tools) && message.tools.length,
    );
  return {
    thread: messagesResult?.thread || null,
    userText: messageText(latestUser),
    assistantText: messageText(latestAssistant),
    latestTool: toolMessage?.tools?.at(-1) || null,
    activity: activityResult?.activity || null,
  };
}

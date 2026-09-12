const attachmentLabels = new Map([
  ["Image attachment", "image"],
  ["Audio attachment", "audio"],
]);

function inputFingerprint(textParts, attachments) {
  return JSON.stringify({
    text: textParts.join("\n"),
    image: attachments.filter((kind) => kind === "image").length,
    audio: attachments.filter((kind) => kind === "audio").length,
  });
}

function pendingFingerprint(entry) {
  const textParts = [],
    attachments = [];
  for (const line of String(entry?.text || "").split("\n")) {
    const match = /^\[(Image attachment|Audio attachment)\]$/.exec(line.trim());
    if (match) attachments.push(attachmentLabels.get(match[1]));
    else textParts.push(line);
  }
  return inputFingerprint(textParts, attachments);
}

function messageFingerprint(message) {
  const textParts = [],
    attachments = [];
  for (const item of message?.content || []) {
    if (item.kind === "text" && typeof item.text === "string") textParts.push(item.text);
    else if (item.kind === "context" && attachmentLabels.has(item.label)) {
      attachments.push(attachmentLabels.get(item.label));
    }
  }
  return inputFingerprint(textParts, attachments);
}

export function pendingInputSummary(text, attachments) {
  const parts = [];
  if (text.trim()) parts.push(text);
  for (const attachment of attachments) {
    parts.push(attachment.type === "audio" ? "[Audio attachment]" : "[Image attachment]");
  }
  return parts.join("\n");
}

export function reconcilePendingMessages(entries, authoritativeMessages) {
  const claimed = new Set(),
    messages = Array.isArray(authoritativeMessages) ? authoritativeMessages : [];
  return entries.filter((entry) => {
    const fingerprint = pendingFingerprint(entry);
    const landedAt = messages.findIndex((message, index) => {
      if (claimed.has(index) || message.role !== "user") return false;
      if (message.id && message.id === entry.id) return true;
      if (!Number.isSafeInteger(entry.after_message_index)) return false;
      if (message.message_index <= entry.after_message_index) return false;
      return messageFingerprint(message) === fingerprint;
    });
    if (landedAt < 0) return true;
    claimed.add(landedAt);
    return false;
  });
}

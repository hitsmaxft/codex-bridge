export function forwardMessagePageRequests(knownEnd, latestStart, pageSize) {
  const values = [knownEnd, latestStart, pageSize].map(Number);
  if (
    !values.every(Number.isSafeInteger) ||
    values[0] < 0 ||
    values[1] < values[0] ||
    values[2] < 1
  )
    return [];
  const requests = [];
  for (let cursor = values[0]; cursor < values[1];) {
    const end = Math.min(cursor + values[2], values[1]);
    requests.push({ before: end, limit: end - cursor });
    cursor = end;
  }
  return requests;
}

export function mergeMessagesByIndex(existing, incoming) {
  const messages = new Map();
  for (const message of [...existing, ...incoming]) messages.set(message.message_index, message);
  return [...messages.values()].sort((left, right) => left.message_index - right.message_index);
}

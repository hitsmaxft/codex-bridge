export function commandPickerTrigger(value, cursor = value.length, explicit = false) {
  const skillCommand = value.match(/^\/skills?(?:\s+(\S*))?$/);
  if (skillCommand)
    return { mode: "skills", query: skillCommand[1] || "", start: 0, end: value.length };
  if (/^\/[^\s]*$/.test(value)) return { mode: "commands", query: value.slice(1) };
  const mention = value.slice(0, cursor).match(/(?:^|\s)\$([^\s]*)$/);
  if (mention) {
    const start = cursor - mention[1].length - 1;
    return {
      mode: "skills",
      query: mention[1],
      start,
      end: cursor + (value.slice(cursor).match(/^[^\s]*/)?.[0].length || 0),
    };
  }
  return explicit ? { mode: "commands", query: "" } : null;
}

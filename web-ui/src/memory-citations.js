export function memoryCitationFileName(source) {
  return String(source || "MEMORY.md")
    .split("/")
    .at(-1)
    .replace(/:\d+(?:-\d+)?$/, "");
}

export function memoryCitationModel(items) {
  const entries = [];
  const seenEntries = new Set();
  for (const item of items || []) {
    const source = String(item?.source || "MEMORY.md");
    const note = typeof item?.note === "string" ? item.note : "";
    const key = `${source}\0${note}`;
    if (seenEntries.has(key)) continue;
    seenEntries.add(key);
    entries.push({ source, note });
  }
  return {
    entries,
    files: [...new Set(entries.map((entry) => memoryCitationFileName(entry.source)))],
  };
}

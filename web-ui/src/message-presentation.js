export function shouldCollapseAssistantOutput(contentHeight, viewportHeight) {
  const content = Number(contentHeight),
    viewport = Math.max(320, Number(viewportHeight) || 0);
  return Number.isFinite(content) && content > viewport + 1;
}

export function compactToolFilePath(path, cwd = "") {
  if (typeof path !== "string" || !path) return "";
  let relative = path.replaceAll("\\", "/");
  const normalizedCwd = typeof cwd === "string" ? cwd.replaceAll("\\", "/").replace(/\/$/, "") : "";
  if (normalizedCwd && relative.startsWith(`${normalizedCwd}/`))
    relative = relative.slice(normalizedCwd.length + 1);
  if (relative.startsWith("/")) return relative.split("/").filter(Boolean).at(-1) || "";
  const parts = relative.split("/").filter((part) => part && part !== ".");
  if (parts.length <= 2) return parts.join("/");
  return `${parts[0]}/…/${parts.at(-1)}`;
}

export function toolFileList(tools, cwd = "", limit = 3) {
  const files = [
    ...new Set(
      tools
        .flatMap((tool) => (Array.isArray(tool.file_paths) ? tool.file_paths : []))
        .map((path) => compactToolFilePath(path, cwd))
        .filter(Boolean),
    ),
  ];
  if (!files.length) return "";
  const visible = files.slice(0, limit).join(", ");
  return files.length > limit ? `${visible} +${files.length - limit}` : visible;
}

export const EXPANDED_PROJECTS_STORAGE_KEY = "codex-bridge.expanded-projects.v1";

export function storedExpandedProjects(storage) {
  try {
    const raw = storage.getItem(EXPANDED_PROJECTS_STORAGE_KEY);
    if (raw === null) return null;
    const paths = JSON.parse(raw);
    if (!Array.isArray(paths)) return null;
    return new Set(paths.filter((path) => typeof path === "string" && path.length > 0).slice(-100));
  } catch {
    return null;
  }
}

export function persistExpandedProjects(storage, expanded) {
  try {
    storage.setItem(
      EXPANDED_PROJECTS_STORAGE_KEY,
      JSON.stringify([...expanded].filter((path) => typeof path === "string").slice(-100)),
    );
  } catch {
    // Sidebar preferences are best effort when browser storage is unavailable.
  }
}

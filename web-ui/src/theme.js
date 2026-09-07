export const THEME_STORAGE_KEY = "codex-bridge.theme.v2";
// Treat browsers without an explicit dark preference as light instead of inheriting :root dark.
export const SYSTEM_THEME_QUERY = "(prefers-color-scheme: dark)";
let systemMedia;

export function normalizeTheme(theme) {
  return ["dark", "light", "system"].includes(theme) ? theme : "system";
}

export function resolveTheme(theme, prefersDark) {
  const preference = normalizeTheme(theme);
  return preference === "system" ? (prefersDark ? "dark" : "light") : preference;
}

export function storedTheme() {
  try {
    return normalizeTheme(localStorage.getItem(THEME_STORAGE_KEY));
  } catch {
    return "system";
  }
}

function systemThemeMedia() {
  if (!systemMedia && typeof matchMedia === "function") {
    systemMedia = matchMedia(SYSTEM_THEME_QUERY);
  }
  return systemMedia || null;
}

export function applyTheme(theme, persist = true) {
  const preference = normalizeTheme(theme),
    resolved = resolveTheme(preference, Boolean(systemThemeMedia()?.matches));
  document.documentElement.dataset.themePreference = preference;
  document.documentElement.dataset.theme = resolved;
  if (persist) {
    try {
      localStorage.setItem(THEME_STORAGE_KEY, preference);
    } catch {
      // Theme persistence is best effort when storage is unavailable.
    }
  }
  const select = document.getElementById("themeSelect");
  if (select) select.value = preference;
}

export function watchSystemTheme() {
  const media = systemThemeMedia();
  if (!media) return;
  const sync = () => {
    if (document.documentElement.dataset.themePreference === "system") {
      applyTheme("system", false);
    }
  };
  if (typeof media.addEventListener === "function") media.addEventListener("change", sync);
  else media.addListener?.(sync);
}

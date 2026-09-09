export const BROWSER_NOTIFICATIONS_STORAGE_KEY = "codex-bridge.browser-notifications.v1";

export function storedBrowserNotifications(storage) {
  try {
    return storage?.getItem(BROWSER_NOTIFICATIONS_STORAGE_KEY) === "enabled";
  } catch {
    return false;
  }
}

export function persistBrowserNotifications(storage, enabled) {
  try {
    if (enabled) storage?.setItem(BROWSER_NOTIFICATIONS_STORAGE_KEY, "enabled");
    else storage?.removeItem(BROWSER_NOTIFICATIONS_STORAGE_KEY);
  } catch {
    // Notification preference persistence is best effort.
  }
}

export function browserNotificationState(NotificationApi, storage) {
  if (!NotificationApi) return { supported: false, permission: "unsupported", enabled: false };
  const permission = NotificationApi.permission || "default";
  return {
    supported: true,
    permission,
    enabled: permission === "granted" && storedBrowserNotifications(storage),
  };
}

export async function enableBrowserNotifications(NotificationApi, storage) {
  if (!NotificationApi) return browserNotificationState(NotificationApi, storage);
  const permission =
    NotificationApi.permission === "default"
      ? await NotificationApi.requestPermission()
      : NotificationApi.permission;
  persistBrowserNotifications(storage, permission === "granted");
  return browserNotificationState(NotificationApi, storage);
}

export function disableBrowserNotifications(NotificationApi, storage) {
  persistBrowserNotifications(storage, false);
  return browserNotificationState(NotificationApi, storage);
}

export function shouldShowBrowserNotification({
  NotificationApi,
  storage,
  documentHidden,
  windowFocused,
}) {
  const status = browserNotificationState(NotificationApi, storage);
  return status.enabled && (documentHidden || !windowFocused);
}

export function showBrowserNotification(NotificationApi, { title, body, tag, onClick }) {
  try {
    const notification = new NotificationApi(title, { body, tag });
    notification.onclick = () => {
      notification.close?.();
      onClick?.();
    };
    return true;
  } catch {
    return false;
  }
}

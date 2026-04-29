/** Generated from src/service-worker/sw-template.js for build __WADDLE_BUILD_SHA__. */

const CACHE_NAME = "waddle-__WADDLE_BUILD_SHA__";
const NOTIFICATION_ICON_URL = "/android-chrome-192x192.png";
const UNREAD_COUNT_MESSAGE_TYPE = "waddle:unread-count";
const PRECACHE_URLS = [
  "/offline.html",
  "/manifest.webmanifest",
  "/waddle-logo.svg",
  "/favicon.ico",
  "/favicon-16x16.png",
  "/favicon-32x32.png",
  "/apple-touch-icon.png",
  NOTIFICATION_ICON_URL,
  "/android-chrome-512x512.png",
];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches.open(CACHE_NAME).then((cache) => cache.addAll(PRECACHE_URLS)),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys().then((keys) =>
      Promise.all(
        keys
          .filter((key) => key.startsWith("waddle-") && key !== CACHE_NAME)
          .map((key) => caches.delete(key)),
      ),
    ).then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;

  if (request.method !== "GET") return;

  const url = new URL(request.url);

  // Navigation requests: network-first, offline fallback
  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request).catch(() => caches.match("/offline.html")),
    );
    return;
  }

  // Static assets (Astro bundles, icons, manifest): cache-first
  if (
    url.pathname.startsWith("/_astro/") ||
    url.pathname === "/manifest.webmanifest" ||
    url.pathname === "/waddle-logo.svg" ||
    url.pathname === "/favicon.ico" ||
    url.pathname === "/favicon-16x16.png" ||
    url.pathname === "/favicon-32x32.png" ||
    url.pathname === "/apple-touch-icon.png" ||
    url.pathname === NOTIFICATION_ICON_URL ||
    url.pathname === "/android-chrome-512x512.png"
  ) {
    event.respondWith(
      caches.match(request).then(
        (cached) =>
          cached ||
          fetch(request).then((response) => {
            if (response.ok) {
              const clone = response.clone();
              caches.open(CACHE_NAME).then((cache) => cache.put(request, clone));
            }
            return response;
          }),
      ),
    );
    return;
  }

  // Everything else: network-only
});

self.addEventListener("message", (event) => {
  if (event.data?.type === "SKIP_WAITING") {
    event.waitUntil(self.skipWaiting());
  }
});

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data?.json() ?? {};
  } catch {
    const text = event.data?.text() ?? "";
    data = { title: "Waddle", body: text };
  }
  const unreadCount = parseUnreadCount(data);
  event.waitUntil(
    Promise.all([
      updateAppBadge(unreadCount),
      postUnreadCountToClients(unreadCount),
      self.registration.showNotification(data.title ?? "Waddle", {
        body: data.body ?? "",
        tag: data.roomJid,
        icon: NOTIFICATION_ICON_URL,
        data: { url: data.url ?? roomJidToPath(data.roomJid) },
      }),
    ]),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = event.notification.data?.url ?? "/";
  event.waitUntil(
    clients.matchAll({ type: "window", includeUncontrolled: true }).then((windowClients) => {
      for (const client of windowClients) {
        if (client.url.includes(self.location.origin) && "focus" in client) {
          return client.focus().then(() => client.navigate(url));
        }
      }
      return clients.openWindow(url);
    }),
  );
});

function roomJidToPath(roomJid) {
  if (typeof roomJid !== "string") return "/";
  const localpart = roomJid.split("@")[0] ?? "";
  const parts = localpart.split("_");
  if (parts.length < 2) return "/";
  return `/${encodeURIComponent(parts[0])}/${encodeURIComponent(parts[1])}`;
}

function parseUnreadCount(data) {
  const value = data?.unreadCount ?? data?.totalUnread ?? data?.["message-count"];
  if (value === undefined || value === null) return null;
  const parsed = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(parsed) || parsed < 0) return null;
  return Math.floor(parsed);
}

async function updateAppBadge(unreadCount) {
  if (unreadCount === null) return;
  try {
    if (unreadCount > 0 && typeof navigator.setAppBadge === "function") {
      await navigator.setAppBadge(unreadCount);
      return;
    }
    if (unreadCount === 0 && typeof navigator.clearAppBadge === "function") {
      await navigator.clearAppBadge();
    }
  } catch {
    // best-effort browser chrome integration
  }
}

async function postUnreadCountToClients(unreadCount) {
  if (unreadCount === null) return;
  try {
    const windowClients = await clients.matchAll({ type: "window", includeUncontrolled: true });
    for (const client of windowClients) {
      client.postMessage?.({ type: UNREAD_COUNT_MESSAGE_TYPE, unreadCount });
    }
  } catch {
    // best-effort open-window sync
  }
}

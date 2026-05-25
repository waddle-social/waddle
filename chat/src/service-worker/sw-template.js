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
    return;
  }
  // Foreground→SW dedup signal. The chat tab calls this from
  // `showMentionNotification` / `showDmNotification` when it renders
  // an in-band notification. Recording the id in `shownItems` lets the
  // subsequent SW `push` event for the same stanza suppress its
  // duplicate banner. See `chat/src/shell/notifications.ts`.
  if (event.data?.type === "waddle:item-shown") {
    const itemId = typeof event.data.itemId === "string" ? event.data.itemId : null;
    if (itemId) noteItemShown(itemId);
  }
});

/// Per-message dedup: when an in-band foreground notification has
/// already been rendered (chat tab open, message hits `showMentionNotification`
/// or `showDmNotification`), the SW push lands ~tens of ms later carrying
/// the same `item` id. Suppressing the SW notification by `item` avoids
/// a double-fire. Bounded to 256 entries with TTL-first, FIFO-fallback
/// eviction. Delete-then-set on touch so re-noting an item moves it to
/// the Map's tail (Map iteration order = insertion order; without the
/// delete a re-touch keeps the original position and the entry can be
/// evicted while still inside its TTL window under retry pressure).
const SHOWN_ITEM_TTL_MS = 60_000;
const SHOWN_ITEM_MAX = 256;
/// Maximum number of TTL-probe iterations per eviction. Bounds the
/// worst-case per-push CPU under bursts > SHOWN_ITEM_MAX items inside
/// the TTL window. 16 probes is enough to find an expired entry under
/// any non-pathological pattern; under a true burst we fall back to
/// FIFO head eviction in O(1).
const SHOWN_ITEM_EVICT_PROBE = 16;
const shownItems = new Map();
function noteItemShown(itemId) {
  if (!itemId) return;
  // delete-then-set so a re-noted item moves to the tail of the Map's
  // insertion order, keeping FIFO eviction recency-aware.
  if (shownItems.has(itemId)) shownItems.delete(itemId);
  shownItems.set(itemId, Date.now());
  if (shownItems.size > SHOWN_ITEM_MAX) {
    // Prefer to evict an already-expired entry over the oldest live
    // one; falls back to FIFO head when nothing has expired. Cap the
    // TTL probe at SHOWN_ITEM_EVICT_PROBE so a hostile burst (>
    // SHOWN_ITEM_MAX items inside SHOWN_ITEM_TTL_MS) doesn't degrade
    // every push to an O(SHOWN_ITEM_MAX) scan. Capped scan + FIFO
    // fallback yields O(SHOWN_ITEM_EVICT_PROBE) per eviction.
    const now = Date.now();
    let evicted = false;
    let probes = 0;
    for (const [key, at] of shownItems) {
      if (probes++ >= SHOWN_ITEM_EVICT_PROBE) break;
      if (now - at > SHOWN_ITEM_TTL_MS) {
        shownItems.delete(key);
        evicted = true;
        break;
      }
    }
    if (!evicted) {
      const oldestKey = shownItems.keys().next().value;
      if (oldestKey !== undefined) shownItems.delete(oldestKey);
    }
  }
}
function isItemAlreadyShown(itemId) {
  if (!itemId) return false;
  const at = shownItems.get(itemId);
  if (at === undefined) return false;
  if (Date.now() - at > SHOWN_ITEM_TTL_MS) {
    shownItems.delete(itemId);
    return false;
  }
  return true;
}

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data?.json() ?? {};
  } catch {
    // Non-JSON push body — treat as an empty envelope and render the
    // minimal "Waddle" notification (required by the Push API spec
    // for `userVisibleOnly: true` subscriptions).
    data = {};
  }
  // The server emits exactly the `v=1` envelope shape defined in
  // `crates/waddle-xmpp/src/push/envelope.rs::PushEnvelope`. There is
  // no migration window: a payload with `v !== 1` is from an
  // unsupported server build and is rendered as the minimal default.
  const parsed = parseV1Envelope(data) ?? {
    conversation: undefined,
    thread: undefined,
    class: undefined,
    item: undefined,
    unread: null,
  };
  // `context` is the navigation-only subset stashed on the
  // notification for the click handler; `item` (dedup key) and
  // `unread` stay local because they're transient transport metadata.
  const context = {
    conversation: parsed.conversation,
    thread: parsed.thread,
    class: parsed.class,
  };
  const unreadCount = parsed.unread;
  const itemId = parsed.item;
  const route = routeFromContext(context) ?? "/";
  // XEP-0357 §4 forbids the Push Service from receiving message
  // content, so the envelope never carries title/body. The SW always
  // renders the count-derived default title with an empty body.
  const title = defaultTitle(unreadCount);
  const body = "";
  const tag = context.conversation ?? "waddle";
  if (isItemAlreadyShown(itemId)) {
    // Foreground tab already rendered this item — only update the
    // badge / unread broadcast, no duplicate banner.
    event.waitUntil(
      Promise.all([updateAppBadge(unreadCount), postUnreadCountToClients(unreadCount)]),
    );
    return;
  }
  noteItemShown(itemId);
  event.waitUntil(
    Promise.all([
      updateAppBadge(unreadCount),
      postUnreadCountToClients(unreadCount),
      self.registration.showNotification(title, {
        body,
        tag,
        icon: NOTIFICATION_ICON_URL,
        data: { url: route, context },
      }),
    ]),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  // Defense in depth: ALWAYS resolve the data.url against our own
  // origin and reject cross-origin paths before opening a window.
  // `routeFromContext` always returns a relative `/dm/…` or `/r/…`
  // today, so this is a guard against a future regression in the
  // payload pipeline (or a poisoned legacy `data.url` field) that
  // would otherwise let `clients.openWindow` navigate to an
  // arbitrary origin.
  const safeUrl = sameOriginUrl(event.notification.data?.url) ?? "/";
  event.waitUntil(
    clients.matchAll({ type: "window", includeUncontrolled: true }).then((windowClients) => {
      for (const client of windowClients) {
        if (client.url.includes(self.location.origin) && "focus" in client) {
          return client.focus().then(() => client.navigate(safeUrl));
        }
      }
      return clients.openWindow(safeUrl);
    }),
  );
});

function sameOriginUrl(candidate) {
  if (typeof candidate !== "string" || candidate.length === 0) return null;
  try {
    const resolved = new URL(candidate, self.location.origin);
    return resolved.origin === self.location.origin ? `${resolved.pathname}${resolved.search}` : null;
  } catch {
    return null;
  }
}

function defaultTitle(unreadCount) {
  if (unreadCount === null || unreadCount === 0) return "Waddle";
  if (unreadCount === 1) return "1 new message";
  return `${unreadCount} new messages`;
}

/// Parse the `"v": 1` envelope emitted by
/// `crates/waddle-xmpp/src/push/envelope.rs::PushEnvelope`:
///
///   * `v`            — schema version (must equal 1)
///   * `class`        — granular NotificationClass (`"dm"`, `"personal_mention"`, …)
///   * `conversation` — DM peer bare JID or MUC room bare JID
///   * `thread`       — XEP-0201 thread id (optional)
///   * `item`         — originating stanza id (used for in-band dedup)
///   * `unread`       — XEP-0357 message-count snapshot (optional)
///
/// Returns `null` when the payload is not a v=1 envelope; the SW
/// renders a minimal "Waddle" notification in that case. There is no
/// legacy / mixed-version fallback — per the project's breaking-
/// changes-by-default rule, the server emits exactly this shape.
function parseV1Envelope(data) {
  if (!data || data.v !== 1) return null;
  const rawUnread = data.unread;
  let unread = null;
  if (rawUnread !== undefined && rawUnread !== null) {
    const parsed = typeof rawUnread === "number" ? rawUnread : Number(rawUnread);
    if (Number.isFinite(parsed) && parsed >= 0) {
      unread = Math.floor(parsed);
    }
  }
  return {
    conversation: typeof data.conversation === "string" ? data.conversation : undefined,
    thread: typeof data.thread === "string" && data.thread.length > 0 ? data.thread : undefined,
    class: typeof data.class === "string" ? data.class : undefined,
    item: typeof data.item === "string" ? data.item : undefined,
    unread,
  };
}

/// Convert a typed routing context to a URL the chat will navigate to
/// on click. The mapping mirrors the chat-side router exactly:
///   * DM    → `/dm/{username}` (router: `chat/src/router/routes/dm.ts`)
///   * MUC   → `/r/{channelId}`  (router: `chat/src/router/routes/channel.ts`)
///   * Thread is a `?thread=…` query string per
///     `chat/src/router/codecs.ts::threadSearch`, NOT a path segment.
///
/// DM-vs-MUC discrimination is driven by the typed `class` field —
/// these strings are pinned to
/// `NotificationClass::as_db_value` in
/// `crates/waddle-server/src/notification_outbox.rs`:
///   * `dm`, `dm_mention`                                    → DM
///   * `personal_mention`, `channel_mention`,
///     `active_channel_mention`, `notify_all`                → MUC
///
/// When `class` is absent (legacy publishes that predate the typed
/// envelope), fall back to the JID's domain prefix —
/// `muc.`/`conference.` are the standard XMPP MUC subdomains. JIDs
/// legally contain underscores per RFC 6122 / PRECIS, so the SW
/// must NOT treat an underscore in a DM JID's localpart as a
/// MUC separator.
const DM_CLASS_VALUES = new Set(["dm", "dm_mention"]);
const MUC_CLASS_VALUES = new Set([
  "personal_mention",
  "channel_mention",
  "active_channel_mention",
  "notify_all",
]);

function routeFromContext(context) {
  const conv = context?.conversation;
  // Return `null` (NOT `/`) when the typed context lacks a
  // conversation. The push handler maps `null` to `/` itself; null
  // here just signals "no routable target."
  if (typeof conv !== "string" || conv.length === 0) return null;
  const at = conv.lastIndexOf("@");
  if (at <= 0) return null;
  const localpart = conv.slice(0, at);
  const domain = conv.slice(at + 1);
  const cls = typeof context?.class === "string" ? context.class : "";
  const isDmByClass = DM_CLASS_VALUES.has(cls);
  const isMucByClass = MUC_CLASS_VALUES.has(cls);
  const isMucByDomain = domain.startsWith("muc.") || domain.startsWith("conference.");
  const isMuc = isMucByClass || (!isDmByClass && isMucByDomain);
  const base = isMuc
    ? `/r/${encodeURIComponent(localpart)}`
    : `/dm/${encodeURIComponent(localpart)}`;
  if (context.thread) {
    return `${base}?thread=${encodeURIComponent(context.thread)}`;
  }
  return base;
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

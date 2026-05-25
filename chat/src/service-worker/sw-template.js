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

/// Per-message dedup: when an in-band foreground notification has
/// already been rendered (chat tab open, message hits `showMentionNotification`
/// or `showDmNotification`), the SW push lands ~tens of ms later carrying
/// the same `item` id. Suppressing the SW notification by `item` avoids
/// a double-fire. Bounded to 256 entries via FIFO eviction.
const SHOWN_ITEM_TTL_MS = 60_000;
const SHOWN_ITEM_MAX = 256;
const shownItems = new Map();
function noteItemShown(itemId) {
  if (!itemId) return;
  shownItems.set(itemId, Date.now());
  if (shownItems.size > SHOWN_ITEM_MAX) {
    const oldestKey = shownItems.keys().next().value;
    if (oldestKey !== undefined) shownItems.delete(oldestKey);
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
    // Server side now sends a JSON envelope; a non-JSON body is a
    // legacy / misconfigured publish. Fall through to the minimal
    // notification with no payload-derived state.
    data = {};
  }
  // PR-D3: switch on the PR-D2 `"v": 1` envelope. Legacy publishers
  // that pre-date the v=1 schema fall through to
  // `parseLegacyRoutingContext`; both shapes feed the same typed
  // routing fields carried into `notificationclick.data.context`.
  // `item` (dedup key) and `unread` are sibling locals because they
  // are transient transport metadata, not navigation state, and
  // shouldn't reach the click handler.
  const parsed = parseV1Envelope(data) ?? parseLegacyRoutingContext(data);
  const context = {
    conversation: parsed.conversation,
    thread: parsed.thread,
    class: parsed.class,
  };
  const unreadCount = parsed.unread ?? null;
  const itemId = parsed.item;
  // Routing precedence:
  //   1. Typed `context.{conversation,thread,class}` envelope.
  //   2. Legacy `data.url` if it's a same-origin path — kept for
  //      mixed-version publishers that may still ship a deep link in
  //      the top-level `url` field. `sameOriginUrl` rejects cross-
  //      origin and javascript: URLs.
  //   3. Final fallback to `/`.
  const route = routeFromContext(context)
    ?? sameOriginUrl(typeof data.url === "string" ? data.url : null)
    ?? "/";
  // Minimal default: no sender, no body preview. XEP-0357 §4 forbids
  // the push service from receiving message content, so the v=1
  // envelope never carries `title`/`body`. The legacy fallback path
  // still honors them.
  const title = typeof data.title === "string" && data.title.length > 0
    ? data.title
    : defaultTitle(unreadCount);
  const body = typeof data.body === "string" ? data.body : "";
  const tag = context.conversation ?? data.roomJid ?? "waddle";
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

/// Parse the PR-D2 `"v": 1` envelope emitted by
/// `crates/waddle-xmpp/src/push/envelope.rs::PushEnvelope`. Field shape:
///
///   * `v`            — schema version (must equal 1)
///   * `class`        — granular NotificationClass (`"dm"`, `"personal_mention"`, …)
///   * `conversation` — DM peer bare JID or MUC room bare JID
///   * `thread`       — XEP-0201 thread id (optional)
///   * `item`         — originating stanza id (used for in-band dedup)
///   * `unread`       — XEP-0357 message-count snapshot (optional)
///
/// Returns `null` when the envelope is absent (legacy publish, or a
/// non-v=1 schema bump the server side may roll out later); the caller
/// falls back to `parseLegacyRoutingContext`.
function parseV1Envelope(data) {
  if (!data || data.v !== 1) return null;
  return {
    conversation: typeof data.conversation === "string" ? data.conversation : undefined,
    thread: typeof data.thread === "string" && data.thread.length > 0 ? data.thread : undefined,
    class: typeof data.class === "string" ? data.class : undefined,
    item: typeof data.item === "string" ? data.item : undefined,
    unread: parseUnreadCount(data),
  };
}

/// Pre-PR-D2 routing fallback. Legacy server builds wrote a nested
/// `data.context = { conversation, thread, class }` object alongside
/// `data.roomJid`. Kept until every reachable server is on the v=1
/// envelope; remove once telemetry confirms no `v` absent / != 1
/// publishes are landing.
function parseLegacyRoutingContext(data) {
  const context = data?.context;
  if (context && typeof context === "object") {
    return {
      conversation: typeof context.conversation === "string" ? context.conversation : undefined,
      thread: typeof context.thread === "string" && context.thread.length > 0 ? context.thread : undefined,
      class: typeof context.class === "string" ? context.class : undefined,
      item: undefined,
      unread: parseUnreadCount(data),
    };
  }
  return {
    conversation: typeof data?.roomJid === "string" ? data.roomJid : undefined,
    thread: undefined,
    class: undefined,
    item: undefined,
    unread: parseUnreadCount(data),
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
  // conversation, so the caller can fall back to a legacy `data.url`
  // if one is present rather than collapsing to root. ChatGPT Codex
  // bot flagged this regression: pre-#528 SW used `data.url`
  // directly when present; we shouldn't silently drop that path
  // for mixed-version publishers.
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

function parseUnreadCount(data) {
  // Prefer the PR-D2 v=1 envelope's `unread` field; fall back to the
  // pre-D2 wire names so a server emitting either shape still surfaces
  // a count.
  const value =
    data?.unread ??
    data?.messageCount ??
    data?.["message-count"] ??
    data?.unreadCount ??
    data?.totalUnread;
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

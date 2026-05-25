import type { BrowserXmppClient } from "@/lib/xmpp-client";

/// localStorage key prefix for the cached VAPID advertisement, namespaced
/// per `(account_jid, server_jid)`. localStorage is sufficient here: each
/// entry is ~120 bytes, all tabs share it, and the cache hit-rate is high
/// (chat fetches once per (account, server) until the server rotates).
const CACHE_PREFIX = "waddle.chat.vapid-cache:";

/// Per-entry max age. The chat re-fetches at startup and after the
/// rotation lock fires; this TTL bounds how stale a cache entry can get
/// when neither happens (long-running tab on a server that silently
/// rotated). 1 h is a deliberate trade between (a) avoiding a re-fetch
/// per reconnect on a non-rotated server and (b) shrinking the window
/// during which a stale-but-cached key keeps the chat from picking up a
/// rotation that lands inside the TTL. The kid check on every
/// `ensureBrowserSubscriptionWithCurrentKey` call catches most rotations
/// regardless of TTL; the TTL is the backstop for long-running tabs.
const CACHE_MAX_AGE_MS = 60 * 60 * 1000;

interface CachedVapidEntry {
  publicKey: string;
  kid: string;
  fetchedAtMs: number;
}

function storageKey(accountJid: string, serverJid: string): string {
  // JIDs are UTF-8 with broad allowed characters; a literal separator
  // could collide if either side legally contained it. `JSON.stringify`
  // on an array gives an unambiguous two-element encoding regardless of
  // the individual values.
  return `${CACHE_PREFIX}${JSON.stringify([accountJid, serverJid])}`;
}

function isCachedVapidEntry(value: unknown): value is CachedVapidEntry {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.publicKey === "string" &&
    candidate.publicKey.length > 0 &&
    typeof candidate.kid === "string" &&
    candidate.kid.length > 0 &&
    typeof candidate.fetchedAtMs === "number" &&
    Number.isFinite(candidate.fetchedAtMs)
  );
}

function readCacheEntry(accountJid: string, serverJid: string): CachedVapidEntry | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(storageKey(accountJid, serverJid));
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!isCachedVapidEntry(parsed)) return null;
    return parsed;
  } catch {
    return null;
  }
}

function writeCacheEntry(
  accountJid: string,
  serverJid: string,
  entry: CachedVapidEntry,
): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(storageKey(accountJid, serverJid), JSON.stringify(entry));
  } catch {
    // Storage quota / private-mode rejection — chat continues without
    // a cache hit on the next call. Not surfaced to the user.
  }
}

export function clearCachedVapidKey(accountJid: string, serverJid: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(storageKey(accountJid, serverJid));
  } catch {
    // best-effort
  }
}

interface LoadVapidPublicKeyOptions {
  client: BrowserXmppClient;
  accountJid: string;
  serverJid: string;
  forceRefresh?: boolean;
  now?: () => number;
}

/**
 * Resolve the Push Service's current VAPID public key for `(account, server)`,
 * fetching via the XEP-0128 disco form when the cache is empty or stale.
 *
 * Returns `null` when the server reachably advertises no VAPID form
 * (Web Push not configured) so the caller can degrade to the foreground
 * Notification API path without raising a UI error.
 */
export async function loadVapidPublicKey(
  opts: LoadVapidPublicKeyOptions,
): Promise<CachedVapidEntry | null> {
  const now = opts.now ?? Date.now;
  if (!opts.forceRefresh) {
    const cached = readCacheEntry(opts.accountJid, opts.serverJid);
    if (cached && now() - cached.fetchedAtMs < CACHE_MAX_AGE_MS) {
      return cached;
    }
  }
  const fresh = await opts.client.fetchVapidPublicKey({ serviceJid: opts.serverJid });
  if (!fresh) return null;
  const entry: CachedVapidEntry = {
    publicKey: fresh.publicKey,
    kid: fresh.kid,
    fetchedAtMs: now(),
  };
  writeCacheEntry(opts.accountJid, opts.serverJid, entry);
  return entry;
}

/** Exposed for tests so they can pin a deterministic Date.now. */
export const VAPID_CACHE_MAX_AGE_MS = CACHE_MAX_AGE_MS;

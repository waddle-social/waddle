/// Account+service-scoped persistence of the XEP-0050 `register-device`
/// outcome: the assigned XEP-0357 push node id and the Push
/// Service-assigned device id. The chat needs BOTH later — the node id
/// flows into the user-server XEP-0357 `<enable/>`, the device id scopes
/// the per-device `disable-device` opt-out.
///
/// Keyed per `(accountJid, serviceJid)` — the same `JSON.stringify`
/// encoding `vapid-cache.ts` and the persisted-kid use — so a shared
/// browser driven by two accounts can't read back each other's
/// node/device ids. The server rejects a foreign `(node, device-id)`
/// with `forbidden`/`item-not-found`, but an account-global key would
/// still let account B's stale ids drive a doomed `disable-device`
/// against account A's row, surfacing confusing errors. Scoping the key
/// keeps each account's local registration state isolated.
///
/// SSR-guarded and `try/catch`-wrapped: Safari Lockdown Mode and some
/// hardened Firefox/Brave configs throw `SecurityError` on
/// `window.localStorage` access itself, which must not kill the push
/// flow.

const DEVICE_ID_STORAGE_PREFIX = "waddle.chat.push-device-id:";
const PUSH_NODE_STORAGE_PREFIX = "waddle.chat.push-node-id:";

function deviceIdKey(accountJid: string, serviceJid: string): string {
  return `${DEVICE_ID_STORAGE_PREFIX}${JSON.stringify([accountJid, serviceJid])}`;
}

function pushNodeKey(accountJid: string, serviceJid: string): string {
  return `${PUSH_NODE_STORAGE_PREFIX}${JSON.stringify([accountJid, serviceJid])}`;
}

function setItem(key: string, value: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(key, value);
  } catch (error) {
    console.warn("[push-device-store] localStorage unavailable; value not persisted:", error);
  }
}

function getItem(key: string): string | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function removeItem(key: string): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(key);
  } catch {
    // best-effort cleanup
  }
}

export function persistDeviceId(accountJid: string, serviceJid: string, deviceId: string): void {
  setItem(deviceIdKey(accountJid, serviceJid), deviceId);
}

export function loadDeviceId(accountJid: string, serviceJid: string): string | null {
  return getItem(deviceIdKey(accountJid, serviceJid));
}

export function clearDeviceId(accountJid: string, serviceJid: string): void {
  removeItem(deviceIdKey(accountJid, serviceJid));
}

export function persistPushNodeId(accountJid: string, serviceJid: string, node: string): void {
  setItem(pushNodeKey(accountJid, serviceJid), node);
}

export function loadPushNodeId(accountJid: string, serviceJid: string): string | null {
  return getItem(pushNodeKey(accountJid, serviceJid));
}

export function clearPushNodeId(accountJid: string, serviceJid: string): void {
  removeItem(pushNodeKey(accountJid, serviceJid));
}

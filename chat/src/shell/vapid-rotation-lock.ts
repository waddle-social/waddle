/// Cross-tab exclusive lock for the VAPID-key rotation flow.
///
/// When the chat detects a kid mismatch between the server-advertised
/// VAPID public key and the locally-cached one, only one tab should
/// drive the re-subscribe sequence; otherwise multiple tabs would race
/// to `pushManager.unsubscribe()` and `pushManager.subscribe()` against
/// the same browser-level push registration, producing log noise and a
/// brief window where some tabs hold a stale subscription.
///
/// `navigator.locks` (Web Locks API) is the canonical mechanism and is
/// available everywhere the chat targets (Chrome 69+, Firefox 96+,
/// Safari 15.4+). When absent — older iOS Safari builds, certain SSR
/// or test environments — we fall back to an in-process Promise-chain
/// serializer. The fallback is best-effort, but the operation it
/// guards is itself idempotent (calling `subscribe({ applicationServerKey })`
/// twice with the same key returns the same browser-level subscription),
/// so a missed lock just produces redundant work, not incorrect state.

const LOCK_NAME = "waddle:push:vapid-rotate";

type RunInLock<T> = () => Promise<T>;

interface NavigatorWithLocks {
  locks: {
    request: <T>(
      name: string,
      options: { mode: "exclusive" } | { mode: "shared" },
      callback: () => Promise<T>,
    ) => Promise<T>;
  };
}

function hasWebLocks(): NavigatorWithLocks | null {
  if (typeof navigator === "undefined") return null;
  // `navigator.locks` is the Web Locks API surface — present everywhere
  // the chat targets except the oldest iOS Safari versions. We probe
  // existence rather than user-agent sniffing so test environments that
  // explicitly stub `navigator.locks = undefined` exercise the fallback.
  const candidate = (navigator as unknown as { locks?: NavigatorWithLocks["locks"] }).locks;
  if (!candidate || typeof candidate.request !== "function") return null;
  return navigator as unknown as NavigatorWithLocks;
}

/// In-process Promise chain used when `navigator.locks` is unavailable.
/// One chain per process (=== one chain per tab); cross-tab races are
/// handled correctness-wise by the idempotence of PushManager.subscribe.
let fallbackChain: Promise<unknown> = Promise.resolve();

async function withFallbackLock<T>(callback: RunInLock<T>): Promise<T> {
  const next = fallbackChain.then(() => callback());
  // Swallow rejections in the chain itself so a failing rotation doesn't
  // permanently block subsequent acquires. The original promise is
  // returned to the caller so the error still propagates.
  fallbackChain = next.catch(() => undefined);
  return next;
}

/**
 * Run `callback` while holding the VAPID-rotation lock. The lock is
 * automatically released when the returned Promise settles. Re-entrant
 * calls within the same tab are serialized; calls from different tabs
 * are serialized through `navigator.locks` when available.
 */
export async function withVapidRotationLock<T>(callback: RunInLock<T>): Promise<T> {
  const navWithLocks = hasWebLocks();
  if (!navWithLocks) {
    return withFallbackLock(callback);
  }
  return navWithLocks.locks.request(LOCK_NAME, { mode: "exclusive" }, callback);
}

/** Exposed for the test suite to reset the fallback chain between tests. */
export function __resetVapidRotationLockForTests(): void {
  fallbackChain = Promise.resolve();
}

/** Exposed so tests can assert on the lock name used at the platform layer. */
export const VAPID_ROTATION_LOCK_NAME = LOCK_NAME;

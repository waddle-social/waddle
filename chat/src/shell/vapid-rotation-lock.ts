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
/// Safari 15.4+). When absent — SSR, test environments, or older
/// browsers — we fall back to an in-process Promise-chain serializer.
/// This is best-effort; the fallback does NOT prevent cross-tab races.
///
/// Cross-tab race window when running on the fallback: two tabs may
/// each call `unsubscribe()` + `subscribe(newKey)` concurrently. Both
/// tabs end up with valid subscriptions, but the SECOND `subscribe`
/// call replaces the first inside the browser's push registration —
/// so the first tab's `register-device` IQ will have written
/// credentials that are immediately stale. The caller layer
/// (`syncPushSubscriptionImpl`) re-issues `register-device` on every
/// reconnect so the stale row gets overwritten on the next sync; the
/// transient inconsistency is bounded by one reconnect.

const LOCK_NAME_PREFIX = "waddle:push:vapid-rotate";

/// Build the per-account lock name. Different accounts in different
/// tabs MUST NOT serialize through one global lock — the rotation flows
/// are independent, and a slow rotation on account A would otherwise
/// hold up an unrelated subscribe on account B. The account JID is
/// already validated upstream (bare JID for the active session), so
/// embedding it verbatim is safe.
function lockNameFor(accountJid: string | undefined): string {
  return accountJid ? `${LOCK_NAME_PREFIX}:${accountJid}` : LOCK_NAME_PREFIX;
}

type RunInLock<T> = () => Promise<T>;

/// Minimal subset of the Web Locks API `Lock` interface — sufficient to
/// type the callback signature without pulling the full `lib.dom`
/// type. Callers ignore the argument today; preserved on the
/// signature so a future caller that needs `lock.mode` / `lock.name`
/// can read them without re-typing.
interface WebLock {
  readonly mode: "exclusive" | "shared";
  readonly name: string;
}

interface NavigatorWithLocks {
  locks: {
    request: <T>(
      name: string,
      options: { mode: "exclusive" } | { mode: "shared" },
      callback: (lock: WebLock | null) => Promise<T>,
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

/// In-process Promise chains used when `navigator.locks` is
/// unavailable. One chain per lock name (per account) so different
/// accounts in the same tab don't serialize through each other —
/// matching the `navigator.locks` path which uses a distinct lock
/// name per account. The map is keyed by the resolved lock name
/// (`lockNameFor(accountJid)`); a missing entry is equivalent to an
/// already-resolved chain. Cross-tab races on the fallback path
/// remain best-effort: a concurrent rotation in another tab CAN
/// produce a transient stale `register-device` row, recovered on
/// the next `syncPushSubscriptionImpl` (i.e. next reconnect).
const fallbackChains = new Map<string, Promise<unknown>>();

async function withFallbackLock<T>(
  lockName: string,
  callback: RunInLock<T>,
): Promise<T> {
  const prev = fallbackChains.get(lockName) ?? Promise.resolve();
  const next = prev.then(() => callback());
  // Use a single `then(success, failure)` form to register the chain's
  // continuation atomically. The two-step `next.catch(...)` pattern
  // would create a brief window in strict unhandledrejection runtimes
  // (Node 15+ with `--unhandled-rejections=strict`, some test runners)
  // where the `.catch` handler hasn't been attached yet — a microtask-
  // synchronous rejection from `callback()` could fire an
  // unhandledrejection event before the `.catch` registers and swallows
  // it. Using `then(_, _)` attaches both handlers in one step.
  fallbackChains.set(
    lockName,
    next.then(
      () => undefined,
      () => undefined,
    ),
  );
  return next;
}

/**
 * Run `callback` while holding the VAPID-rotation lock for `accountJid`.
 * The lock is automatically released when the returned Promise settles.
 * Re-entrant calls within the same tab are serialized; calls from
 * different tabs are serialized through `navigator.locks` when
 * available. Different accounts use distinct lock names so cross-
 * account rotations don't block each other.
 */
export async function withVapidRotationLock<T>(
  accountJid: string | undefined,
  callback: RunInLock<T>,
): Promise<T> {
  const lockName = lockNameFor(accountJid);
  const navWithLocks = hasWebLocks();
  if (!navWithLocks) {
    return withFallbackLock(lockName, callback);
  }
  return navWithLocks.locks.request(lockName, { mode: "exclusive" }, () => callback());
}

/** Exposed for the test suite to reset the fallback chains between tests. */
export function __resetVapidRotationLockForTests(): void {
  fallbackChains.clear();
}

/** Exposed so tests can assert on the lock-name shape used at the platform layer. */
export function vapidRotationLockNameForTests(accountJid: string | undefined): string {
  return lockNameFor(accountJid);
}

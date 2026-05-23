/**
 * Per-instance serialization helper for the Push Service enable /
 * disable flows in `notifications.ts`. Round-4 hostile-client
 * adversarial review on PR #760 found a real race: a rapid
 * Enable→Disable toggle could interleave the multi-step flows,
 * leaving a registered device with no UI affordance to remove it.
 *
 * Usage:
 *
 * ```ts
 * const lock = createPushFlowLock();
 * lock.run(() => syncPushSubscriptionImpl(...));
 * lock.run(() => disablePushSubscriptionImpl(...));
 * ```
 *
 * The two calls serialize: the second `run` waits for the first
 * to settle (resolve OR reject) before invoking its work fn.
 * The lock chain rejects-quiet on each step so a thrown work fn
 * doesn't poison subsequent runs, but the returned promise still
 * carries the original rejection back to the caller.
 *
 * Memory: each chain link is GC-eligible as soon as it resolves
 * and the next link's reassignment drops the prior reference.
 */
interface PushFlowLock {
  run<T>(work: () => Promise<T>): Promise<T>;
}

export function createPushFlowLock(): PushFlowLock {
  let chain: Promise<unknown> = Promise.resolve();
  return {
    run<T>(work: () => Promise<T>): Promise<T> {
      const next = chain.then(work, work);
      chain = next.catch(() => undefined);
      return next;
    },
  };
}

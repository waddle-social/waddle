// Pin the round-4 push-flow mutex. Removing `createPushFlowLock`
// or changing its serialization semantics breaks this test.
//
// The helper is extracted into `chat/src/shell/push-flow-lock.ts`
// (see the doc comment there) precisely so this test doesn't have
// to stub the entire WASM + ServiceWorker + Notification API
// surface that `usePushNotifications` would otherwise need.

import { describe, expect, test } from "bun:test";
import { createPushFlowLock } from "../src/shell/push-flow-lock";

function deferred<T>() {
  let resolveFn: (value: T) => void = () => {};
  let rejectFn: (reason: unknown) => void = () => {};
  const promise = new Promise<T>((resolve, reject) => {
    resolveFn = resolve;
    rejectFn = reject;
  });
  return { promise, resolve: resolveFn, reject: rejectFn };
}

describe("createPushFlowLock", () => {
  test("serializes two overlapping runs in FIFO order", async () => {
    const lock = createPushFlowLock();
    const order: string[] = [];
    const firstGate = deferred<void>();

    const first = lock.run(async () => {
      order.push("first-enter");
      await firstGate.promise;
      order.push("first-exit");
      return 1;
    });
    // Kick off second BEFORE the first resolves. It MUST wait.
    const second = lock.run(async () => {
      order.push("second-enter");
      order.push("second-exit");
      return 2;
    });
    // Yield once. The first work is awaiting the gate; the second
    // MUST NOT have started.
    await Promise.resolve();
    expect(order).toEqual(["first-enter"]);

    firstGate.resolve();
    const [firstValue, secondValue] = await Promise.all([first, second]);
    expect(firstValue).toBe(1);
    expect(secondValue).toBe(2);
    expect(order).toEqual([
      "first-enter",
      "first-exit",
      "second-enter",
      "second-exit",
    ]);
  });

  test("rejection in one run does not poison the chain", async () => {
    const lock = createPushFlowLock();
    const failing = lock.run<number>(async () => {
      throw new Error("boom");
    });
    let caught: unknown = null;
    try {
      await failing;
    } catch (err) {
      caught = err;
    }
    expect((caught as Error)?.message).toBe("boom");

    // Subsequent run still works.
    const succeeded = await lock.run(async () => 42);
    expect(succeeded).toBe(42);
  });

  test("propagates the work-fn's rejection to the caller", async () => {
    const lock = createPushFlowLock();
    await expect(
      lock.run(async () => {
        throw new Error("upstream");
      }),
    ).rejects.toThrow("upstream");
  });

  test("chain remains FIFO across more than two runs", async () => {
    const lock = createPushFlowLock();
    const log: number[] = [];
    const promises = [1, 2, 3, 4, 5].map((n) =>
      lock.run(async () => {
        log.push(n);
        // Force a microtask hop so a missing serialization would
        // produce out-of-order pushes.
        await Promise.resolve();
      }),
    );
    await Promise.all(promises);
    expect(log).toEqual([1, 2, 3, 4, 5]);
  });
});
